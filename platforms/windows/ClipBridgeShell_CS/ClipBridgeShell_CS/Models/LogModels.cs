using System;
using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Models;

public sealed class LogRow
{
    [JsonPropertyName("id")]
    public long Id { get; set; }
    
    [JsonPropertyName("ts_utc")]
    public long Time_Unix { get; set; }   // ms
    
    [JsonPropertyName("level")]
    public int Level { get; set; }        // 0..6
    
    [JsonPropertyName("component")]
    public string Component { get; set; } = ""; // 组件来源（Core/Shell）
    
    [JsonPropertyName("category")]
    public string Category { get; set; } = "";
    
    [JsonPropertyName("message")]
    public string Message { get; set; } = "";
    
    [JsonPropertyName("exception")]
    public string? Exception { get; set; }
    
    [JsonPropertyName("props_json")]
    public string? Props_Json { get; set; }

    // 便于 XAML 绑定显示
    public DateTimeOffset Time => DateTimeOffset.FromUnixTimeMilliseconds(Time_Unix);
    public string LevelName => Level switch {
        0 => "Trace", 1 => "Debug", 2 => "Info", 3 => "Warn", 4 => "Error", 5 => "Critical", _ => "Unknown"
    };
    public string TimeStr => Time.ToLocalTime().ToString("HH:mm:ss.fff");

}

public sealed class LogStats
{
    public long Count { get; set; }
    public long? First_Ms { get; set; }
    public long? Last_Ms { get; set; }
    public long[] By_Level { get; set; } = new long[7];
    public long? Disk_Bytes { get; set; } // 日志文件磁盘占用（如核心提供）
    public long Stash_Count { get; set; } // 暂存日志条数（核心就绪前）
}
