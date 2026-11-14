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
using Microsoft.Extensions.Logging;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Windows.AppLifecycle;
using Microsoft.Windows.AppNotifications.Builder;

using Windows.Storage;

using WinRT.Interop;

using WinUI3Localizer;

namespace ClipBridgeShell_CS;

// To learn more about WinUI 3, see https://docs.microsoft.com/windows/apps/winui/winui3/.
public partial class App : Application
{
    // 只保留 DI 容器本身：应用从未使用 IHost 的生命周期（Start/StopAsync）、
    // IHostedService 或 IHostEnvironment，因此不需要通用主机的启动/停止语义。
    private readonly IServiceProvider _services;

    public IServiceProvider Services => _services;

    public static T GetService<T>()
        where T : class
    {
        if ((App.Current as App)!._services.GetService(typeof(T)) is not T service)
        {
            throw new ArgumentException($"{typeof(T)} needs to be registered in ConfigureServices within App.xaml.cs.");
        }

        return service;
    }

    //public static Window MainWindow { get; } = new MainWindow();
    public static Window? MainWindow
    {
        get; set;
    }

    public static UIElement? AppTitlebar
    {
        get; set;
    }
    public static NavigationViewItem? SettingsNavItem
    {
        get; set;
    }

    /// <summary>
    /// 测量模式：<c>CB_STARTUP_NO_FOREGROUND=1</c> 时窗口"可见但不抢焦点"，
    /// 并抑制启动期的登录提醒弹窗。用于连续测量启动耗时，避免把用户正在用的窗口反复抢掉。
    /// 未设置时行为与原先完全一致。
    /// </summary>
    internal static readonly bool SuppressForeground = MeasurementWindow.SuppressForeground;

    /// <summary>
    /// 后台本地化初始化任务。默认模式下 <see cref="OnLaunched"/> 只启动它、不等待；
    /// 启动期需要本地化的消费者（延迟 Shell 构建、托盘菜单、窗口标题、登录提醒）各自 await。
    /// 任务内部已捕获全部异常，因此它永远以成功状态完成：await 它既不会抛异常，
    /// 也不会把延迟 Shell 构建卡死（这一点是“能否安全延迟”的前提）。
    /// </summary>
    internal static Task? LocalizerReady
    {
        get;
        private set;
    }

    /// <summary>
    /// 启动期“主题设置读取”任务（由 ActivationService 在创建窗口之前启动）。
    /// 本地化初始化里的设置读取会先等它完成：LocalSettingsService 的首次初始化
    /// 会整体替换内部字典，两条并发首次读取会互相覆盖内存中的设置。
    /// 两者都发生在首帧之外，串行等待不影响窗口可见耗时。
    /// </summary>
    internal static Task? StartupThemeReadTask
    {
        get; set;
    }

    /// <summary>
    /// 等待本地化器就绪；若本地化器根本没启动（例如单实例重定向后提前 Exit），立即返回。
    /// 用方法而不是直接 await 字段，避免 null 任务带来的空引用。
    /// </summary>
    internal static Task WaitLocalizerReadyAsync() => LocalizerReady ?? Task.CompletedTask;

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    /// <summary>SW_SHOWNOACTIVATE：显示窗口但不激活它。</summary>
    internal const int SW_SHOWNOACTIVATE = 4;

    public App()
    {
        StartupTrace.Mark("app-ctor-enter");
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
        StartupTrace.Mark("app-initcomponent-done");

        _services = BuildServices();

        // 注意：通知注册与启动日志自检已移到 OnLaunched 的窗口显示之后。
        // 实测（Release 冷启动 6 次）：AppNotification.Initialize ≈103ms，
        // 一次 ILogger 落盘 ≈30ms，合计约 133ms 且都不影响首帧。

        UnhandledException += App_UnhandledException;
        StartupTrace.Mark("app-ctor-exit");
    }

