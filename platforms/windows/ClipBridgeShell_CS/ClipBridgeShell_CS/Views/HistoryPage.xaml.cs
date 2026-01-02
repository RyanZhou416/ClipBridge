using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace ClipBridgeShell_CS.Views;

public sealed partial class HistoryPage : Page
{
    public HistoryViewModel ViewModel { get; }

    public HistoryPage()
    {
        ViewModel = App.GetService<HistoryViewModel>();
        InitializeComponent();
    }
}
