using System.Text.Json.Serialization;
using ClipBridgeShell_CS.Core.Models.Events; // 为了引用 ItemMetaPayload

namespace ClipBridgeShell_CS.Core.Models;

/// <summary>
/// 对应 Core 的查询参数 JSON
/// </summary>
public class HistoryQuery
{
    [JsonPropertyName("limit")]
    public int Limit { get; set; } = 20;

    // 游标：上一页最后一条的 sort_ts_ms。null 表示第一页。
    [JsonPropertyName("cursor")]
    public long? Cursor
    {
        get; set;
    }

    // 过滤条件容器
    [JsonPropertyName("filter")]
    public HistoryFilter? Filter
    {
        get; set;
    }
}

public class HistoryFilter
{
    [JsonPropertyName("filter_text")]
    public string? FilterText
    {
        get; set;
    }

    [JsonPropertyName("kind")]
    public string? Kind
    {
        get; set;
    } // "text", "image", "file"

    [JsonPropertyName("device_id")]
    public string? DeviceId
    {
        get; set;
    }

    // 时间范围等其他条件可按需扩展
}

/// <summary>
/// 对应 Core 返回的分页结果 JSON
/// </summary>
public class HistoryPage
{
    [JsonPropertyName("items")]
    public List<ItemMetaPayload> Items { get; set; } = new();

    // 下一页的游标。如果为 null，表示没有更多数据。
    [JsonPropertyName("next_cursor")]
    public long? NextCursor
    {
        get; set;
    }
}
