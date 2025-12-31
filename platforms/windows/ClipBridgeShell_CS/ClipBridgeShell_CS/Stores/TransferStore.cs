using System.Collections.ObjectModel;
using ClipBridgeShell_CS.Core.Models.Events;
using Microsoft.UI.Dispatching;

namespace ClipBridgeShell_CS.Stores;

public class TransferStore
{
    // UI 绑定这个集合来显示进度条
    public ObservableCollection<TransferUpdatePayload> Transfers { get; } = new();

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
}
