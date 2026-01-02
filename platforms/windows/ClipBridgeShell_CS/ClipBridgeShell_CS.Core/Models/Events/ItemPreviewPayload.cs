using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json.Serialization;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models.Events;

public class ItemPreviewPayload
{
    [JsonPropertyName("text")]
    public string? Text { get; set; }
}
