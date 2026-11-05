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

    /// <summary>
    /// 主题初始化 A/B 对照开关（默认关闭）：
    /// <c>CB_SYNC_THEME=1</c> → 旧路径：窗口创建前就 await 主题设置读取（对照臂）；
    /// 未设置              → 新路径：主题读取与窗口构造重叠，首帧之后再套用（满足视觉规则时）。
    /// </summary>
    private static readonly bool SyncThemeForAb =
        string.Equals(Environment.GetEnvironmentVariable("CB_SYNC_THEME"), "1", StringComparison.Ordinal);

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
        // ① 主题设置读取（LEVER 2）
        // 默认：只启动、不等待，让它与下面的窗口构造重叠（实测主题初始化约 68ms）。
        // 窗口显示之前再根据“视觉正确性规则”决定：能安全推迟就推迟到首帧之后套用，
        // 不能就回退到同步路径。CB_SYNC_THEME=1 时完全保持改动前的顺序。
        Task? themeReadTask;
        if (SyncThemeForAb)
        {
            StartupTrace.Mark("ab-mode-theme-sync");
            themeReadTask = StartThemeRead();
            await themeReadTask;
            StartupTrace.Mark("theme-init-done");
        }
        else
        {
            StartupTrace.Mark("ab-mode-theme-deferred");
            themeReadTask = StartThemeRead();
        }

        if (App.MainWindow == null)
        {
            App.MainWindow = new MainWindow();
        }
        StartupTrace.Mark("mainwindow-created");

        var mainWindow = App.MainWindow!;

        // 主题是否允许推迟到首帧之后套用（只有“推迟不会造成可见闪烁”时才为 true）。
        var themeApplyDeferred = false;
        if (!SyncThemeForAb)
        {
            // 视觉正确性规则（不可妥协）：
            // 窗口首帧一定用“当前系统主题”绘制。若保存的主题与它不同，把套用推迟到首帧之后
            // 就会先看到系统主题、再跳成保存主题——一次可见的闪烁。
            // 因此必须在窗口可见（Activate）之前拿到保存值，并做一次廉价判定：
            //   ① 保存值 = Default      → 它本来就等于系统主题，之后套用是无操作，安全；
            //   ② 保存值 = 当前系统主题 → 之后套用同样无视觉变化，安全；
            //   ③ 其它                  → 本次启动回退同步路径（主题留在关键路径上），宁可慢也不闪。
            // 判定只是枚举比较；保存值来自上面同一次设置读取。正常情况该读取已与窗口构造
            // 重叠完成，await 一个已完成任务不占关键路径。
            var readWasInFlightWhenWindowReady = !themeReadTask!.IsCompleted;
            await themeReadTask;
            if (readWasInFlightWhenWindowReady)
            {
                // 读取比窗口构造慢：只能在窗口可见前等它，这属于回退，不是默认路径。
                StartupTrace.Mark("theme-read-waited");
            }
            StartupTrace.Mark("theme-init-done");

            themeApplyDeferred = CanApplyThemeAfterFirstFrame();
            StartupTrace.Mark(themeApplyDeferred ? "theme-apply-deferred" : "theme-sync-fallback");
        }

        // 关键改动：先让窗口可见，Shell 排到首帧之后构建。
        // 旧顺序里 ShellPage 的 XAML（实测约 164ms）发生在 Activate() 之前，直接抬高“到窗口可见”。
        var buildShellDeferred = false;

        if (SyncShellForAb)
        {
            // A/B 对照臂：与改动前完全一致的顺序（先建 Shell 再激活），供同一二进制交替实测。
            StartupTrace.Mark("ab-mode-sync");
            if (mainWindow.Content == null)
            {
                // 本地化器可能还在后台构建（LEVER 1）：ShellPage 的 XAML l:Uids 与 OnLoaded
                // 都依赖本地化资源，必须在 GetService 之前等它就绪。
                // App.LocalizerReady 内部已捕获异常，永远不会抛，不会把这里卡死。
                await App.WaitLocalizerReadyAsync();
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
            // 测量模式：可见但不抢焦点；CB_STARTUP_OFFSCREEN=1 时移到屏幕外，屏幕上完全看不到。
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(mainWindow);
            StartupTrace.Mark(MeasurementWindow.ShowWithoutFocus(hwnd));
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
        // 主题若可安全推迟、且 Shell 确实排在首帧之后构建，那么让那次构建去套用主题
        // （BuildShellAndHandleActivationAsync 本来就会重新套用一次）。
        // 其余情况（同步模式 / 视觉规则不允许 / 没有延迟构建）保持改动前的关键路径套用。
        await StartupAsync(applyThemeOnCriticalPath: !(themeApplyDeferred && buildShellDeferred));
        StartupTrace.Mark("startup-await-done");

        // 窗口已经可见，再异步启动 Core 初始化（不阻塞激活路径）
        StartCoreInitialization();
        StartupTrace.Mark("core-init-kicked-off");
    }

    /// <summary>
    /// 启动主题设置读取（LEVER 2）：只启动、不等待。
    /// 同时登记到 <see cref="App.StartupThemeReadTask"/>，让后台本地化里的设置读取排在它之后，
    /// 避免 LocalSettingsService 的并发首次初始化（内部字典会被整体替换）。
    /// 读取完成时刻用 continuation 打点：窗口构造期间 UI 线程被占住，
    /// 若等到 await 之后再打点，theme-read-done 会失真成“窗口构造之后”。
    /// </summary>
    private Task StartThemeRead()
    {
        StartupTrace.Mark("theme-init-enter");
        var task = InitializeThemeAsync();
        App.StartupThemeReadTask = task;
        _ = task.ContinueWith(_ => StartupTrace.Mark("theme-read-done"), TaskScheduler.Default);
        return task;
    }

    /// <summary>
    /// 视觉正确性规则（不可妥协）：只有保存的主题与窗口首帧将要绘制的主题一致时，
    /// 才允许把“套用主题”推迟到首帧之后；否则必须让主题留在关键路径上，宁可慢也不闪。
    /// 该判定只做枚举比较，没有额外 IO。
    /// </summary>
    private bool CanApplyThemeAfterFirstFrame()
    {
        var theme = _themeSelectorService.Theme;

        // Default = 跟随系统，本身就是首帧的主题；之后再套用是无操作。
        if (theme == ElementTheme.Default)
        {
            return true;
        }

        var systemTheme = Application.Current.RequestedTheme == ApplicationTheme.Dark
            ? ElementTheme.Dark
            : ElementTheme.Light;

        return theme == systemTheme;
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

    private async Task StartupAsync(bool applyThemeOnCriticalPath)
    {
        if (applyThemeOnCriticalPath)
        {
            await _themeSelectorService.SetRequestedThemeAsync();
        }

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
            // 本地化器（LEVER 1）可能还在后台构建：ShellPage 的 XAML l:Uids 需要它先就绪。
            // 这里在首帧之后，等待不占关键路径；任务内部吞掉异常，不会让构建失败。
            await App.WaitLocalizerReadyAsync();

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
