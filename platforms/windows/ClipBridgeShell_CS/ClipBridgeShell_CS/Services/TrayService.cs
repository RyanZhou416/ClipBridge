using H.NotifyIcon;

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

using WinUI3Localizer;

namespace ClipBridgeShell_CS.Services;

public sealed class TrayService : IDisposable
{
    private TaskbarIcon? _trayIcon;
    private bool _disposed;

    public event EventHandler? ShowWindowRequested;
    public event EventHandler? ExitRequested;

    public void Initialize()
    {
        if (_trayIcon != null)
        {
            return;
        }

        _trayIcon = new TaskbarIcon
        {
            ToolTipText = "ClipBridge",
        };

        var iconPath = Path.Combine(AppContext.BaseDirectory, "Assets", "WindowIcon.ico");
        if (File.Exists(iconPath))
        {
            _trayIcon.Icon = new System.Drawing.Icon(iconPath);
        }

        RebuildContextMenu();

        _trayIcon.LeftClickCommand = new TrayRelayCommand(OnShowWindow);
        _trayIcon.DoubleClickCommand = new TrayRelayCommand(OnShowWindow);

        _trayIcon.ForceCreate();
    }

    public void RebuildContextMenu()
    {
        if (_trayIcon == null)
        {
            return;
        }

        var loc = Localizer.Get();

        var showLabel = loc.GetLocalizedString("Tray_ShowWindow");
        if (string.IsNullOrEmpty(showLabel) || showLabel == "Tray_ShowWindow")
        {
            showLabel = "Show Window";
        }

        var exitLabel = loc.GetLocalizedString("Tray_Exit");
        if (string.IsNullOrEmpty(exitLabel) || exitLabel == "Tray_Exit")
        {
            exitLabel = "Exit";
        }

        var menu = new MenuFlyout();
        var showItem = new MenuFlyoutItem { Text = showLabel };
        showItem.Click += (_, _) => OnShowWindow();
        menu.Items.Add(showItem);

        menu.Items.Add(new MenuFlyoutSeparator());

        var exitItem = new MenuFlyoutItem { Text = exitLabel };
        exitItem.Click += (_, _) => OnExit();
        menu.Items.Add(exitItem);

        _trayIcon.ContextFlyout = menu;
    }

    public void ShowBalloonTip(string title, string text)
    {
        _trayIcon?.ShowNotification(title, text);
    }

    private void OnShowWindow()
    {
        ShowWindowRequested?.Invoke(this, EventArgs.Empty);
    }

    private void OnExit()
    {
        ExitRequested?.Invoke(this, EventArgs.Empty);
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }
        _disposed = true;
        _trayIcon?.Dispose();
        _trayIcon = null;
    }

    private sealed class TrayRelayCommand : System.Windows.Input.ICommand
    {
        private readonly Action _execute;
        public TrayRelayCommand(Action execute) => _execute = execute;
#pragma warning disable CS0067
        public event EventHandler? CanExecuteChanged;
#pragma warning restore CS0067
        public bool CanExecute(object? parameter) => true;
        public void Execute(object? parameter) => _execute();
    }
}
