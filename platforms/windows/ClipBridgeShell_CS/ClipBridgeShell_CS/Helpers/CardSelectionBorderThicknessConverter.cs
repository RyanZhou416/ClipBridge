using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.ViewModels;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 根据卡片选中状态返回边框厚度
/// 选中：2px，未选中：1px
/// </summary>
public sealed class CardSelectionBorderThicknessConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is ItemMetaPayload item)
        {
            try
            {
                var viewModel = App.GetService<MainViewModel>();
                if (viewModel != null && viewModel.IsItemSelected(item.ItemId))
                {
                    return new Thickness(2);
                }
            }
            catch
            {
                // 如果获取 ViewModel 失败，返回默认值
            }
        }
        return new Thickness(1);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
