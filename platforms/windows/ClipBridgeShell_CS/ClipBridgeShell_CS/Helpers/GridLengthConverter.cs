using System;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将 double 转换为 GridLength
/// </summary>
public sealed class GridLengthConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is double doubleValue)
        {
            return new GridLength(doubleValue);
        }
        return new GridLength(0);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        if (value is GridLength gridLength)
        {
            return gridLength.Value;
        }
        return 0.0;
    }
}
