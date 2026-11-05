using System.Collections.Concurrent;

namespace ClipBridgeShell_CS.Services;

public sealed class LocalContentRef
{
    public required string TransferId { get; init; }
    public string? ItemId { get; init; }
    public string? TextUtf8 { get; init; }
    public string? LocalPath { get; init; }
    public string? Mime { get; init; }
}

public sealed class ContentFetchAwaiter
{
    /// <summary>
    /// 内容获取超时（10s）。Core 可能在 TRANSFER_FAILED / CONTENT_CACHED 之前因网络问题长期静默，
    /// 而 AsyncRelayCommand 不可重入：无限等待会让复制/锁定命令在进程剩余生命周期内一直禁用。
    /// </summary>
    public static readonly TimeSpan Timeout = TimeSpan.FromSeconds(10);

    private readonly ConcurrentDictionary<string, TaskCompletionSource<LocalContentRef>> _waiters = new();
    private readonly ConcurrentDictionary<string, LocalContentRef> _pendingLocal = new();
    // 失败缓存：TRANSFER_FAILED 可能早于 WaitAsync 注册到达（EnsureContentCachedAsync 返回后、
    // 调用方注册等待前），没有这项缓存时该失败会丢失，下一次等待将永远挂起。
    private readonly ConcurrentDictionary<string, Exception> _pendingFailures = new();

    public Task<LocalContentRef> WaitAsync(string transferId, CancellationToken ct)
        => WaitAsync(transferId, ct, Timeout);

    /// <summary>
    /// 等待指定 transfer 的内容；<paramref name="timeout"/> 仅供测试缩短，生产路径用 <see cref="Timeout"/>。
    /// </summary>
    public async Task<LocalContentRef> WaitAsync(string transferId, CancellationToken ct, TimeSpan timeout)
    {
        var tcs = new TaskCompletionSource<LocalContentRef>(TaskCreationOptions.RunContinuationsAsynchronously);
        var existing = _waiters.GetOrAdd(transferId, tcs);
        if (existing != tcs)
        {
            // 极端并发：同一 transfer_id 已有等待者，复用同一个结果（超时/取消由首个等待者收尾）
            return await existing.Task.ConfigureAwait(false);
        }

        // 先注册等待者、再检查缓存：Resolve/Fail 无论发生在注册前还是注册后都不会丢失。
        if (_pendingFailures.TryRemove(transferId, out var earlyFailure))
        {
            RemoveWaiter(transferId, tcs);
            tcs.TrySetException(earlyFailure);
            return await tcs.Task.ConfigureAwait(false);
        }

        if (_pendingLocal.TryRemove(transferId, out var earlyLocal))
        {
            RemoveWaiter(transferId, tcs);
            tcs.TrySetResult(earlyLocal);
            return await tcs.Task.ConfigureAwait(false);
        }

        // 超时与调用方取消都会收尾等待者，避免 AsyncRelayCommand 永久失活。
        using var registration = ct.Register(() =>
        {
            if (RemoveWaiter(transferId, tcs))
                tcs.TrySetCanceled(ct);
        });

        try
        {
            var timeoutTask = Task.Delay(timeout, CancellationToken.None);
            var winner = await Task.WhenAny(tcs.Task, timeoutTask).ConfigureAwait(false);
            if (winner != tcs.Task && RemoveWaiter(transferId, tcs))
            {
                tcs.TrySetException(new TimeoutException(
                    $"content fetch timed out after {timeout.TotalSeconds:0.#}s (transfer_id={transferId})"));
            }

            return await tcs.Task.ConfigureAwait(false);
        }
        finally
        {
            // 显式释放注册，避免 transfer 完成后仍持有调用方的 CancellationToken 回调
            registration.Dispose();
        }
    }

    public void Resolve(string transferId, LocalContentRef localRef)
    {
        if (_waiters.TryRemove(transferId, out var w))
        {
            w.TrySetResult(localRef);
            return;
        }

        // 没有等待者：缓存成功结果（同时清掉更早的失败，保持“最后一次终态”语义）
        _pendingFailures.TryRemove(transferId, out _);
        _pendingLocal[transferId] = localRef;
    }

    public void Fail(string transferId, Exception ex)
    {
        if (_waiters.TryRemove(transferId, out var w))
        {
            w.TrySetException(ex);
            return;
        }

        // 没有等待者：缓存失败，留给下一次 WaitAsync 立即抛出（否则失败事件丢失）
        _pendingLocal.TryRemove(transferId, out _);
        _pendingFailures[transferId] = ex;
    }

    public void NotifyContentCached(LocalContentRef local) => Resolve(local.TransferId, local);

    /// <summary>
    /// 仅在字典里仍是本 TCS 时才移除；并发复用同一 TCS 时恢复条目，避免影响其他等待者。
    /// </summary>
    private bool RemoveWaiter(string transferId, TaskCompletionSource<LocalContentRef> tcs)
    {
        if (!_waiters.TryRemove(transferId, out var removed))
            return false;

        if (ReferenceEquals(removed, tcs))
            return true;

        _waiters.TryAdd(transferId, removed);
        return false;
    }
}
