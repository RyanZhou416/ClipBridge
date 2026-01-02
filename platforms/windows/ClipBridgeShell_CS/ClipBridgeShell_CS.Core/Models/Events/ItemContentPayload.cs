using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json.Serialization;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models.Events;

public class ItemContentPayload
{
    // [Core] content.mime
    [JsonPropertyName("mime")]
    public string Mime { get; set; } = string.Empty;

    // [Core] content.sha256
    [JsonPropertyName("sha256")]
    public string Sha256 { get; set; } = string.Empty;

    // [Core] content.total_bytes
    [JsonPropertyName("total_bytes")]
    public long TotalBytes
    {
        get; set;
    }
}
