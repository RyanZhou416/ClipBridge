using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json.Serialization;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models.Events;

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
