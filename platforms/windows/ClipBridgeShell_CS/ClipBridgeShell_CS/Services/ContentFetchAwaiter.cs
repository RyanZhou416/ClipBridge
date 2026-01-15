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
    private readonly ConcurrentDictionary<string, TaskCompletionSource<LocalContentRef>> _waiters = new();
    private readonly ConcurrentDictionary<string, LocalContentRef> _pendingLocal = new();


    public Task<LocalContentRef> WaitAsync(string transferId, CancellationToken ct)
    {
        if (_pendingLocal.TryRemove(transferId, out var local))
            return Task.FromResult(local);
        var tcs = new TaskCompletionSource<LocalContentRef>(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!_waiters.TryAdd(transferId, tcs))
        {
            // 极端情况：并发插入，尝试复用或抛错
            if (_waiters.TryGetValue(transferId, out var existing))
                return existing.Task;
        }

        if (ct.CanBeCanceled)
        {
            ct.Register(() =>
            {
                if (_waiters.TryRemove(transferId, out var w))
                    w.TrySetCanceled(ct);
            });
        }
        return tcs.Task;
    }

    public void Resolve(string transferId, LocalContentRef localRef)
    {
        if (_waiters.TryRemove(transferId, out var w))
        {
            w.TrySetResult(localRef);
        }
        else
        {
            // 如果没有人等待，必须存入 Pending，否则后续的 WaitAsync 会死锁
            _pendingLocal[transferId] = localRef;
        }
    }

    public void Fail(string transferId, Exception ex)
    {
        if (_waiters.TryRemove(transferId, out var w))
            w.TrySetException(ex);
    }

    public void NotifyContentCached(LocalContentRef local)
    {
        if (_waiters.TryRemove(local.TransferId, out var tcs))
        {
            tcs.TrySetResult(local);
            return;
        }

        _pendingLocal[local.TransferId] = local;
    }
}
