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

        var dataDir = ApplicationData.Current.LocalFolder.Path;
        var db = Path.Combine(dataDir, "core.db");
        var wal = db + "-wal";
        var shm = db + "-shm";

        var confirm = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Title"),
            Content = $"{loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Content")}\n\n{db}",
            PrimaryButtonText = loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Primary"),
            CloseButtonText = loc.GetLocalizedString("Settings_DeleteCloudDbConfirm_Secondary"),
            XamlRoot = this.Content.XamlRoot
        };

        var result = await confirm.ShowAsync();
        if (result != ContentDialogResult.Primary)
            return;

        try
        {
            // 先关 core，避免 SQLite/WAL 文件被占用
            var coreHost = App.GetService<ICoreHostService>();
            await coreHost.ShutdownAsync();
            for (int i = 0; i < 20; i++) // 最多等 4 秒
            {
                if (coreHost.State == CoreState.NotLoaded)
                {
                    // 再确认一次，防止瞬态
                    await Task.Delay(200);
                    if (coreHost.State == CoreState.NotLoaded)
                        break;
                }

                await Task.Delay(200);
            }

            // 额外保险延时（SQLite 释放句柄）
            await Task.Delay(300);
            await DeleteFileWithRetryAsync(db);
            await DeleteFileWithRetryAsync(wal);
            await DeleteFileWithRetryAsync(shm);

            // 删除后立即重启 core，避免外壳长期处于 degraded
            await coreHost.InitializeAsync();

            var done = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCloudDbDone"),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await done.ShowAsync();
        } catch (Exception ex)
        {
            var err = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCloudDbFailed"),
                Content = ex.ToString(),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await err.ShowAsync();
        }
    }

    private async void DeleteCache_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();

        var cacheRoot = ApplicationData.Current.LocalCacheFolder.Path;
        var blobsDir = Path.Combine(cacheRoot, "blobs");
        var tmpDir = Path.Combine(cacheRoot, "tmp");

        var confirm = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_DeleteCacheConfirm_Title"),
            Content = $"{loc.GetLocalizedString("Settings_DeleteCacheConfirm_Content")}\n\n{blobsDir}\n{tmpDir}",
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
            await coreHost.ShutdownAsync();

            for (int i = 0; i < 20; i++) // 最多等 4 秒
            {
                if (coreHost.State == CoreState.NotLoaded)
                {
                    // 再确认一次，防止瞬态
                    await Task.Delay(200);
                    if (coreHost.State == CoreState.NotLoaded)
                        break;
                }

                await Task.Delay(200);
            }

            // 额外保险延时（SQLite 释放句柄）
            await Task.Delay(300);

            await DeleteDirectoryWithRetryAsync(blobsDir);
            await DeleteDirectoryWithRetryAsync(tmpDir);

            await coreHost.InitializeAsync();

            var done = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCacheDone"),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await done.ShowAsync();
        } catch (Exception ex)
        {
            var err = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCacheFailed"),
                Content = ex.ToString(),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await err.ShowAsync();
        }
    }

    private static async Task DeleteFileWithRetryAsync(string path)
    {
        if (!File.Exists(path))
            return;

        for (int i = 0; i < 10; i++) // 最多重试 10 次
        {
            try
            {
                File.Delete(path);
                return;
            } catch (IOException) when (i < 9)
            {
                await Task.Delay(200); // 等待核心彻底释放句柄
            } catch (UnauthorizedAccessException) when (i < 9)
            {
                await Task.Delay(200);
            }
        }

        // 最后再尝试一次，若仍失败则抛出异常
        File.Delete(path);
    }

    private static async Task DeleteDirectoryWithRetryAsync(string path)
    {
        if (!Directory.Exists(path))
            return;

        for (int i = 0; i < 3; i++)
        {
            try
            {
                Directory.Delete(path, recursive: true);
                return;
            } catch (IOException) when (i < 2)
            {
                await Task.Delay(150);
            } catch (UnauthorizedAccessException) when (i < 2)
            {
                await Task.Delay(150);
            }
        }

        Directory.Delete(path, recursive: true);
    }
}
