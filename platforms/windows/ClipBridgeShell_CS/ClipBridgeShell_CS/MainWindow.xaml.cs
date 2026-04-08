using System.Runtime.InteropServices;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Services;

using Microsoft.UI.Windowing;

using Windows.UI.ViewManagement;

using WinUI3Localizer;

namespace ClipBridgeShell_CS;

public sealed partial class MainWindow : WindowEx
{
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    private const uint WM_NCMOUSELEAVE = 0x02A2;

    private Microsoft.UI.Dispatching.DispatcherQueue dispatcherQueue;
    private UISettings settings;
    private bool _closeHintShown;

    public MainWindow()
    {
        InitializeComponent();

        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets/WindowIcon.ico"));
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
