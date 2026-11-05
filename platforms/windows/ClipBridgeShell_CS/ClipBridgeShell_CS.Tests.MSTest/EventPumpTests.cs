using System.Diagnostics;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;
using Microsoft.VisualStudio.TestTools.UnitTesting; // 确保引用 MSTest
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class EventPumpTests
{
    // EventPump 内部用后台 Channel 消费，事件是异步处理的。
    // 这里用“轮询 + 超时”等待条件成立，避免固定 Task.Delay(100) 在慢机器上偶发失败。
    private static async Task<bool> WaitUntilAsync(Func<bool> condition, int timeoutMs = 5000)
    {
        var sw = Stopwatch.StartNew();
        while (!condition())
        {
            if (sw.ElapsedMilliseconds >= timeoutMs)
                return false;
            await Task.Delay(10);
        }
        return true;
    }

    // 辅助方法：创建一整套全新的环境
    private static (EventPumpService Pump, HistoryStore History, PeerStore Peer, TransferStore Transfer, ContentFetchAwaiter Awaiter) CreateSystem()
    {
        // 确保都在 UI 线程上下文之外运行，TryEnqueue 逻辑在测试中通常会同步执行或直接执行
        // 注意：实际 Store 中的 TryEnqueue 在没有 Window 的情况下可能会直接执行 handler()，这正合测试之意
        var historyStore = new HistoryStore();
        var peerStore = new PeerStore();
        var transferStore = new TransferStore();
        var awaiter = new ContentFetchAwaiter();
        var pump = new EventPumpService(historyStore, peerStore, transferStore, awaiter);
        return (pump, historyStore, peerStore, transferStore, awaiter);
    }

    [TestMethod]
    public async Task Enqueue_ItemMetaAdded_ShouldUpdateHistoryStore()
    {
        // 1. Arrange
        var (pump, historyStore, _, _, _) = CreateSystem();

        // 真实契约：本地 ingest 的事件（cb_core/src/api.rs:399-407）
        // {"type":"ITEM_META_ADDED","meta":<ItemMeta>,"policy":{...}}
        // ItemMeta 字段见 cb_core/src/model.rs：item_id / kind / created_ts_ms /
        // source_device_id / size_bytes / preview{text} / content{mime,sha256,total_bytes}
        var json = """
        {
            "type": "ITEM_META_ADDED",
            "meta": {
                "type": "ItemMeta",
                "item_id": "item-local-001",
                "kind": "text",
                "created_ts_ms": 1703923200000,
                "source_device_id": "device_local_01",
                "source_device_name": "Local PC",
                "size_bytes": 18,
                "preview": { "text": "History Item Test" },
                "content": {
                    "mime": "text/plain",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "total_bytes": 18
                },
                "files": []
            },
            "policy": { "needs_user_confirm": false, "strategy": "AutoShare" }
        }
        """;

        // 2. Act
        pump.Enqueue(json);

        // 3. Assert
        Assert.IsTrue(await WaitUntilAsync(() => historyStore.Items.Count == 1),
            "HistoryStore 应增加 1 条记录（ITEM_META_ADDED 顶层 meta 形态）");
        var item = historyStore.Items[0];
        Assert.AreEqual("item-local-001", item.ItemId);
        Assert.AreEqual("text", item.Kind);
        Assert.AreEqual("device_local_01", item.SourceDeviceId);
        Assert.AreEqual("History Item Test", item.Preview?.Text);
        Assert.AreEqual(1703923200000L, item.CreatedTsMs);

        pump.Shutdown();
    }

    [TestMethod]
    public async Task Enqueue_PeerChanged_ShouldUpdatePeerStore()
    {
        // 1. Arrange
        var (pump, _, peerStore, _, _) = CreateSystem();

        // 真实契约：cb_core/src/api.rs:629-634，字段在顶层（没有 payload）
        // {"type":"peer_changed","device_id":"...","share_to_peer":bool,"accept_from_peer":bool}
        var json = """
        {
            "type": "peer_changed",
            "device_id": "peer_abc_123",
            "share_to_peer": true,
            "accept_from_peer": false
        }
        """;

        // 2. Act
        pump.Enqueue(json);

        // 3. Assert
        Assert.IsTrue(await WaitUntilAsync(() => peerStore.Peers.Count == 1),
            "PeerStore 应增加 1 个设备（peer_changed 顶层字段形态）");
        var peer = peerStore.Peers[0];
        Assert.AreEqual("peer_abc_123", peer.DeviceId);
        Assert.IsTrue(peer.ShareToPeer);
        Assert.IsFalse(peer.AcceptFromPeer);
        // 该事件不携带在线状态/名称，模型保持默认值
        Assert.IsFalse(peer.IsOnline);

        pump.Shutdown();
    }

    [TestMethod]
    public async Task Enqueue_TransferProgress_ShouldUpdateTransferStore()
    {
        // 1. Arrange
        var (pump, _, _, transferStore, _) = CreateSystem();

        // 真实契约：cb_core/src/session/actor.rs handle_data_chunk
        // {"type":"TRANSFER_PROGRESS","payload":{"transfer_id":"<uuid>","item_id":"<uuid>",
        //   "state":"downloading","received":n,"total":n}}
        // transfer_id/item_id 都是字符串；state 由 Core 直接给出。
        var json = """
        {
            "type": "TRANSFER_PROGRESS",
            "payload": {
                "transfer_id": "55555555-5555-5555-5555-555555555555",
                "item_id": "item-remote-progress",
                "state": "downloading",
                "received": 1024,
                "total": 2048
            }
        }
        """;

        // 2. Act
        pump.Enqueue(json);

        // 3. Assert
        Assert.IsTrue(await WaitUntilAsync(() => transferStore.Transfers.Count == 1),
            "TransferStore 应增加 1 条传输记录（TRANSFER_PROGRESS）");
        var transfer = transferStore.Transfers[0];
        Assert.AreEqual("55555555-5555-5555-5555-555555555555", transfer.TransferId);
        Assert.AreEqual("item-remote-progress", transfer.ItemId);
        Assert.AreEqual("downloading", transfer.State);
        Assert.AreEqual(1024L, transfer.Received);
        Assert.AreEqual(2048L, transfer.Total);
        Assert.AreEqual(0.5, transfer.Progress, 0.001, "进度计算应为 50%");

        // 幂等性：同一 transfer_id 的后续进度应更新原行而不是新增
        var second = """
        {
            "type": "TRANSFER_PROGRESS",
            "payload": {
                "transfer_id": "55555555-5555-5555-5555-555555555555",
                "item_id": "item-remote-progress",
                "state": "downloading",
                "received": 2048,
                "total": 2048
            }
        }
        """;
        pump.Enqueue(second);
        Assert.IsTrue(await WaitUntilAsync(() =>
                transferStore.Transfers.Count == 1 && transferStore.Transfers[0].Progress >= 1.0),
            "同一 transfer_id 的进度更新应原地覆盖，不新增行");
        Assert.AreEqual(1, transferStore.Transfers.Count);

        pump.Shutdown();
    }

    [TestMethod]
    public async Task Enqueue_ContentCached_ShouldResolveAwaiter()
    {
        // 1. Arrange
        var (pump, _, _, _, awaiter) = CreateSystem();

        // 真实契约：cb_core/src/session/actor.rs:1123-1131（接收侧）与 api.rs:715-723（本机快路径）
        var json = """
        {
            "type": "CONTENT_CACHED",
            "payload": {
                "transfer_id": "tr-cached-001",
                "item_id": "item-remote-001",
                "file_id": null,
                "local_ref": {
                    "local_path": "C:\\cache\\blobs\\0123abcd",
                    "item_id": "item-remote-001",
                    "mime": "text/plain",
                    "kind": "text",
                    "sha256": "0123abcd",
                    "total_bytes": 18
                }
            }
        }
        """;

        // 先注册 waiter，确保 Resolve 命中而不是进 pending
        var waitTask = awaiter.WaitAsync("tr-cached-001", CancellationToken.None);

        // 2. Act
        pump.Enqueue(json);

        // 3. Assert
        var local = await waitTask.WaitAsync(TimeSpan.FromSeconds(5));
        Assert.AreEqual("tr-cached-001", local.TransferId);
        Assert.AreEqual("item-remote-001", local.ItemId);
        Assert.AreEqual("C:\\cache\\blobs\\0123abcd", local.LocalPath);
        Assert.AreEqual("text/plain", local.Mime);

        pump.Shutdown();
    }

    [TestMethod]
    public async Task Enqueue_ContentCached_ShouldCompleteTransfer()
    {
        // 1. Arrange：先让进度事件建出一行 downloading
        var (pump, _, _, transferStore, _) = CreateSystem();

        var progressJson = """
        {
            "type": "TRANSFER_PROGRESS",
            "payload": {
                "transfer_id": "tr-cached-002",
                "item_id": "item-remote-002",
                "state": "downloading",
                "received": 512,
                "total": 1024
            }
        }
        """;
        pump.Enqueue(progressJson);
        Assert.IsTrue(await WaitUntilAsync(() =>
                transferStore.Transfers.Count == 1 && transferStore.Transfers[0].State == "downloading"),
            "CONTENT_CACHED 之前应有 1 条 downloading 传输");

        // 真实契约：cb_core/src/session/actor.rs handle_content_end（CONTENT_CACHED）
        var cachedJson = """
        {
            "type": "CONTENT_CACHED",
            "payload": {
                "transfer_id": "tr-cached-002",
                "item_id": "item-remote-002",
                "file_id": null,
                "local_ref": {
                    "local_path": "C:\\cache\\blobs\\4567efgh",
                    "item_id": "item-remote-002",
                    "mime": "text/plain",
                    "kind": "text",
                    "sha256": "4567efgh",
                    "total_bytes": 1024
                }
            }
        }
        """;

        // 2. Act
        pump.Enqueue(cachedJson);

        // 3. Assert：成功缓存必须把传输落到 completed 终态，
        //    否则 ActiveTransfersCount 永不下降、UI 永远停留在 "Downloading 100%"。
        Assert.IsTrue(await WaitUntilAsync(() =>
                transferStore.Transfers.Count == 1 && transferStore.Transfers[0].State == "completed"),
            "CONTENT_CACHED 后对应传输应进入 completed 终态");
        Assert.AreEqual("tr-cached-002", transferStore.Transfers[0].TransferId);
        Assert.AreEqual("item-remote-002", transferStore.Transfers[0].ItemId);

        pump.Shutdown();
    }

    [TestMethod]
    public async Task Enqueue_ItemDeleted_ShouldRemoveFromHistoryStore()
    {
        // 1. Arrange
        var (pump, historyStore, _, _, _) = CreateSystem();

        var metaJson = """
        {
            "type": "ITEM_META_ADDED",
            "meta": {
                "type": "ItemMeta",
                "item_id": "item-to-delete",
                "kind": "text",
                "created_ts_ms": 1703923200000,
                "source_device_id": "device_local_01",
                "size_bytes": 4,
                "preview": { "text": "bye" },
                "content": { "mime": "text/plain", "sha256": "abc", "total_bytes": 4 }
            }
        }
        """;
        pump.Enqueue(metaJson);
        Assert.IsTrue(await WaitUntilAsync(() => historyStore.Items.Count == 1),
            "删除前应先有 1 条历史记录");

        // 真实契约：cb_core/src/api.rs:792-796（item_id 在顶层）
        var deletedJson = """
        {
            "type": "ITEM_DELETED",
            "item_id": "item-to-delete"
        }
        """;

        // 2. Act
        pump.Enqueue(deletedJson);

        // 3. Assert
        Assert.IsTrue(await WaitUntilAsync(() => historyStore.Items.Count == 0),
            "ITEM_DELETED 后 HistoryStore 应移除对应记录");

        pump.Shutdown();
    }

    [TestMethod]
    public void Upsert_ShouldBeIdempotentAndNotDowngradeTerminalState()
    {
        var store = new TransferStore();

        store.Upsert(new TransferUpdatePayload { TransferId = "t-1", State = "downloading", Received = 1, Total = 2 });
        store.Upsert(new TransferUpdatePayload { TransferId = "t-1", State = "downloading", Received = 2, Total = 2 });

        Assert.AreEqual(1, store.Transfers.Count, "同一 TransferId 只能有一行");
        Assert.AreEqual(2L, store.Transfers[0].Received);

        // 终态后迟到的非终态更新不得回退 UI 状态
        store.Upsert(new TransferUpdatePayload { TransferId = "t-1", State = "failed", Message = "boom" });
        store.Upsert(new TransferUpdatePayload { TransferId = "t-1", State = "downloading", Received = 0, Total = 2 });

        Assert.AreEqual(1, store.Transfers.Count);
        Assert.AreEqual("failed", store.Transfers[0].State);
    }

    [TestMethod]
    public void Upsert_ShouldCapCollectionSize()
    {
        var store = new TransferStore();

        for (var i = 0; i < TransferStore.MaxTransfers + 50; i++)
        {
            store.Upsert(new TransferUpdatePayload
            {
                TransferId = $"t-{i}",
                State = "completed",
                Received = i,
                Total = TransferStore.MaxTransfers + 50
            });
        }

        Assert.AreEqual(TransferStore.MaxTransfers, store.Transfers.Count, "集合不得超过上限");
        Assert.IsFalse(store.Transfers.Any(t => t.TransferId == "t-0"), "最老的终态行应被淘汰");
        Assert.IsTrue(store.Transfers.Any(t => t.TransferId == "t-249"), "最新行必须保留");
    }
}
