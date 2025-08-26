using System.Diagnostics;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;

using Microsoft.UI.Xaml;

namespace ClipBridgeShell_CS.Services;

public class ThemeSelectorService : IThemeSelectorService
{
    private const string SettingsKey = "AppBackgroundRequestedTheme";

    public ElementTheme Theme { get; set; } = ElementTheme.Default;

    private readonly ILocalSettingsService _localSettingsService;

    public ThemeSelectorService(ILocalSettingsService localSettingsService)
    {
        _localSettingsService = localSettingsService;
    }

    public async Task InitializeAsync()
    {
        Debug.WriteLine("[THEME] InitializeAsync");
        Theme = await LoadThemeFromSettingsAsync();
        await Task.CompletedTask;
    }

    public async Task SetThemeAsync(ElementTheme theme)
    {
        Debug.WriteLine($"[THEME] SetThemeAsync({theme})");
        Theme = theme;

        await SetRequestedThemeAsync();
        await SaveThemeInSettingsAsync(Theme);
    }

    public async Task SetRequestedThemeAsync()
    {
        if (App.MainWindow.Content is FrameworkElement rootElement)
        {
            rootElement.RequestedTheme = Theme;
            Debug.WriteLine($"[THEME] root={rootElement.GetType().Name}, RequestedTheme -> {rootElement.RequestedTheme}");
            TitleBarHelper.UpdateTitleBar(Theme);
        }
        else
        {
            Debug.WriteLine("[THEME] App.MainWindow.Content is NOT FrameworkElement");
        }

        await Task.CompletedTask;
    }

    private async Task<ElementTheme> LoadThemeFromSettingsAsync()
    {
        var themeName = await _localSettingsService.ReadSettingAsync<string>(SettingsKey);

        if (Enum.TryParse(themeName, out ElementTheme cacheTheme))
        {
            return cacheTheme;
        }

        return ElementTheme.Default;
    }

    private async Task SaveThemeInSettingsAsync(ElementTheme theme)
    {
        await _localSettingsService.SaveSettingAsync(SettingsKey, theme.ToString());
    }
}
