using System.Globalization;
using System.Reflection;
using System.Windows.Input;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;

using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

using Microsoft.UI.Xaml;

using Windows.ApplicationModel;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.ViewModels;

public partial class SettingsViewModel : ObservableRecipient
{
    private readonly IThemeSelectorService _themeSelectorService;
    private readonly ILocalSettingsService _settingsService;

    // 常量定义
    private const string CaptureSettingsKey = "IsClipboardCaptureEnabled";
    private const string LanguageSettingsKey = "PreferredLanguage";
    private const string RecentItemsCountKey = "MainPage_RecentItemsCount";

    public sealed record ComboOption<T>(T Value, string Label);
    private bool _suppressSettingWrites;

    [ObservableProperty]
    private ElementTheme _elementTheme;

    [ObservableProperty]
    private string _versionDescription;

    [ObservableProperty]
    private IReadOnlyList<ComboOption<ElementTheme>> _themeOptions = Array.Empty<ComboOption<ElementTheme>>();

    [ObservableProperty]
    private IReadOnlyList<ComboOption<string>> _languageOptions = Array.Empty<ComboOption<string>>();

    private bool _isClipboardCaptureEnabled;
    private string _currentLanguage = "en-US";
    private int _recentItemsCount = 10;

    // 命令：重置设置
    public ICommand ResetSettingsCommand
    {
        get;
    }

    public SettingsViewModel(IThemeSelectorService themeSelectorService, ILocalSettingsService settingsService)
    {
        _themeSelectorService = themeSelectorService;
        _settingsService = settingsService;

        // 初始化当前主题 (从 Service 同步)
        _elementTheme = _themeSelectorService.Theme;
        _versionDescription = GetVersionDescription();

        // 初始化命令
        ResetSettingsCommand = new RelayCommand(OnResetSettings);
    }

    // 初始化入口：页面加载时调用
    public async Task InitializeAsync()
    {
        // 1. 读取剪贴板开关
        _suppressSettingWrites = true;
        try
        {
            IsClipboardCaptureEnabled = await _settingsService.ReadSettingAsync<bool?>(CaptureSettingsKey) ?? true;
        } finally
        {
            _suppressSettingWrites = false;
        }

        // 2. 初始化语言状态
        var current = Localizer.Get().GetCurrentLanguage();
        CurrentLanguage = NormalizeLanguageTag(current);
        RefreshComboOptions();

        // 3. 读取RecentItemsCount
        _suppressSettingWrites = true;
        try
        {
            RecentItemsCount = await _settingsService.ReadSettingAsync<int?>(RecentItemsCountKey) ?? 10;
        }
        finally
        {
            _suppressSettingWrites = false;
        }
    }

    #region Properties (Data Binding)

    // 剪贴板开关
    public bool IsClipboardCaptureEnabled
    {
        get => _isClipboardCaptureEnabled;
        set
        {
            if (SetProperty(ref _isClipboardCaptureEnabled, value))
            {
                if (_suppressSettingWrites)
                    return;

                _ = _settingsService.SaveSettingAsync(CaptureSettingsKey, value);
            }

        }
    }

    // 当前语言
    public string CurrentLanguage
    {
        get => _currentLanguage;
        set
        {
            if (SetProperty(ref _currentLanguage, value))
            {
                SwitchLanguage(value);
            }
        }
    }

    // 主页显示的卡片数量
    public int RecentItemsCount
    {
        get => _recentItemsCount;
        set
        {
            // 限制范围：1-50
            var clampedValue = Math.Clamp(value, 1, 50);
            if (SetProperty(ref _recentItemsCount, clampedValue))
            {
                if (_suppressSettingWrites)
                    return;

                _ = _settingsService.SaveSettingAsync(RecentItemsCountKey, clampedValue);
            }
        }
    }

    // 重写 ElementTheme 的 Setter 以触发服务调用
    partial void OnElementThemeChanged(ElementTheme value)
    {
        // 只有当服务中的主题和当前不一致时才调用，避免递归
        if (_themeSelectorService.Theme != value)
        {
            _ = _themeSelectorService.SetThemeAsync(value);
        }
    }

    #endregion

    #region Logic Methods

    private async void SwitchLanguage(string langTag)
    {
        if (string.IsNullOrEmpty(langTag))
            return;

        var loc = Localizer.Get();
        // 如果语言真的变了才切换
        if (loc.GetCurrentLanguage() != langTag)
        {
            loc.SetLanguage(langTag);
            await _settingsService.SaveSettingAsync(LanguageSettingsKey, langTag);
            RefreshComboOptions();
        }
    }

    private void RefreshComboOptions()
    {
        var loc = Localizer.Get();

        ThemeOptions = new[]
        {
            new ComboOption<ElementTheme>(ElementTheme.Default, loc.GetLocalizedString("Settings_Theme_Default")),
            new ComboOption<ElementTheme>(ElementTheme.Light,   loc.GetLocalizedString("Settings_Theme_Light")),
            new ComboOption<ElementTheme>(ElementTheme.Dark,    loc.GetLocalizedString("Settings_Theme_Dark")),
        };

        LanguageOptions = new[]
        {
            new ComboOption<string>("en-US", loc.GetLocalizedString("Settings_Lang_English")),
            new ComboOption<string>("zh-CN", loc.GetLocalizedString("Settings_Lang_Chinese")),
        };

        SelectedThemeOption = ThemeOptions.FirstOrDefault(o => o.Value == ElementTheme);
        SelectedLanguageOption = LanguageOptions.FirstOrDefault(o => o.Value == CurrentLanguage);

    }

    private async void OnResetSettings()
    {
        // 1. 清空所有本地设置
        Windows.Storage.ApplicationData.Current.LocalSettings.Values.Clear();

        // 2. 恢复默认语言 (跟随系统)
        string defaultLang = NormalizeLanguageTag(CultureInfo.CurrentUICulture.Name);
        // 强制触发 setter 逻辑
        CurrentLanguage = defaultLang;

        // 3. 恢复默认开关
        IsClipboardCaptureEnabled = true;

        // 4. 恢复默认主题
        ElementTheme = ElementTheme.Default;
        // OnElementThemeChanged 会自动被触发并调用 Service
    }

    // 辅助方法：规范化语言标签
    public static string NormalizeLanguageTag(string? t)
    {
        if (string.IsNullOrWhiteSpace(t))
            return "en-US";
        t = t.Trim();
        if (t.Equals("en", StringComparison.OrdinalIgnoreCase))
            return "en-US";
        if (t.Equals("zh", StringComparison.OrdinalIgnoreCase))
            return "zh-CN";
        if (t.Equals("zh-Hans", StringComparison.OrdinalIgnoreCase))
            return "zh-CN";
        return t;
    }
    private ComboOption<ElementTheme>? _selectedThemeOption;
    public ComboOption<ElementTheme>? SelectedThemeOption
    {
        get => _selectedThemeOption;
        set
        {
            if (SetProperty(ref _selectedThemeOption, value) && value is not null)
            {
                // 只在不同的时候写回，避免循环
                if (ElementTheme != value.Value)
                    ElementTheme = value.Value;
            }
        }
    }

    private ComboOption<string>? _selectedLanguageOption;
    public ComboOption<string>? SelectedLanguageOption
    {
        get => _selectedLanguageOption;
        set
        {
            if (SetProperty(ref _selectedLanguageOption, value) && value is not null)
            {
                if (CurrentLanguage != value.Value)
                    CurrentLanguage = value.Value;
            }
        }
    }
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

    #endregion
}
