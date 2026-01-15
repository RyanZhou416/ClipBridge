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
                var dateTime = DateTimeOffset.FromUnixTimeMilliseconds(timestamp);
                return dateTime.ToString("yyyy-MM-dd HH:mm:ss");
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
