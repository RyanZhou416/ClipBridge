using System;
using System.Text.Json;
using System.Text.Json.Serialization;
using WinUI3Localizer;

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
    
    /// <summary>
    /// 简化的分类显示（去除命名空间前缀）
    /// </summary>
    public string CategoryDisplay
    {
        get
        {
            if (string.IsNullOrEmpty(Category))
                return Category;
            
            // 如果包含点号，取最后一部分（类名）
            var lastDot = Category.LastIndexOf('.');
            if (lastDot >= 0 && lastDot < Category.Length - 1)
            {
                return Category.Substring(lastDot + 1);
            }
            
            return Category;
        }
    }
    
    [JsonPropertyName("message")]
    public string Message { get; set; } = "";
    
    /// <summary>
    /// 显示消息（Rust侧已经根据语言提取，这里直接返回Message）
    /// 如果Message是JSON格式（向后兼容），则尝试解析
    /// </summary>
    public string DisplayMessage
    {
        get
        {
            // Rust侧已经在查询时根据语言提取了消息，所以Message应该已经是本地化的字符串
            // 但如果Message是JSON格式（向后兼容），则尝试解析
            if (string.IsNullOrEmpty(Message))
            {
                return Message;
            }
            
            // 如果Message不是JSON格式，直接返回（Rust侧已经处理了本地化）
            if (!Message.TrimStart().StartsWith("{"))
            {
                return Message;
            }
            
            // 向后兼容：如果Message是JSON格式，尝试解析
            try
            {
                using var doc = JsonDocument.Parse(Message);
                var root = doc.RootElement;
                
                if (root.ValueKind != JsonValueKind.Object)
                {
                    return Message;
                }
                
                // 尝试从WinUI3Localizer获取当前语言
                string? currentLang = null;
                try
                {
                    var loc = WinUI3Localizer.Localizer.Get();
                    currentLang = loc.GetCurrentLanguage();
                }
                catch { }
                
                // 规范化语言代码
                string normalizedLang = NormalizeLanguageCode(currentLang);
                
                // 尝试获取目标语言
                if (!string.IsNullOrEmpty(normalizedLang) && root.TryGetProperty(normalizedLang, out var langValue))
                {
                    if (langValue.ValueKind == JsonValueKind.String)
                    {
                        return langValue.GetString() ?? Message;
                    }
                }
                
                // 回退到 "en"
                if (normalizedLang != "en" && root.TryGetProperty("en", out var enValue))
                {
                    if (enValue.ValueKind == JsonValueKind.String)
                    {
                        return enValue.GetString() ?? Message;
                    }
                }
                
                return Message;
            }
            catch
            {
                return Message;
            }
        }
    }
    
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
    public string TimeStrFull => Time.ToLocalTime().ToString("yyyy-MM-dd HH:mm:ss.fff");
    
    // 选择状态（用于选择模式）
    public bool IsSelected { get; set; }

    /// <summary>
    /// 从多语言 JSON 中提取指定语言的消息（向后兼容辅助方法）
    /// 注意：Rust 侧已经在查询时提取了正确语言，此方法主要用于向后兼容
    /// </summary>
    public string GetMessageForLanguage(string? lang)
    {
        // 如果 Message 已经是普通字符串（非 JSON），直接返回
        if (string.IsNullOrEmpty(Message) || !Message.TrimStart().StartsWith("{"))
        {
            return Message;
        }

        // 尝试解析为 JSON
        try
        {
            using var doc = JsonDocument.Parse(Message);
            var root = doc.RootElement;
            
            if (root.ValueKind != JsonValueKind.Object)
            {
                return Message; // 不是对象，返回原始值
            }

            // 规范化语言代码
            string normalizedLang = NormalizeLanguageCode(lang);
            
            // 尝试获取目标语言
            if (!string.IsNullOrEmpty(normalizedLang) && root.TryGetProperty(normalizedLang, out var langValue))
            {
                if (langValue.ValueKind == JsonValueKind.String)
                {
                    return langValue.GetString() ?? Message;
                }
            }
            
            // 回退到 "en"
            if (normalizedLang != "en" && root.TryGetProperty("en", out var enValue))
            {
                if (enValue.ValueKind == JsonValueKind.String)
                {
                    return enValue.GetString() ?? Message;
                }
            }
            
            // 如果找不到，返回原始 Message
            return Message;
        }
        catch
        {
            // 解析失败，返回原始 Message
            return Message;
        }
    }

    private static string NormalizeLanguageCode(string? lang)
    {
        if (string.IsNullOrWhiteSpace(lang))
            return "en";
        
        lang = lang.Trim();
        if (lang.Equals("en", StringComparison.OrdinalIgnoreCase) || lang.StartsWith("en-", StringComparison.OrdinalIgnoreCase))
            return "en";
        if (lang.Equals("zh", StringComparison.OrdinalIgnoreCase) || lang.Equals("zh-Hans", StringComparison.OrdinalIgnoreCase) || lang.StartsWith("zh-", StringComparison.OrdinalIgnoreCase))
            return "zh-CN";
        return lang;
    }
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
