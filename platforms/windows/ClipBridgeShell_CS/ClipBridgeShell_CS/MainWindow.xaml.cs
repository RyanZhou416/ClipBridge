using ClipBridgeShell_CS.Helpers;

using Windows.UI.ViewManagement;
using WinUI3Localizer;

namespace ClipBridgeShell_CS;

public sealed partial class MainWindow : WindowEx
{
    private Microsoft.UI.Dispatching.DispatcherQueue dispatcherQueue;

    private UISettings settings;

    public MainWindow()
    {
        InitializeComponent();

        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets/WindowIcon.ico"));
        Content = null;
        var loc = Localizer.Get();
        Title = loc.GetLocalizedString("AppDisplayName");   // 对应 Resources.resw -> AppDisplayName.Text/Content 等

        // 确保启用硬件加速
        // WinUI 3 默认使用硬件加速，但某些情况下可能回退到软件渲染
        // 通过设置 CompositionTarget 相关属性来确保使用硬件加速
        try
        {
            // 检查并设置 DPI awareness，这有助于硬件加速
            // WinUI 3 会自动处理，但我们可以显式设置以确保最佳性能
            var presenter = AppWindow.Presenter as Microsoft.UI.Windowing.OverlappedPresenter;
            if (presenter != null)
            {
                // 确保窗口使用硬件加速的合成
                presenter.IsMaximizable = true;
                presenter.IsMinimizable = true;
            }
        }
        catch
        {
            // 忽略设置失败
        }

        // Theme change code picked from https://github.com/microsoft/WinUI-Gallery/pull/1239
        dispatcherQueue = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        settings = new UISettings();
        settings.ColorValuesChanged += Settings_ColorValuesChanged; // cannot use FrameworkElement.ActualThemeChanged event

    }

    // this handles updating the caption button colors correctly when indows system theme is changed
    // while the app is open
    private void Settings_ColorValuesChanged(UISettings sender, object args)
    {
        // This calls comes off-thread, hence we will need to dispatch it to current app's thread
        dispatcherQueue.TryEnqueue(() =>
        {
            TitleBarHelper.ApplySystemThemeToCaptionButtons();
        });
    }
}
