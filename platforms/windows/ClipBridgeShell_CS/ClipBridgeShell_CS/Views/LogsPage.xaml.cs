//platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Views/LogsPage.xaml.cs
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using System.Threading.Tasks;
using Windows.ApplicationModel.DataTransfer;
using Windows.UI.Core;
using System.Linq;
using System;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsPage : Page
{
    public LogsViewModel VM { get; }
    private const double ScrollThreshold = 10.0; // 底部吸附阈值（像素）
    private bool _isLoadingOlderLogs = false; // 防止重复加载

    public LogsPage()
    {
        VM = App.GetService<LogsViewModel>();
        InitializeComponent();
        DataContext = VM;
        
        // 订阅新日志事件（只有在滚动到底部时才自动滚动）
        VM.TailRequested += OnTailRequested;
        
        // 订阅滚动到底部请求
        VM.ScrollToBottomRequested += OnScrollToBottomRequested;
        
        // 订阅加载更早日志请求
        VM.LoadOlderLogsRequested += OnLoadOlderLogsRequested;
        
    }
    
    private void OnTailRequested(object? sender, EventArgs e)
    {
        // 只有在滚动到底部时才自动滚动
        if (VM.IsScrolledToBottom && VM.Items.Count > 0)
        {
            ScrollToBottom();
        }
    }
    
    private void OnScrollToBottomRequested(object? sender, EventArgs e)
    {
        ScrollToBottom();
    }
    
    private void ScrollToBottom()
    {
        if (ContentScrollViewer != null)
        {
            ContentScrollViewer.ChangeView(null, ContentScrollViewer.ScrollableHeight, null);
        }
    }
    
    private void OnLoadOlderLogsRequested(object? sender, LoadOlderLogsEventArgs e)
    {
        // 加载更早的日志后，需要保持滚动位置
        // 由于使用 ItemsRepeater，需要手动计算滚动位置
        if (e.OldFirstId.HasValue && ContentScrollViewer != null)
        {
            // 查找旧的第一条日志在新列表中的位置
            double itemHeight = 30; // 估算每行高度
            int index = 0;
            for (int i = 0; i < VM.Items.Count; i++)
            {
                if (VM.Items[i].Id == e.OldFirstId.Value)
                {
                    index = i;
                    break;
                }
            }
            // 保持滚动位置（添加的日志数量 * 每行高度）
            double newOffset = ContentScrollViewer.VerticalOffset + (e.AddedCount * itemHeight);
            ContentScrollViewer.ChangeView(null, newOffset, null);
        }
    }
    
    private void OnScrollViewChanged(object? sender, ScrollViewerViewChangedEventArgs e)
    {
        if (sender is ScrollViewer scrollViewer)
        {
            // 计算是否滚动到底部
            double verticalOffset = scrollViewer.VerticalOffset;
            double scrollableHeight = scrollViewer.ScrollableHeight;
            double viewportHeight = scrollViewer.ViewportHeight;
            
            // 判断是否接近底部（在阈值内）
            bool isAtBottom = (verticalOffset + viewportHeight) >= (scrollableHeight - ScrollThreshold);
            
            // 更新 ViewModel 的滚动状态
            VM.IsScrolledToBottom = isAtBottom;
            
            // 如果滚动接近顶部，触发加载更早的日志
            if (!_isLoadingOlderLogs && verticalOffset < ScrollThreshold && scrollableHeight > 0)
            {
                _isLoadingOlderLogs = true;
                _ = Task.Run(async () =>
                {
                    await VM.LoadOlderLogsAsync();
                    // 延迟重置标志，避免频繁触发
                    await Task.Delay(500);
                    _isLoadingOlderLogs = false;
                });
            }
        }
    }
    
    private void OnScrollToBottomClick(object sender, RoutedEventArgs e)
    {
        VM.ScrollToBottom();
    }
    
    // 列宽调整相关
    private bool _isResizing = false;
    private int _resizingColumn = -1;
    private double _resizeStartX = 0;
    private double _resizeStartWidth = 0;
    
    // 光标改变事件
    private void OnCol1ResizeEnter(object sender, PointerRoutedEventArgs e)
    {
        if (Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.SizeWestEast, 0);
        }
    }
    
    private void OnCol1ResizeExit(object sender, PointerRoutedEventArgs e)
    {
        if (!_isResizing && Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
        }
    }
    
    private void OnCol2ResizeEnter(object sender, PointerRoutedEventArgs e)
    {
        if (Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.SizeWestEast, 0);
        }
    }
    
    private void OnCol2ResizeExit(object sender, PointerRoutedEventArgs e)
    {
        if (!_isResizing && Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
        }
    }
    
    private void OnCol3ResizeEnter(object sender, PointerRoutedEventArgs e)
    {
        if (Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.SizeWestEast, 0);
        }
    }
    
    private void OnCol3ResizeExit(object sender, PointerRoutedEventArgs e)
    {
        if (!_isResizing && Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
        }
    }
    
    private void OnCol4ResizeEnter(object sender, PointerRoutedEventArgs e)
    {
        if (Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.SizeWestEast, 0);
        }
    }
    
    private void OnCol4ResizeExit(object sender, PointerRoutedEventArgs e)
    {
        if (!_isResizing && Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
        }
    }
    
    private void OnCol5ResizeEnter(object sender, PointerRoutedEventArgs e)
    {
        if (Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.SizeWestEast, 0);
        }
    }
    
    private void OnCol5ResizeExit(object sender, PointerRoutedEventArgs e)
    {
        if (!_isResizing && Window.Current?.CoreWindow != null)
        {
            Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
        }
    }
    
    // 表头点击事件（显示过滤弹窗）
    private void OnTimestampHeaderClick(object sender, RoutedEventArgs e)
    {
        ShowTimestampFilterFlyout(sender);
    }
    
    private void OnLevelHeaderClick(object sender, RoutedEventArgs e)
    {
        ShowLevelFilterFlyout(sender);
    }
    
    private void OnComponentHeaderClick(object sender, RoutedEventArgs e)
    {
        ShowComponentFilterFlyout(sender);
    }
    
    private void OnCategoryHeaderClick(object sender, RoutedEventArgs e)
    {
        ShowCategoryFilterFlyout(sender);
    }
    
    private void OnMessageHeaderClick(object sender, RoutedEventArgs e)
    {
        // 消息列使用搜索框过滤，不需要弹窗
    }
    
    private void OnExceptionHeaderClick(object sender, RoutedEventArgs e)
    {
        // 异常列使用搜索框过滤，不需要弹窗
    }
    
    // 列宽调整事件
    private void OnCol1ResizeStart(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        _isResizing = true;
        _resizingColumn = 1;
        _resizeStartX = e.GetCurrentPoint(HeaderGrid).Position.X;
        _resizeStartWidth = VM.Col1Width;
        (sender as UIElement)?.CapturePointer(e.Pointer);
    }
    
    private void OnCol1ResizeMove(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 1)
        {
            var currentX = e.GetCurrentPoint(HeaderGrid).Position.X;
            var delta = currentX - _resizeStartX;
            var newWidth = Math.Max(50, _resizeStartWidth + delta);
            VM.Col1Width = newWidth;
        }
    }
    
    private void SyncItemColumnWidth(int columnIndex, double width)
    {
        // 同步列宽到所有 ItemsRepeater 项目
        // 由于 ItemsRepeater 的 DataTemplate 是独立的，我们需要使用其他方法
        // 这里可以通过更新 ViewModel 中的列宽属性，然后在 XAML 中绑定
        // 或者使用代码隐藏遍历所有项目并更新列宽
    }
    
    private void OnCol1ResizeEnd(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 1)
        {
            _isResizing = false;
            (sender as UIElement)?.ReleasePointerCapture(e.Pointer);
            if (Window.Current?.CoreWindow != null)
            {
                Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
            }
        }
    }
    
    private void OnCol2ResizeStart(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        _isResizing = true;
        _resizingColumn = 2;
        _resizeStartX = e.GetCurrentPoint(HeaderGrid).Position.X;
        _resizeStartWidth = VM.Col2Width;
        (sender as UIElement)?.CapturePointer(e.Pointer);
    }
    
    private void OnCol2ResizeMove(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 2)
        {
            var currentX = e.GetCurrentPoint(HeaderGrid).Position.X;
            var delta = currentX - _resizeStartX;
            var newWidth = Math.Max(50, _resizeStartWidth + delta);
            VM.Col2Width = newWidth;
        }
    }
    
    private void OnCol2ResizeEnd(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 2)
        {
            _isResizing = false;
            (sender as UIElement)?.ReleasePointerCapture(e.Pointer);
            if (Window.Current?.CoreWindow != null)
            {
                Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
            }
        }
    }
    
    private void OnCol3ResizeStart(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        _isResizing = true;
        _resizingColumn = 3;
        _resizeStartX = e.GetCurrentPoint(HeaderGrid).Position.X;
        _resizeStartWidth = VM.Col3Width;
        (sender as UIElement)?.CapturePointer(e.Pointer);
    }
    
    private void OnCol3ResizeMove(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 3)
        {
            var currentX = e.GetCurrentPoint(HeaderGrid).Position.X;
            var delta = currentX - _resizeStartX;
            var newWidth = Math.Max(50, _resizeStartWidth + delta);
            VM.Col3Width = newWidth;
        }
    }
    
    private void OnCol3ResizeEnd(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 3)
        {
            _isResizing = false;
            (sender as UIElement)?.ReleasePointerCapture(e.Pointer);
            if (Window.Current?.CoreWindow != null)
            {
                Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
            }
        }
    }
    
    private void OnCol4ResizeStart(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        _isResizing = true;
        _resizingColumn = 4;
        _resizeStartX = e.GetCurrentPoint(HeaderGrid).Position.X;
        _resizeStartWidth = VM.Col4Width;
        (sender as UIElement)?.CapturePointer(e.Pointer);
    }
    
    private void OnCol4ResizeMove(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 4)
        {
            var currentX = e.GetCurrentPoint(HeaderGrid).Position.X;
            var delta = currentX - _resizeStartX;
            var newWidth = Math.Max(50, _resizeStartWidth + delta);
            VM.Col4Width = newWidth;
        }
    }
    
    private void OnCol4ResizeEnd(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 4)
        {
            _isResizing = false;
            (sender as UIElement)?.ReleasePointerCapture(e.Pointer);
            if (Window.Current?.CoreWindow != null)
            {
                Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
            }
        }
    }
    
    private void OnCol5ResizeStart(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        // 分类列是 * 宽度，不支持调整
    }
    
    private void OnCol5ResizeMove(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        // 分类列是 * 宽度，不支持调整
    }
    
    private void OnCol5ResizeEnd(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_isResizing && _resizingColumn == 5)
        {
            _isResizing = false;
            (sender as UIElement)?.ReleasePointerCapture(e.Pointer);
            if (Window.Current?.CoreWindow != null)
            {
                Window.Current.CoreWindow.PointerCursor = new CoreCursor(CoreCursorType.Arrow, 0);
            }
        }
    }
    
    // 单独的过滤窗口
    private void ShowTimestampFilterFlyout(object sender)
    {
        var flyout = new Flyout
        {
            Content = new LogsTimestampFilterFlyout
            {
                ViewModel = VM
            }
        };
        flyout.ShowAt(sender as FrameworkElement);
    }
    
    private void ShowLevelFilterFlyout(object sender)
    {
        var flyout = new Flyout
        {
            Content = new LogsLevelFilterFlyout
            {
                ViewModel = VM
            }
        };
        flyout.ShowAt(sender as FrameworkElement);
    }
    
    private void ShowComponentFilterFlyout(object sender)
    {
        var flyout = new Flyout
        {
            Content = new LogsComponentFilterFlyout
            {
                ViewModel = VM
            }
        };
        flyout.ShowAt(sender as FrameworkElement);
    }
    
    private void ShowCategoryFilterFlyout(object sender)
    {
        var flyout = new Flyout
        {
            Content = new LogsCategoryFilterFlyout
            {
                ViewModel = VM
            }
        };
        flyout.ShowAt(sender as FrameworkElement);
    }
    
    // 选择框事件
    private void OnSelectAllChecked(object sender, RoutedEventArgs e)
    {
        VM.SelectAllCmd.Execute(null);
    }
    
    private void OnSelectAllUnchecked(object sender, RoutedEventArgs e)
    {
        VM.DeselectAllCmd.Execute(null);
    }
    
    private void OnItemChecked(object sender, RoutedEventArgs e)
    {
        if (sender is CheckBox checkBox && checkBox.DataContext is Models.LogRow row)
        {
            if (!VM.SelectedLogIds.Contains(row.Id))
            {
                VM.SelectedLogIds.Add(row.Id);
            }
        }
    }
    
    private void OnItemUnchecked(object sender, RoutedEventArgs e)
    {
        if (sender is CheckBox checkBox && checkBox.DataContext is Models.LogRow row)
        {
            VM.SelectedLogIds.Remove(row.Id);
        }
    }
    
    // 复制功能
    private void OnCopyClick(object sender, RoutedEventArgs e)
    {
        var selectedItems = VM.Items.Where(item => item.IsSelected).ToList();
        if (selectedItems.Count == 0)
        {
            selectedItems = VM.Items.ToList(); // 如果没有选中，复制所有
        }
        
        if (selectedItems.Count == 0) return;
        
        // 构建制表符分隔的表格内容
        var sb = new System.Text.StringBuilder();
        
        // 表头
        sb.AppendLine("时间戳\t级别\t组件\t分类\t消息\t异常");
        
        // 数据行
        foreach (var item in selectedItems)
        {
            sb.AppendLine($"{item.TimeStrFull}\t{item.LevelName}\t{item.Component}\t{item.Category}\t{item.Message}\t{item.Exception ?? ""}");
        }
        
        // 复制到剪贴板
        var dataPackage = new DataPackage();
        dataPackage.SetText(sb.ToString());
        Clipboard.SetContent(dataPackage);
    }
}
