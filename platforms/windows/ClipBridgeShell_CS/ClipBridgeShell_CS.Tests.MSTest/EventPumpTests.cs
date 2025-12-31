using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;
using Microsoft.VisualStudio.TestTools.UnitTesting; // 确保引用 MSTest
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class EventPumpTests
{
    // 辅助方法：创建一整套全新的环境
    private (EventPumpService Pump, HistoryStore History, PeerStore Peer, TransferStore Transfer) CreateSystem()
    {
        // 确保都在 UI 线程上下文之外运行，TryEnqueue 逻辑在测试中通常会同步执行或直接执行
        // 注意：实际 Store 中的 TryEnqueue 在没有 Window 的情况下可能会直接执行 handler()，这正合测试之意
        var historyStore = new HistoryStore();
        var peerStore = new PeerStore();
        var transferStore = new TransferStore();
        var pump = new EventPumpService(historyStore, peerStore, transferStore);
        return (pump, historyStore, peerStore, transferStore);
    }

    [TestMethod]
    public async Task Enqueue_ItemAdded_ShouldUpdateHistoryStore()
    {
        // 1. Arrange
        var (pump, historyStore, _, _) = CreateSystem();

        var json = """
        {
            "type": "item_added",
            "payload": {
                "id": 1001,
                "timestamp": 1703923200000,
                "mime_type": "text/plain",
                "preview_text": "History Item Test",
                "device_id": "device_test_01"
            }
        }
        """;

        // 2. Act
        pump.Enqueue(json);
        await Task.Delay(100); // 等待 Channel 消费

        // 3. Assert
        Assert.AreEqual(1, historyStore.Items.Count, "HistoryStore 应增加 1 条记录");
        var item = historyStore.Items[0];
        Assert.AreEqual((ulong)1001, item.Id);
        Assert.AreEqual("History Item Test", item.PreviewText);

        pump.Shutdown();
    }

    [TestMethod]
    public async Task Enqueue_PeerFound_ShouldUpdatePeerStore()
    {
        // 1. Arrange
        var (pump, _, peerStore, _) = CreateSystem();

        // 模拟 Core 发来的 peer_found 事件
        var json = """
        {
            "type": "peer_found",
            "payload": {
                "device_id": "peer_abc_123",
                "name": "Ryan's Laptop",
                "is_online": true,
                "last_seen": 1703923200000
            }
        }
        """;

        // 2. Act
        pump.Enqueue(json);
        await Task.Delay(100);

        // 3. Assert
        Assert.AreEqual(1, peerStore.Peers.Count, "PeerStore 应增加 1 个设备");
        var peer = peerStore.Peers[0];
        Assert.AreEqual("peer_abc_123", peer.DeviceId);
        Assert.AreEqual("Ryan's Laptop", peer.Name);
        Assert.IsTrue(peer.IsOnline);

        pump.Shutdown();
    }

    [TestMethod]
    public async Task Enqueue_TransferProgress_ShouldUpdateTransferStore()
    {
        // 1. Arrange
        var (pump, _, _, transferStore) = CreateSystem();

        // 模拟 Core 发来的 transfer_progress 事件
        var json = """
        {
            "type": "transfer_progress",
            "payload": {
                "transfer_id": 555,
                "item_id": 1001,
                "state": "downloading",
                "processed_bytes": 1024,
                "total_bytes": 2048,
                "error_msg": null
            }
        }
        """;

        // 2. Act
        pump.Enqueue(json);
        await Task.Delay(100);

        // 3. Assert
        Assert.AreEqual(1, transferStore.Transfers.Count, "TransferStore 应增加 1 个任务");
        var task = transferStore.Transfers[0];
        Assert.AreEqual((ulong)555, task.TransferId);
        Assert.AreEqual("downloading", task.State);
        Assert.AreEqual(0.5, task.Progress, 0.001, "进度计算应为 50%");

        pump.Shutdown();
    }
}
