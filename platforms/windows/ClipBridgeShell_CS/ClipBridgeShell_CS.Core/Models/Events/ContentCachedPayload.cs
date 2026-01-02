using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json.Serialization;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models.Events;

public class ContentCachedPayload
{
    [JsonPropertyName("transfer_id")]
    public string TransferId { get; set; } = string.Empty;

    [JsonPropertyName("item_id")]
    public string ItemId { get; set; } = string.Empty;

    [JsonPropertyName("file_id")]
    public string? FileId { get; set; }

    [JsonPropertyName("local_ref")]
    public LocalContentRef LocalRef { get; set; } = new();
}
