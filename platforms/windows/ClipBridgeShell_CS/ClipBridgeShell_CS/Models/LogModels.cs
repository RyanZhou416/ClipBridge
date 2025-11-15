using System;

namespace ClipBridge.Models
{
    public sealed class LogRow
    {
        public long Id { get; set; }
        public long Time_Unix { get; set; }   // ms
        public int Level { get; set; }        // 0..6
        public string Category { get; set; } = "";
        public string Message { get; set; } = "";
        public string? Exception { get; set; }
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
    }
}
