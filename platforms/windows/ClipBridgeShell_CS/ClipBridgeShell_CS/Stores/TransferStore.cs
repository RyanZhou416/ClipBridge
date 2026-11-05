using System.Collections.Concurrent;
using System.Collections.ObjectModel;
using ClipBridgeShell_CS.Core.Models.Events;
using Microsoft.UI.Dispatching;

namespace ClipBridgeShell_CS.Stores;

public class TransferStore
{
    /// <summary>集合上限：防止长时间运行后无限增长。</summary>
    public const int MaxTransfers = 200;

    /// <summary>终态（completed/failed/cancelled）行在集合中的最长保留时间。</summary>
    public const int TerminalRetentionMs = 120_000;

    // UI 绑定这个集合来显示进度条
    public ObservableCollection<TransferUpdatePayload> Transfers { get; } = new();
    private readonly ConcurrentDictionary<string, TaskCompletionSource<ContentCachedPayload>> _contentWaiters = new();
    private readonly ConcurrentDictionary<string, ContentCachedPayload> _pendingCached = new();

    // 终态行的进入时刻（仅用于淘汰）；与 Transfers 一样只在 UI 线程/同一处理线程访问
    private readonly Dictionary<string, long> _terminalSinceMs = new();

    /// <summary>
    /// 按 TransferId 幂等更新一行传输记录。
    /// 终态行不会被迟到的非终态更新回退（事件乱序/重试保护）。
    /// </summary>
    public void Upsert(TransferUpdatePayload update)
    {
        if (update == null || string.IsNullOrWhiteSpace(update.TransferId))
            return;

        TryEnqueue(() =>
        {
            var now = NowMs();
            var existing = Transfers.FirstOrDefault(t => t.TransferId == update.TransferId);

            if (existing != null)
            {
                if (IsTerminalState(existing.State) && !IsTerminalState(update.State))
                    return;

                // 只替换命中的这一行（Replace 通知），避免整表 Reset 造成 UI 抖动
                var index = Transfers.IndexOf(existing);
                Transfers[index] = update;
            }
            else
            {
                Transfers.Add(update);
            }

            if (IsTerminalState(update.State))
                _terminalSinceMs[update.TransferId] = now;
            else
                _terminalSinceMs.Remove(update.TransferId);

            EvictExpiredTerminal(now);
            TrimToMax();
        });
    }

    /// <summary>兼容旧调用名。</summary>
    public void Update(TransferUpdatePayload update) => Upsert(update);

    private static long NowMs() => Environment.TickCount64;

    private static bool IsTerminalState(string? state) =>
        string.Equals(state, "completed", StringComparison.OrdinalIgnoreCase)
        || string.Equals(state, "failed", StringComparison.OrdinalIgnoreCase)
        || string.Equals(state, "cancelled", StringComparison.OrdinalIgnoreCase);

    private void EvictExpiredTerminal(long nowMs)
    {
        if (_terminalSinceMs.Count == 0)
            return;

        var expired = _terminalSinceMs
            .Where(kv => nowMs - kv.Value >= TerminalRetentionMs)
            .Select(kv => kv.Key)
            .ToList();

        foreach (var transferId in expired)
            RemoveTransfer(transferId);
    }

    private void TrimToMax()
    {
        while (Transfers.Count > MaxTransfers)
        {
            // 优先淘汰集合中最靠前（最老）的终态行
            var terminal = Transfers.FirstOrDefault(t => IsTerminalState(t.State));
            if (terminal != null)
            {
                RemoveTransfer(terminal.TransferId);
                continue;
            }

            // 没有终态行可回收时，丢弃最旧的一条，保证硬上限
            RemoveTransfer(Transfers[0].TransferId);
        }
    }

    private void RemoveTransfer(string transferId)
    {
        var existing = Transfers.FirstOrDefault(t => t.TransferId == transferId);
        if (existing != null)
            Transfers.Remove(existing);
        _terminalSinceMs.Remove(transferId);
    }

    private void TryEnqueue(DispatcherQueueHandler handler)
    {
        DispatcherQueue? dispatcher = null;

        try
        {
            // 加固：单元测试或非 UI 线程环境下 App.MainWindow 可能不可用
            if (Microsoft.UI.Xaml.Application.Current != null)
                dispatcher = App.MainWindow?.DispatcherQueue;
        }
        catch
        {
            dispatcher = null;
        }

        if (dispatcher != null && !dispatcher.HasThreadAccess)
        {
            dispatcher.TryEnqueue(handler);
        }
        else
        {
            handler();
        }
    }

    public Task<ContentCachedPayload> WaitForContentCachedAsync(string transferId, CancellationToken ct)
    {
        if (_pendingCached.TryRemove(transferId, out var cached))
            return Task.FromResult(cached);
        var tcs = new TaskCompletionSource<ContentCachedPayload>(TaskCreationOptions.RunContinuationsAsynchronously);

        if (!_contentWaiters.TryAdd(transferId, tcs))
            throw new InvalidOperationException($"Duplicate waiter for transfer_id={transferId}");

        ct.Register(() =>
        {
            if (_contentWaiters.TryRemove(transferId, out var existing))
                existing.TrySetCanceled(ct);
        });

        return tcs.Task;
    }

    public void NotifyContentCached(ContentCachedPayload payload)
    {
        if (_contentWaiters.TryRemove(payload.TransferId, out var tcs))
        {
            tcs.TrySetResult(payload);
            return;
        }

        // 没有 waiter：缓存起来，等待 WaitForContentCachedAsync 消费
        _pendingCached[payload.TransferId] = payload;

    }

    public void NotifyTransferUpdate(TransferUpdatePayload t)
    {
        // 可选：失败时提前结束 waiter
        if ((t.State ?? "").Equals("failed", StringComparison.OrdinalIgnoreCase)
            && !string.IsNullOrWhiteSpace(t.TransferId)
            && _contentWaiters.TryRemove(t.TransferId, out var tcs))
        {
            tcs.TrySetException(new InvalidOperationException($"Transfer failed: {t.Code} {t.Message}".Trim()));
        }
    }
}
