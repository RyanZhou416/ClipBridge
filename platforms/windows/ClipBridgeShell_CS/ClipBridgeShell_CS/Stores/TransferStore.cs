using System.Collections.Concurrent;
using System.Collections.ObjectModel;
using ClipBridgeShell_CS.Core.Models.Events;
using Microsoft.UI.Dispatching;

namespace ClipBridgeShell_CS.Stores;

public class TransferStore
{
    // UI 绑定这个集合来显示进度条
    public ObservableCollection<TransferUpdatePayload> Transfers { get; } = new();
    private readonly ConcurrentDictionary<string, TaskCompletionSource<ContentCachedPayload>> _contentWaiters = new();
    private readonly ConcurrentDictionary<string, ContentCachedPayload> _pendingCached = new();

    public void Update(TransferUpdatePayload update)
    {
        TryEnqueue(() =>
        {
            var existing = Transfers.FirstOrDefault(t => t.TransferId == update.TransferId);

            // 如果是完成或取消状态，且你想让它从 UI 消失，可以在这里做 Remove 逻辑
            // 目前策略：保留在列表中，让 UI 显示"完成"状态，由用户或定时器清理

            if (existing != null)
            {
                var index = Transfers.IndexOf(existing);
                Transfers[index] = update;
            }
            else
            {
                Transfers.Add(update);
            }
        });
    }

    private void TryEnqueue(DispatcherQueueHandler handler)
    {
        var dispatcher = App.MainWindow?.DispatcherQueue;
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
