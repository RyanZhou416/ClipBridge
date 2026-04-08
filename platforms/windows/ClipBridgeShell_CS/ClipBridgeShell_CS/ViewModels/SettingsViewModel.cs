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
    private const string StartupEnabledKey = "StartupEnabled";
    private const string PerformanceEnableAcrylicOnCardsKey = "Performance_EnableAcrylicOnCards";
    private const string PerformanceEnableAcrylicOnStatsKey = "Performance_EnableAcrylicOnStatsCards";

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

    [ObservableProperty]
    private IReadOnlyList<ComboOption<string>> _closeBehaviorOptions = Array.Empty<ComboOption<string>>();

    [ObservableProperty]
    private IReadOnlyList<ComboOption<string>> _startupMethodOptions = Array.Empty<ComboOption<string>>();

    private bool _isClipboardCaptureEnabled;
    private string _currentLanguage = "en-US";
    private int _recentItemsCount = 10;
    private string? _backgroundImagePath;
    private bool _isStartupEnabled;
    private string? _startupStateDescription;
    private bool _enableAcrylicOnCards = true;
    private bool _enableAcrylicOnStatsCards = true;

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

        _elementTheme = _themeSelectorService.Theme;
        _versionDescription = GetVersionDescription();

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

            // 6. 读取自启方式（首次运行根据运行模式自动选择）
            var savedMethodStr = await _settingsService.ReadSettingAsync<string?>(StartupMethodKey);
            var available = _startupService.GetAvailableMethods();
            var savedMethod = RuntimeHelper.IsMSIX ? StartupMethod.MsixStartupTask : StartupMethod.Registry;
            if (savedMethodStr != null
                && Enum.TryParse<StartupMethod>(savedMethodStr, true, out var parsedMethod)
                && available.Contains(parsedMethod))
            {
                savedMethod = parsedMethod;
            }
            else
            {
                savedMethodStr = savedMethod.ToString();
                await _settingsService.SaveSettingAsync(StartupMethodKey, savedMethodStr);
            }

            // 7. 读取自启开关（首次运行写入默认值）
            var savedEnabled = await _settingsService.ReadSettingAsync<bool?>(StartupEnabledKey);
            if (savedEnabled == null)
            {
                savedEnabled = false;
                await _settingsService.SaveSettingAsync(StartupEnabledKey, false);
            }

            if (savedEnabled.Value)
            {
                var state = await _startupService.GetStateAsync(savedMethod);
                savedEnabled = state == StartupState.Enabled;
                StartupStateDescription = GetStartupStateDescription(state);
            }

            _isStartupEnabled = savedEnabled.Value;
            OnPropertyChanged(nameof(IsStartupEnabled));

            // 8. 构建所有下拉选项（只构建一次）
            RefreshComboOptions();

            // 9. 从已构建的选项中选中当前值（使用同一实例引用）
            SelectedCloseBehaviorOption = CloseBehaviorOptions.FirstOrDefault(o => o.Value == savedCloseBehavior);
            SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == savedMethodStr);

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
            if (SetProperty(ref _selectedCloseBehaviorOption, value) && value is not null && !_suppressSettingWrites)
            {
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
            if (SetProperty(ref _selectedStartupMethodOption, value) && value is not null && !_suppressSettingWrites)
            {
                _ = OnStartupMethodChangedAsync(value.Value);
            }
        }
    }

    public bool IsStartupEnabled
    {
        get => _isStartupEnabled;
        set
        {
            if (SetProperty(ref _isStartupEnabled, value) && !_suppressSettingWrites)
            {
                _ = OnStartupEnabledChangedAsync(value);
            }
        }
    }

    public string? StartupStateDescription
    {
        get => _startupStateDescription;
        set => SetProperty(ref _startupStateDescription, value);
    }

    /// <summary>开启首页最近条目卡片的亚克力效果（默认开启，持久化）</summary>
    public bool EnableAcrylicOnCards
    {
        get => _enableAcrylicOnCards;
        set
        {
            if (SetProperty(ref _enableAcrylicOnCards, value) && !_suppressSettingWrites)
                _ = _settingsService.SaveSettingAsync(PerformanceEnableAcrylicOnCardsKey, value);
        }
    }

    /// <summary>开启首页统计信息卡片的亚克力效果（默认开启，持久化）</summary>
    public bool EnableAcrylicOnStatsCards
    {
        get => _enableAcrylicOnStatsCards;
        set
        {
            if (SetProperty(ref _enableAcrylicOnStatsCards, value) && !_suppressSettingWrites)
                _ = _settingsService.SaveSettingAsync(PerformanceEnableAcrylicOnStatsKey, value);
        }
    }

    private async Task OnStartupEnabledChangedAsync(bool enable)
    {
        var defaultMethod = RuntimeHelper.IsMSIX ? StartupMethod.MsixStartupTask : StartupMethod.Registry;
        var methodStr = SelectedStartupMethodOption?.Value ?? defaultMethod.ToString();
        if (!Enum.TryParse<StartupMethod>(methodStr, true, out var method))
        {
            method = defaultMethod;
        }

        if (enable)
        {
            var state = await _startupService.SetEnabledAsync(method, true);
            StartupStateDescription = GetStartupStateDescription(state);

            if (state == StartupState.DisabledByUser)
            {
                _suppressSettingWrites = true;
                IsStartupEnabled = false;
                _suppressSettingWrites = false;
            }
            else
            {
                await _settingsService.SaveSettingAsync(StartupEnabledKey, state == StartupState.Enabled);
                await _settingsService.SaveSettingAsync(StartupMethodKey, methodStr);
            }
        }
        else
        {
            await _startupService.DisableAllAsync();
            StartupStateDescription = null;
            await _settingsService.SaveSettingAsync(StartupEnabledKey, false);
        }
    }

    private async Task OnStartupMethodChangedAsync(string methodStr)
    {
        if (!IsStartupEnabled)
        {
            await _settingsService.SaveSettingAsync(StartupMethodKey, methodStr);
            return;
        }

        var defaultMethod = RuntimeHelper.IsMSIX ? StartupMethod.MsixStartupTask : StartupMethod.Registry;
        if (!Enum.TryParse<StartupMethod>(methodStr, true, out var method))
        {
            method = defaultMethod;
        }

        var state = await _startupService.SetEnabledAsync(method, true);
        StartupStateDescription = GetStartupStateDescription(state);
        await _settingsService.SaveSettingAsync(StartupMethodKey, methodStr);

        if (state == StartupState.DisabledByUser)
        {
            _suppressSettingWrites = true;
            IsStartupEnabled = false;
            _suppressSettingWrites = false;
            await _settingsService.SaveSettingAsync(StartupEnabledKey, false);
        }
    }

    private string? GetStartupStateDescription(StartupState state)
    {
        if (state == StartupState.DisabledByUser)
        {
            var loc = Localizer.Get();
            var desc = loc.GetLocalizedString("Settings_Startup_DisabledByUser");
            return string.IsNullOrEmpty(desc) || desc == "Settings_Startup_DisabledByUser"
                ? "This method is disabled by system. Enable in Task Manager > Startup, or switch to another startup method below."
                : desc;
        }
        return null;
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

        CloseBehaviorOptions = new[]
        {
            new ComboOption<string>("MinimizeToTray", GetLocalized(loc, "Settings_CloseBehavior_MinimizeToTray", "Minimize to tray")),
            new ComboOption<string>("ExitApp", GetLocalized(loc, "Settings_CloseBehavior_ExitApp", "Exit application")),
        };

        var methodOptions = new List<ComboOption<string>>();
        foreach (var m in _startupService.GetAvailableMethods())
        {
            var label = m switch
            {
                StartupMethod.Registry => GetLocalized(loc, "Settings_StartupMethod_Registry", "Registry Run Key"),
                StartupMethod.ScheduledTask => GetLocalized(loc, "Settings_StartupMethod_ScheduledTask", "Scheduled Task"),
                StartupMethod.MsixStartupTask => GetLocalized(loc, "Settings_StartupMethod_MsixStartupTask", "App Startup Task (MSIX)"),
                _ => m.ToString(),
            };
            methodOptions.Add(new ComboOption<string>(m.ToString(), label));
        }
        StartupMethodOptions = methodOptions;

        SelectedThemeOption = ThemeOptions.FirstOrDefault(o => o.Value == ElementTheme);
        SelectedLanguageOption = LanguageOptions.FirstOrDefault(o => o.Value == CurrentLanguage);

        // CloseBehavior / StartupMethod 的选中由 InitializeAsync 或语言切换时重新匹配
        var currentCloseBehavior = SelectedCloseBehaviorOption?.Value;
        if (currentCloseBehavior != null)
        {
            SelectedCloseBehaviorOption = CloseBehaviorOptions.FirstOrDefault(o => o.Value == currentCloseBehavior);
        }

        var currentMethod = SelectedStartupMethodOption?.Value;
        if (currentMethod != null)
        {
            SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == currentMethod);
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
        IsStartupEnabled = false;
        StartupStateDescription = null;
        SelectedCloseBehaviorOption = CloseBehaviorOptions.FirstOrDefault(o => o.Value == "MinimizeToTray");
        var defaultMethod = RuntimeHelper.IsMSIX ? StartupMethod.MsixStartupTask : StartupMethod.Registry;
        SelectedStartupMethodOption = StartupMethodOptions.FirstOrDefault(o => o.Value == defaultMethod.ToString());

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
