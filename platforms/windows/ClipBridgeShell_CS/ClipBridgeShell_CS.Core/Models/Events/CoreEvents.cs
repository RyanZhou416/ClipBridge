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
