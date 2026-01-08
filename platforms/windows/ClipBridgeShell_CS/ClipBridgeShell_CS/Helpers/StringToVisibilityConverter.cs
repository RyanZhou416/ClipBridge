using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将字符串转换为Visibility：非空字符串显示，空或null隐藏
/// </summary>
public sealed class StringToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is string str)
        {
            return string.IsNullOrWhiteSpace(str) ? Visibility.Collapsed : Visibility.Visible;
        }
        return Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
