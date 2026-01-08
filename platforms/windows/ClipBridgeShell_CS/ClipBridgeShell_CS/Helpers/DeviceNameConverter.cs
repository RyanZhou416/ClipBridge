using System;
using Microsoft.UI.Xaml.Data;
using ClipBridgeShell_CS.ViewModels;

namespace ClipBridgeShell_CS.Helpers;

public class DeviceNameConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is string deviceId)
        {
            // 从 Application 获取 MainViewModel
            if (parameter is MainViewModel viewModel)
            {
                return viewModel.GetDeviceName(deviceId);
            }
            // 如果没有提供 ViewModel，尝试从 App 获取
            try
            {
                var vm = App.GetService<MainViewModel>();
                return vm?.GetDeviceName(deviceId) ?? deviceId;
            }
            catch
            {
                return deviceId;
            }
        }
        return value?.ToString() ?? "Unknown";
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        throw new NotImplementedException();
    }
}
