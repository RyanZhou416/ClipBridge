using System.Text.Json;
using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Core.Models;

/// <summary>
/// 文件列表快照中的单个文件。字段名与 Rust <c>cb_core::clipboard::ClipboardFileEntry</c>
/// （rel_name / abs_path / size_bytes / sha256）完全一致。
/// </summary>
public class ClipboardFileEntry
{
    [JsonPropertyName("rel_name")]
    public string RelName { get; set; } = string.Empty;

    [JsonPropertyName("abs_path")]
    public string? AbsPath { get; set; }

    [JsonPropertyName("size_bytes")]
    public long SizeBytes { get; set; }

    [JsonPropertyName("sha256")]
    public string? Sha256 { get; set; }
}

/// <summary>
/// 表示一次剪贴板的瞬时快照，用于传递给 Core 进行 Ingest
/// </summary>
/// <remarks>
/// <see cref="ToJson"/> 输出的是 Rust <c>ClipboardSnapshotDto</c>
/// （crates/cb_core_ffi/src/marshal.rs::parse_snapshot）要求的契约：
/// <code>
/// {
///   "type": "ClipboardSnapshot",
///   "ts_ms": 1700000000000,
///   "kind": "text" | "image" | "file_list",
///   "text":  { "mime": "text/plain", "utf8": "..." },
///   "image": { "mime": "image/png",  "bytes_b64": "<base64>" },
///   "files": [ { "rel_name": "...", "abs_path": "...", "size_bytes": 1, "sha256": "..." } ],
///   "share_mode": "default" | "local_only" | "force"  // 可选
/// }
/// </code>
/// 旧的 mime_type/data/preview_text/timestamp/fingerprint 只供外壳内部使用，不再发送给 Core。
/// </remarks>
public class ClipboardSnapshot
{
    public const string KindText = "text";
    public const string KindImage = "image";
    public const string KindFileList = "file_list";

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
    /// 快照类型，取 Rust <c>SnapshotKind</c> 的 snake_case 拼写：text / image / file_list。
    /// 为空时按 ImageBytes / Files 自动推断，默认 text（现有文本采集路径无需改动）。
    /// </summary>
    public string? Kind { get; set; }

    /// <summary>
    /// 可选共享模式，取 Rust <c>ShareMode</c> 的 snake_case 拼写：default / local_only / force。
    /// 为 null 时不发送该字段，由 Core 取默认值 default。
    /// </summary>
    public string? ShareMode { get; set; }

    /// <summary>图片原始字节；kind=image 时必填，序列化时转 base64 写入 image.bytes_b64。</summary>
    public byte[]? ImageBytes { get; set; }

    /// <summary>文件列表；kind=file_list 时必填，序列化为 files[]。</summary>
    public List<ClipboardFileEntry>? Files { get; set; }

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        // Rust 侧 Option 字段缺省即 None，省略 null 比显式写 null 更贴近契约最小集
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };

    /// <summary>
    /// 序列化为 Core <c>ClipboardSnapshotDto</c> 要求的 JSON 格式。
    /// </summary>
    public string ToJson()
    {
        var kind = ResolveKind();
        var dto = new SnapshotDto
        {
            TsMs = Timestamp,
            Kind = kind,
            ShareMode = NormalizeShareMode(ShareMode),
        };

        switch (kind)
        {
            case KindImage:
                if (ImageBytes is not { Length: > 0 })
                    throw new InvalidOperationException("image 快照缺少 ImageBytes");
                dto.Image = new ImageDto
                {
                    Mime = string.IsNullOrWhiteSpace(MimeType) ? "application/octet-stream" : MimeType,
                    BytesB64 = Convert.ToBase64String(ImageBytes),
                };
                break;

            case KindFileList:
                if (Files is not { Count: > 0 })
                    throw new InvalidOperationException("file_list 快照缺少 Files");
                dto.Files = Files;
                break;

            default:
                dto.Text = new TextDto
                {
                    Mime = string.IsNullOrWhiteSpace(MimeType) ? "text/plain" : MimeType,
                    Utf8 = Data,
                };
                break;
        }

        return JsonSerializer.Serialize(dto, JsonOpts);
    }

    private string ResolveKind()
    {
        if (!string.IsNullOrWhiteSpace(Kind))
        {
            return Kind.Trim().ToLowerInvariant() switch
            {
                KindText => KindText,
                KindImage => KindImage,
                KindFileList or "filelist" => KindFileList,
                _ => throw new InvalidOperationException($"未知的快照类型: {Kind}"),
            };
        }

        // 兼容既有调用方：未显式设置 Kind 时按载荷推断，纯文本仍是默认类型
        if (ImageBytes is { Length: > 0 })
            return KindImage;

        if (Files is { Count: > 0 })
            return KindFileList;

        return KindText;
    }

    private static string? NormalizeShareMode(string? shareMode)
    {
        if (string.IsNullOrWhiteSpace(shareMode))
            return null;

        return shareMode.Trim().ToLowerInvariant() switch
        {
            "default" => "default",
            "local_only" or "localonly" => "local_only",
            "force" => "force",
            _ => throw new InvalidOperationException($"未知的 share_mode: {shareMode}"),
        };
    }

    // 以下 DTO 只用于产生 Core 契约 JSON，外壳其余部分继续使用上面的 ClipboardSnapshot 属性。
    private sealed class SnapshotDto
    {
        [JsonPropertyName("type")]
        public string Type => "ClipboardSnapshot";

        [JsonPropertyName("ts_ms")]
        public long TsMs { get; set; }

        [JsonPropertyName("kind")]
        public string Kind { get; set; } = KindText;

        [JsonPropertyName("share_mode")]
        public string? ShareMode { get; set; }

        [JsonPropertyName("text")]
        public TextDto? Text { get; set; }

        [JsonPropertyName("image")]
        public ImageDto? Image { get; set; }

        [JsonPropertyName("files")]
        public List<ClipboardFileEntry>? Files { get; set; }
    }

    private sealed class TextDto
    {
        [JsonPropertyName("mime")]
        public string Mime { get; set; } = "text/plain";

        [JsonPropertyName("utf8")]
        public string Utf8 { get; set; } = string.Empty;
    }

    private sealed class ImageDto
    {
        [JsonPropertyName("mime")]
        public string Mime { get; set; } = string.Empty;

        [JsonPropertyName("bytes_b64")]
        public string BytesB64 { get; set; } = string.Empty;
    }
}
