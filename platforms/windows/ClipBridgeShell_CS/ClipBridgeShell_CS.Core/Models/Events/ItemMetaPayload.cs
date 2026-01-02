using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Core.Models.Events;

/// <summary>
/// 对应 Core ItemMeta 的 JSON 结构
/// </summary>
public class ItemMetaPayload
{
// [Core] item_id
    [JsonPropertyName("item_id")]
    public string ItemId { get; set; } = string.Empty;

    // [Core] kind (text | image | file_list)
    [JsonPropertyName("kind")]
    public string Kind { get; set; } = "text";

    // [Core] created_ts_ms
    [JsonPropertyName("created_ts_ms")]
    public long CreatedTsMs { get; set; }

    // [Core] source_device_id
    [JsonPropertyName("source_device_id")]
    public string SourceDeviceId { get; set; } = string.Empty;

    // [Core] preview
    [JsonPropertyName("preview")]
    public ItemPreviewPayload? Preview { get; set; }

    // [Core] content (这里包含 mime, sha256 等)
    [JsonPropertyName("content")]
    public ItemContentPayload? Content { get; set; }

    // [Helper] 方便属性：直接获取 ItemContentPayload的内容，解决 XAML 绑定报错
    [JsonIgnore] 
    public string Mime => Content?.Mime ?? string.Empty;

    [JsonIgnore]
    public string Sha256 => Content?.Sha256 ?? string.Empty;

    [JsonIgnore]
    public string TotalBytes => Content?.Sha256 ?? string.Empty;
}

