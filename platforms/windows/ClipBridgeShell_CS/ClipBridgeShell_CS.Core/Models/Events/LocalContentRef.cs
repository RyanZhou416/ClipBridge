using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json.Serialization;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models.Events;

public class LocalContentRef
{
    [JsonPropertyName("kind")]
    public string Kind { get; set; } = string.Empty; // text|image|file

    [JsonPropertyName("item_id")]
    public string ItemId { get; set; } = string.Empty;

    [JsonPropertyName("mime")]
    public string Mime { get; set; } = string.Empty;

    [JsonPropertyName("text_utf8")]
    public string? TextUtf8 { get; set; }

    [JsonPropertyName("local_path")]
    public string? LocalPath { get; set; }

    [JsonPropertyName("sha256")]
    public string Sha256 { get; set; } = string.Empty;

    [JsonPropertyName("total_bytes")]
    public long TotalBytes { get; set; }
}
