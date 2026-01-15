using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace ClipBridgeShell_CS.Views;

public sealed partial class DevicesPage : Page
{
    public DevicesViewModel ViewModel
    {
        get;
    }

    public DevicesPage()
    {
        ViewModel = App.GetService<DevicesViewModel>();
        InitializeComponent();
    }

    private void OnDeviceTapped(object sender, TappedRoutedEventArgs e)
    {
        if (sender is Border border && border.Tag is PeerMetaPayload device)
        {
            ViewModel.SelectedDevice = device;
        }
    }

    private void OnShareToToggled(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is ToggleSwitch toggle && toggle.Tag is PeerMetaPayload device)
        {
            // TwoWay 绑定已经更新了 device.ShareToPeer 和 toggle.IsOn
            // 直接使用 toggle.IsOn 作为新值（这是用户想要的状态）
            var newValue = toggle.IsOn;
            ViewModel.SetShareToDeviceCommand.Execute((device, newValue));
        }
    }

    private void OnAcceptFromToggled(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is ToggleSwitch toggle && toggle.Tag is PeerMetaPayload device)
        {
            // TwoWay 绑定已经更新了 device.AcceptFromPeer 和 toggle.IsOn
            // 直接使用 toggle.IsOn 作为新值（这是用户想要的状态）
            var newValue = toggle.IsOn;
            ViewModel.SetAcceptFromDeviceCommand.Execute((device, newValue));
        }
    }

    private void OnDeviceMenuClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Button button && button.Tag is PeerMetaPayload device)
        {
            ViewModel.SelectedDevice = device;
            // 显示右键菜单（如果需要）
        }
    }

    private void OnShareToMenuClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is ToggleMenuFlyoutItem item && item.Tag is PeerMetaPayload device)
        {
            // 使用当前菜单项的 IsChecked 状态作为新值
            var newValue = item.IsChecked;
            ViewModel.SetShareToDeviceCommand.Execute((device, newValue));
        }
    }

    private void OnAcceptFromMenuClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is ToggleMenuFlyoutItem item && item.Tag is PeerMetaPayload device)
        {
            // 使用当前菜单项的 IsChecked 状态作为新值
            var newValue = item.IsChecked;
            ViewModel.SetAcceptFromDeviceCommand.Execute((device, newValue));
        }
    }

    private void OnCopyDeviceInfoClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem item && item.Tag is PeerMetaPayload device)
        {
            ViewModel.CopyDeviceInfoCommand.Execute(device);
        }
    }

    private void OnSetAliasClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem item && item.Tag is PeerMetaPayload device)
        {
            ViewModel.SetDeviceAliasCommand.Execute(device);
        }
    }

    private void OnClearFingerprintClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem item && item.Tag is PeerMetaPayload device)
        {
            ViewModel.ClearPeerFingerprintCommand.Execute(device);
        }
    }
}
