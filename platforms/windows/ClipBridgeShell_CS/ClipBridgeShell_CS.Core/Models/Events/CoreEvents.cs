using System.Text.Json;
using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Core.Models.Events;

/// <summary>
/// 基础事件信封，对应 Core 发出的最外层 JSON
/// </summary>
public class CoreEventEnvelope
{
    [JsonPropertyName("type")]
    public string Type { get; set; } = string.Empty;

    // 使用 JsonElement 延迟解析，直到我们在 Pump 中决定了具体类型
    [JsonPropertyName("payload")]
    public JsonElement Payload
    {
        get; set;
    }
}

/// <summary>
/// 历史记录元数据 (对应 item_meta_added / item_meta_updated)
///
/// </summary>
public class ItemMetaPayload
{
    [JsonPropertyName("id")]
    public ulong Id
    {
        get; set;
    } // Rust item_id (u64)

    [JsonPropertyName("timestamp")]
    public long Timestamp
    {
        get; set;
    }

    [JsonPropertyName("mime_type")]
    public string MimeType { get; set; } = string.Empty;

    [JsonPropertyName("preview_text")]
    public string? PreviewText
    {
        get; set;
    }

    [JsonPropertyName("device_id")]
    public string DeviceId { get; set; } = string.Empty;

    // 后续可补充 size, cached_state 等字段
}
