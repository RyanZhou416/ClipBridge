using System.Runtime.InteropServices;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Services;

using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml.Media;

using Windows.UI.ViewManagement;

using WinUI3Localizer;

namespace ClipBridgeShell_CS;

public sealed partial class MainWindow : WindowEx
{
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    private const uint WM_NCMOUSELEAVE = 0x02A2;

    // === 启动测量开关（仅用于 A/B 对比，测量完成后应连同这里一起删除） ===
    // 背景：MainWindow 构造在冷启动里约占 94.5ms，但无法从一次运行看出 Mica 背景、
    // 窗口图标、WinUIEx 位置恢复各自占多少。这里用环境变量在同一份构建产物上
    // 逐项关闭候选开销，方便集成方 A/B 后决定去留；变量未设置时默认全开，
    // 行为与改动前逐字节等价。
    //
    // 用 static readonly 只在类型初始化时读一次环境变量，
    // 未设置开关时每个开关的成本仅为一次环境变量查找的纳秒级开销。
    private static readonly bool NoMica =
        string.Equals(Environment.GetEnvironmentVariable("CB_WIN_NO_MICA"), "1", StringComparison.Ordinal);

    private static readonly bool NoIcon =
        string.Equals(Environment.GetEnvironmentVariable("CB_WIN_NO_ICON"), "1", StringComparison.Ordinal);

    private static readonly bool NoPersist =
        string.Equals(Environment.GetEnvironmentVariable("CB_WIN_NO_PERSIST"), "1", StringComparison.Ordinal);

    private Microsoft.UI.Dispatching.DispatcherQueue dispatcherQueue;
    private UISettings settings;
    private bool _closeHintShown;

    public MainWindow()
    {
        StartupTrace.Mark("mainwindow-ctor-enter");
        InitializeComponent();

        // 原来在 XAML 里的 <Window.SystemBackdrop><MicaBackdrop/></Window.SystemBackdrop>
        // 必须挪到代码里才能被 CB_WIN_NO_MICA=1 跳过。new MicaBackdrop() 的默认
        // Kind=Base，与 XAML 空元素写法完全等价；这里紧接 InitializeComponent 赋值，
        // 仍在窗口首次显示（Activate）之前，视觉效果与改动前一致。
        if (!NoMica)
        {
            this.SystemBackdrop = new MicaBackdrop();
        }

        StartupTrace.Mark("mainwindow-backdrop-done");

        // WinUIEx 的 WindowEx.PersistenceId 只是对 WindowManager.PersistenceId 的透传，
        // 它本身不做任何事：真正的位置恢复发生在窗口第一次收到 WM_SHOWWINDOW
        // （即 App.ActivationService 里的 Activate()）时的 LoadPersistence()，
        // 而该函数只是在读取 PersistenceId 处判断是否为空。因此只要在 Activate 之前
        // 把构造期由 XAML 设置的 PersistenceId 清空，就能零成本关闭恢复；
        // 同时窗口关闭时的 SavePersistence() 也会跳过（测量期间不再写入新位置）。
        // 注意：WindowEx 基类在构造函数里无条件创建 WindowManager / 消息监视器，
        // 那部分（可能产生的额外窗口句柄）不在 PersistenceId 控制范围内。
        if (NoPersist)
        {
            PersistenceId = null;
        }

        if (!NoIcon)
        {
            AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets/WindowIcon.ico"));
        }

        StartupTrace.Mark("mainwindow-icon-done");

        Content = null;
        var loc = Localizer.Get();
        Title = loc.GetLocalizedString("AppDisplayName");

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

    private void Settings_ColorValuesChanged(UISettings sender, object args)
    {
        dispatcherQueue.TryEnqueue(() =>
        {
            TitleBarHelper.ApplySystemThemeToCaptionButtons();
        });
    }
}
