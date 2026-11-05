using System.Runtime.InteropServices;

using ClipBridgeShell_CS.Activation;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Views;

using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using static ClipBridgeShell_CS.App;

namespace ClipBridgeShell_CS.Services;

public class ActivationService : IActivationService
{
    private readonly ActivationHandler<LaunchActivatedEventArgs> _defaultHandler;
    private readonly IEnumerable<IActivationHandler> _activationHandlers;
    private readonly IThemeSelectorService _themeSelectorService;
    private readonly ICoreHostService _coreHostService;
    private UIElement? _shell = null;

    // 启动耗时 A/B 对照开关（默认关闭，只用于同一份二进制内交替实测）：
    //   CB_SYNC_SHELL=1 → 旧路径：先构建 ShellPage 再 Activate（对照臂）；
    //   未设置           → 新路径：先显示占位层，首帧之后再低优先级构建 Shell。
    // 跨会话测量噪声很大，只有同一二进制内交替运行才能把“这次改造本身”的收益量出来。
    private static readonly bool SyncShellForAb =
        string.Equals(Environment.GetEnvironmentVariable("CB_SYNC_SHELL"), "1", StringComparison.Ordinal);

    // 测量模式：CB_STARTUP_NO_FOREGROUND=1 时窗口“可见但不抢焦点”。
    // 连续测 5~7 次启动耗时时，Activate() 会反复把用户正在用的窗口抢到前台，必须避免。
    private static readonly bool SuppressForeground =
        string.Equals(Environment.GetEnvironmentVariable("CB_STARTUP_NO_FOREGROUND"), "1", StringComparison.Ordinal);

    /// <summary>SW_SHOWNOACTIVATE：显示窗口但不激活它。</summary>
    private const int SW_SHOWNOACTIVATE = 4;

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    public ActivationService(ActivationHandler<LaunchActivatedEventArgs> defaultHandler, IEnumerable<IActivationHandler> activationHandlers, IThemeSelectorService themeSelectorService, ICoreHostService coreHostService)
    {
        _defaultHandler = defaultHandler;
        _activationHandlers = activationHandlers;
        _themeSelectorService = themeSelectorService;
        _coreHostService = coreHostService;
    }

    public async Task ActivateAsync(object activationArgs)
    {
        // Execute tasks before activation.
        // 只初始化主题（读取设置），不在此处启动 Core：
        // Core 初始化会打开 SQLite、生成 TLS 证书、启动 mDNS，不能阻塞窗口创建与激活路径。
        await InitializeThemeAsync();
        StartupTrace.Mark("theme-init-done");

        if (App.MainWindow == null)
        {
            App.MainWindow = new MainWindow();
        }
        StartupTrace.Mark("mainwindow-created");

        var mainWindow = App.MainWindow!;

        // 关键改动：先让窗口可见，Shell 排到首帧之后构建。
        // 旧顺序里 ShellPage 的 XAML（实测约 164ms）发生在 Activate() 之前，直接抬高“到窗口可见”。
        var buildShellDeferred = false;

        if (SyncShellForAb)
        {
            // A/B 对照臂：与改动前完全一致的顺序（先建 Shell 再激活），供同一二进制交替实测。
            StartupTrace.Mark("ab-mode-sync");
            if (mainWindow.Content == null)
            {
                _shell = App.GetService<ShellPage>();
                mainWindow.Content = _shell ?? new Frame();
            }
            StartupTrace.Mark("shellpage-created");

            await HandleActivationAsync(activationArgs);
            StartupTrace.Mark("activation-handlers-done");
        }
        else
        {
            StartupTrace.Mark("ab-mode-deferred");
            if (mainWindow.Content == null)
            {
                // 提前进入自定义标题栏模式：SetTitleBar(AppTitleBar) 只能等 Shell 建好后才能调用，
                // 若扩展模式也等到那时才打开，用户会看到标准标题栏突然变成自定义标题栏。
                mainWindow.ExtendsContentIntoTitleBar = true;

                // 首帧占位层：不解析任何 XAML，成本约等于 0，只负责让窗口立即可见。
                mainWindow.Content = CreatePlaceholderContent();
                StartupTrace.Mark("placeholder-set");
                buildShellDeferred = true;
            }
            else
            {
                // 非首次激活（内容已存在）：维持原有行为，直接走激活处理器。
                await HandleActivationAsync(activationArgs);
                StartupTrace.Mark("activation-handlers-done");
            }
        }

        // 自启到托盘：若由注册表/计划任务传入 --startup-to-tray，不激活主窗口，直接隐藏到托盘
        if (StartupService.IsStartupToTrayRequested())
        {
            mainWindow.Hide();
        }
        else if (SuppressForeground)
        {
            // 测量模式：可见但不抢焦点。不能调用 Activate()（它内部会 SetForegroundWindow）。
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(mainWindow);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            StartupTrace.Mark("window-shown-noactivate");
        }
        else
        {
            mainWindow.Activate();
        }
        // 阶段名保持不变，便于与历史数据对比；测量模式会额外打一条 window-shown-noactivate。
        StartupTrace.Mark("window-activated");

        if (buildShellDeferred)
        {
            // 窗口已经可见，再把 Shell（及其 XAML）排到低优先级队列里构建。
            ScheduleShellBuild(activationArgs);
        }

        // Execute tasks after activation.
        await StartupAsync();
        StartupTrace.Mark("startup-await-done");

        // 窗口已经可见，再异步启动 Core 初始化（不阻塞激活路径）
        StartCoreInitialization();
        StartupTrace.Mark("core-init-kicked-off");
    }

