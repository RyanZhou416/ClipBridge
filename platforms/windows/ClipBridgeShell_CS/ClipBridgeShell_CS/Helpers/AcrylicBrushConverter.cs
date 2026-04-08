using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将“是否关闭亚克力”转换为背景画刷：true = 使用统一灰色打底，false = 使用亚克力背景。
/// 关闭亚克力时使用 App 中 CardSolidBackgroundBrush（按主题），开启时使用系统亚克力画刷。
/// </summary>
public sealed class AcrylicBrushConverter : IValueConverter
{
    private const string AcrylicKey = "AcrylicBackgroundFillColorDefaultBrush";
    private const string SolidKey = "CardSolidBackgroundBrush";

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is not bool disableAcrylic)
            return GetSolidFallback();

        try
        {
            var app = Microsoft.UI.Xaml.Application.Current;
            var res = app?.Resources as Microsoft.UI.Xaml.ResourceDictionary;
            if (res == null) return GetSolidFallback();

            // 关闭亚克力：从当前主题字典取 CardSolidBackgroundBrush（与首页/统计卡片一致灰底）
            if (disableAcrylic)
            {
                var solid = GetThemeBrush(res, SolidKey);
                if (solid != null) return solid;
                return GetSolidFallback();
            }

            // 开启亚克力：先尝试系统亚克力画刷，没有则用灰色打底避免透明
            if (res.TryGetValue(AcrylicKey, out var acrylic) && acrylic is Brush acrylicBrush)
                return acrylicBrush;
            var fallbackSolid = GetThemeBrush(res, SolidKey);
            if (fallbackSolid != null) return fallbackSolid;
            return GetSolidFallback();
        }
        catch
        {
            return GetSolidFallback();
        }
    }

    private static Brush? GetThemeBrush(Microsoft.UI.Xaml.ResourceDictionary res, string key)
    {
        try
        {
            var theme = Microsoft.UI.Xaml.Application.Current?.RequestedTheme ?? Microsoft.UI.Xaml.ApplicationTheme.Dark;
            var themeKey = theme == Microsoft.UI.Xaml.ApplicationTheme.Light ? "Light" : "Dark";
            if (res.ThemeDictionaries?.TryGetValue(themeKey, out var themeDict) == true
                && themeDict is Microsoft.UI.Xaml.ResourceDictionary td
                && td.TryGetValue(key, out var brush)
                && brush is Brush b)
                return b;
        }
        catch { /* ignore */ }
        return null;
    }

    private static Brush GetSolidFallback()
    {
        var theme = Microsoft.UI.Xaml.Application.Current?.RequestedTheme ?? Microsoft.UI.Xaml.ApplicationTheme.Dark;
        var color = theme == Microsoft.UI.Xaml.ApplicationTheme.Light
            ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xF6, 0xF1, 0xF6)
            : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x2A, 0x2A, 0x2A);
        return new SolidColorBrush(color);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
