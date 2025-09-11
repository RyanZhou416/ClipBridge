using System.Globalization;
using System.Linq;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage;
using WinUI3Localizer;



namespace ClipBridgeShell_CS.Views;

// TODO: Set the URL for your privacy policy by updating SettingsPage_PrivacyTermsLink.NavigateUri in Resources.resw.
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

        this.Loaded += SettingsPage_Loaded;

        var current = WinUI3Localizer.Localizer.Get().GetCurrentLanguage();
        foreach (var it in LanguageCombo.Items.OfType<ComboBoxItem>())
        {
            if (it.Tag?.ToString().Equals(current, StringComparison.OrdinalIgnoreCase) == true)
            {
                LanguageCombo.SelectedItem = it;
                break;
            }
        }
    }

    private void SettingsPage_Loaded(object sender, RoutedEventArgs e)
    {
        // 1) 让主题下拉框显示当前主题
        var themeSvc = App.GetService<IThemeSelectorService>();
        var currentTheme = themeSvc is not null ? themeSvc.Theme : ElementTheme.Default; // Theme 从 LocalSettings 加载过来
        foreach (var it in ThemeCombo.Items.OfType<ComboBoxItem>())
        {
            if (string.Equals(it.Tag?.ToString(), currentTheme.ToString(), StringComparison.OrdinalIgnoreCase))
            {
                ThemeCombo.SelectedItem = it;
                break;
            }
        }

        var loc = Localizer.Get();
        var currentLan = loc.GetCurrentLanguage(); // 可能返回 "en" / "zh" / "zh-Hans" / "en-US" 等

        var tag = NormalizeTag(currentLan);
        foreach (var it in LanguageCombo.Items.OfType<ComboBoxItem>())
        {
            if (string.Equals(it.Tag?.ToString(), tag, StringComparison.OrdinalIgnoreCase))
            {
                LanguageCombo.SelectedItem = it;
                break;
            }
        }
    }

    private static string NormalizeTag(string t)
    {
        if (string.IsNullOrWhiteSpace(t)) return "en-US";
        t = t.Trim();

        // 常见对齐：en -> en-US； zh/zh-Hans -> zh-CN
        if (t.Equals("en", StringComparison.OrdinalIgnoreCase)) return "en-US";
        if (t.Equals("zh", StringComparison.OrdinalIgnoreCase)) return "zh-CN";
        if (t.Equals("zh-Hans", StringComparison.OrdinalIgnoreCase)) return "zh-CN";

        return t;
    }

    private async void ShowLocalFolder_Click(object sender, RoutedEventArgs e)
    {
        //用于显示应用的本地文件夹路径
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

    private async void LanguageCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (LanguageCombo?.SelectedItem is ComboBoxItem item && item.Tag is string lang && !string.IsNullOrEmpty(lang))
        {
            var loc = Localizer.Get();
            loc.SetLanguage(lang); // 触发全 UI 使用 l:Uids 的控件自动刷新

            // 手动刷新“代码里赋值过”的文本（窗口标题等）
            if (App.MainWindow is not null)
            {
                App.MainWindow.Title = loc.GetLocalizedString("AppDisplayName");
            }

            if (App.AppTitlebar is TextBlock tb)
            {
                tb.Text = loc.GetLocalizedString("AppDisplayName");
            }
            
            if (App.SettingsNavItem is NavigationViewItem settings)
            {
                settings.Content = loc.GetLocalizedString("Shell_Settings");
            }

            await App.GetService<ILocalSettingsService>().SaveSettingAsync("PreferredLanguage", lang);
        }

    }


    private async void ResetSettings_Click(object sender, RoutedEventArgs e)
    {
        var loc = Localizer.Get();
        var dlg = new ContentDialog
        {
            Title = Localizer.Get().GetLocalizedString("Settings_ResetConfirm_Title"),
            Content = loc.GetLocalizedString("Settings_ResetConfirm_Content"),
            PrimaryButtonText = loc.GetLocalizedString("Settings_ResetConfirm_Primary"),
            CloseButtonText = loc.GetLocalizedString("Settings_ResetConfirm_Secondary"),
            XamlRoot = this.Content.XamlRoot
        };
        var result = await dlg.ShowAsync();
        if (result != ContentDialogResult.Primary) return;

        // 2) 清空 LocalSettings（保存的所有首选项都会被清掉）
        ApplicationData.Current.LocalSettings.Values.Clear();

        // 3) 决定“重置后的语言” = 系统 UI 语言（规范化成我们支持的标记）
        string defaultLang = NormalizeLanguageTag(CultureInfo.CurrentUICulture.Name);

        // 4) 切换 Localizer 到默认语言（立即热刷新 UI）
        loc.SetLanguage(defaultLang);

        // 5) 刷新“代码里赋值”的文本
        if (App.MainWindow is not null)
            App.MainWindow.Title = loc.GetLocalizedString("AppDisplayName");

        if (App.AppTitlebar is TextBlock tb)
            tb.Text = loc.GetLocalizedString("AppDisplayName");

        if (App.SettingsNavItem is NavigationViewItem settingsItem)
            settingsItem.Content = loc.GetLocalizedString("Shell_Settings");

        // 6) 让语言下拉框选中新的语言
        foreach (var it in LanguageCombo.Items.OfType<ComboBoxItem>())
        {
            if (it.Tag?.ToString().Equals(defaultLang, StringComparison.OrdinalIgnoreCase) == true)
            {
                LanguageCombo.SelectedItem = it;
                break;
            }
        }

        // 7) 把“重置后的默认值”写回 LocalSettings（以后启动也按这个来）
        await App.GetService<ILocalSettingsService>().SaveSettingAsync("PreferredLanguage", defaultLang);

        // 8) 提示成功（可选）
        var done = new ContentDialog
        {
            Title = loc.GetLocalizedString("Settings_ResetDone"),
            PrimaryButtonText = "OK",
            XamlRoot = this.Content.XamlRoot
        };
        await done.ShowAsync();
    }

    private static string NormalizeLanguageTag(string? t)
    {
        if (string.IsNullOrWhiteSpace(t)) return "en-US";
        t = t.Trim();
        if (t.Equals("en", StringComparison.OrdinalIgnoreCase)) return "en-US";
        if (t.Equals("zh", StringComparison.OrdinalIgnoreCase)) return "zh-CN";
        if (t.Equals("zh-Hans", StringComparison.OrdinalIgnoreCase)) return "zh-CN";
        return t;
    }
    private async void ThemeCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (ThemeCombo?.SelectedItem is ComboBoxItem item && item.Tag is string tag &&
            Enum.TryParse<ElementTheme>(tag, out var theme))
        {
            // 2) 通过服务真正切换 + 持久化（这是原来 RadioButton 的实质动作）
            var themeSvc = App.GetService<IThemeSelectorService>();
            await themeSvc.SetThemeAsync(theme); // 更新 RequestedTheme & 标题栏 & 保存到 LocalSettings

            // 3)（可选）把 VM 的 ElementTheme 也同步一下，便于界面上其它绑定立即反映
            ViewModel.ElementTheme = theme;
        }
    }

}
