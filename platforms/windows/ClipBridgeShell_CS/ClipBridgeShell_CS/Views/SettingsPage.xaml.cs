using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Services;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Storage.Pickers;
using Windows.System;
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

        ViewModel.PropertyChanged += ViewModel_PropertyChanged;
        ViewModel.StartupChangeCompleted += OnStartupChangeCompleted;
    }

    private async void OnStartupChangeCompleted(bool success, string message)
    {
        var loc = Localizer.Get();
        var dialog = new ContentDialog
        {
            Title = success
                ? GetLocalized(loc, "Settings_Startup_Dialog_SuccessTitle", "Success")
                : GetLocalized(loc, "Settings_Startup_Dialog_FailTitle", "Failed"),
            Content = message,
            PrimaryButtonText = "OK",
            XamlRoot = Content.XamlRoot,
        };
        await dialog.ShowAsync();
    }

    private static string GetLocalized(ILocalizer loc, string key, string fallback)
    {
        var val = loc.GetLocalizedString(key);
        return string.IsNullOrEmpty(val) || val == key ? fallback : val;
    }

    // 每次进入页面时初始化数据
    protected async override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await ViewModel.InitializeAsync();

        // 初始化核心开关状态
        var coreHost = App.GetService<ICoreHostService>();
        CoreToggleSwitch.IsOn = coreHost.State == CoreState.Ready;

        // 订阅状态变化
        coreHost.StateChanged += OnCoreStateChanged;
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        // 取消订阅
        var coreHost = App.GetService<ICoreHostService>();
        coreHost.StateChanged -= OnCoreStateChanged;
    }

    private void OnCoreStateChanged(CoreState state)
    {
        // 在UI线程更新开关状态
        App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
        {
            CoreToggleSwitch.IsOn = state == CoreState.Ready;
        });
    }

    private async void CoreToggleSwitch_Toggled(object sender, RoutedEventArgs e)
    {
        var toggleSwitch = sender as ToggleSwitch;
        if (toggleSwitch == null) return;

        var coreHost = App.GetService<ICoreHostService>();

        if (toggleSwitch.IsOn)
        {
            // 开启核心
            if (coreHost.State != CoreState.Ready)
            {
                try
                {
                    await coreHost.InitializeAsync();
                }
                catch (Exception ex)
                {
                    var err = new ContentDialog
                    {
                        Title = "错误",
                        Content = $"启动核心失败: {ex.Message}",
                        PrimaryButtonText = "OK",
                        XamlRoot = this.Content.XamlRoot
                    };
                    await err.ShowAsync();
                    // 恢复开关状态
                    toggleSwitch.IsOn = false;
                }
            }
        }
        else
        {
            // 关闭核心
            if (coreHost.State != CoreState.NotLoaded)
            {
                try
                {
                    await coreHost.ShutdownAsync();
                }
                catch (Exception ex)
                {
                    var err = new ContentDialog
                    {
                        Title = "错误",
                        Content = $"关闭核心失败: {ex.Message}",
                        PrimaryButtonText = "OK",
                        XamlRoot = this.Content.XamlRoot
                    };
                    await err.ShowAsync();
                    // 恢复开关状态
                    toggleSwitch.IsOn = true;
                }
            }
        }
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
        {
            settings.Content = loc.GetLocalizedString("Shell_Settings");
        }
    }

    // 打开本地文件夹
    private async void ShowLocalFolder_Click(object sender, RoutedEventArgs e)
    {
        string path;
        if (ClipBridgeShell_CS.Helpers.RuntimeHelper.IsMSIX)
        {
            path = Windows.Storage.ApplicationData.Current.LocalFolder.Path;
        }
        else
        {
            // 与 core 使用同一个解析器：便携模式下这里显示的必须是 <exeDir>\data。
            path = CoreConfigBuilder.ResolveDataRoot("ClipBridge");
        }

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

            // 使用 Core Bridge 清空数据库
            if (!await coreHost.ClearCoreDbAsync())
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

            // 使用 Core Bridge 清空缓存
            if (!await coreHost.ClearCacheAsync())
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

            var done = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_DeleteCacheDone"),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await done.ShowAsync();
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

    /// <summary>
    /// 导出一次性备份（单向导出，没有恢复入口）。成功对话框展示目标目录与各部分尺寸。
    /// </summary>
    private async void ExportBackup_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();
        var coreHost = App.GetService<ICoreHostService>();

        if (coreHost.State != CoreState.Ready)
        {
            var notReady = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_ExportBackupFailed"),
                Content = loc.GetLocalizedString("Settings_ExportBackupCoreNotReady"),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await notReady.ShowAsync();
            return;
        }

        // 在选目录 + 导出期间禁用按钮，避免重复点击打开多个 picker / 并发导出。
        ExportBackupButton.IsEnabled = false;
        try
        {
            // WinUI 3 的 FolderPicker 必须先绑定窗口句柄，否则抛出 COMException。
            var picker = new FolderPicker();
            var windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, windowHandle);
            picker.SuggestedStartLocation = PickerLocationId.DocumentsLibrary;
            picker.FileTypeFilter.Add("*");

            var folder = await picker.PickSingleFolderAsync();
            if (folder is null)
            {
                return; // 用户取消
            }

            var includeContent = ExportBackupIncludeContent.IsChecked == true;
            ExportBackupProgress.Visibility = Visibility.Visible;
            ExportBackupProgress.IsActive = true;

            var info = await coreHost.ExportBackupAsync(folder.Path, includeContent);
            var done = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_ExportBackupDone_Title"),
                Content = string.Format(
                    loc.GetLocalizedString("Settings_ExportBackupDone_Content"),
                    info.Dest,
                    info.CoreDbBytes,
                    info.LogsDbBytes,
                    info.ContentBytes,
                    info.Items,
                    info.History),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await done.ShowAsync();
        }
        catch (Exception ex)
        {
            var err = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_ExportBackupFailed"),
                Content = ex.Message,
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await err.ShowAsync();
        }
        finally
        {
            ExportBackupProgress.IsActive = false;
            ExportBackupProgress.Visibility = Visibility.Collapsed;
            ExportBackupButton.IsEnabled = true;
        }
    }

    private async void DeleteStatsDb_Click(object sender, RoutedEventArgs e)
    {
        _ = Localizer.Get();

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

            // 使用 Core Bridge 清空统计数据
            if (!await coreHost.ClearStatsDbAsync())
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

    private async void ClearLocalCert_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();

        var confirm = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_ClearLocalCertConfirm_Title"),
            Content = loc.GetLocalizedString("Settings_ClearLocalCertConfirm_Content"),
            PrimaryButtonText = loc.GetLocalizedString("Settings_ClearLocalCertConfirm_Primary"),
            CloseButtonText = loc.GetLocalizedString("Settings_ClearLocalCertConfirm_Secondary"),
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
                    Content = "核心未就绪，无法清除本地证书",
                    PrimaryButtonText = "OK",
                    XamlRoot = this.Content.XamlRoot
                };
                await err.ShowAsync();
                return;
            }

            // 调用清除本地证书
            await Task.Run(() =>
            {
                coreHost.ClearLocalCert();
            });

            var done = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_ClearLocalCertDone"),
                Content = loc.GetLocalizedString("Settings_ClearLocalCertDone_Content"),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await done.ShowAsync();
        }
        catch (Exception ex)
        {
            var err = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_ClearLocalCertFailed"),
                Content = ex.Message,
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

    // 选择背景图片
    private async void SelectBackgroundImage_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var loc = Localizer.Get();

            // 创建文件选择器
            var picker = new FileOpenPicker();
            var windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, windowHandle);

            picker.ViewMode = PickerViewMode.Thumbnail;
            picker.SuggestedStartLocation = PickerLocationId.PicturesLibrary;
            picker.FileTypeFilter.Add(".jpg");
            picker.FileTypeFilter.Add(".jpeg");
            picker.FileTypeFilter.Add(".png");
            picker.FileTypeFilter.Add(".bmp");

            var file = await picker.PickSingleFileAsync();
            if (file == null)
                return;

            // 验证图片尺寸（最小 1920x600，推荐 2560x800 或更高）
            var minWidth = 1920;
            var minHeight = 600;
            var recommendedWidth = 2560;
            var recommendedHeight = 800;

            int width, height;
            using (var stream = await file.OpenAsync(FileAccessMode.Read))
            {
                var decoder = await BitmapDecoder.CreateAsync(stream);
                width = (int)decoder.PixelWidth;
                height = (int)decoder.PixelHeight;

                if (width < minWidth || height < minHeight)
                {
                    var error = new ContentDialog
                    {
                        Title = loc.GetLocalizedString("Settings_BackgroundImage_InvalidSize_Title"),
                        Content = string.Format(loc.GetLocalizedString("Settings_BackgroundImage_InvalidSize_Content"),
                            width, height, minWidth, minHeight, recommendedWidth, recommendedHeight),
                        PrimaryButtonText = "OK",
                        XamlRoot = this.Content.XamlRoot
                    };
                    await error.ShowAsync();
                    return;
                }

                // 如果尺寸小于推荐尺寸，给出警告但允许继续
                if (width < recommendedWidth || height < recommendedHeight)
                {
                    var warning = new ContentDialog
                    {
                        Title = loc.GetLocalizedString("Settings_BackgroundImage_LowResolution_Title"),
                        Content = string.Format(loc.GetLocalizedString("Settings_BackgroundImage_LowResolution_Content"),
                            width, height, recommendedWidth, recommendedHeight),
                        PrimaryButtonText = loc.GetLocalizedString("Settings_BackgroundImage_Continue"),
                        CloseButtonText = loc.GetLocalizedString("Settings_BackgroundImage_Cancel"),
                        XamlRoot = this.Content.XamlRoot
                    };
                    var result = await warning.ShowAsync();
                    if (result != ContentDialogResult.Primary)
                        return;
                }
            }

            // 复制文件到本地存储
            StorageFolder imagesFolder;
            if (ClipBridgeShell_CS.Helpers.RuntimeHelper.IsMSIX)
            {
                var localFolder = ApplicationData.Current.LocalFolder;
                imagesFolder = await localFolder.CreateFolderAsync("BackgroundImages", CreationCollisionOption.OpenIfExists);
            }
            else
            {
                // 背景图也跟随数据根目录，避免便携版把用户数据留在 LocalAppData。
                var localFolderPath = CoreConfigBuilder.ResolveDataRoot("ClipBridge");
                var imagesFolderPath = Path.Combine(localFolderPath, "BackgroundImages");
                if (!Directory.Exists(imagesFolderPath)) Directory.CreateDirectory(imagesFolderPath);
                imagesFolder = await StorageFolder.GetFolderFromPathAsync(imagesFolderPath);
            }

            // 删除旧的背景图片（如果存在）
            if (!string.IsNullOrEmpty(ViewModel.BackgroundImagePath))
            {
                try
                {
                    var oldFile = await StorageFile.GetFileFromPathAsync(ViewModel.BackgroundImagePath);
                    if (oldFile != null && oldFile.Path.StartsWith(imagesFolder.Path))
                    {
                        await oldFile.DeleteAsync();
                    }
                }
                catch
                {
                    // 忽略删除失败
                }
            }

            // 复制新文件
            var newFile = await file.CopyAsync(imagesFolder, $"background_{DateTime.Now.Ticks}.{file.FileType.TrimStart('.')}", NameCollisionOption.ReplaceExisting);

            // 保存路径到设置
            ViewModel.BackgroundImagePath = newFile.Path;

            var success = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_BackgroundImage_Success_Title"),
                Content = string.Format(loc.GetLocalizedString("Settings_BackgroundImage_Success_Content"), width, height),
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await success.ShowAsync();
        }
        catch (Exception ex)
        {
            var loc = Localizer.Get();
            var error = new ContentDialog
            {
                Title = loc.GetLocalizedString("Settings_BackgroundImage_Error_Title"),
                Content = $"{loc.GetLocalizedString("Settings_BackgroundImage_Error_Content")}: {ex.Message}",
                PrimaryButtonText = "OK",
                XamlRoot = this.Content.XamlRoot
            };
            await error.ShowAsync();
        }
    }

    // 重置背景图片为默认
    private async void ResetBackgroundImage_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();

        var confirm = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_BackgroundImage_ResetConfirm_Title"),
            Content = loc.GetLocalizedString("Settings_BackgroundImage_ResetConfirm_Content"),
            PrimaryButtonText = loc.GetLocalizedString("Settings_BackgroundImage_ResetConfirm_Primary"),
            CloseButtonText = loc.GetLocalizedString("Settings_BackgroundImage_ResetConfirm_Secondary"),
            XamlRoot = this.Content.XamlRoot
        };

        var result = await confirm.ShowAsync();
        if (result != ContentDialogResult.Primary)
            return;

        // 删除自定义背景图片文件（如果存在）
        if (!string.IsNullOrEmpty(ViewModel.BackgroundImagePath))
        {
            try
            {
                var file = await StorageFile.GetFileFromPathAsync(ViewModel.BackgroundImagePath);
                if (file != null)
                {
                    await file.DeleteAsync();
                }
            }
            catch
            {
                // 忽略删除失败
            }
        }

        // 清除设置
        ViewModel.BackgroundImagePath = null;

        var done = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_BackgroundImage_ResetDone"),
            PrimaryButtonText = "OK",
            XamlRoot = this.Content.XamlRoot
        };
        await done.ShowAsync();
    }

    private async void LicenseLink_Click(object sender, RoutedEventArgs e)
    {
        var uri = new Uri("https://github.com/RyanZhou416/ClipBridge/blob/main/LICENSE");
        await Launcher.LaunchUriAsync(uri);
    }

    private async void GitHubLink_Click(object sender, RoutedEventArgs e)
    {
        var uri = new Uri("https://github.com/RyanZhou416/ClipBridge");
        await Launcher.LaunchUriAsync(uri);
    }
}
