using ClipBridgeShell_CS.ViewModels;

using Microsoft.UI.Xaml.Controls;

namespace ClipBridgeShell_CS.Views;

public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel
    {
        get;
    }

    public MainPage()
    {
        ViewModel = App.GetService<MainViewModel>();
        InitializeComponent();
    }
}
