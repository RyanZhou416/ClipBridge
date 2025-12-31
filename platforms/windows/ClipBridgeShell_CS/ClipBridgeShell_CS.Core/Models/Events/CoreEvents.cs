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

/// <summary>
/// 设备/节点信息负载 (对应 peer_found / peer_changed)
/// </summary>
public class PeerMetaPayload
{
    [JsonPropertyName("device_id")]
    public string DeviceId { get; set; } = string.Empty; // 核心生成的唯一节点ID

    [JsonPropertyName("name")]
    public string Name { get; set; } = "Unknown";      // 设备名称

    [JsonPropertyName("is_online")]
    public bool IsOnline
    {
        get; set;
    }               // 在线状态

    [JsonPropertyName("last_seen")]
    public long LastSeen
    {
        get; set;
    }               // 最后可见时间 (Unix ms)

    // 预留字段：未来支持允许/拒绝策略
    [JsonPropertyName("is_allowed")]
    public bool IsAllowed { get; set; } = true;
}

/// <summary>
/// 传输状态更新负载 (对应 transfer_progress / transfer_update)
/// </summary>
public class TransferUpdatePayload
{
    [JsonPropertyName("transfer_id")]
    public ulong TransferId
    {
        get; set;
    }  // 传输任务的唯一ID

    [JsonPropertyName("item_id")]
    public ulong ItemId
    {
        get; set;
    }      // 关联的历史记录ID

    [JsonPropertyName("state")]
    public string State { get; set; } = string.Empty;
    // 例如: "pending", "running", "completed", "failed", "cancelled"

    [JsonPropertyName("processed_bytes")]
    public long ProcessedBytes
    {
        get; set;
    } // 已传输字节数

    [JsonPropertyName("total_bytes")]
    public long TotalBytes
    {
        get; set;
    }     // 总字节数

    [JsonPropertyName("error_msg")]
    public string? ErrorMessage
    {
        get; set;
    } // 如果失败，错误信息

    // 辅助属性：计算百分比 (0.0 - 1.0)
    [JsonIgnore]
    public double Progress => TotalBytes > 0 ? (double)ProcessedBytes / TotalBytes : 0;
}
