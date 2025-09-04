using System.Linq;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
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
        var loc = Localizer.Get();
        var current = loc.GetCurrentLanguage(); // 可能返回 "en" / "zh" / "zh-Hans" / "en-US" 等

        var tag = NormalizeTag(current);
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


}
