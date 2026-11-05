using System.Text.Json;
using System.Text.Json.Nodes;
using ClipBridgeShell_CS.Core.Models;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 锁定 C# 侧发给 Core 的快照 JSON 契约。Rust 侧对应
/// <c>crates/cb_core_ffi/src/marshal.rs::parse_snapshot</c> 的 <c>ClipboardSnapshotDto</c>；
/// 测试无法直接调用 Rust，因此这里按 Rust 的字段名/枚举拼写断言完整 JSON。
/// </summary>
[TestClass]
public class ClipboardSnapshotJsonTests
{
    private static string ToJson(ClipboardSnapshot snapshot) => snapshot.ToJson();

    private static void AssertContract(string expectedJson, string actualJson)
    {
        var expected = JsonNode.Parse(expectedJson);
        var actual = JsonNode.Parse(actualJson);
        Assert.IsTrue(JsonNode.DeepEquals(expected, actual),
            $"序列化结果与 Rust ClipboardSnapshotDto 契约不一致。\n期望:\n{expectedJson}\n实际:\n{actualJson}");
    }

    [TestMethod]
    public void ToJson_Text_ShouldUseRustDtoShape()
    {
        // 现有文本采集路径不设置 Kind，必须仍然推断为 text
        var snapshot = new ClipboardSnapshot
        {
            MimeType = "text/plain",
            Data = "hello 世界",
            PreviewText = "hello",
            Timestamp = 1700000000000,
            Fingerprint = "legacy-fingerprint",
        };

        var json = ToJson(snapshot);

        AssertContract("""
        {
          "type": "ClipboardSnapshot",
          "ts_ms": 1700000000000,
          "kind": "text",
          "text": { "mime": "text/plain", "utf8": "hello 世界" }
        }
        """, json);

        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;
        Assert.AreEqual("ClipboardSnapshot", root.GetProperty("type").GetString());
        Assert.AreEqual(1700000000000L, root.GetProperty("ts_ms").GetInt64());
        Assert.AreEqual("text", root.GetProperty("kind").GetString());
        Assert.AreEqual("hello 世界", root.GetProperty("text").GetProperty("utf8").GetString());
        // 旧契约字段不得再出现（否则 Rust 解析不到 kind/text）
        Assert.IsFalse(root.TryGetProperty("mime_type", out _));
        Assert.IsFalse(root.TryGetProperty("data", out _));
        Assert.IsFalse(root.TryGetProperty("preview_text", out _));
        Assert.IsFalse(root.TryGetProperty("timestamp", out _));
        Assert.IsFalse(root.TryGetProperty("fingerprint", out _));
    }

    [TestMethod]
    public void ToJson_Image_ShouldBase64EncodeBytes()
    {
        var snapshot = new ClipboardSnapshot
        {
            Kind = ClipboardSnapshot.KindImage,
            MimeType = "image/png",
            ImageBytes = new byte[] { 1, 2, 3, 255 },
            Timestamp = 1700000000001,
        };

        var json = ToJson(snapshot);

        AssertContract("""
        {
          "type": "ClipboardSnapshot",
          "ts_ms": 1700000000001,
          "kind": "image",
          "image": { "mime": "image/png", "bytes_b64": "AQID/w==" }
        }
        """, json);
    }

    [TestMethod]
    public void ToJson_FileList_ShouldUseRustFileEntryShape()
    {
        var snapshot = new ClipboardSnapshot
        {
            Kind = ClipboardSnapshot.KindFileList,
            Timestamp = 1700000000002,
            ShareMode = "local_only",
            Files = new List<ClipboardFileEntry>
            {
                new() { RelName = "a.txt", AbsPath = @"C:\tmp\a.txt", SizeBytes = 3, Sha256 = "aa" },
                new() { RelName = "b.bin", SizeBytes = 5 },
            },
        };

        var json = ToJson(snapshot);

        AssertContract("""
        {
          "type": "ClipboardSnapshot",
          "ts_ms": 1700000000002,
          "kind": "file_list",
          "share_mode": "local_only",
          "files": [
            { "rel_name": "a.txt", "abs_path": "C:\\tmp\\a.txt", "size_bytes": 3, "sha256": "aa" },
            { "rel_name": "b.bin", "size_bytes": 5 }
          ]
        }
        """, json);
    }
}
