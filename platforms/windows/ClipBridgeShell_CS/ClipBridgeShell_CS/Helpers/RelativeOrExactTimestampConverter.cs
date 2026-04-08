using System;
using Microsoft.UI.Xaml.Data;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 纯相对时间：秒/分钟/小时/天/月/年前，不退回具体日期。
/// 优先使用 WinUI3Localizer 的当前语言来决定中/英文格式。
/// </summary>
public sealed class RelativeOrExactTimestampConverter : IValueConverter
{
    private const long SecondsPerMinute = 60;
    private const long SecondsPerHour = 60 * 60;
    private const long SecondsPerDay = 24 * 60 * 60;
    private const long DaysPerMonth = 30;
    private const long DaysPerYear = 365;

    private static bool IsCurrentLanguageZh()
    {
        try
        {
            var lang = WinUI3Localizer.Localizer.Get().GetCurrentLanguage();
            return lang?.StartsWith("zh", StringComparison.OrdinalIgnoreCase) == true;
        }
        catch
        {
            return false;
        }
    }

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is not long createdMs || createdMs <= 0)
            return string.Empty;

        try
        {
            var created = DateTimeOffset.FromUnixTimeMilliseconds(createdMs);
            var now = DateTimeOffset.UtcNow;
            var diff = (long)(now - created).TotalSeconds;
            if (diff < 0) diff = 0;

            bool isZh = IsCurrentLanguageZh();

            if (diff < SecondsPerMinute)
                return isZh ? $"{diff}秒前" : $"{diff}s ago";
            if (diff < SecondsPerHour)
            {
                var m = diff / SecondsPerMinute;
                return isZh ? $"{m}分钟前" : $"{m}m ago";
            }
            if (diff < SecondsPerDay)
            {
                var h = diff / SecondsPerHour;
                return isZh ? $"{h}小时前" : $"{h}h ago";
            }

            var days = diff / SecondsPerDay;
            if (days < DaysPerMonth)
                return isZh ? $"{days}天前" : $"{days}d ago";
            if (days < DaysPerYear)
            {
                var months = days / DaysPerMonth;
                return isZh ? $"{months}个月前" : $"{months}mo ago";
            }
            var years = days / DaysPerYear;
            return isZh ? $"{years}年前" : $"{years}y ago";
        }
        catch
        {
            return string.Empty;
        }
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
