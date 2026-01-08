using System;
using Microsoft.UI;
using Microsoft.UI.Xaml.Data;
using Windows.UI;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将布尔值转换为颜色：true=绿色，false=红色
/// </summary>
public sealed class BooleanToColorConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is bool isOnline)
        {
            return isOnline ? Colors.Green : Colors.Red;
        }
        return Colors.Gray;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
