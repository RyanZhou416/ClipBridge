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

    private Microsoft.UI.Dispatching.DispatcherQueue dispatcherQueue;
    private UISettings settings;
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

        // 标题栏拖动 / 边框缩放期间临时摘掉 Mica 背景，松手后装回同一个实例。
        // 原因：Mica 由 DWM 逐帧重新合成，窗口移动时会把合成器压满并反压 Present（因果链详见
        // Helpers/WindowExtensions.SetDragBackdropToggle 的注释）。只在“窗口真的在动”的区间里
        // 关掉，静止外观与改动前完全一致；CB_NO_DRAG_BACKDROP_TOGGLE=1 可关闭该行为用于 A/B。
        this.SetDragBackdropToggle();

        dispatcherQueue = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        settings = new UISettings();
        settings.ColorValuesChanged += Settings_ColorValuesChanged;

        AppWindow.Closing += AppWindow_Closing;

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