    /// <summary>
    /// 构建 DI 容器：<see cref="ServiceCollection"/> + BuildServiceProvider()。
    /// 通用主机（Host.CreateDefaultBuilder）除 DI 之外还会加载配置源、环境变量、命令行、
    /// 主机生命周期与环境探测，本应用一样都没用到，却要在冷启动里付出约 118ms（LEVER 3）。
    /// 注册项与顺序保持与改动前逐条一致，只把“配置”换成等值的代码内构造。
    /// </summary>
    private static IServiceProvider BuildServices()
    {
        StartupTrace.Mark("ab-mode-host-lite");
        var services = new ServiceCollection();

        // 与 Host.CreateDefaultBuilder() 内部配置的日志提供者逐项一致：
        // Console / Debug / EventSource，Windows 上再加 EventLog。不增不减，避免丢掉诊断输出。
        // （原代码里的 ConfigureLogging(logging => { }) 是空委托，提供者全部来自默认主机，
        //   所以这里必须显式还原，否则 CoreLoggerProvider 之外的日志会静默消失。）
        services.AddLogging(logging =>
        {
            logging.AddConsole();
            logging.AddDebug();
            logging.AddEventSourceLogger();
            if (OperatingSystem.IsWindows())
            {
                logging.AddEventLog();
            }
        });

        RegisterServices(services);

        var provider = services.BuildServiceProvider();
        StartupTrace.Mark("host-built-lite");
        StartupTrace.Mark("app-host-built");
        return provider;
    }

    /// <summary>
    /// 所有服务注册（与改动前逐条、同序）。
    /// appsettings.json 只提供 LocalSettingsOptions，且其值与下面代码内构造的完全相同。
    /// </summary>
    private static void RegisterServices(IServiceCollection services)
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

        // System integration services
        services.AddSingleton<IStartupService, StartupService>();
        services.AddSingleton<TrayService>();

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
        services.AddSingleton<MainViewModel>(sp =>
        {
            var historyStore = sp.GetRequiredService<Stores.HistoryStore>();
            var pump = sp.GetRequiredService<EventPumpService>();
            var coreHost = sp.GetRequiredService<ICoreHostService>();
            var peerStore = sp.GetRequiredService<Stores.PeerStore>();
            var transferStore = sp.GetRequiredService<Stores.TransferStore>();
            var navigationService = sp.GetRequiredService<INavigationService>();
            var localSettings = sp.GetRequiredService<ILocalSettingsService>();
            var clipboardApply = sp.GetRequiredService<ClipboardApplyService>();
            var clipboardService = sp.GetRequiredService<IClipboardService>();
            return new MainViewModel(historyStore, pump, coreHost, peerStore, transferStore, navigationService, localSettings, clipboardApply, clipboardService);
        });
        services.AddTransient<MainPage>();
        services.AddTransient<ShellPage>();
        services.AddTransient<ShellViewModel>();

        // Configuration
        // 数值与 appsettings.json 完全一致（同时它们也是 LocalSettingsService 的内置默认值），
        // 因此不再为了这两个字段去读 JSON + 建配置树。
        services.Configure<LocalSettingsOptions>(options =>
        {
            options.ApplicationDataFolder = "ClipBridgeShell_CS/ApplicationData";
            options.LocalSettingsFile = "LocalSettings.json";
        });
        services.AddSingleton<ClipboardWatcher>(sp =>
        {
            var clipboardService = sp.GetRequiredService<IClipboardService>();
            var coreHostService = sp.GetRequiredService<ICoreHostService>();
            var localSettings = sp.GetRequiredService<ILocalSettingsService>();
            var loggerFactory = sp.GetService<Microsoft.Extensions.Logging.ILoggerFactory>();
            return new ClipboardWatcher(clipboardService, coreHostService, localSettings, loggerFactory);
        });

        // History Service（注入 MainViewModel 以同步锁定与复制选中）
        services.AddTransient<HistoryViewModel>(sp =>
        {
            var coreService = sp.GetRequiredService<ICoreHostService>();
            var clipboardApply = sp.GetRequiredService<ClipboardApplyService>();
            var historyStore = sp.GetRequiredService<Stores.HistoryStore>();
            var clipboardService = sp.GetRequiredService<IClipboardService>();
            var mainViewModel = sp.GetRequiredService<MainViewModel>();
            return new HistoryViewModel(coreService, clipboardApply, historyStore, clipboardService, mainViewModel);
        });
        services.AddTransient<HistoryPage>();
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
        StartupTrace.Mark("onlaunched-enter");

        //App.GetService<IAppNotificationService>().Show(string.Format("AppNotificationSamplePayload".GetLocalized(), AppContext.BaseDirectory));

        // ⓪ 单实例保护：非主实例把激活重定向给主实例后退出，避免两个进程共用同一份 Core 数据
        if (!await EnsureSingleInstanceAsync())
        {
            Exit();
            return;
        }
        StartupTrace.Mark("single-instance-done");

