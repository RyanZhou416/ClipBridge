using System.Diagnostics;
using System.Reflection;
using System.Windows.Input;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Services;

using CommunityToolkit.Mvvm.ComponentModel;

using Microsoft.UI.Xaml;

using Windows.ApplicationModel;

namespace ClipBridgeShell_CS.ViewModels;

public partial class SettingsViewModel : ObservableRecipient
{
    private readonly IThemeSelectorService _themeSelectorService;

    // 新增：注入本地设置与语言服务
    private readonly ILocalSettingsService _localSettings;
    private readonly ILocalizationService _loc;

    [ObservableProperty] private LanguageOption? selectedLanguage;

    // 新增：可选语言项
    public record LanguageOption(string Tag, string Display);
    public LanguageOption[] Languages
    {
        get;
    } =
    {
        new("en-US","English"),
        new("zh-CN","简体中文"),
    };
    // 新增：当前选择（绑定 ComboBox.SelectedValue）
    private bool _suspendLangApply = true;

    [ObservableProperty]
    private ElementTheme _elementTheme;

    [ObservableProperty]
    private string _versionDescription;

    public ICommand SwitchThemeCommand
    {
        get;
    }

    public SettingsViewModel(IThemeSelectorService themeSelectorService, ILocalSettingsService localSettings, ILocalizationService loc)
    {

        _themeSelectorService = themeSelectorService;
        _localSettings = localSettings;
        _loc = loc;

        _elementTheme = _themeSelectorService.Theme;
        _versionDescription = GetVersionDescription();

        var current = _loc.CurrentLanguageTag; // 已经是规范化的，如 zh-CN
        // 选中当前生效语言对应的项
        SelectedLanguage = Languages.FirstOrDefault(x => string.Equals(x.Tag, current, StringComparison.OrdinalIgnoreCase))
        ?? Languages.FirstOrDefault(x =>   // 处理 zh-Hans / zh-TW 等落到中文
        current.StartsWith("zh", StringComparison.OrdinalIgnoreCase) &&
        x.Tag.StartsWith("zh", StringComparison.OrdinalIgnoreCase))
        ?? Languages[0];

        _suspendLangApply = false;


    }


    partial void OnSelectedLanguageChanged(LanguageOption? value)
    {
        Debug.WriteLine($"[{DateTime.Now:HH:mm:ss.fff}] [VM] SelectedLanguageChanged → {value?.Tag}");
        if (_suspendLangApply) return;
        if (value is null) return;

        _ = ApplyLanguageAsync(value.Tag);
    }

    private async Task ApplyLanguageAsync(string languageTag)
    {
        Debug.WriteLine($"[{DateTime.Now:HH:mm:ss.fff}] [VM] ApplyLanguageAsync save+switch → {languageTag}");
        await _localSettings.SaveSettingAsync(PreferredLanguageKey, languageTag);
        _loc.SetLanguage(languageTag, hotReload: true);
    }

    private const string PreferredLanguageKey = "PreferredLanguage";


    private static string GetVersionDescription()
    {
        Version version;

        if (RuntimeHelper.IsMSIX)
        {
            var packageVersion = Package.Current.Id.Version;

            version = new(packageVersion.Major, packageVersion.Minor, packageVersion.Build, packageVersion.Revision);
        }
        else
        {
            version = Assembly.GetExecutingAssembly().GetName().Version!;
        }

        return $"{"AppDisplayName".GetLocalized()} - {version.Major}.{version.Minor}.{version.Build}.{version.Revision}";
    }
}
