using System;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI;
using Windows.UI;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将日志级别转换为浅色背景画笔（用于整行背景）
/// </summary>
public sealed class LogLevelToBackgroundColorConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is int level)
        {
            // 使用浅色背景，透明度较低
            Color color = level switch
            {
                0 => Color.FromArgb(20, 128, 128, 128),  // Trace: 浅灰色
                1 => Color.FromArgb(20, 0, 191, 255),     // Debug: 浅青色
                2 => Color.FromArgb(20, 30, 144, 255),   // Info: 浅蓝色
                3 => Color.FromArgb(30, 255, 165, 0),    // Warn: 浅橙色
                4 => Color.FromArgb(30, 255, 0, 0),     // Error: 浅红色
                5 => Color.FromArgb(40, 139, 0, 0),      // Critical: 浅深红色
                _ => Colors.Transparent
            };
            return new SolidColorBrush(color);
        }
        return new SolidColorBrush(Colors.Transparent);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
