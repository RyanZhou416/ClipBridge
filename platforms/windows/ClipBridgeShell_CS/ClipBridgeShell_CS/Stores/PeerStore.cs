using System.Collections.ObjectModel;
using ClipBridgeShell_CS.Core.Models.Events;
using Microsoft.UI.Dispatching;

namespace ClipBridgeShell_CS.Stores;

public class PeerStore
{
    // UI 直接绑定这个集合
    public ObservableCollection<PeerMetaPayload> Peers { get; } = new();

    public void Upsert(PeerMetaPayload peer)
    {
        // 必须切回 UI 线程操作 ObservableCollection
        TryEnqueue(() =>
        {
            // 查找是否已存在该设备
            var existing = Peers.FirstOrDefault(p => p.DeviceId == peer.DeviceId);
            if (existing != null)
            {
                // 更新现有条目
                var index = Peers.IndexOf(existing);
                Peers[index] = peer;
            }
            else
            {
                // 新设备加入
                Peers.Add(peer);
            }
        });
    }

    // 辅助方法：确保在 UI 线程执行 (复用 HistoryStore 的逻辑)
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
