using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.ViewModels;
using ClipBridgeShell_CS.Views;

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Storage;
using Windows.System;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Views;

// TODO: Update NavigationViewItem titles and icons in ShellPage.xaml.
public sealed partial class ShellPage : Page
{
    public ShellViewModel ViewModel
    {
        get;
    }

    public ShellPage(ShellViewModel viewModel)
    {
        ViewModel = viewModel;
        InitializeComponent();

        ViewModel.NavigationService.Frame = NavigationFrame;
        ViewModel.NavigationViewService.Initialize(NavigationViewControl);

        // TODO: Set the title bar icon by updating /Assets/WindowIcon.ico.
        // A custom title bar is required for full window theme and Mica support.
        // https://docs.microsoft.com/windows/apps/develop/title-bar?tabs=winui3#full-customization
        App.MainWindow.ExtendsContentIntoTitleBar = true;
        App.MainWindow.SetTitleBar(AppTitleBar);
        App.MainWindow.Activated += MainWindow_Activated;
        AppTitleBarText.Text = WinUI3Localizer.Localizer.Get().GetLocalizedString("AppDisplayName");
    }

    private void OnLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        TitleBarHelper.UpdateTitleBar(RequestedTheme);

        KeyboardAccelerators.Add(BuildKeyboardAccelerator(VirtualKey.Left, VirtualKeyModifiers.Menu));
        KeyboardAccelerators.Add(BuildKeyboardAccelerator(VirtualKey.GoBack));


    }

    private void MainWindow_Activated(object sender, WindowActivatedEventArgs args)
    {
        App.AppTitlebar = AppTitleBarText as UIElement;
    }

    private void NavigationViewControl_DisplayModeChanged(NavigationView sender, NavigationViewDisplayModeChangedEventArgs args)
    {
        AppTitleBar.Margin = new Thickness()
        {
            Left = sender.CompactPaneLength * (sender.DisplayMode == NavigationViewDisplayMode.Minimal ? 2 : 1),
            Top = AppTitleBar.Margin.Top,
            Right = AppTitleBar.Margin.Right,
            Bottom = AppTitleBar.Margin.Bottom
        };
    }

    private static KeyboardAccelerator BuildKeyboardAccelerator(VirtualKey key, VirtualKeyModifiers? modifiers = null)
    {
        var keyboardAccelerator = new KeyboardAccelerator() { Key = key };

        if (modifiers.HasValue)
        {
            keyboardAccelerator.Modifiers = modifiers.Value;
        }

        keyboardAccelerator.Invoked += OnKeyboardAcceleratorInvoked;

        return keyboardAccelerator;
    }

    private static void OnKeyboardAcceleratorInvoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        var navigationService = App.GetService<INavigationService>();

        var result = navigationService.GoBack();

        args.Handled = result;
    }

    private void NavigationViewControl_Loaded(object sender, RoutedEventArgs e)
    {
        if (NavigationViewControl.SettingsItem is NavigationViewItem settings)
        {
            App.SettingsNavItem = settings;
            var loc = Localizer.Get();
            settings.Content = loc.GetLocalizedString("Shell_Settings");
            AutomationProperties.SetName(settings, loc.GetLocalizedString("Shell_Settings"));
        }
    }

    private async void OnAvatarTapped(object sender, TappedRoutedEventArgs e)
    {
        var accountService = App.GetService<IAccountService>();
        var hasAccount = await accountService.HasAccountAsync();

        if (!hasAccount)
        {
            // 显示登录窗口
            var loginDialog = new LoginDialog(accountService);
            loginDialog.XamlRoot = this.XamlRoot;
            await loginDialog.ShowAsync();
        }
        else
        {
            // 显示账号信息和更换账号选项
            var account = await accountService.LoadAccountAsync();
            if (account.HasValue)
            {
                var deviceName = System.Environment.MachineName;
                
                // 创建内容面板
                var contentPanel = new StackPanel
                {
                    Spacing = 12
                };
                
                var loc = Localizer.Get();
                
                // 账号信息
                var accountInfo = new TextBlock
                {
                    Text = string.Format(loc.GetLocalizedString("AccountInfo_Account"), account.Value.username),
                    FontSize = 14,
                    Margin = new Thickness(0, 0, 0, 8)
                };
                contentPanel.Children.Add(accountInfo);
                
                // 设备信息
                var deviceInfo = new TextBlock
                {
                    Text = string.Format(loc.GetLocalizedString("AccountInfo_Device"), deviceName),
                    FontSize = 14
                };
                contentPanel.Children.Add(deviceInfo);
                
                var dialog = new ContentDialog
                {
                    Title = loc.GetLocalizedString("AccountInfo_Title"),
                    Content = contentPanel,
                    PrimaryButtonText = loc.GetLocalizedString("AccountInfo_SwitchAccount"),
                    CloseButtonText = loc.GetLocalizedString("AccountInfo_Close"),
                    XamlRoot = this.XamlRoot
                };
                
                var result = await dialog.ShowAsync();
                
                // 如果点击了"更换账号"
                if (result == ContentDialogResult.Primary)
                {
                    // 清除当前账号
                    await accountService.ClearAccountAsync();
                    
                    // 关闭核心（如果正在运行）
                    var coreHost = App.GetService<ICoreHostService>();
                    if (coreHost.State == CoreState.Ready || 
                        coreHost.State == CoreState.Loading)
                    {
                        await coreHost.ShutdownAsync();
                    }
                    
                    // 显示登录对话框
                    var loginDialog = new LoginDialog(accountService);
                    loginDialog.XamlRoot = this.XamlRoot;
                    await loginDialog.ShowAsync();
                }
            }
        }
    }

}
