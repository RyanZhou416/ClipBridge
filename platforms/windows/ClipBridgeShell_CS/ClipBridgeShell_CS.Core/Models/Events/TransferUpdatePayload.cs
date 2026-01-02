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

