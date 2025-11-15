//platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/App.xaml.cs
using System.Diagnostics;
using System.Globalization;
using System.IO;
using ClipBridgeShell_CS.Activation;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Core.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Models;
using ClipBridgeShell_CS.Notifications;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.ViewModels;
using ClipBridgeShell_CS.Views;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Windows.AppNotifications.Builder;
using Windows.Storage;
using WinRT.Interop;
using WinRT.Interop;
using WinUI3Localizer;
using Microsoft.Windows.AppNotifications.Builder;

namespace ClipBridgeShell_CS;

// To learn more about WinUI 3, see https://docs.microsoft.com/windows/apps/winui/winui3/.
public partial class App : Application
{
    // The .NET Generic Host provides dependency injection, configuration, logging, and other services.
    // https://docs.microsoft.com/dotnet/core/extensions/generic-host
    // https://docs.microsoft.com/dotnet/core/extensions/dependency-injection
    // https://docs.microsoft.com/dotnet/core/extensions/configuration
    // https://docs.microsoft.com/dotnet/core/extensions/logging
    public IHost Host
    {
        get;
    }

    public static T GetService<T>()
        where T : class
    {
        if ((App.Current as App)!.Host.Services.GetService(typeof(T)) is not T service)
        {
            throw new ArgumentException($"{typeof(T)} needs to be registered in ConfigureServices within App.xaml.cs.");
        }

        return service;
    }

    //public static WindowEx MainWindow { get; } = new MainWindow();
    public static WindowEx? MainWindow
    {
        get; set;
    }

    public static UIElement? AppTitlebar { get; set; }
    public static NavigationViewItem? SettingsNavItem
    {
        get; set;
    }
    public App()
    {
        InitializeComponent();

        Host = Microsoft.Extensions.Hosting.Host.
        CreateDefaultBuilder().
        UseContentRoot(AppContext.BaseDirectory).
        ConfigureServices((context, services) =>
        {
            // Default Activation Handler
            services.AddTransient<ActivationHandler<LaunchActivatedEventArgs>, DefaultActivationHandler>();

            // Other Activation Handlers
            services.AddTransient<IActivationHandler, AppNotificationActivationHandler>();

            // Services
            services.AddSingleton<IAppNotificationService, AppNotificationService>();
            services.AddSingleton<ILocalSettingsService, LocalSettingsService>();
            services.AddSingleton<IThemeSelectorService, ThemeSelectorService>();
            services.AddTransient<INavigationViewService, NavigationViewService>();

            services.AddSingleton<IActivationService, ActivationService>();
            services.AddSingleton<IPageService, PageService>();
            services.AddSingleton<INavigationService, NavigationService>();

            // Core Services
            services.AddSingleton<ISampleDataService, SampleDataService>();
            services.AddSingleton<IFileService, FileService>();

            // Views and ViewModels
            services.AddTransient<SettingsViewModel>();
            services.AddTransient<SettingsPage>();
            services.AddTransient<LogsViewModel>();
            services.AddTransient<LogsPage>();
            services.AddTransient<DevicesViewModel>();
            services.AddTransient<DevicesPage>();
            services.AddTransient<MainViewModel>();
            services.AddTransient<MainPage>();
            services.AddTransient<ShellPage>();
            services.AddTransient<ShellViewModel>();

            // Configuration
            services.Configure<LocalSettingsOptions>(context.Configuration.GetSection(nameof(LocalSettingsOptions)));
        }).
        Build();

        App.GetService<IAppNotificationService>().Initialize();

        UnhandledException += App_UnhandledException;
    }

