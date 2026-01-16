using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.ViewModels;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 根据卡片选中状态返回边框颜色
/// 选中：从 ThemeDictionaries 获取 CardSelectedBorderBrush
/// 未选中：使用 CardStrokeColorDefaultBrush
/// </summary>
public sealed class CardSelectionBorderBrushConverter : IValueConverter
{
    private static FrameworkElement? _pageReference;

    /// <summary>
    /// 设置 Page 引用，用于访问 ThemeDictionaries
    /// 应该在 MainPage 的 Loaded 事件中调用
    /// </summary>
    public static void SetPageReference(FrameworkElement page)
    {
        _pageReference = page;
    }

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is ItemMetaPayload item)
        {
            try
            {
                var viewModel = App.GetService<MainViewModel>();
                if (viewModel != null && viewModel.IsItemSelected(item.ItemId))
                {
                    // 选中时，从 Page 的 ThemeDictionaries 获取
                    if (_pageReference != null)
                    {
                        var themeDictionaries = _pageReference.Resources?.ThemeDictionaries;
                        if (themeDictionaries != null)
                        {
                            var currentTheme = _pageReference.ActualTheme == ElementTheme.Light ? "Light" : "Dark";
                            if (themeDictionaries.TryGetValue(currentTheme, out var themeDict) && 
                                themeDict is ResourceDictionary themeResourceDict)
                            {
                                if (themeResourceDict.TryGetValue("CardSelectedBorderBrush", out var resource) && 
                                    resource is SolidColorBrush themeBrush)
                                {
                                    return themeBrush;
                                }
                            }
                        }
                    }
                    
                    // 如果无法从 Page 获取，尝试从 Application 资源获取
                    if (Application.Current.Resources.TryGetValue("CardSelectedBorderBrush", out var appResource) &&
                        appResource is SolidColorBrush appBrush)
                    {
                        return appBrush;
                    }
                }
            }
            catch
            {
                // 如果获取失败，返回默认值
            }
        }
        
        // 未选中时使用默认边框颜色
        if (Application.Current.Resources.TryGetValue("CardStrokeColorDefaultBrush", out var defaultResource) &&
            defaultResource is Brush defaultBrush)
        {
            return defaultBrush;
        }
        
        // 最后的后备方案
        return new SolidColorBrush(Microsoft.UI.Colors.Gray);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