    private async Task HandleActivationAsync(object activationArgs)
    {
        var activationHandler = _activationHandlers.FirstOrDefault(h => h.CanHandle(activationArgs));

        if (activationHandler != null)
        {
            await activationHandler.HandleAsync(activationArgs);
        }

        if (_defaultHandler.CanHandle(activationArgs))
        {
            await _defaultHandler.HandleAsync(activationArgs);
        }
    }

    private async Task InitializeThemeAsync()
    {
        await _themeSelectorService.InitializeAsync();
    }

    private async Task StartupAsync()
    {
        await _themeSelectorService.SetRequestedThemeAsync();
        await Task.CompletedTask;
    }

    /// <summary>
    /// 首帧之后（Low 优先级）再构建 ShellPage，并替换掉占位层。
    /// 失败时同步重试一次，避免窗口永久停在占位层（空白）。
    /// </summary>
    private void ScheduleShellBuild(object activationArgs)
    {
        var mainWindow = App.MainWindow;
        if (mainWindow == null)
        {
            return;
        }

        // DispatcherQueueHandler 是 async void 语义，异常必须在回调内部兜住，不能逸出到消息循环。
        mainWindow.DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, async () =>
        {
            try
            {
                await BuildShellAndHandleActivationAsync(activationArgs);
                StartupTrace.Mark("shell-built-after-first-frame");
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[ActivationService] 延迟构建 Shell 失败，改为同步重试: {ex}");
                try
                {
                    await BuildShellAndHandleActivationAsync(activationArgs);
                    StartupTrace.Mark("shell-built-after-first-frame-fallback");
                }
                catch (Exception retryEx)
                {
                    System.Diagnostics.Debug.WriteLine($"[ActivationService] 同步重试构建 Shell 仍失败: {retryEx}");
                }
            }
        });
    }

    /// <summary>
    /// 构建 Shell → 替换占位层 → 执行激活处理器。
    /// 顺序不可交换：ViewModel.NavigationService.Frame 在 ShellPage 构造函数里才被赋值，
    /// 必须先有 Shell，DefaultActivationHandler 才能导航到 MainPage。
    /// </summary>
    private async Task BuildShellAndHandleActivationAsync(object activationArgs)
    {
        var mainWindow = App.MainWindow;
        if (mainWindow == null)
        {
            return;
        }

        if (_shell == null)
        {
            _shell = App.GetService<ShellPage>();
        }

        // 重试时 Content 可能已经就是这个 Shell，重复赋值会抛“元素已有父级”。
        if (!ReferenceEquals(mainWindow.Content, _shell))
        {
            mainWindow.Content = _shell;
        }

        // Shell 替换占位层后重新套用已保存的主题（新根元素不会继承占位层的 RequestedTheme），
        // 顺带按新主题刷新标题栏按钮颜色。
        await _themeSelectorService.SetRequestedThemeAsync();

        await HandleActivationAsync(activationArgs);
    }

    /// <summary>
    /// 首帧占位层：一个不画任何内容的 Grid（成本约等于 0）。
    ///
    /// 为什么保持透明而不是涂 ApplicationPageBackgroundThemeBrush：
    /// ShellPage 的根 Grid 本身没有 Background，窗口的真实背景是 MainWindow 的 MicaBackdrop，
    /// 内容层必须保持透明 Mica 才能透出。占位层若涂成不透明主题色，首帧到 Shell 替换之间
    /// 会明显闪一下与 Mica 不一致的颜色；保持透明才能和最终 Shell 完全一致。
    /// </summary>
    private static UIElement CreatePlaceholderContent()
    {
        return new Grid();
    }

    /// <summary>
    /// 在窗口显示后异步初始化 Core。初始化失败或未登录不影响窗口，
    /// 状态通过 CoreHostService 的 CoreState / StateChanged / LastError / Diagnostics 反映。
    /// </summary>
    private void StartCoreInitialization()
    {
        _ = InitializeCoreAsync();
    }

    private async Task InitializeCoreAsync()
    {
        try
        {
            await _coreHostService.InitializeAsync();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[ActivationService] Core initialization failed: {ex}");
        }
    }
}
