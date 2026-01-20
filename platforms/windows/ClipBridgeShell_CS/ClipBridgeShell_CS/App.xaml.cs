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
using Microsoft.Extensions.Logging;
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
        // 确保使用硬件加速（WinUI 3 默认启用，但某些情况下可能回退到软件渲染）
        // 检查并设置环境变量，强制使用硬件加速
        try
        {
            // 移除可能强制软件渲染的环境变量
            var disableHwAccel = Environment.GetEnvironmentVariable("WINUI_DISABLE_HW_ACCEL");
            if (disableHwAccel == "1")
            {
                Environment.SetEnvironmentVariable("WINUI_DISABLE_HW_ACCEL", "0");
            }
        }
        catch
        {
            // 忽略环境变量设置失败
        }
        
        InitializeComponent();

        Host = Microsoft.Extensions.Hosting.Host.
        CreateDefaultBuilder().
        UseContentRoot(AppContext.BaseDirectory).
        ConfigureLogging(logging =>
        {
            // 先配置日志系统，确保 ILoggerFactory 可用
        }).
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
            services.AddSingleton<IAccountService, AccountService>();
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
            services.AddSingleton<ContentFetchAwaiter>();
            services.AddSingleton<EventPumpService>();

            // Logging - 先注册日志系统，避免循环依赖
            services.AddSingleton<Services.Logging.StashLogManager>();
            
            // 先创建 CoreHostService（不使用 ILoggerFactory，避免循环依赖）
            services.AddSingleton<CoreHostService>(sp =>
            {
                var eventPump = sp.GetRequiredService<EventPumpService>();
                var localSettings = sp.GetRequiredService<ILocalSettingsService>();
                var accountService = sp.GetRequiredService<IAccountService>();
                // 暂时不传入 loggerFactory，避免循环依赖
                return new CoreHostService(eventPump, localSettings, null, accountService);
            });
            services.AddSingleton<ICoreHostService>(sp => sp.GetRequiredService<CoreHostService>());
            
            // 现在注册日志提供者（需要 ICoreHostService）
            services.AddSingleton<Services.Logging.CoreLogDispatcher>(sp =>
            {
                var coreHost = sp.GetRequiredService<ICoreHostService>();
                var eventPump = sp.GetRequiredService<EventPumpService>();
                return new Services.Logging.CoreLogDispatcher(coreHost, eventPump);
            });
            services.AddSingleton<Services.Logging.CoreLoggerProvider>(sp =>
            {
                var coreHost = sp.GetRequiredService<ICoreHostService>();
                var dispatcher = sp.GetRequiredService<Services.Logging.CoreLogDispatcher>();
                var stashManager = sp.GetRequiredService<Services.Logging.StashLogManager>();
                return new Services.Logging.CoreLoggerProvider(coreHost, dispatcher, stashManager);
            });
            // 注册日志提供者
            services.AddSingleton<Microsoft.Extensions.Logging.ILoggerProvider>(sp =>
            {
                var provider = sp.GetRequiredService<Services.Logging.CoreLoggerProvider>();
                return provider;
            });

            services.AddSingleton<IClipboardService, ClipboardService>();
            services.AddSingleton<ClipboardApplyService>();

            // Views and ViewModels
            services.AddTransient<SettingsViewModel>();
            services.AddTransient<SettingsPage>();
            services.AddTransient<LogsViewModel>(sp =>
            {
                var coreHost = sp.GetRequiredService<ICoreHostService>();
                var stashManager = sp.GetService<Services.Logging.StashLogManager>();
                var eventPump = sp.GetRequiredService<EventPumpService>();
                var localSettings = sp.GetRequiredService<ILocalSettingsService>();
                var loggerFactory = sp.GetService<Microsoft.Extensions.Logging.ILoggerFactory>();
                return new LogsViewModel(coreHost, stashManager, eventPump, localSettings, loggerFactory);
            });
            services.AddTransient<LogsPage>();
            services.AddTransient<DevicesViewModel>(sp =>
            {
                var coreHost = sp.GetRequiredService<ICoreHostService>();
                var localSettings = sp.GetRequiredService<ILocalSettingsService>();
                var eventPump = sp.GetRequiredService<EventPumpService>();
                return new DevicesViewModel(coreHost, localSettings, eventPump);
            });
            services.AddTransient<DevicesPage>();
            services.AddTransient<MainViewModel>(sp =>
            {
                var historyStore = sp.GetRequiredService<Stores.HistoryStore>();
                var pump = sp.GetRequiredService<EventPumpService>();
                var coreHost = sp.GetRequiredService<ICoreHostService>();
                var peerStore = sp.GetRequiredService<Stores.PeerStore>();
                var transferStore = sp.GetRequiredService<Stores.TransferStore>();
                var navigationService = sp.GetRequiredService<INavigationService>();
                var localSettings = sp.GetRequiredService<ILocalSettingsService>();
                return new MainViewModel(historyStore, pump, coreHost, peerStore, transferStore, navigationService, localSettings);
            });
            services.AddTransient<MainPage>();
            services.AddTransient<ShellPage>();
            services.AddTransient<ShellViewModel>();

            // Configuration
            services.Configure<LocalSettingsOptions>(context.Configuration.GetSection(nameof(LocalSettingsOptions)));
            services.AddSingleton<ClipboardWatcher>(sp =>
            {
                var clipboardService = sp.GetRequiredService<IClipboardService>();
                var coreHostService = sp.GetRequiredService<ICoreHostService>();
                var localSettings = sp.GetRequiredService<ILocalSettingsService>();
                var loggerFactory = sp.GetService<Microsoft.Extensions.Logging.ILoggerFactory>();
                return new ClipboardWatcher(clipboardService, coreHostService, localSettings, loggerFactory);
            });

            // History Service
            services.AddTransient<HistoryViewModel>(sp =>
            {
                var coreService = sp.GetRequiredService<ICoreHostService>();
                var clipboardApply = sp.GetRequiredService<ClipboardApplyService>();
                var historyStore = sp.GetRequiredService<Stores.HistoryStore>();
                return new HistoryViewModel(coreService, clipboardApply, historyStore);
            });
            services.AddTransient<HistoryPage>();
        }).
        Build();

        try
        {
            App.GetService<IAppNotificationService>().Initialize();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Notification init failed: {ex.Message}");
        }

        // 测试日志系统 - 验证日志提供者是否工作
        try
        {
            var loggerFactory = Host.Services.GetRequiredService<Microsoft.Extensions.Logging.ILoggerFactory>();
            var testLogger = loggerFactory.CreateLogger("App");
            testLogger.LogInformation("App started - testing log system");
        }
        catch (Exception ex)
        {
            // 日志系统初始化失败，但不影响应用启动
        }

        UnhandledException += App_UnhandledException;
    }

    private void App_UnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        // 记录未处理的异常
        System.Diagnostics.Debug.WriteLine($"[App] UnhandledException: {e.Exception}");
        System.Diagnostics.Debug.WriteLine($"[App] Exception Message: {e.Exception.Message}");
        System.Diagnostics.Debug.WriteLine($"[App] Exception StackTrace: {e.Exception.StackTrace}");
        if (e.Exception.InnerException != null)
        {
            System.Diagnostics.Debug.WriteLine($"[App] InnerException: {e.Exception.InnerException}");
        }
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
        // 检测渲染模式（仅在 Debug 模式下）
        CheckHardwareAcceleration();