        // ① 初始化 Localizer（打包应用版）
        // 默认为“后台构建、不等待”（LEVER 1）：本地化器构建（实测约 56ms）不再挡住窗口创建与激活。
        // 安全前提：
        //   - 首帧只画一个空占位层（不解析任何本地化资源）；
        //   - ShellPage 已排到首帧之后的低优先级回调里，构建前会显式 await App.LocalizerReady；
        //   - 窗口标题先用字面量兜底，就绪后再换成正式文本。
        StartupTrace.Mark("ab-mode-localizer-deferred");

        LocalizerReady = RunLocalizerSafelyAsync();

        // ② 再走 Template Studio 原有流程（里面会调用 ActivationService）
        await App.GetService<IActivationService>().ActivateAsync(args);
        StartupTrace.Mark("activate-async-done");

        // 运行时帧时序诊断（默认完全关闭）：CB_FRAME_TRACE=1 时，窗口此刻已经可见
        // （CB_STARTUP_NO_FOREGROUND=1 走的是 SW_SHOWNOACTIVATE，可见但不抢焦点），
        // 从这里开始采集帧间隔与 UI 线程停顿；场景跑完后由 FrameTrace 自行落盘并退出进程。
        // 未设置环境变量时这里只是一次静态布尔判断，不订阅事件、不起线程、不建文件。
        if (FrameTrace.Enabled && MainWindow is { } traceWindow)
        {
            FrameTrace.StartAfterWindowVisible(
                traceWindow.DispatcherQueue,
                WindowNative.GetWindowHandle(traceWindow));
        }

        // 本地化器就绪后，在 UI 线程把窗口标题从字面量兜底值换成正式本地化文本。
        // 最终标题与改动前完全一致；只是可能在首帧之后的一两个帧内先显示兜底值。
        _ = ApplyLocalizedTitleWhenReadyAsync();

        // 与首帧无关的初始化必须真正排到首帧之后：
        // 直接 inline 调用仍然占住 UI 线程，只是把阻塞从 Activate() 之前挪到之后，
        // 首帧要等 UI 线程回到消息泵才画得出来（实测：inline 反而更慢）。
        App.MainWindow?.DispatcherQueue.TryEnqueue(
            Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () =>
            {
                StartupTrace.Mark("deferred-init-scheduled");

                // 这两件事都不依赖 UI 线程（通知注册只是 AppNotificationManager.Register()，
                // 日志自检是文件 IO），却实测占住 UI 线程约 99ms，把图表首绘与背景图一起往后推。
                // 交替 A/B（6 对，隐藏桌面）：移到线程池后"图表/背景图就绪"配对中位数 -70.1ms
                // （4/6 同向），"可用时间"变化在噪声内。因此固定放线程池。
                _ = Task.Run(() =>
                {
                    StartupTrace.Mark("deferred-init-start");
                    InitializeNotifications();
                    RunStartupLogSmokeTest();
                    StartupTrace.Mark("deferred-init-done");
                });
            });


#if DEBUG
        PrintCoreDllInfo();
        // 检测渲染模式（仅在 Debug 模式下）
        CheckHardwareAcceleration();
#endif

