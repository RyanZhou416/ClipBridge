using ClipBridgeShell_CS.Activation;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Views;

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

        // Set the MainWindow Content.
        if (App.MainWindow.Content == null)
        {
            _shell = App.GetService<ShellPage>();
            App.MainWindow.Content = _shell ?? new Frame();
        }
        StartupTrace.Mark("shellpage-created");

        // Handle activation via ActivationHandlers.
        await HandleActivationAsync(activationArgs);
        StartupTrace.Mark("activation-handlers-done");

        // 自启到托盘：若由注册表/计划任务传入 --startup-to-tray，不激活主窗口，直接隐藏到托盘
        if (StartupService.IsStartupToTrayRequested())
        {
            App.MainWindow.Hide();
        }
        else
        {
            App.MainWindow.Activate();
        }
        StartupTrace.Mark("window-activated");

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
