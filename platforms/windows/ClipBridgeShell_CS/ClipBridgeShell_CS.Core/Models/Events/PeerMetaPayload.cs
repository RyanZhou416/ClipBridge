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

    [JsonPropertyName("share_to_peer")]
    public bool ShareToPeer { get; set; } = true;  // Outbound allow（策略状态）

    [JsonPropertyName("accept_from_peer")]
    public bool AcceptFromPeer { get; set; } = true;  // Inbound allow（策略状态）

    [JsonPropertyName("state")]
    public string ConnectionState { get; set; } = "Offline";  // 连接状态（Online/Offline/Backoff等）

    [JsonIgnore]
    public string? LocalAlias { get; set; }  // 本地别名（仅用于 UI 显示，存储在 LocalSettingsService）

    /// <summary>
    /// 获取显示名称（优先使用本地别名）
    /// </summary>
    [JsonIgnore]
    public string DisplayName => !string.IsNullOrEmpty(LocalAlias) ? LocalAlias : Name;
}