        // 核心初始化已统一放在 ActivationService（窗口可见后异步执行），这里只处理未登录提示
        var accountService = App.GetService<IAccountService>();
        var hasAccount = await accountService.HasAccountAsync();
        if (!hasAccount && !SuppressForeground)
        {
            // 如果没有账号，弹出登录提醒弹窗（延迟一会确保 UI 已加载）
            _ = Task.Run(async () =>
            {
                await Task.Delay(1000);
                App.MainWindow?.DispatcherQueue.TryEnqueue(async () =>
                {
                    if (App.MainWindow == null) return;

                    // 登录提醒文案来自本地化器：等它就绪再取（延迟 1s 后通常早已完成，等待代价为 0）。
                    // 不等待的话，延迟本地化模式下这里可能拿到空字符串 / 未就绪的 Localizer。
                    await WaitLocalizerReadyAsync();

                    var loc = WinUI3Localizer.Localizer.Get();
                    var currentLang = loc.GetCurrentLanguage();
                    var isChinese = currentLang.StartsWith("zh", StringComparison.OrdinalIgnoreCase);

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

        // 初始化托盘图标
        // 托盘菜单文本来自 Localizer.Get()，必须等本地化器就绪；此时窗口已经可见，等待不影响首帧。
        await InitializeTrayAsync();

        // 便携模式回退提示：必须在窗口/托盘就绪之后提示（解析器结果已缓存，这里不重复探测）。
        NotifyPortableDataRootFallback();
    }

    /// <summary>
    /// 便携模式回退提示：exe 旁存在 <c>ClipBridge.portable</c>，但 <c>&lt;exeDir&gt;\data</c> 不可写时，
    /// <see cref="CoreConfigBuilder.ResolveDataRootInfo"/> 已整体回退到 <c>%LOCALAPPDATA%\ClipBridge</c>。
    /// 这里把原因明确告诉用户（对话框；失败则退化为托盘气泡），避免“以为在用便携版、
    /// 数据却写到了别处”的静默半分裂。
    /// </summary>
    private static void NotifyPortableDataRootFallback()
    {
        var resolution = CoreConfigBuilder.ResolveDataRootInfo();
        if (string.IsNullOrEmpty(resolution.PortableFallbackReason))
        {
            return;
        }

        var window = MainWindow;
        if (window == null)
        {
            return;
        }

        window.DispatcherQueue.TryEnqueue(async () =>
        {
            // 让“未登录提醒”等已有启动弹窗先走；避免同时打开两个 ContentDialog。
            await Task.Delay(1500);
            try
            {
                await WaitLocalizerReadyAsync();
                var isChinese = Localizer.Get().GetCurrentLanguage()
                    .StartsWith("zh", StringComparison.OrdinalIgnoreCase);

                var title = isChinese ? "便携模式已回退" : "Portable mode fallback";
                var content = string.Format(
                    isChinese
                        ? "检测到 ClipBridge.portable，但程序目录下的 data 文件夹不可写，数据已改存到：\n{0}\n\n原因：{1}\n\n如需真正的便携模式，请把整个文件夹解压/复制到可写位置后重新启动。"
                        : "ClipBridge.portable was found, but the data folder next to the executable is not writable. Data is stored in:\n{0}\n\nReason: {1}\n\nFor real portable mode, extract/copy the whole folder to a writable location and restart.",
                    resolution.Root,
                    resolution.PortableFallbackReason);

                await window.ShowMessageDialogAsync(content, title);
            }
            catch (Exception ex)
            {
                // 对话框失败（例如另一个 ContentDialog 还开着）时退化为托盘气泡；
                // 提示本身绝不能影响启动。
                System.Diagnostics.Debug.WriteLine($"[App] Portable fallback dialog failed: {ex.Message}");
                try
                {
                    var isChinese = Localizer.Get().GetCurrentLanguage()
                        .StartsWith("zh", StringComparison.OrdinalIgnoreCase);
                    App.GetService<TrayService>().ShowBalloonTip(
                        isChinese ? "便携模式已回退" : "Portable mode fallback",
                        isChinese
                            ? $"data 文件夹不可写，数据已改存到 {resolution.Root}"
                            : $"The data folder is not writable; data is stored in {resolution.Root}");
                }
                catch
                {
                }
            }
        });
    }

    /// <summary>
    /// 单实例保护：主实例返回 true；非主实例把激活重定向给主实例并返回 false（调用方应退出进程）。
    /// </summary>
    private static async Task<bool> EnsureSingleInstanceAsync()
    {
        try
        {
            var current = AppInstance.GetCurrent();
            var primary = AppInstance.FindOrRegisterForKey("ClipBridgeShellMain");

            if (primary.IsCurrent)
            {
                // 主实例：后续激活（例如再次双击图标）会被重定向到这里，把窗口带回前台
                primary.Activated += (_, _) => ActivateMainWindow();
                return true;
            }

            await primary.RedirectActivationToAsync(current.GetActivatedEventArgs());
            return false;
        }
        catch (Exception ex)
        {
            // 单实例检查失败时不要阻止应用启动
            System.Diagnostics.Debug.WriteLine($"[App] Single instance check failed: {ex.Message}");
            return true;
        }
    }

    private static void ActivateMainWindow()
    {
        StartupTrace.Mark("warm-activate-request");
        var window = MainWindow;
        if (window == null) return;

        window.DispatcherQueue.TryEnqueue(() =>
        {
            StartupTrace.Mark("warm-show-begin");
            if (SuppressForeground)
            {
                // 测量模式：可见但不抢焦点；CB_STARTUP_OFFSCREEN=1 时移到屏幕外，屏幕上完全看不到。
                var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
                StartupTrace.Mark(MeasurementWindow.ShowWithoutFocus(hwnd));
            }
            else
            {
                window.Show();
            }
            if (window.AppWindow.Presenter is OverlappedPresenter presenter)
            {
                presenter.Restore();
            }
            if (!SuppressForeground)
            {
                window.Activate();
            }

            // 窗口已重新可见：取消可能还挂在 3 秒延迟上的工作集裁剪（见 Helpers/WorkingSetTrimmer）。
            WorkingSetTrimmer.OnWindowVisible();

            StartupTrace.Mark("warm-activate-done");
        });
    }

    /// <summary>
    /// 注册应用通知。原先在 App 构造里执行，实测占 Release 冷启动约 103ms，
    /// 而通知不可能在首帧之前到达，故移动到窗口显示之后。
    /// </summary>
    private void InitializeNotifications()
    {
        try
        {
            App.GetService<IAppNotificationService>().Initialize();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Notification init failed: {ex.Message}");
        }
    }

    /// <summary>
    /// 启动日志自检。原先在 App 构造里执行，会经过 CoreLogDispatcher → StashLogManager
    /// 同步落盘（实测约 30ms）；移动到窗口显示之后，避免占用启动关键路径。
    /// </summary>
    private void RunStartupLogSmokeTest()
    {
        try
        {
            var loggerFactory = _services.GetRequiredService<Microsoft.Extensions.Logging.ILoggerFactory>();
            loggerFactory.CreateLogger("App").LogInformation("App started - testing log system");
        }
        catch
        {
            // 日志系统初始化失败，但不影响应用启动
        }
    }

    private async Task InitializeTrayAsync()
    {
        // TrayService.RebuildContextMenu() 在 Initialize() 内部就会调用 Localizer.Get()。
        // 延迟本地化模式下必须显式等待，否则托盘菜单会拿到未就绪的本地化 key。
        // 本方法在窗口激活之后才执行，等待不占用“到窗口可见”的时间。
        await WaitLocalizerReadyAsync();

        try
        {
            var tray = App.GetService<TrayService>();
            tray.Initialize();

            // 托盘-only 生命周期（主窗口为 null）时应用本来就不可见：按“已隐藏”排一次裁剪。
            // WorkingSetTrimmer 不依赖窗口对象，因此这里不会因为 null 而失败。
            if (App.MainWindow == null)
            {
                WorkingSetTrimmer.OnWindowHidden();
            }

            tray.ShowWindowRequested += (_, _) =>
            {
                if (App.MainWindow != null)
                {
                    App.MainWindow.Show();
                    // 从最小化状态恢复到正常窗口
                    if (App.MainWindow.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter p)
                    {
                        p.Restore();
                    }
                    App.MainWindow.Activate();

                    // 从托盘显示后窗口重新可见：取消尚未触发的延迟裁剪。
                    WorkingSetTrimmer.OnWindowVisible();
                }
            };

            tray.ExitRequested += async (_, _) =>
            {
                await Helpers.AppLifecycleHelper.GracefulShutdownAsync();
            };
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Tray init failed: {ex.Message}");
        }
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

    /// <summary>
    /// 本地化初始化的安全包装：任何异常都只写 Debug 输出，绝不让任务进入 faulted 状态。
    ///
    /// 为什么必须这样：延迟 Shell 构建、托盘初始化、标题补写都会 await 这个任务。
    /// 任务一旦 faulted，每次 await 都会抛出异常 —— 延迟 Shell 构建会直接失败，
    /// 窗口将永久停在空白占位层。这样包一层之后，await 它永远安全。
    /// </summary>
    private static async Task RunLocalizerSafelyAsync()
    {
        try
        {
            await ((App)Current).InitializeLocalizer();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Localizer initialization failed: {ex}");
        }
        finally
        {
            StartupTrace.Mark("localizer-done");
        }
    }

    /// <summary>
    /// 本地化器就绪后，在 UI 线程把窗口标题换成正式本地化文本。
    ///
    /// MainWindow 构造时本地化器可能还没建好（延迟本地化），所以构造期只设置固定字面量兜底；
    /// 最终标题仍是 Localizer.Get().GetLocalizedString("AppDisplayName")，与改动前逐字一致。
    /// </summary>
    private static async Task ApplyLocalizedTitleWhenReadyAsync()
    {
        try
        {
            await WaitLocalizerReadyAsync();

            var window = MainWindow;
            if (window == null)
            {
                return;
            }

            // 必须回到 UI 线程才能改 Window.Title。
            // App.MainWindow 的静态类型是 Window，先转成 MainWindow 再调用标题方法。
            window.DispatcherQueue.TryEnqueue(() => (window as MainWindow)?.ApplyLocalizedTitle());
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Apply localized title failed: {ex}");
        }
    }

    private async Task InitializeLocalizer()
    {
        StartupTrace.Mark("localizer-enter");
        // 兼容非打包（unpackaged）模式：ApplicationData.Current 在非打包模式下会崩溃
        string localFolderPath;
        try
        {
            localFolderPath = ApplicationData.Current.LocalFolder.Path;
        }
        catch (InvalidOperationException)
        {
            // 非打包模式：本地化缓存目录跟随统一解析的数据根
            // （默认 %LOCALAPPDATA%\ClipBridge，便携模式下是 <exeDir>\data）。
            localFolderPath = CoreConfigBuilder.ResolveDataRoot("ClipBridge");
            if (!Directory.Exists(localFolderPath)) Directory.CreateDirectory(localFolderPath);
        }
        StartupTrace.Mark("localizer-folder-done");

        // 1) 准备 Strings 文件夹
        var stringsPath = Path.Combine(localFolderPath, "Strings");
        if (!Directory.Exists(stringsPath)) Directory.CreateDirectory(stringsPath);

        const string resw = "Resources.resw";
        await CreateStringResourceFileIfNotExists(stringsPath, "en-US", resw);
        await CreateStringResourceFileIfNotExists(stringsPath, "zh-CN", resw);
        StartupTrace.Mark("localizer-resw-done");

        // 2) 读取已保存的语言
        // 启动期串行化：LocalSettingsService 的首次 InitializeAsync 会整体替换内部设置字典，
        // 不是线程安全的。LEVER 2 让“主题设置读取”与本地化并发启动，如果两条首次读取
        // 同时进行，可能互相覆盖内存中的设置（例如语言偏好）。
        // 主题读取一定先于本处启动（同一 UI 线程，本方法前面还有文件 IO），所以这里等它完成即可；
        // 本地化本身已在首帧之外，这点等待不影响窗口可见耗时。
        if (StartupThemeReadTask is { } themeRead)
        {
            try
            {
                await themeRead;
            }
            catch
            {
                // 主题读取失败由 ActivationService 自己处理，这里只是串行化，不能因此中断本地化。
            }
        }

        var settings = App.GetService<ILocalSettingsService>();
        var saved = await settings.ReadSettingAsync<string>("PreferredLanguage");
        var defaultLang = NormalizeLanguageTag(saved ?? CultureInfo.CurrentUICulture.Name);

        if (string.IsNullOrEmpty(saved) || saved != defaultLang)
        {
            await settings.SaveSettingAsync("PreferredLanguage", defaultLang);
            if (string.IsNullOrEmpty(saved))
                await settings.SaveSettingAsync("IsFirstRun", false);
        }
        StartupTrace.Mark("localizer-settings-done");

        // 3) 用默认语言构建 Localizer，然后显式 SetLanguage 覆盖内部持久化
        var localizer = await new LocalizerBuilder()
            .AddStringResourcesFolderForLanguageDictionaries(stringsPath)
            .SetOptions(o => o.DefaultLanguage = defaultLang)
            .Build();
        StartupTrace.Mark("localizer-build-done");
        if (localizer.GetCurrentLanguage() != defaultLang)
            _ = localizer.SetLanguage(defaultLang);
    }

    private static string NormalizeLanguageTag(string? t)
    {
        if (string.IsNullOrWhiteSpace(t)) return "en-US";
        t = t.Trim();
        if (t.StartsWith("en", StringComparison.OrdinalIgnoreCase)) return "en-US";
        if (t.StartsWith("zh", StringComparison.OrdinalIgnoreCase)) return "zh-CN";
        return "en-US";
    }

    private static async Task CreateStringResourceFileIfNotExists(string stringsPath, string language, string resourceFileName)
    {
        var languagePath = Path.Combine(stringsPath, language);
        if (!Directory.Exists(languagePath)) Directory.CreateDirectory(languagePath);

        var targetFilePath = Path.Combine(languagePath, resourceFileName);

        // 开发阶段或版本更新时，我们希望覆盖旧的本地化文件以确保新键值生效；
        // 版本未变化且目标文件已存在时跳过复制，避免每次启动都重写 LocalFolder。
        var version = GetAppVersion();
        var stampFilePath = Path.Combine(stringsPath, $".{resourceFileName}.{language}.version");
        try
        {
            if (File.Exists(targetFilePath) && File.Exists(stampFilePath)
                && string.Equals(await File.ReadAllTextAsync(stampFilePath), version, StringComparison.Ordinal))
            {
                return;
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Failed to read resource version stamp {language}: {ex.Message}");
        }

        var resourceRelativePath = Path.Combine("Strings", language, resourceFileName);
        try
        {
            var file = await LoadStringResourcesFileFromAppResource(resourceRelativePath);
            await file.CopyAsync(await StorageFolder.GetFolderFromPathAsync(languagePath), resourceFileName, NameCollisionOption.ReplaceExisting);

            // 记录本次复制的版本，供下次启动比对
            await File.WriteAllTextAsync(stampFilePath, version);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] Failed to copy resource {language}: {ex.Message}");
        }
    }

    /// <summary>
    /// 获取用于本地化资源版本戳的应用版本号（打包/非打包均可工作）。
    /// </summary>
    private static string GetAppVersion()
    {
        try
        {
            var package = Windows.ApplicationModel.Package.Current;
            if (package != null)
            {
                var v = package.Id.Version;
                return $"{v.Major}.{v.Minor}.{v.Build}.{v.Revision}";
            }
        }
        catch
        {
            // 非打包模式下 Package.Current 会抛异常，退回到程序集版本
        }

        try
        {
            return typeof(App).Assembly.GetName().Version?.ToString() ?? "0.0.0.0";
        }
        catch
        {
            return "0.0.0.0";
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
            var dllName = "core_ffi_windows.dll";

            // 1. 找到 WinUI 实际运行时使用的 DLL
            var appDllPath = FindCoreDll(dllName);

            // 2. Rust 官方输出目录 DLL
            var rustDllPath =
                @"C:\Project\ClipBridge\target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll";

            string header;
            string body;

            var appDllExists = !string.IsNullOrEmpty(appDllPath) && File.Exists(appDllPath);
            var rustDllExists = File.Exists(rustDllPath);

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

            // 开发诊断：写 Debug 输出而不是弹系统通知。
            // 原因：① 每次调试启动都弹通知是纯噪声；② 通知注册已移到首帧之后，
            // 这里若提前 Show 会因尚未 Initialize 而失效。
            System.Diagnostics.Debug.WriteLine($"[CoreDll] {header}\n{body}");
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[CoreDll] 读取 DLL 信息失败: {ex.Message}");
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
        var current = AppContext.BaseDirectory?
            .TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);

        if (string.IsNullOrEmpty(current))
            return null;

        const int maxLevels = 5;
        var level = 0;

        while (current != null && level <= maxLevels)
        {
            // 1. 当前目录
            var candidate = Path.Combine(current, dllName);
            if (File.Exists(candidate))
                return candidate;

            // 2. 当前目录下的 win-x64
            var winX64 = Path.Combine(current, "win-x64", dllName);
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
        var dir = Path.GetDirectoryName(dllPath) ?? string.Empty;

        var parts = dir
            .Split(new[] { Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar },
                   StringSplitOptions.RemoveEmptyEntries);

        // 配置：Debug / Release
        var configuration = parts.FirstOrDefault(p =>
            p.Equals("Debug", StringComparison.OrdinalIgnoreCase) ||
            p.Equals("Release", StringComparison.OrdinalIgnoreCase)
        ) ?? "未知";

        // TFM：例如 net9.0-windows10.0.26100.0
        var tfm = parts.FirstOrDefault(p =>
            p.StartsWith("net", StringComparison.OrdinalIgnoreCase)
        ) ?? "未知";

        // 架构目录：x64 / x86 / arm64
        var arch = parts.FirstOrDefault(p =>
            p.Equals("x64", StringComparison.OrdinalIgnoreCase) ||
            p.Equals("x86", StringComparison.OrdinalIgnoreCase) ||
            p.Equals("arm64", StringComparison.OrdinalIgnoreCase)
        ) ?? "未知";

        // RID 目录：win-x64 / win-x86 / win-arm64 等
        var ridDir = parts.FirstOrDefault(p =>
            p.StartsWith("win-", StringComparison.OrdinalIgnoreCase)
        ) ?? "无";

        return (configuration, tfm, arch, ridDir);
    }



}
