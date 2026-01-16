using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.ViewModels;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 根据卡片选中状态返回内边距
/// 选中：11px（补偿边框从1px变为2px）
/// 未选中：12px
/// </summary>
public sealed class CardSelectionPaddingConverter : IValueConverter
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
                    return new Thickness(11, 11, 11, 11);
                }
            }
            catch
            {
                // 如果获取 ViewModel 失败，返回默认值
            }
        }
        return new Thickness(12, 12, 12, 12);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
