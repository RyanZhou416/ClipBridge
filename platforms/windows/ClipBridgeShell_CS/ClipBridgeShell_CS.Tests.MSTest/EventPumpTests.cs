using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;
using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class EventPumpTests
{
    [TestMethod]
    public async Task Enqueue_ValidJson_ShouldUpdateStore()
    {
        // 1. Arrange (准备环境)
        var historyStore = new HistoryStore();
        var pump = new EventPumpService(historyStore);

        // 构造一个模拟的 Core 事件 JSON
        // 注意：字段名必须与 CoreEvents.cs 中的 [JsonPropertyName] 一致
        var json = """
        {
            "type": "item_added",
            "payload": {
                "id": 1001,
                "timestamp": 1703923200000,
                "mime_type": "text/plain",
                "preview_text": "Hello form Test",
                "device_id": "device_test_01"
            }
        }
        """;

        // 2. Act (执行)
        pump.Enqueue(json);

        // 因为 Pump 是异步处理的 (Channel)，我们需要稍等一下
        // 在实际项目中可以使用 TaskCompletionSource 或轮询，这里简单 Delay
        await Task.Delay(100);

        // 3. Assert (验证)
        Assert.AreEqual(1, historyStore.Items.Count, "Store 应该包含 1 个条目");
        var item = historyStore.Items[0];
        Assert.AreEqual((ulong)1001, item.Id);
        Assert.AreEqual("Hello form Test", item.PreviewText);
        Assert.AreEqual("text/plain", item.MimeType);

        // 清理
        pump.Shutdown();
    }
}
