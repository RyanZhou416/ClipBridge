using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将布尔值转换为Visibility：true=Visible，false=Collapsed
/// </summary>
public sealed class BoolToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is bool boolValue)
        {
            return boolValue ? Visibility.Visible : Visibility.Collapsed;
        }
        return Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        if (value is Visibility visibility)
        {
            return visibility == Visibility.Visible;
        }
        return false;
    }
}
