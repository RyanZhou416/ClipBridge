using ClipBridgeShell_CS.Services;

using Microsoft.UI.Xaml;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// Unified graceful shutdown following the documented exit sequence (5.7.6):
/// 1. Stop ClipboardWatcher
/// 2. Flush log queue
/// 3. CoreHost.Shutdown()
/// 4. Release tray resources
/// 5. Exit process
/// </summary>
public static class AppLifecycleHelper
{
    private static bool _isShuttingDown;

    public static async Task GracefulShutdownAsync()
    {
        if (_isShuttingDown)
        {
            return;
        }
        _isShuttingDown = true;

        try
        {
            var clipboardWatcher = App.GetService<ClipboardWatcher>();
            clipboardWatcher.Shutdown();
        }
        catch
        {
        }

        try
        {
            var coreHost = App.GetService<Contracts.Services.ICoreHostService>();
            using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
            await coreHost.ShutdownAsync(cts.Token);
        }
        catch
        {
        }

        try
        {
            var tray = App.GetService<TrayService>();
            tray.Dispose();
        }
        catch
        {
        }

        Application.Current.Exit();
    }
}
