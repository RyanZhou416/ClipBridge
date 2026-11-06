using System.Runtime.InteropServices;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Services;

using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

using Windows.UI.ViewManagement;

using WinUI3Localizer;

namespace ClipBridgeShell_CS;

public sealed partial class MainWindow : Window
{
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    private const uint WM_NCMOUSELEAVE = 0x02A2;

    // === 启动测量开关（仅用于 A/B 对比，测量完成后应连同这里一起删除） ===
    // 背景：MainWindow 构造在冷启动里约占 94.5ms，但无法从一次运行看出 Mica 背景、
    // 窗口图标各自占多少。这里用环境变量在同一份构建产物上逐项关闭候选开销，
    // 方便集成方 A/B 后决定去留；变量未设置时默认全开，行为与改动前逐字节等价。
    //
    // 用 static readonly 只在类型初始化时读一次环境变量，
    // 未设置开关时每个开关的成本仅为一次环境变量查找的纳秒级开销。
    //
    // 注意：CB_WIN_NO_PERSIST / CB_WIN_NO_MICA / CB_WIN_NO_ICON 三个测量开关已删除。
    // 前者随 WinUIEx.WindowEx 一起去掉（普通 Window 没有 PersistenceId，且非打包应用里
    // 那条恢复路径本来就是空操作）；后两者经交替 A/B 实测为噪声（+21ms / -2ms，同向对数
    // 不足），按"不留看起来更快但没有实测支撑的代码"的原则移除，只保留打点。

    private readonly Microsoft.UI.Dispatching.DispatcherQueue dispatcherQueue;
    private readonly UISettings settings;
    private bool _closeHintShown;

    /// <summary>
    /// 本地化器尚未就绪时的标题兜底值。
    /// 最终标题仍由 <see cref="ApplyLocalizedTitle"/> 从 "AppDisplayName" 资源里取，
    /// 与改动前完全一致；这里只是避免在构造期调用 Localizer.Get()。
    /// </summary>
    private const string FallbackTitle = "ClipBridge";

    /// <summary>
    /// <c>CB_NO_MICA=1</c>：启动时不挂 Mica 背景，<see cref="Window.SystemBackdrop"/> 保持 null。
    /// 默认不设置 = 保持现状（创建 MicaBackdrop），所以正常用户完全看不到差异。
    ///
    /// 用途：同一份构建产物、只切这一个环境变量，截两张同位置截图做像素对比，
    /// 量化 Mica 在“页面主体是透明 XAML、hero 图只盖住顶部”的情况下到底覆盖了多少可见面积 ——
    /// 这件事只能靠像素差异客观回答，不能靠猜。只认字符串 "1"。
    ///
    /// 与 <c>CB_NO_DRAG_BACKDROP_TOGGLE</c> 互不干扰：这里没挂 Mica 时，拖拽开关读到的
    /// SystemBackdrop 本来就是 null，会直接返回、什么都不做（见 WindowExtensions.SetDragBackdropToggle）。
    /// </summary>
    private static readonly bool MicaDisabledByEnv =
        string.Equals(Environment.GetEnvironmentVariable("CB_NO_MICA")?.Trim(), "1", StringComparison.Ordinal);

