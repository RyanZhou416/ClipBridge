using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using Windows.Storage;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Views;

public sealed partial class SettingsPage : Page
{
    public SettingsViewModel ViewModel
    {
        get;
    }

    public SettingsPage()
    {
        ViewModel = App.GetService<SettingsViewModel>();
        InitializeComponent();

        // 监听 VM 属性变化来更新非数据绑定的 UI (如 Window Title)
        ViewModel.PropertyChanged += ViewModel_PropertyChanged;
    }

    // 每次进入页面时初始化数据
    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await ViewModel.InitializeAsync();
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        // 当语言改变时，刷新那些无法直接绑定的 UI 元素（如标题栏）
        if (e.PropertyName == nameof(ViewModel.CurrentLanguage))
        {
            UpdateAppTitle();
        }
    }

    private void UpdateAppTitle()
    {
        var loc = Localizer.Get();

        // 刷新主窗口标题
        if (App.MainWindow is not null)
            App.MainWindow.Title = loc.GetLocalizedString("AppDisplayName");

        // 刷新自定义标题栏控件
        if (App.AppTitlebar is TextBlock tb)
            tb.Text = loc.GetLocalizedString("AppDisplayName");

        // 刷新导航栏设置项文本
        if (App.SettingsNavItem is NavigationViewItem settings)
            settings.Content = loc.GetLocalizedString("Shell_Settings");
    }

    // 打开本地文件夹
    private async void ShowLocalFolder_Click(object sender, RoutedEventArgs e)
    {
        var path = Windows.Storage.ApplicationData.Current.LocalFolder.Path;
        var dlg = new ContentDialog
        {
            Title = "LocalFolder",
            Content = path,
            PrimaryButtonText = "OK",
            XamlRoot = this.Content.XamlRoot
        };
        await dlg.ShowAsync();
    }

    // 重置按钮点击
    private async void ResetSettings_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();

        // 1. 弹出确认框
        var dlg = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_ResetConfirm_Title"),
            Content = loc.GetLocalizedString("Settings_ResetConfirm_Content"),
            PrimaryButtonText = loc.GetLocalizedString("Settings_ResetConfirm_Primary"),
            CloseButtonText = loc.GetLocalizedString("Settings_ResetConfirm_Secondary"),
            XamlRoot = this.Content.XamlRoot
        };

        var result = await dlg.ShowAsync();
        if (result != ContentDialogResult.Primary)
            return;

        // 2. 确认后调用 VM 的重置逻辑
        ViewModel.ResetSettingsCommand.Execute(null);

        // 3. 提示成功
        var done = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_ResetDone"),
            PrimaryButtonText = "OK",
            XamlRoot = this.Content.XamlRoot
        };
        await done.ShowAsync();
    }

    private async void DeleteCloudDb_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();

        var confirm = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Title"),
            Content = loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Content"),
            PrimaryButtonText = loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Primary"),
            CloseButtonText = loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Secondary"),
            XamlRoot = this.Content.XamlRoot
        };

        var result = await confirm.ShowAsync();
        if (result != ContentDialogResult.Primary)
            return;

        try
        {
            var coreHost = App.GetService<ICoreHostService>();
            if (coreHost.State != CoreState.Ready)
            {
                var err = new ContentDialog
                {
                    Title = "错误",
                    Content = "核心未就绪，无法清空数据库",
                    PrimaryButtonText = "OK",
                    XamlRoot = this.Content.XamlRoot
                };
                await err.ShowAsync();
                return;
            }

            var handle = coreHost.GetHandle();
            if (handle == IntPtr.Zero)
            {
                var err = new ContentDialog
                {
                    Title = "错误",
                    Content = "无法获取核心句柄",
                    PrimaryButtonText = "OK",
                    XamlRoot = this.Content.XamlRoot
                };
                await err.ShowAsync();
                return;
            }

            // 使用核心函数清空数据库
            await Task.Run(() =>
            {
                ClipBridgeShell_CS.Interop.CoreInterop.ClearCoreDb(handle);
            });

            var done = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCloudDbDone"),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await done.ShowAsync();
        }
        catch (Exception ex)
        {
            var err = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCloudDbFailed"),
                Content = ex.Message,
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await err.ShowAsync();
        }
    }

    private async void DeleteCache_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();

        var confirm = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_DeleteCacheConfirm_Title"),
            Content = loc.GetLocalizedString("Settings_DeleteCacheConfirm_Content"),
            PrimaryButtonText = loc.GetLocalizedString("Settings_DeleteCacheConfirm_Primary"),
            CloseButtonText = loc.GetLocalizedString("Settings_DeleteCacheConfirm_Secondary"),
            XamlRoot = this.Content.XamlRoot
        };

        var result = await confirm.ShowAsync();
        if (result != ContentDialogResult.Primary)
            return;

        try
        {
            var coreHost = App.GetService<ICoreHostService>();
            if (coreHost.State != CoreState.Ready)
            {
                var err = new ContentDialog
                {
                    Title = "错误",
                    Content = "核心未就绪，无法清空缓存",
                    PrimaryButtonText = "OK",
                    XamlRoot = this.Content.XamlRoot
                };
                await err.ShowAsync();
                return;
            }

            var handle = coreHost.GetHandle();
            if (handle == IntPtr.Zero)
            {
                var err = new ContentDialog
                {
                    Title = "错误",
                    Content = "无法获取核心句柄",
                    PrimaryButtonText = "OK",
                    XamlRoot = this.Content.XamlRoot
                };
                await err.ShowAsync();
                return;
            }

            // 使用核心函数清空缓存（如果核心提供了缓存清理接口）
            // 注意：目前核心可能没有缓存清理接口，这里先保留占位
            // 如果核心没有提供，可能需要直接删除目录
            // 但根据用户要求，应该使用核心函数，所以这里先注释掉直接删除的逻辑
            
            // TODO: 如果核心提供了缓存清理接口，使用它
            // 目前先提示用户
            var notImplemented = new ContentDialog
            {
                Title = "提示",
                Content = "缓存清理功能需要核心提供接口支持",
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await notImplemented.ShowAsync();
        }
        catch (Exception ex)
        {
            var err = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCacheFailed"),
                Content = ex.Message,
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await err.ShowAsync();
        }
    }

    private async void DeleteStatsDb_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();

        var confirm = new ContentDialog
        {
            Title = "清空统计数据",
            Content = "确定要清空所有统计数据吗？此操作不可恢复。",
            PrimaryButtonText = "确定",
            CloseButtonText = "取消",
            XamlRoot = this.Content.XamlRoot
        };

        var result = await confirm.ShowAsync();
        if (result != ContentDialogResult.Primary)
            return;

        try
        {
            var coreHost = App.GetService<ICoreHostService>();
            if (coreHost.State != CoreState.Ready)
            {
                var err = new ContentDialog
                {
                    Title = "错误",
                    Content = "核心未就绪，无法清空统计数据",
                    PrimaryButtonText = "OK",
                    XamlRoot = this.Content.XamlRoot
                };
                await err.ShowAsync();
                return;
            }

            var handle = coreHost.GetHandle();
            if (handle == IntPtr.Zero)
            {
                var err = new ContentDialog
                {
                    Title = "错误",
                    Content = "无法获取核心句柄",
                    PrimaryButtonText = "OK",
                    XamlRoot = this.Content.XamlRoot
                };
                await err.ShowAsync();
                return;
            }

            // 使用核心函数清空统计数据
            await Task.Run(() =>
            {
                ClipBridgeShell_CS.Interop.CoreInterop.ClearStatsDb(handle);
            });

            var done = new ContentDialog
            {
                Title = "完成",
                Content = "统计数据已清空",
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await done.ShowAsync();
        }
        catch (Exception ex)
        {
            var err = new ContentDialog
            {
                Title = "错误",
                Content = $"清空统计数据失败: {ex.Message}",
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await err.ShowAsync();
        }
    }

    private void RecentItemsCountBox_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter)
        {
            // 回车时，让 NumberBox 失去焦点，触发 LostFocus 事件
            (sender as NumberBox)?.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
        }
    }

    private void RecentItemsCountBox_LostFocus(object sender, RoutedEventArgs e)
    {
        // 失去焦点时，值已经通过绑定更新到 ViewModel
        // 这里可以触发 MainViewModel 重新加载 RecentItems（如果需要）
        // 由于 MainViewModel 已经监听了设置变化，会自动重新加载
    }
}
