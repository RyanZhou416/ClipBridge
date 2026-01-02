using System.Diagnostics;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml;
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

    private async void CopyButton_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var fe = sender as FrameworkElement;
            var meta = fe?.DataContext as ItemMetaPayload;

            Debug.WriteLine($"[HistoryPage] CopyButton_Click meta={(meta == null ? "null" : meta.ItemId)} kind={meta?.Kind}");

            if (meta == null)
                return;

            await ViewModel.CopyCommand.ExecuteAsync(meta);
        } catch (Exception ex)
        {
            Debug.WriteLine($"[HistoryPage] CopyButton_Click EX: {ex}");

            // 建议：显示一个简单的错误提示给用户
            var dialog = new ContentDialog
            {
                Title = "复制失败",
                Content = $"无法写入剪贴板，请重试。\n错误信息: {ex.Message}",
                CloseButtonText = "确定",
                XamlRoot = this.Content.XamlRoot
            };
            try
            {
                await dialog.ShowAsync();
            } catch { /* 如果 Dialog 正在显示可能会报错，忽略 */ }
        }
    }
}