    public MainWindow()
    {
        StartupTrace.Mark("mainwindow-ctor-enter");
        InitializeComponent();

        // 原来在 XAML 里的 <Window.SystemBackdrop><MicaBackdrop/></Window.SystemBackdrop>
        // 现在放在代码里设置。new MicaBackdrop() 的默认 Kind=Base，与 XAML 空元素写法等价；
        // 这里紧接 InitializeComponent 赋值，仍在窗口首次显示之前，视觉与改动前一致。
        //
        // CB_NO_MICA=1 时不创建 MicaBackdrop（SystemBackdrop 保持 null），用于像素级 A/B 截图；
        // 未设置该变量时走 else 分支，与改动前逐字节等价。日志由 StartupTrace 自身门控，
        // 只有 CB_STARTUP_TRACE=1 时才会写入 startup.log，正常运行时保持安静。
        if (MicaDisabledByEnv)
        {
            StartupTrace.Mark("mica-disabled-by-env");
        }
        else
        {
            this.SystemBackdrop = new MicaBackdrop();
        }

        StartupTrace.Mark("mainwindow-backdrop-done");

        // 位置/尺寸持久化（WinUIEx WindowEx.PersistenceId）已随 WindowEx 一并移除：
        // 非打包应用没有包标识，WindowManager.LoadPersistence() 本来就什么都不做。

        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets/WindowIcon.ico"));

        StartupTrace.Mark("mainwindow-icon-done");

        Content = null;

        // 本地化器可能还在后台构建（见 App.OnLaunched 的延迟本地化，LEVER 1），
        // 构造期不能调用 Localizer.Get()，否则会拿到未就绪的实例 / 空文本。
        // 先用固定字面量兜底；App 会在 App.LocalizerReady 完成后于 UI 线程调用
        // ApplyLocalizedTitle() 换成正式本地化标题，最终标题与改动前一致。
        Title = FallbackTitle;

        try
        {
            var presenter = AppWindow.Presenter as OverlappedPresenter;
            if (presenter != null)
            {
                presenter.IsMaximizable = true;
                presenter.IsMinimizable = true;
            }
        }
        catch
        {
        }

        // 替代 XAML 里 WindowEx 的 MinWidth="500" / MinHeight="500"：
        // 用与 WinUIEx 相同的 WM_GETMINMAXINFO 子类化机制，按下当前 DPI 换算成物理像素。
        this.SetMinimumSize(500, 500);

        // 标题栏拖动 / 边框缩放期间临时关掉 Mica 背景，松手后恢复。
        // 原因：Mica 由 DWM 逐帧重新合成，窗口移动时会把合成器压满并反压 Present（因果链详见
        // Helpers/WindowExtensions.SetDragBackdropToggle 的注释）。只在“窗口真的在动”的区间里
        // 关掉；关掉的同时用 Mica 文档回退色铺满内容根，避免透明页面露出桌面壁纸
        // （见 Helpers/DragBackdropFill）。CB_NO_DRAG_BACKDROP_TOGGLE=1 可关闭该行为用于 A/B。
        this.SetDragBackdropToggle();

        // 调试：CB_FORCE_DRAG_FILL=1 / CB_FORCE_DRAG_SNAPSHOT=1 时让窗口从启动起就停留在
        // 对应的“伪拖拽”状态（填充，或屏幕快照覆盖层 + DWMSBT_NONE），用于截图对比。
        // 快照优先级更高：两个都设置时只进入快照状态（EnableForcedDragFill 内部会跳过）。
        // 未设置时两个调用都立即返回，行为零变化。
        this.EnableForcedDragFill();
        this.EnableForcedDragSnapshot();

        dispatcherQueue = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        settings = new UISettings();
        settings.ColorValuesChanged += Settings_ColorValuesChanged;

        AppWindow.Closing += AppWindow_Closing;

        // 窗口不可见（最小化 / 隐藏到托盘）时延迟归还工作集；还原/显示时取消。
        // 普通失焦不触发（窗口仍然可见）。策略与安全约束见 Helpers/WorkingSetTrimmer.cs。
        AppWindow.Changed += AppWindow_Changed;

        // AppWindow.Changed 在状态切换瞬间可能仍读到旧状态（实测：还原事件里 presenter 还是
        // Minimized），只靠它会让“本次隐藏期已裁剪”标记在还原后不复位，导致下一次最小化不再裁剪。
        // Window.Activated 在窗口真正回到前台之后才触发，作为补充入口再同步一次状态。
        Activated += MainWindow_Activated;

        StartupTrace.Mark("mainwindow-ctor-exit");
    }

    private async void AppWindow_Closing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        args.Cancel = true;

        var settingsService = App.GetService<ILocalSettingsService>();
        var closeBehavior = await settingsService.ReadSettingAsync<string>("CloseBehavior");

        if (closeBehavior == "ExitApp")
        {
            await AppLifecycleHelper.GracefulShutdownAsync();
            return;
        }

