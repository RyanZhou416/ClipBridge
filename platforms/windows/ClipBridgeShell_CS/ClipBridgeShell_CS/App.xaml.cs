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

using WinUI3Localizer;

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

            services.AddSingleton<Stores.HistoryStore>();
            services.AddSingleton<Stores.PeerStore>();
            services.AddSingleton<Stores.TransferStore>();
            services.AddSingleton<EventPumpService>();

            //CB Core Host Service
            services.AddSingleton<CoreHostService>();
            services.AddSingleton<ICoreHostService, CoreHostService>();
            services.AddSingleton<IClipboardService, ClipboardService>();

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
            services.AddSingleton<ClipboardWatcher>();

            // History Service
            services.AddTransient<HistoryViewModel>();
            services.AddTransient<HistoryPage>();
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


#if DEBUG
        PrintCoreDllInfo();
#endif
        _ = App.GetService<CoreHostService>().InitializeAsync();

        // 启动剪贴板监听
        App.GetService<ClipboardWatcher>().Initialize();

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

    /// <summary>
    /// 检测 WinUI 壳当前正在使用的 core_ffi_windows.dll，
    /// 并与 Rust 输出目录中的最新 DLL 进行比对，
    /// 在通知中显示：是否为最新版本、文件大小、创建/修改时间、
    /// Debug/Release、TFM、架构（x64）、RID（win-x64）等信息。
    ///
    /// 逻辑顺序：
    /// 1. 自动在运行目录及周边搜索 DLL（FindCoreDll）
    /// 2. 读取 Rust 的官方构建目录 DLL
    /// 3. 比较 LastWriteTime 判断是否为最新
    /// 4. 使用 AnalyzeBuildInfo 解析 DLL 所在路径的构建信息
    /// 5. 通过通知显示最终检测结果
    /// </summary>
    private void PrintCoreDllInfo()
    {
        try
        {
            string dllName = "core_ffi_windows.dll";

            // 1. 找到 WinUI 实际运行时使用的 DLL
            string? appDllPath = FindCoreDll(dllName);

            // 2. Rust 官方输出目录 DLL
            string rustDllPath =
                @"C:\Project\ClipBridge\target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll";

            string header;
            string body;

            bool appDllExists = !string.IsNullOrEmpty(appDllPath) && File.Exists(appDllPath);
            bool rustDllExists = File.Exists(rustDllPath);

            FileInfo? appInfo = appDllExists ? new FileInfo(appDllPath!) : null;
            FileInfo? rustInfo = rustDllExists ? new FileInfo(rustDllPath) : null;

            // ============================
            // 🔍 3. 生成结论 (header)
            // ============================
            if (appDllExists && rustDllExists)
            {
                if (appInfo!.LastWriteTime >= rustInfo!.LastWriteTime)
                    header = $"✔ 已是最新 DLL";
                else
                    header = $"❌ DLL 落后 — Rust 有更新版本";
            }
            else if (!rustDllExists)
            {
                header = "⚠ Rust 输出 DLL 不存在";
            }
            else
            {
                header = "❌ 未找到应用 DLL";
            }

            // ============================
            // 📄 4. body 显示所有详细信息
            // ============================
            var buildInfo = appDllExists ? AnalyzeBuildInfo(appDllPath!) : default;

            body =
                $"应用DLL: {(appDllExists ? appDllPath : "未找到")}\n" +
                (appDllExists ?
                    $"大小: {appInfo!.Length:N0} 字节\n" +
                    $"创建: {appInfo.CreationTime}\n" +
                    $"修改: {appInfo.LastWriteTime}\n" +
                    $"配置: {buildInfo.Configuration}\n" +
                    $"TFM: {buildInfo.Tfm}\n" +
                    $"架构目录: {buildInfo.Arch}\n" +
                    $"RID目录: {buildInfo.RidDir}\n\n"
                    : ""
                ) +
                $"Rust DLL: {(rustDllExists ? rustDllPath : "不存在")}\n" +
                (rustDllExists ?
                    $"创建: {rustInfo!.CreationTime}\n" +
                    $"修改: {rustInfo.LastWriteTime}\n" : "");

            var builder = new AppNotificationBuilder()
                .AddText(header)
                .AddText(body);

            App.GetService<IAppNotificationService>()
               .Show(builder.BuildNotification().Payload);
        }
        catch (Exception ex)
        {
            var builder = new AppNotificationBuilder()
                .AddText("读取 DLL 信息失败")
                .AddText(ex.Message);

            App.GetService<IAppNotificationService>()
               .Show(builder.BuildNotification().Payload);
        }
    }


    /// <summary>
    /// 在 WinUI 壳的运行目录 (AppContext.BaseDirectory) 以及其父目录中
    /// 自动搜索 core_ffi_windows.dll，
    /// 支持查找：
    /// - 当前目录
    /// - 当前目录下的 win-x64 目录
    /// - 向上最多 5 层目录
    ///
    /// 适配各种输出结构：
    /// bin/x64/Debug/net9.0/
    /// bin/Debug/net9.0/win-x64/
    /// Release/net8.0/
    ///
    /// 返回：DLL 的完整路径（如果找到）
    /// </summary>
    private static string? FindCoreDll(string dllName)
    {
        string? current = AppContext.BaseDirectory?
            .TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);

        if (string.IsNullOrEmpty(current))
            return null;

        const int maxLevels = 5;
        int level = 0;

        while (current != null && level <= maxLevels)
        {
            // 1. 当前目录
            string candidate = Path.Combine(current, dllName);
            if (File.Exists(candidate))
                return candidate;

            // 2. 当前目录下的 win-x64
            string winX64 = Path.Combine(current, "win-x64", dllName);
            if (File.Exists(winX64))
                return winX64;

            // 上一层
            var parent = Directory.GetParent(current);
            current = parent?.FullName;
            level++;
        }

        return null;
    }

    /// <summary>
    /// 解析 DLL 所在路径中的构建信息，包括：
    /// - Debug / Release
    /// - TFM（如 net9.0-windows10.0.26100.0）
    /// - 架构目录（x64 / arm64）
    /// - RID 目录（win-x64）
    ///
    /// 解析逻辑来自目录名称自动推断，
    /// 不依赖任何写死路径，适配所有 WinUI3 输出结构。
    /// </summary>
    private static (string Configuration, string Tfm, string Arch, string RidDir) AnalyzeBuildInfo(string dllPath)
    {
        string dir = Path.GetDirectoryName(dllPath) ?? string.Empty;

        var parts = dir
            .Split(new[] { Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar },
                   StringSplitOptions.RemoveEmptyEntries);

        // 配置：Debug / Release
        string configuration = parts.FirstOrDefault(p =>
            p.Equals("Debug", StringComparison.OrdinalIgnoreCase) ||
            p.Equals("Release", StringComparison.OrdinalIgnoreCase)
        ) ?? "未知";

        // TFM：例如 net9.0-windows10.0.26100.0
        string tfm = parts.FirstOrDefault(p =>
            p.StartsWith("net", StringComparison.OrdinalIgnoreCase)
        ) ?? "未知";

        // 架构目录：x64 / x86 / arm64
        string arch = parts.FirstOrDefault(p =>
            p.Equals("x64", StringComparison.OrdinalIgnoreCase) ||
            p.Equals("x86", StringComparison.OrdinalIgnoreCase) ||
            p.Equals("arm64", StringComparison.OrdinalIgnoreCase)
        ) ?? "未知";

        // RID 目录：win-x64 / win-x86 / win-arm64 等
        string ridDir = parts.FirstOrDefault(p =>
            p.StartsWith("win-", StringComparison.OrdinalIgnoreCase)
        ) ?? "无";

        return (configuration, tfm, arch, ridDir);
    }



}