#endif
        
        // 检查账号状态，如果有账号才初始化核心
        var accountService = App.GetService<IAccountService>();
        var hasAccount = await accountService.HasAccountAsync();
        if (hasAccount)
        {
            _ = App.GetService<CoreHostService>().InitializeAsync();
        }
        else
        {
            // 如果没有账号，弹出登录提醒弹窗（延迟一会确保 UI 已加载）
            _ = Task.Run(async () =>
            {
                await Task.Delay(1000);
                App.MainWindow?.DispatcherQueue.TryEnqueue(async () =>
                {
                    if (App.MainWindow == null) return;
                    
                    var loc = WinUI3Localizer.Localizer.Get();
                    var currentLang = loc.GetCurrentLanguage();
                    bool isChinese = currentLang.StartsWith("zh", StringComparison.OrdinalIgnoreCase);
                    
                    // 尝试获取本地化字符串
                    var title = loc.GetLocalizedString("Resources/Startup_LoginRequired_Title");
                    if (string.IsNullOrEmpty(title) || title == "Resources/Startup_LoginRequired_Title") 
                    {
                        title = isChinese ? "需要登录" : "Login Required";
                    }

                    var content = loc.GetLocalizedString("Resources/Startup_LoginRequired_Content");
                    if (string.IsNullOrEmpty(content) || content == "Resources/Startup_LoginRequired_Content")
                    {
                        content = isChinese 
                            ? "需要登录账号才能初始化核心功能。账号和密码可以随意填写，但必须在所有设备上保持一致才能同步剪贴板。" 
                            : "A login account is required to initialize core functions. You can use any username and password, but they must be consistent across all devices to sync your clipboard.";
                    }

                    var primaryButton = loc.GetLocalizedString("Resources/Startup_LoginRequired_PrimaryButton");
                    if (string.IsNullOrEmpty(primaryButton) || primaryButton == "Resources/Startup_LoginRequired_PrimaryButton")
                    {
                        primaryButton = isChinese ? "前往登录" : "Go to Login";
                    }

                    var dialog = new ContentDialog
                    {
                        Title = title,
                        Content = content,
                        PrimaryButtonText = primaryButton,
                        CloseButtonText = "OK",
                        XamlRoot = App.MainWindow.Content.XamlRoot
                    };

                    var result = await dialog.ShowAsync();
                    if (result == ContentDialogResult.Primary)
                    {
                        // 直接弹出登录对话框
                        var loginDialog = new ClipBridgeShell_CS.Views.LoginDialog(App.GetService<IAccountService>());
                        loginDialog.XamlRoot = App.MainWindow.Content.XamlRoot;
                        await loginDialog.ShowAsync();
                    }
                });
            });
        }

        // 启动剪贴板监听（即使没有账号也启动，等待登录后使用）
        App.GetService<ClipboardWatcher>().Initialize();

    }

    /// <summary>
    /// 检测硬件加速状态（仅在 Debug 模式下）
    /// </summary>
    private void CheckHardwareAcceleration()
    {
        try
        {
            // 检查环境变量
            var disableHwAccel = Environment.GetEnvironmentVariable("WINUI_DISABLE_HW_ACCEL");
            var dxgiDisableHwAccel = Environment.GetEnvironmentVariable("DXGI_DISABLE_HW_ACCEL");
            
            var message = "硬件加速检测:\n";
            message += $"WINUI_DISABLE_HW_ACCEL: {disableHwAccel ?? "未设置"}\n";
            message += $"DXGI_DISABLE_HW_ACCEL: {dxgiDisableHwAccel ?? "未设置"}\n";
            message += "\n提示: 如果使用 'Microsoft Basic Render Driver'，请检查:\n";
            message += "1. Windows 设置 → 系统 → 显示 → 图形设置\n";
            message += "2. 添加应用并设置为 '高性能'\n";
            message += "3. 确保显卡驱动是最新版本\n";
            message += "4. 使用 Release 模式运行（Debug 模式可能使用软件渲染）";
            
            System.Diagnostics.Debug.WriteLine(message);
        }
        catch
        {
            // 忽略检测失败
        }
    }

    private async Task InitializeLocalizer()
    {
        // 兼容非打包（unpackaged）模式：ApplicationData.Current 在非打包模式下会崩溃
        string localFolderPath;
        try
        {
            localFolderPath = ApplicationData.Current.LocalFolder.Path;
        }
        catch (InvalidOperationException)
        {
            // 非打包模式下，使用标准的 LocalAppData 路径
            localFolderPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ClipBridge");
            if (!Directory.Exists(localFolderPath)) Directory.CreateDirectory(localFolderPath);
        }

        // 1) 准备 Strings 文件夹
        string stringsPath = Path.Combine(localFolderPath, "Strings");
        if (!Directory.Exists(stringsPath)) Directory.CreateDirectory(stringsPath);
        
        const string resw = "Resources.resw";
        await CreateStringResourceFileIfNotExists(stringsPath, "en-US", resw);
        await CreateStringResourceFileIfNotExists(stringsPath, "zh-CN", resw);

        // 2) 读取已保存的语言
        var settings = App.GetService<ILocalSettingsService>();
        var saved = await settings.ReadSettingAsync<string>("PreferredLanguage");
        string defaultLang = NormalizeLanguageTag(saved ?? CultureInfo.CurrentUICulture.Name);

        if (string.IsNullOrEmpty(saved))
        {
            await settings.SaveSettingAsync("PreferredLanguage", defaultLang);
            await settings.SaveSettingAsync("IsFirstRun", false);
        }

        // 3) 用默认语言构建 Localizer
        _ = await new LocalizerBuilder()
            .AddStringResourcesFolderForLanguageDictionaries(stringsPath)
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

    private static async Task CreateStringResourceFileIfNotExists(string stringsPath, string language, string resourceFileName)
    {
        string languagePath = Path.Combine(stringsPath, language);
        if (!Directory.Exists(languagePath)) Directory.CreateDirectory(languagePath);

        string targetFilePath = Path.Combine(languagePath, resourceFileName);
        // 开发阶段或版本更新时，我们希望覆盖旧的本地化文件以确保新键值生效
        // if (File.Exists(targetFilePath)) return; 

        string resourceRelativePath = Path.Combine("Strings", language, resourceFileName);
        try 
        {
            var file = await LoadStringResourcesFileFromAppResource(resourceRelativePath);
            await file.CopyAsync(await StorageFolder.GetFolderFromPathAsync(languagePath), resourceFileName, NameCollisionOption.ReplaceExisting);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Failed to copy resource {language}: {ex.Message}");
        }
    }

    private static async Task<StorageFile> LoadStringResourcesFileFromAppResource(string filePath)
    {
        try 
        {
            Uri resourcesFileUri = new($"ms-appx:///{filePath}");
            return await StorageFile.GetFileFromApplicationUriAsync(resourcesFileUri);
        }
        catch
        {
            // 非打包模式下的退路：尝试直接从文件系统读取
            var fullPath = Path.Combine(AppContext.BaseDirectory, filePath);
            if (File.Exists(fullPath))
            {
                return await StorageFile.GetFileFromPathAsync(fullPath);
            }
            throw;
        }
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
