using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
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
}
