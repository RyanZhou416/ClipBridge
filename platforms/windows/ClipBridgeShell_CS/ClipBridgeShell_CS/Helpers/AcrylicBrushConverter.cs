using Microsoft.UI.Xaml;
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

    // 缓存已解析的画刷，避免每次绑定求值都查找资源字典并分配新画刷
    private static readonly Dictionary<(ApplicationTheme Theme, bool DisableAcrylic), Brush> s_brushCache = new();
    private static readonly object s_cacheLock = new();

    // 兜底画刷只创建一次，后续重复使用
    private static readonly Brush s_lightSolidFallback =
        new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xF6, 0xF1, 0xF6));
    private static readonly Brush s_darkSolidFallback =
        new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x2A, 0x2A, 0x2A));

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var theme = Application.Current?.RequestedTheme ?? ApplicationTheme.Dark;

        if (value is not bool disableAcrylic)
            return GetSolidFallback(theme);

        var key = (theme, disableAcrylic);
        lock (s_cacheLock)
        {
            if (s_brushCache.TryGetValue(key, out var cached))
                return cached;
        }

        Brush result;
        try
        {
            var res = Application.Current?.Resources;
            if (res == null) return GetSolidFallback(theme);

            // 关闭亚克力：从当前主题字典取 CardSolidBackgroundBrush（与首页/统计卡片一致灰底）
            if (disableAcrylic)
            {
                result = GetThemeBrush(res, SolidKey, theme) ?? GetSolidFallback(theme);
            }
            // 开启亚克力：先尝试系统亚克力画刷，没有则用灰色打底避免透明
            else if (res.TryGetValue(AcrylicKey, out var acrylic) && acrylic is Brush acrylicBrush)
            {
                result = acrylicBrush;
            }
            else
            {
                result = GetThemeBrush(res, SolidKey, theme) ?? GetSolidFallback(theme);
            }
        }
        catch
        {
            return GetSolidFallback(theme);
        }

        lock (s_cacheLock)
        {
            s_brushCache[key] = result;
        }

        return result;
    }

    private static Brush? GetThemeBrush(ResourceDictionary res, string key, ApplicationTheme theme)
    {
        try
        {
            var themeKey = theme == ApplicationTheme.Light ? "Light" : "Dark";
            if (res.ThemeDictionaries?.TryGetValue(themeKey, out var themeDict) == true
                && themeDict is ResourceDictionary td
                && td.TryGetValue(key, out var brush)
                && brush is Brush b)
                return b;
        }
        catch { /* ignore */ }
        return null;
    }

    private static Brush GetSolidFallback(ApplicationTheme theme)
    {
        return theme == ApplicationTheme.Light ? s_lightSolidFallback : s_darkSolidFallback;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
