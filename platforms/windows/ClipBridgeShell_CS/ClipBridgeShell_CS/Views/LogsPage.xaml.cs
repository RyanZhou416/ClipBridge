//platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Views/LogsPage.xaml.cs
using ClipBridgeShell_CS.ViewModels;

using Microsoft.UI.Xaml.Controls;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsPage : Page
{
    public LogsViewModel VM { get; } = new();

    public LogsPage()
    {
        InitializeComponent();
        DataContext = VM;
        VM.TailRequested += (_, __) =>
        {
            if (List.Items.Count > 0)
                List.ScrollIntoView(List.Items[^1]);
        };
    }
}
