using System;
using Microsoft.UI.Xaml.Data;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 将 ItemKind (text, image, file_list) 转换为本地化的类型字符串
/// </summary>
public class ItemKindConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is not string kind)
            return string.Empty;

        try
        {
            // 获取本地化字符串
            var localizer = Localizer.Get();
            string localizedKind;
            
            switch (kind.ToLowerInvariant())
            {
                case "text":
                    localizedKind = localizer.GetLocalizedString("ItemKind_Text");
                    break;
                case "image":
                    localizedKind = localizer.GetLocalizedString("ItemKind_Image");
                    break;
                case "file_list":
                case "filelist":
                    localizedKind = localizer.GetLocalizedString("ItemKind_FileList");
                    break;
                default:
                    localizedKind = kind; // 未知类型，返回原值
                    break;
            }
            
            // 返回格式化的字符串："类型：{localizedKind}"
            var typeLabel = localizer.GetLocalizedString("ItemKind_TypeLabel");
            return $"{typeLabel}{localizedKind}";
        }
        catch
        {
            // 如果本地化失败，返回默认格式
            return $"类型：{kind}";
        }
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
