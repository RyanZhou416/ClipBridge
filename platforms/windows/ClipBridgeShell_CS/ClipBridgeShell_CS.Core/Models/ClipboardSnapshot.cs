using System.Text.Json;
using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Core.Models;

/// <summary>
/// 表示一次剪贴板的瞬时快照，用于传递给 Core 进行 Ingest
/// </summary>
public class ClipboardSnapshot
{
    [JsonPropertyName("mime_type")]
    public string MimeType { get; set; } = string.Empty; // text/plain, image/png, files-list

    [JsonPropertyName("data")]
    public string Data { get; set; } = string.Empty; // 文本内容 或 文件绝对路径

    [JsonPropertyName("preview_text")]
    public string? PreviewText
    {
        get; set;
    } // 用于 UI 显示的摘要

    [JsonPropertyName("timestamp")]
    public long Timestamp
    {
        get; set;
    } // UTC 毫秒

    [JsonPropertyName("fingerprint")]
    public string? Fingerprint
    {
        get; set;
    } // 用于去重和回环检测的哈希

    /// <summary>
    /// 序列化为 Core API 要求的 JSON 格式
    /// </summary>
    public string ToJson()
    {
        return JsonSerializer.Serialize(this);
    }
}
