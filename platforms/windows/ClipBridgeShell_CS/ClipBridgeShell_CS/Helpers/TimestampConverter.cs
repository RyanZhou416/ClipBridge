using System;
using Microsoft.UI.Xaml.Data;

namespace ClipBridgeShell_CS.Helpers;

public class TimestampConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is long timestamp && timestamp > 0)
        {
            try
            {
                // 存储为 UTC（Unix 毫秒），显示时转为用户本地时间（含时区与冬/夏令时）
                var utc = DateTimeOffset.FromUnixTimeMilliseconds(timestamp);
                return utc.ToLocalTime().ToString("yyyy-MM-dd HH:mm:ss");
            }
            catch
            {
                return "Unknown";
            }
        }
        return "Never";
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
