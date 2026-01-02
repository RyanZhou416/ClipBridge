using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json.Serialization;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models.Events;

/// <summary>
/// 传输状态更新负载 (对应 transfer_progress / transfer_update)
/// </summary>
public class TransferUpdatePayload
{
    [JsonPropertyName("transfer_id")]
    public string TransferId { get; set; } = string.Empty; // 传输任务的唯一ID

    [JsonPropertyName("item_id")]
    public string? ItemId { get; set; }      // 关联的历史记录ID

    [JsonPropertyName("state")]
    public string State { get; set; } = string.Empty;
    // 例如: "pending", "running", "completed", "failed", "cancelled"

    [JsonPropertyName("received")]
    public long? Received { get; set; }

    [JsonPropertyName("total")]
    public long? Total { get; set; }

    [JsonPropertyName("bytes_done")]
    public long? BytesDone { get; set; }

    [JsonPropertyName("bytes_total")]
    public long? BytesTotal { get; set; }

    [JsonIgnore]
    public long ProcessedBytes => Received ?? BytesDone ?? 0; // 已传输字节数

    [JsonIgnore]
    public long TotalBytes => Total ?? BytesTotal ?? 0;    // 总字节数

    [JsonPropertyName("code")]
    public string? Code { get; set; }

    [JsonPropertyName("message")]
    public string? Message { get; set; }

    [JsonPropertyName("error_msg")]
    public string? ErrorMessage
    {
        get; set;
    } // 如果失败，错误信息

    // 辅助属性：计算百分比 (0.0 - 1.0)
    [JsonIgnore]
    public double Progress => TotalBytes > 0 ? (double)ProcessedBytes / TotalBytes : 0;
}

