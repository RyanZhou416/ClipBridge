using System;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI;
using Windows.UI;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将日志级别转换为颜色画笔
/// 0=Trace(灰色), 1=Debug(青色), 2=Info(蓝色), 3=Warn(橙色), 4=Error(红色), 5=Critical(深红色)
/// </summary>
public sealed class LogLevelToColorConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is int level)
        {
            Color color = level switch
            {
                0 => Color.FromArgb(255, 128, 128, 128),    // Trace: 灰色
                1 => Color.FromArgb(255, 0, 191, 255),      // Debug: 青色
                2 => Color.FromArgb(255, 30, 144, 255),     // Info: 蓝色
                3 => Color.FromArgb(255, 255, 140, 0),       // Warn: 橙色
                4 => Color.FromArgb(255, 220, 20, 60),       // Error: 红色
                5 => Color.FromArgb(255, 139, 0, 0),         // Critical: 深红色
                _ => Color.FromArgb(255, 128, 128, 128)
            };
            return new SolidColorBrush(color);
        }
        return new SolidColorBrush(Color.FromArgb(255, 128, 128, 128));
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
