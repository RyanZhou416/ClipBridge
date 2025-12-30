using System.Collections.ObjectModel;
using ClipBridgeShell_CS.Core.Models.Events;
using Microsoft.UI.Dispatching;

namespace ClipBridgeShell_CS.Stores;

public class HistoryStore
{
    public ObservableCollection<ItemMetaPayload> Items { get; } = new();

    public void Upsert(ItemMetaPayload meta)
    {
        TryEnqueue(() =>
        {
            var existing = Items.FirstOrDefault(x => x.Id == meta.Id);
            if (existing != null)
            {
                var index = Items.IndexOf(existing);
                Items[index] = meta;
            }
            else
            {
                Items.Insert(0, meta);
            }
        });
    }

    private void TryEnqueue(DispatcherQueueHandler handler)
    {
        Microsoft.UI.Dispatching.DispatcherQueue? dispatcher = null;

        try
        {
            // [FIX] 加固：在单元测试或非 UI 线程环境中，访问 App.MainWindow 可能会有问题
            // 只有当 App.Current 不为 null 时才尝试访问 MainWindow
            if (Microsoft.UI.Xaml.Application.Current != null)
            {
                dispatcher = App.MainWindow?.DispatcherQueue;
            }
        }
        catch
        {
            // 忽略所有获取 Dispatcher 时的异常 (例如测试环境)
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
}