        // 清除标题栏按钮的悬浮状态，然后直接隐藏窗口
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        PostMessageW(hwnd, WM_NCMOUSELEAVE, IntPtr.Zero, IntPtr.Zero);
        this.Hide();

        // 显式通知一次（AppWindow.Changed 的可见性变化通常也会触发；WorkingSetTrimmer 幂等）。
        WorkingSetTrimmer.OnWindowHidden();

        if (!_closeHintShown)
        {
            var showHint = await settingsService.ReadSettingAsync<bool?>("ShowFirstCloseHint") ?? true;
            if (showHint)
            {
                _closeHintShown = true;
                var tray = App.GetService<TrayService>();
                var loc = Localizer.Get();

                var title = loc.GetLocalizedString("Tray_MinimizedHint_Title");
                if (string.IsNullOrEmpty(title) || title == "Tray_MinimizedHint_Title")
                    title = "ClipBridge is running";

                var content = loc.GetLocalizedString("Tray_MinimizedHint_Content");
                if (string.IsNullOrEmpty(content) || content == "Tray_MinimizedHint_Content")
                    content = "ClipBridge has been minimized to the system tray.";

                tray.ShowBalloonTip(title, content);
                await settingsService.SaveSettingAsync("ShowFirstCloseHint", false);
            }
        }
    }

    /// <summary>
    /// 维护“不可见时裁剪工作集”的触发策略。
    ///
    /// 只把“最小化（presenter = Minimized）”和“隐藏到托盘（IsVisible = false）”当作不可见：
    /// 普通失焦 / 切到别的窗口时窗口仍然可见，用户随时会看回来，绝不能裁剪。
    /// 这里只观察可见性与呈现器状态变化，窗口大小/位置变化直接忽略。
    /// </summary>
    private void AppWindow_Changed(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (!args.DidVisibilityChange && !args.DidPresenterChange)
        {
            return;
        }

        UpdateWorkingSetTrimPolicy();
    }

    /// <summary>
    /// 窗口真正被激活（还原 / 从托盘显示 / 启动）后补一次状态同步：
    /// <see cref="AppWindow.Changed"/> 触发时 presenter 状态可能还没更新完，不能作为唯一入口。
    /// </summary>
    private void MainWindow_Activated(object sender, WindowActivatedEventArgs args)
    {
        UpdateWorkingSetTrimPolicy();
    }

    /// <summary>
    /// 按窗口当前状态通知 <see cref="WorkingSetTrimmer"/>：不可见 → 排队延迟裁剪；可见 → 取消并复位。
    /// 任何异常都只写 Debug 输出，不影响窗口本身。
    /// </summary>
    private void UpdateWorkingSetTrimPolicy()
    {
        try
        {
            var minimized = AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized };
            var hidden = !AppWindow.IsVisible || minimized;

            // CB_STARTUP_TRACE=1 时可复核状态切换顺序（未开启时不写文件；切换本身很稀疏）。
            StartupTrace.Mark($"ws-policy hidden={hidden} visible={AppWindow.IsVisible} minimized={minimized}");

            if (hidden)
            {
                WorkingSetTrimmer.OnWindowHidden();
            }
            else
            {
                WorkingSetTrimmer.OnWindowVisible();
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[MainWindow] Working-set trim policy update failed: {ex}");
        }
    }

    /// <summary>
    /// 本地化器就绪后由 App 在 UI 线程调用：把兜底标题换成正式本地化标题。
    /// 与改动前构造函数里直接读取 "AppDisplayName" 的结果逐字相同。
    /// </summary>
    internal void ApplyLocalizedTitle()
    {
        try
        {
            Title = Localizer.Get().GetLocalizedString("AppDisplayName");
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[MainWindow] Apply localized title failed: {ex}");
        }
    }

    private void Settings_ColorValuesChanged(UISettings sender, object args)
    {
        dispatcherQueue.TryEnqueue(() =>
        {
            TitleBarHelper.ApplySystemThemeToCaptionButtons();
        });
    }
}