    private void App_UnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        // TODO: Log and handle exceptions as appropriate.
        // https://docs.microsoft.com/windows/windows-app-sdk/api/winrt/microsoft.ui.xaml.application.unhandledexception.
    }

    protected async override void OnLaunched(LaunchActivatedEventArgs args)
    {
        base.OnLaunched(args);

        //App.GetService<IAppNotificationService>().Show(string.Format("AppNotificationSamplePayload".GetLocalized(), AppContext.BaseDirectory));

        // ① 初始化 Localizer（打包应用版）
        await InitializeLocalizer();

        // ② 再走 Template Studio 原有流程（里面会调用 ActivationService）
        await App.GetService<IActivationService>().ActivateAsync(args);

        PrintCoreDllInfo();

    }

    private async Task InitializeLocalizer()
    {
        // 1) 准备 LocalFolder\Strings（保持你原有的代码）
        StorageFolder localFolder = ApplicationData.Current.LocalFolder;
        StorageFolder stringsFolder = await localFolder.CreateFolderAsync("Strings", CreationCollisionOption.OpenIfExists);
        const string resw = "Resources.resw";
        await CreateStringResourceFileIfNotExists(stringsFolder, "en-US", resw);
        await CreateStringResourceFileIfNotExists(stringsFolder, "zh-CN", resw);

        // 2) 读取已保存的语言；如果没有 → 第一次启动，用系统语言推断一个
        var settings = App.GetService<ILocalSettingsService>();
        var saved = await settings.ReadSettingAsync<string>("PreferredLanguage");
        string defaultLang = NormalizeLanguageTag(saved ?? CultureInfo.CurrentUICulture.Name);

        // 如果是第一次启动（没有 PreferredLanguage），保存下来
        if (string.IsNullOrEmpty(saved))
        {
            await settings.SaveSettingAsync("PreferredLanguage", defaultLang);
            // 你也可以顺手保存一个 “IsFirstRun = false”
            await settings.SaveSettingAsync("IsFirstRun", false);
        }

        // 3) 用默认语言构建 Localizer
        _ = await new LocalizerBuilder()
            .AddStringResourcesFolderForLanguageDictionaries(stringsFolder.Path)
            .SetOptions(o => o.DefaultLanguage = defaultLang)
            .Build();
    }

    // 规范化常见语言代码：en -> en-US，zh / zh-Hans -> zh-CN
    private static string NormalizeLanguageTag(string? t)
    {
        if (string.IsNullOrWhiteSpace(t)) return "en-US";
        t = t.Trim();
        if (t.Equals("en", StringComparison.OrdinalIgnoreCase)) return "en-US";
        if (t.Equals("zh", StringComparison.OrdinalIgnoreCase)) return "zh-CN";
        if (t.Equals("zh-Hans", StringComparison.OrdinalIgnoreCase)) return "zh-CN";
        return t;
    }

    private static async Task CreateStringResourceFileIfNotExists(StorageFolder stringsFolder, string language, string resourceFileName)
    {
        StorageFolder languageFolder = await stringsFolder.CreateFolderAsync(
            language, CreationCollisionOption.OpenIfExists);

        string resourceFilePath = Path.Combine(stringsFolder.Name, language, resourceFileName);
        StorageFile resourceFile = await LoadStringResourcesFileFromAppResource(resourceFilePath);


        _ = await resourceFile.CopyAsync(languageFolder, resourceFileName, NameCollisionOption.ReplaceExisting);
    }

    private static async Task<StorageFile> LoadStringResourcesFileFromAppResource(string filePath)
    {
        Uri resourcesFileUri = new($"ms-appx:///{filePath}");
        return await StorageFile.GetFileFromApplicationUriAsync(resourcesFileUri);
    }

    private void PrintCoreDllInfo()
    {
        try
        {
            string dllName = "core_ffi_windows.dll";
            string dllPath = Path.Combine(AppContext.BaseDirectory, dllName);

            string header, body;
            if (File.Exists(dllPath))
            {
                FileInfo info = new FileInfo(dllPath);
                header = dllName;
                body =
                    $"{info.DirectoryName}\n" +
                    $"大小: {info.Length:N0} 字节\n" +
                    $"创建: {info.CreationTime}\n" +
                    $"修改: {info.LastWriteTime}";
            }
            else
            {
                header = dllName;
                body = $"未找到该文件\n路径: {dllPath}";
            }

            var builder = new AppNotificationBuilder()
                .AddText(header)
                .AddText(body);

            string payloadXml = builder.BuildNotification().Payload;
            App.GetService<IAppNotificationService>().Show(payloadXml);
        }
        catch (Exception ex)
        {
            var builder = new AppNotificationBuilder()
                .AddText("读取 DLL 信息失败")
                .AddText(ex.Message);

            string payloadXml = builder.BuildNotification().Payload;
            App.GetService<IAppNotificationService>().Show(payloadXml);
        }
    }


}
