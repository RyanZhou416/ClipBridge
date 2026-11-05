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

    public MainWindow()
    {
        StartupTrace.Mark("mainwindow-ctor-enter");
        InitializeComponent();

        // 原来在 XAML 里的 <Window.SystemBackdrop><MicaBackdrop/></Window.SystemBackdrop>
        // 现在放在代码里设置。new MicaBackdrop() 的默认 Kind=Base，与 XAML 空元素写法等价；
        // 这里紧接 InitializeComponent 赋值，仍在窗口首次显示之前，视觉与改动前一致。
        this.SystemBackdrop = new MicaBackdrop();

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
