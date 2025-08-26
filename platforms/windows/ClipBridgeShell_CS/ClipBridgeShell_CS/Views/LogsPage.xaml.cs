using ClipBridgeShell_CS.ViewModels;

using Microsoft.UI.Xaml.Controls;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsPage : Page
{
    public LogsViewModel ViewModel
    {
        get;
    }

    public LogsPage()
    {
        ViewModel = App.GetService<LogsViewModel>();
        InitializeComponent();
    }
}
