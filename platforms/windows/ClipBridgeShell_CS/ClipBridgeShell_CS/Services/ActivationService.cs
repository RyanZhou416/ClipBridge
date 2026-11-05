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

    // 测量模式：CB_STARTUP_NO_FOREGROUND=1 时窗口“可见但不抢焦点”。
    // 连续测 5~7 次启动耗时时，Activate() 会反复把用户正在用的窗口抢到前台，必须避免。
    private static readonly bool SuppressForeground =
        string.Equals(Environment.GetEnvironmentVariable("CB_STARTUP_NO_FOREGROUND"), "1", StringComparison.Ordinal);

    /// <summary>SW_SHOWNOACTIVATE：显示窗口但不激活它。</summary>
    private const int SW_SHOWNOACTIVATE = 4;

    /// <summary>
    /// 实测记录（勿重复试验）：
    /// 1) "Theme == Default 时跳过 Shell 重建后的重套主题" —— 无效，反而 +23.4ms。那 89ms 是
    ///    Content 赋值后的首次布局，不是 RequestedTheme 赋值本身，跳过赋值救不了布局。
    /// 2) "延迟构建改用 Normal 优先级" —— 可见 +11.5ms、可用 +44.7ms，两项都更差。
    /// 结论：这两条都不做，保持"Low 优先级 + 始终重套主题"。
    /// </summary>

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
        // 只启动、不等待，让它与下面的窗口构造重叠（实测主题初始化约 68ms）。
        // 窗口显示之前再根据“视觉正确性规则”决定：能安全推迟就推迟到首帧之后套用，
        // 不能就回退到同步路径。
        StartupTrace.Mark("ab-mode-theme-deferred");
        var themeReadTask = StartThemeRead();

        if (App.MainWindow == null)
        {
            App.MainWindow = new MainWindow();
        }
        StartupTrace.Mark("mainwindow-created");

        var mainWindow = App.MainWindow!;

        // 主题是否允许推迟到首帧之后套用（只有“推迟不会造成可见闪烁”时才为 true）。
        var themeApplyDeferred = false;

        // 视觉正确性规则（不可妥协）：
        // 窗口首帧一定用“当前系统主题”绘制。若保存的主题与它不同，把套用推迟到首帧之后
        // 就会先看到系统主题、再跳成保存主题——一次可见的闪烁。
        // 因此必须在窗口可见（Activate）之前拿到保存值，并做一次廉价判定：
        //   ① 保存值 = Default      → 它本来就等于系统主题，之后套用是无操作，安全；
        //   ② 保存值 = 当前系统主题 → 之后套用同样无视觉变化，安全；
        //   ③ 其它                  → 本次启动回退同步路径（主题留在关键路径上），宁可慢也不闪。
        // 判定只是枚举比较；保存值来自上面同一次设置读取。正常情况该读取已与窗口构造
        // 重叠完成，await 一个已完成任务不占关键路径。
        var readWasInFlightWhenWindowReady = !themeReadTask.IsCompleted;
        await themeReadTask;
        if (readWasInFlightWhenWindowReady)
        {
            // 读取比窗口构造慢：只能在窗口可见前等它，这属于回退，不是默认路径。
            StartupTrace.Mark("theme-read-waited");
        }
        StartupTrace.Mark("theme-init-done");

        themeApplyDeferred = CanApplyThemeAfterFirstFrame();
        StartupTrace.Mark(themeApplyDeferred ? "theme-apply-deferred" : "theme-sync-fallback");

        // 关键改动：先让窗口可见，Shell 排到首帧之后构建。
        // 旧顺序里 ShellPage 的 XAML（实测约 164ms）发生在 Activate() 之前，直接抬高“到窗口可见”。
        var buildShellDeferred = false;

        StartupTrace.Mark("ab-mode-deferred");
        if (mainWindow.Content == null)
        {
            // 顺序：先挂占位内容，再打开扩展内容模式。
            // 实测 ExtendsContentIntoTitleBar 单独占 77.7ms，而内容岛只有 0.2ms；
            // 先有内容再扩展，可省约 20ms（77.7 → 57.5），且视觉完全不变。
            mainWindow.Content = CreatePlaceholderContent();
            StartupTrace.Mark("placeholder-set");
            mainWindow.ExtendsContentIntoTitleBar = true;
            StartupTrace.Mark("titlebar-extended");
            buildShellDeferred = true;
        }
        else
        {
            // 非首次激活（内容已存在）：维持原有行为，直接走激活处理器。
            await HandleActivationAsync(activationArgs);
            StartupTrace.Mark("activation-handlers-done");
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
        // 固定使用 Low：实测改用 Normal 会让"窗口可见"恶化约 +11.5ms、"可交互"恶化约 +44.7ms
        // （8 对交替，Normal 只在 4/8 对的可用时间上更快），说明抢占 UI 线程反而拖慢首帧。
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
            StartupTrace.Mark("deferred-build-enter");
            // 本地化器（LEVER 1）可能还在后台构建：ShellPage 的 XAML l:Uids 需要它先就绪。
            // 这里在首帧之后，等待不占关键路径；任务内部吞掉异常，不会让构建失败。
            await App.WaitLocalizerReadyAsync();
            StartupTrace.Mark("deferred-localizer-waited");

            _shell = App.GetService<ShellPage>();
            StartupTrace.Mark("deferred-shell-resolved");
        }

        // 重试时 Content 可能已经就是这个 Shell，重复赋值会抛“元素已有父级”。
        if (!ReferenceEquals(mainWindow.Content, _shell))
        {
            mainWindow.Content = _shell;
        }
        StartupTrace.Mark("deferred-content-set");

        // 诊断（CB_STARTUP_DIAG=1）：排查"半透明效果消失"。把窗口背景类型、Shell 根节点背景、
        // Mica 是否受支持直接写进 trace —— Mica(Light) 与不透明白底肉眼难以区分，必须看数值。
        if (Environment.GetEnvironmentVariable("CB_STARTUP_DIAG") == "1")
        {
            var backdrop = mainWindow.SystemBackdrop?.GetType().Name ?? "null";
            var shellBg = (_shell as Panel)?.Background;
            var shellBgDesc = shellBg switch
            {
                null => "null",
                Microsoft.UI.Xaml.Media.SolidColorBrush s => $"solid A={s.Color.A} R={s.Color.R} G={s.Color.G} B={s.Color.B}",
                _ => shellBg.GetType().Name
            };
            var micaSupported = Microsoft.UI.Composition.SystemBackdrops.MicaController.IsSupported();
            StartupTrace.Mark($"diag backdrop={backdrop} shellBg={shellBgDesc} micaSupported={micaSupported}");
        }

        // Shell 替换占位层后重新套用已保存的主题（新根元素不会继承占位层的 RequestedTheme），
        // 顺带按新主题刷新标题栏按钮颜色。
        //
        // 曾试过"Theme == Default 时跳过这次重套"：交替 A/B 实测无效（配对 Δ 中位数 +23.4ms，
        // 仅 2/8 更快）。原因是这段 89ms 并非赋值本身，而是 Content 赋值后首次布局/渲染
        // 落在 await 让出期间；跳过赋值救不了布局。已回滚。
        await _themeSelectorService.SetRequestedThemeAsync();
        StartupTrace.Mark("deferred-theme-applied");

        await HandleActivationAsync(activationArgs);
        StartupTrace.Mark("deferred-navigation-done");
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
