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
    private readonly IStartupService _startupService;

    // 常量定义
    private const string CaptureSettingsKey = "IsClipboardCaptureEnabled";
    private const string LanguageSettingsKey = "PreferredLanguage";
    private const string RecentItemsCountKey = "MainPage_RecentItemsCount";
    private const string BackgroundImagePathKey = "MainPage_BackgroundImagePath";
    private const string CloseBehaviorKey = "CloseBehavior";
    private const string StartupMethodKey = "StartupMethod";
    private const string PerformanceEnableAcrylicOnCardsKey = "Performance_EnableAcrylicOnCards";
    private const string PerformanceEnableAcrylicOnStatsKey = "Performance_EnableAcrylicOnStatsCards";

    public sealed record ComboOption<T>(T Value, string Label);
    private bool _suppressSettingWrites;

    [ObservableProperty]
    public partial ElementTheme ElementTheme { get; set; }

    [ObservableProperty]
    public partial string VersionDescription { get; set; }

    [ObservableProperty]
    public partial IReadOnlyList<ComboOption<ElementTheme>> ThemeOptions { get; set; }

    [ObservableProperty]
    public partial IReadOnlyList<ComboOption<string>> LanguageOptions { get; set; }

    [ObservableProperty]
    public partial IReadOnlyList<ComboOption<string>> CloseBehaviorOptions { get; set; }

    [ObservableProperty]
    public partial IReadOnlyList<ComboOption<string>> StartupMethodOptions { get; set; }

    private bool _isClipboardCaptureEnabled;
    private string _currentLanguage = "en-US";
    private int _recentItemsCount = 10;
    private string? _backgroundImagePath;
    private string? _startupStateDescription;
    private bool _enableAcrylicOnCards = true;
    private bool _enableAcrylicOnStatsCards = true;

    private string _closeBehaviorValue = "MinimizeToTray";
    private string? _startupMethodValue;

    /// <summary>Raised after a startup method change completes. Args: (bool success, string message).</summary>
    public event Action<bool, string>? StartupChangeCompleted;

    // 命令：重置设置
    public ICommand ResetSettingsCommand
    {
        get;
    }

    public SettingsViewModel(IThemeSelectorService themeSelectorService, ILocalSettingsService settingsService, IStartupService startupService)
    {
        _themeSelectorService = themeSelectorService;
        _settingsService = settingsService;
        _startupService = startupService;

        ElementTheme = _themeSelectorService.Theme;
        VersionDescription = GetVersionDescription();
        ThemeOptions = Array.Empty<ComboOption<ElementTheme>>();
        LanguageOptions = Array.Empty<ComboOption<string>>();
        CloseBehaviorOptions = Array.Empty<ComboOption<string>>();
        StartupMethodOptions = Array.Empty<ComboOption<string>>();

        ResetSettingsCommand = new RelayCommand(OnResetSettings);
    }

    // 初始化入口：页面加载时调用
    public async Task InitializeAsync()
    {
        _suppressSettingWrites = true;
        try
        {
            // 1. 读取剪贴板开关
            IsClipboardCaptureEnabled = await _settingsService.ReadSettingAsync<bool?>(CaptureSettingsKey) ?? true;

            // 2. 初始化语言状态
            var current = Localizer.Get().GetCurrentLanguage();
            _currentLanguage = NormalizeLanguageTag(current);

            // 3. 读取 RecentItemsCount
            RecentItemsCount = await _settingsService.ReadSettingAsync<int?>(RecentItemsCountKey) ?? 10;

            // 4. 读取背景图片路径
            BackgroundImagePath = await _settingsService.ReadSettingAsync<string?>(BackgroundImagePathKey);

            // 5. 读取关闭行为（首次运行写入默认值）
            var savedCloseBehavior = await _settingsService.ReadSettingAsync<string?>(CloseBehaviorKey);
            if (savedCloseBehavior == null)
            {
                savedCloseBehavior = "MinimizeToTray";
                await _settingsService.SaveSettingAsync(CloseBehaviorKey, savedCloseBehavior);
            }

            // 6. 检测实际自启状态 — 遍历可用方式，找到已启用的那一个
            _startupMethodValue = StartupMethod.None.ToString();
            StartupStateDescription = null;
            foreach (var m in _startupService.GetAvailableMethods())
            {
                var state = await _startupService.GetStateAsync(m);
                if (state == StartupState.Enabled)
                {
                    _startupMethodValue = m.ToString();
                    StartupStateDescription = GetStartupStateDescription(state);
                    break;
                }
            }

            // 7. 保存逻辑值，然后构建下拉选项并选中
            _closeBehaviorValue = savedCloseBehavior ?? "MinimizeToTray";
            RefreshComboOptions();

            // 10. 性能选项（开启效果，默认 true）
            EnableAcrylicOnCards = await _settingsService.ReadSettingAsync<bool?>(PerformanceEnableAcrylicOnCardsKey) ?? true;
            EnableAcrylicOnStatsCards = await _settingsService.ReadSettingAsync<bool?>(PerformanceEnableAcrylicOnStatsKey) ?? true;
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
            if (_currentLanguage != value)
            {
                // 先切换语言，再更新属性（这样 PropertyChanged 触发时语言已经切换完成）
                SwitchLanguage(value);
                SetProperty(ref _currentLanguage, value);
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

    // 背景图片路径（null 表示使用默认图片）
    public string? BackgroundImagePath
    {
        get => _backgroundImagePath;
        set
        {
            if (SetProperty(ref _backgroundImagePath, value))
            {
                if (_suppressSettingWrites)
                    return;

                _ = _settingsService.SaveSettingAsync(BackgroundImagePathKey, value);
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

    #region Close Behavior & Startup Properties

    private ComboOption<string>? _selectedCloseBehaviorOption;
    public ComboOption<string>? SelectedCloseBehaviorOption
    {
        get => _selectedCloseBehaviorOption;
        set
        {
            if (SetProperty(ref _selectedCloseBehaviorOption, value) && value is not null)
            {
                _closeBehaviorValue = value.Value;
                if (!_suppressSettingWrites)
                    _ = _settingsService.SaveSettingAsync(CloseBehaviorKey, value.Value);
            }
        }
    }

    private ComboOption<string>? _selectedStartupMethodOption;
    public ComboOption<string>? SelectedStartupMethodOption
    {
        get => _selectedStartupMethodOption;
        set
        {
            if (SetProperty(ref _selectedStartupMethodOption, value) && value is not null)
            {
                _startupMethodValue = value.Value;
                if (!_suppressSettingWrites)
                    _ = ApplyStartupMethodAsync(value.Value);
            }
        }
    }

    public string? StartupStateDescription
    {
        get => _startupStateDescription;
        set => SetProperty(ref _startupStateDescription, value);
    }

    public bool EnableAcrylicOnCards
    {
        get => _enableAcrylicOnCards;
        set
        {
            if (SetProperty(ref _enableAcrylicOnCards, value) && !_suppressSettingWrites)
                _ = _settingsService.SaveSettingAsync(PerformanceEnableAcrylicOnCardsKey, value);
        }
    }

    public bool EnableAcrylicOnStatsCards
    {
        get => _enableAcrylicOnStatsCards;
        set
        {
            if (SetProperty(ref _enableAcrylicOnStatsCards, value) && !_suppressSettingWrites)
                _ = _settingsService.SaveSettingAsync(PerformanceEnableAcrylicOnStatsKey, value);
        }
    }

    private async Task ApplyStartupMethodAsync(string methodStr)
    {
        var loc = Localizer.Get();

        if (methodStr == StartupMethod.None.ToString())
        {
            await _startupService.DisableAllAsync();
            StartupStateDescription = GetStartupStateDescription(StartupState.Disabled);
            StartupChangeCompleted?.Invoke(true,
                GetLocalized(loc, "Settings_Startup_Result_Disabled", "Auto-start has been disabled"));
            return;
        }

        if (!Enum.TryParse<StartupMethod>(methodStr, true, out var method))
        {
            return;
        }

        await _startupService.DisableAllAsync();
        var state = await _startupService.SetEnabledAsync(method, true);
        StartupStateDescription = GetStartupStateDescription(state);

        var methodLabel = SelectedStartupMethodOption?.Label ?? methodStr;
        switch (state)
        {
            case StartupState.Enabled:
                StartupChangeCompleted?.Invoke(true,
                    GetLocalized(loc, "Settings_Startup_Result_Enabled", "Auto-start has been enabled successfully")
                    + $" ({methodLabel})");
                break;
            case StartupState.DisabledByUser:
                _suppressSettingWrites = true;
                SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == StartupMethod.None.ToString());
                _suppressSettingWrites = false;
                StartupChangeCompleted?.Invoke(false,
                    GetLocalized(loc, "Settings_Startup_State_DisabledByUser",
                        "This method is disabled by system. Enable in Task Manager > Startup, or switch to another method"));
                break;
            case StartupState.NotSupported:
                _suppressSettingWrites = true;
                SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == StartupMethod.None.ToString());
                _suppressSettingWrites = false;
                StartupChangeCompleted?.Invoke(false,
                    GetLocalized(loc, "Settings_Startup_State_NotSupported",
                        "This startup method is not supported on the current system"));
                break;
            default:
                StartupChangeCompleted?.Invoke(false,
                    GetLocalized(loc, "Settings_Startup_State_Unknown",
                        "Unable to determine startup status — the operation may have failed"));
                break;
        }
    }

    private string? GetStartupStateDescription(StartupState state)
    {
        var loc = Localizer.Get();
        return state switch
        {
            StartupState.Enabled => GetLocalized(loc, "Settings_Startup_State_Enabled",
                "Auto-start is enabled"),
            StartupState.Disabled => GetLocalized(loc, "Settings_Startup_State_Disabled",
                "Auto-start is disabled"),
            StartupState.DisabledByUser => GetLocalized(loc, "Settings_Startup_State_DisabledByUser",
                "This method is disabled by system. Enable in Task Manager > Startup, or switch to another method"),
            StartupState.NotSupported => GetLocalized(loc, "Settings_Startup_State_NotSupported",
                "This startup method is not supported on the current system"),
            StartupState.Unknown => GetLocalized(loc, "Settings_Startup_State_Unknown",
                "Unable to determine startup status — the operation may have failed"),
            _ => null,
        };
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
            _ = loc.SetLanguage(langTag);
            await _settingsService.SaveSettingAsync(LanguageSettingsKey, langTag);
            RefreshComboOptions();
        }
    }

    private void RefreshComboOptions()
    {
        // ComboBox TwoWay 绑定会在 ItemsSource 替换时将 SelectedItem 置 null，
        // 所以必须在重建列表之前通过后备字段保存当前选中值。
        var savedTheme = _selectedThemeOption?.Value ?? ElementTheme;
        var savedLang = _selectedLanguageOption?.Value ?? _currentLanguage;
        var savedCloseBehavior = _selectedCloseBehaviorOption?.Value ?? _closeBehaviorValue;
        var savedMethod = _selectedStartupMethodOption?.Value ?? _startupMethodValue;

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

        CloseBehaviorOptions = new[]
        {
            new ComboOption<string>("MinimizeToTray", GetLocalized(loc, "Settings_CloseBehavior_MinimizeToTray", "Minimize to tray")),
            new ComboOption<string>("ExitApp", GetLocalized(loc, "Settings_CloseBehavior_ExitApp", "Exit application")),
        };

        var methodOptions = new List<ComboOption<string>>
        {
            new(StartupMethod.None.ToString(), GetLocalized(loc, "Settings_StartupMethod_None", "Disabled")),
        };
        foreach (var m in _startupService.GetAvailableMethods())
        {
            var label = m switch
            {
                StartupMethod.Registry => GetLocalized(loc, "Settings_StartupMethod_Registry", "Registry Run Key"),
                StartupMethod.ScheduledTask => GetLocalized(loc, "Settings_StartupMethod_ScheduledTask", "Scheduled Task"),
                StartupMethod.MsixStartupTask => GetLocalized(loc, "Settings_StartupMethod_MsixStartupTask", "App Startup Task (MSIX)"),
                _ => m.ToString(),
            };
            methodOptions.Add(new(m.ToString(), label));
        }
        StartupMethodOptions = methodOptions;

        SelectedThemeOption = ThemeOptions.FirstOrDefault(o => o.Value == savedTheme);
        SelectedLanguageOption = LanguageOptions.FirstOrDefault(o => o.Value == savedLang);
        SelectedCloseBehaviorOption = CloseBehaviorOptions.FirstOrDefault(o => o.Value == savedCloseBehavior);
        SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == savedMethod);

        // WinUI ComboBox 在替换 ItemsSource 后可能异步重置 SelectedItem，
        // 通过 DispatcherQueue 延迟再次确认选中值，确保 UI 同步。
        var dispatcher = App.MainWindow?.DispatcherQueue;
        if (dispatcher != null)
        {
            var capturedMethod = savedMethod;
            var capturedCloseBehavior = savedCloseBehavior;
            var suppressWrites = _suppressSettingWrites;
            dispatcher.TryEnqueue(() =>
            {
                var prevSuppress = _suppressSettingWrites;
                _suppressSettingWrites = true;
                try
                {
                    if (SelectedStartupMethodOption == null && StartupMethodOptions.Count > 0)
                    {
                        SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == capturedMethod)
                                                      ?? StartupMethodOptions[0];
                    }
                    if (SelectedCloseBehaviorOption == null && CloseBehaviorOptions.Count > 0)
                    {
                        SelectedCloseBehaviorOption = CloseBehaviorOptions.FirstOrDefault(o => o.Value == capturedCloseBehavior)
                                                      ?? CloseBehaviorOptions[0];
                    }
                }
                finally
                {
                    _suppressSettingWrites = prevSuppress;
                }
            });
        }
    }

    private static string GetLocalized(ILocalizer loc, string key, string fallback)
    {
        var val = loc.GetLocalizedString(key);
        return string.IsNullOrEmpty(val) || val == key ? fallback : val;
    }

    private async void OnResetSettings()
    {
        if (ClipBridgeShell_CS.Helpers.RuntimeHelper.IsMSIX)
        {
            Windows.Storage.ApplicationData.Current.LocalSettings.Values.Clear();
        }

        string defaultLang = NormalizeLanguageTag(CultureInfo.CurrentUICulture.Name);
        CurrentLanguage = defaultLang;

        IsClipboardCaptureEnabled = true;
        ElementTheme = ElementTheme.Default;

        // Reset startup and close behavior
        await _startupService.DisableAllAsync();
        StartupStateDescription = null;
        SelectedCloseBehaviorOption = CloseBehaviorOptions.FirstOrDefault(o => o.Value == "MinimizeToTray");
        SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == StartupMethod.None.ToString());

        // Reset performance options（效果默认开启）
        EnableAcrylicOnCards = true;
        EnableAcrylicOnStatsCards = true;
    }

    // 辅助方法：规范化语言标签
    public static string NormalizeLanguageTag(string? t)
    {
        if (string.IsNullOrWhiteSpace(t))
            return "en-US";
        t = t.Trim();
        if (t.StartsWith("en", StringComparison.OrdinalIgnoreCase))
            return "en-US";
        if (t.StartsWith("zh", StringComparison.OrdinalIgnoreCase))
            return "zh-CN";
        return "en-US";
    }
    private ComboOption<ElementTheme>? _selectedThemeOption;
    public ComboOption<ElementTheme>? SelectedThemeOption
    {
        get => _selectedThemeOption;
        set
        {
            if (SetProperty(ref _selectedThemeOption, value) && value is not null)
            {
                if (!_suppressSettingWrites && ElementTheme != value.Value)
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
                if (!_suppressSettingWrites && CurrentLanguage != value.Value)
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
