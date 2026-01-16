using System;
using System.Linq;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;
using Windows.UI;

namespace ClipBridgeShell_CS.Views;

public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel
    {
        get;
    }

    private Microsoft.UI.Xaml.Controls.InfoBar? _errorInfoBar;

    public MainPage()
    {
        try
        {
            ViewModel = App.GetService<MainViewModel>();
            InitializeComponent();
            
            // 监听数据变化，更新图表（在Loaded之后）
            Loaded += OnPageLoaded;
        }
        catch (Exception ex)
        {
            ShowError($"初始化主页失败: {ex.Message}");
        }
    }

    private void OnPageLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        try
        {
            // 监听数据变化，更新图表
            ViewModel.CacheSeries.CollectionChanged += (s, args) => SafeDrawChart(DrawCacheChart, "Cache");
            ViewModel.NetworkSeries.CollectionChanged += (s, args) => SafeDrawChart(DrawNetworkChart, "Network");
            ViewModel.ActivitySeries.CollectionChanged += (s, args) => SafeDrawChart(DrawActivityChart, "Activity");
            
            // 初始绘制
            SafeDrawChart(DrawCacheChart, "Cache");
            SafeDrawChart(DrawNetworkChart, "Network");
            SafeDrawChart(DrawActivityChart, "Activity");

            // 监听Canvas大小变化
            CacheChartCanvas.SizeChanged += (s, args) => SafeDrawChart(DrawCacheChart, "Cache");
            NetworkChartCanvas.SizeChanged += (s, args) => SafeDrawChart(DrawNetworkChart, "Network");
            ActivityChartCanvas.SizeChanged += (s, args) => SafeDrawChart(DrawActivityChart, "Activity");

            // 监听RecentItems和选中状态变化
            ViewModel.RecentItems.CollectionChanged += (s, e) => 
            {
                // 当RecentItems变化时，可能需要更新UI
                // 由于ItemsRepeater会自动更新，这里可以留空或添加其他逻辑
            };
            ViewModel.PropertyChanged += (s, e) =>
            {
                if (e.PropertyName == nameof(ViewModel.SelectedItemId))
                {
                    // 选中状态变化时，更新所有卡片的视觉效果
                    UpdateCardSelection();
                }
            };
            
            // 设置视差滚动效果
            if (ContentScrollViewer != null)
            {
                ContentScrollViewer.ViewChanged += OnScrollViewChanged;
            }
            
            // 检测背景图片颜色并调整标题颜色
            if (HeroImage != null)
            {
                HeroImage.ImageOpened += OnHeroImageOpened;
                // 如果图片已经加载，立即检测
                if (HeroImage.Source != null)
                {
                    _ = DetectBackgroundColorAndUpdateTitle();
                }
            }
        }
        catch (Exception ex)
        {
            ShowError($"加载主页内容失败: {ex.Message}");
        }
    }
    
    private async void OnHeroImageOpened(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        await DetectBackgroundColorAndUpdateTitle();
    }
    
    private async Task DetectBackgroundColorAndUpdateTitle()
    {
        try
        {
            if (HeroImage?.Source == null || TitleTextBlock == null)
                return;
            
            // 获取图片源
            var imageSource = HeroImage.Source;
            if (imageSource is Microsoft.UI.Xaml.Media.Imaging.BitmapImage bitmapImage)
            {
                // 读取图片文件
                var uri = bitmapImage.UriSource;
                if (uri != null)
                {
                    var file = await Windows.Storage.StorageFile.GetFileFromApplicationUriAsync(uri);
                    await DetectImageColorAndUpdateTitle(file);
                }
            }
        }
        catch (Exception ex)
        {
            // 如果检测失败，使用默认颜色
            System.Diagnostics.Debug.WriteLine($"检测背景颜色失败: {ex.Message}");
        }
    }
    
    private async Task DetectImageColorAndUpdateTitle(Windows.Storage.StorageFile file)
    {
        try
        {
            using (var stream = await file.OpenAsync(Windows.Storage.FileAccessMode.Read))
            {
                var decoder = await BitmapDecoder.CreateAsync(stream);
                
                // 计算标题区域在图片中的位置（标题在图片的上方，大约在 48-100 像素的位置）
                // 采样标题区域的颜色（左上角 36, 48 到 200, 100 的区域）
                var width = (uint)Math.Min(200, decoder.PixelWidth);
                var height = (uint)Math.Min(100, decoder.PixelHeight);
                var x = (uint)Math.Min(36, decoder.PixelWidth);
                var y = (uint)Math.Min(48, decoder.PixelHeight);
                
                // 读取像素数据
                var transform = new BitmapTransform
                {
                    Bounds = new BitmapBounds
                    {
                        X = x,
                        Y = y,
                        Width = width,
                        Height = height
                    },
                    ScaledWidth = width,
                    ScaledHeight = height
                };
                
                var pixelData = await decoder.GetPixelDataAsync(
                    BitmapPixelFormat.Rgba8,
                    BitmapAlphaMode.Premultiplied,
                    transform,
                    ExifOrientationMode.RespectExifOrientation,
                    ColorManagementMode.DoNotColorManage);
                
                var bytes = pixelData.DetachPixelData();
                
                // 计算平均亮度
                long totalR = 0, totalG = 0, totalB = 0;
                int pixelCount = 0;
                
                for (int i = 0; i < bytes.Length; i += 4)
                {
                    byte r = bytes[i];
                    byte g = bytes[i + 1];
                    byte b = bytes[i + 2];
                    // bytes[i + 3] 是 alpha，这里忽略
                    
                    totalR += r;
                    totalG += g;
                    totalB += b;
                    pixelCount++;
                }
                
                if (pixelCount > 0)
                {
                    int avgR = (int)(totalR / pixelCount);
                    int avgG = (int)(totalG / pixelCount);
                    int avgB = (int)(totalB / pixelCount);
                    
                    // 计算亮度（使用相对亮度公式：0.299*R + 0.587*G + 0.114*B）
                    double luminance = 0.299 * avgR + 0.587 * avgG + 0.114 * avgB;
                    
                    // 如果亮度小于 128（深色背景），使用白色文字；否则使用黑色文字
                    Color textColor = luminance < 128 ? Colors.White : Colors.Black;
                    
                    // 更新标题颜色（确保在 UI 线程上执行）
                    if (TitleTextBlock != null)
                    {
                        var dispatcherQueue = DispatcherQueue.GetForCurrentThread();
                        if (dispatcherQueue != null)
                        {
                            dispatcherQueue.TryEnqueue(DispatcherQueuePriority.Normal, () =>
                            {
                                if (TitleTextBlock != null)
                                {
                                    TitleTextBlock.Foreground = new SolidColorBrush(textColor);
                                }
                            });
                        }
                        else
                        {
                            // 如果已经在 UI 线程，直接设置
                            TitleTextBlock.Foreground = new SolidColorBrush(textColor);
                        }
                    }
                }
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"检测图片颜色失败: {ex.Message}");
        }
    }
    
    private void OnScrollViewChanged(object? sender, Microsoft.UI.Xaml.Controls.ScrollViewerViewChangedEventArgs e)
    {
        if (ContentScrollViewer != null && ParallaxTransform != null)
        {
            // 获取当前滚动位置
            var scrollOffset = ContentScrollViewer.VerticalOffset;
            
            // 视差效果：卡片容器向上移动更快
            // 调整这个倍数来改变速度：
            // - 1.0 = 正常速度（无加速）
            // - 1.2 = 轻微加速
            // - 1.5 = 当前速度（中等加速）
            // - 2.0 = 快速加速
            // 值越大，卡片容器向上移动越快，越早盖住背景
            var parallaxSpeed = 1.1; // 在这里调整速度倍数
            var parallaxOffset = scrollOffset * parallaxSpeed;
            ParallaxTransform.Y = -parallaxOffset;
        }
    }

    private void OnCardTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder && cardBorder.Tag is ItemMetaPayload item)
        {
            ViewModel.SelectItemCommand.Execute(item);
            UpdateCardSelection(cardBorder);
        }
    }

    private void OnCardLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder && cardBorder.Tag is ItemMetaPayload item)
        {
            UpdateCardSelection(cardBorder);
            
            // 设置设备名称
            var deviceNameTextBlock = FindVisualChildByName<Microsoft.UI.Xaml.Controls.TextBlock>(cardBorder, "DeviceNameText");
            if (deviceNameTextBlock != null && deviceNameTextBlock.Inlines.Count > 1)
            {
                string deviceName;
                // 优先使用 ItemMeta 中的 source_device_name
                if (!string.IsNullOrEmpty(item.SourceDeviceName))
                {
                    deviceName = item.SourceDeviceName;
                    System.Diagnostics.Debug.WriteLine($"[MainPage] 使用 SourceDeviceName: {deviceName} (SourceDeviceId: {item.SourceDeviceId})");
                }
                else
                {
                    // 如果 source_device_name 为空，判断是否为本机设备
                    var localDeviceId = ViewModel.GetLocalDeviceId();
                    System.Diagnostics.Debug.WriteLine($"[MainPage] SourceDeviceName 为空，SourceDeviceId: {item.SourceDeviceId}, LocalDeviceId: {localDeviceId}");
                    
                    if (!string.IsNullOrEmpty(localDeviceId) && item.SourceDeviceId == localDeviceId)
                    {
                        // 本机设备，使用 MachineName
                        deviceName = System.Environment.MachineName;
                        System.Diagnostics.Debug.WriteLine($"[MainPage] 本机设备，使用 MachineName: {deviceName}");
                    }
                    else
                    {
                        // 其他设备，从 PeerStore 查找
                        deviceName = ViewModel.GetDeviceName(item.SourceDeviceId);
                        System.Diagnostics.Debug.WriteLine($"[MainPage] 从 PeerStore 获取: {deviceName}");
                        
                        // 如果还是找不到（返回的是 deviceId），尝试使用 MachineName 作为后备
                        if (deviceName == item.SourceDeviceId)
                        {
                            // 如果 PeerStore 中找不到，可能是本机设备但 localDeviceId 还没加载
                            // 或者确实是未知设备，使用 MachineName 作为临时显示
                            deviceName = System.Environment.MachineName;
                            System.Diagnostics.Debug.WriteLine($"[MainPage] PeerStore 中未找到，使用 MachineName 作为后备: {deviceName}");
                        }
                    }
                }
                
                if (deviceNameTextBlock.Inlines[1] is Microsoft.UI.Xaml.Documents.Run run)
                {
                    run.Text = deviceName;
                }
            }
        }
    }
    
    private static T? FindVisualChildByName<T>(Microsoft.UI.Xaml.DependencyObject parent, string name) where T : Microsoft.UI.Xaml.DependencyObject
    {
        for (int i = 0; i < Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(parent); i++)
        {
            var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(parent, i);
            if (child is T result)
            {
                if (child is Microsoft.UI.Xaml.FrameworkElement fe && fe.Name == name)
                    return result;
            }
            var childOfChild = FindVisualChildByName<T>(child, name);
            if (childOfChild != null)
                return childOfChild;
        }
        return null;
    }

    private void OnStateBadgeLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border badge)
        {
            // 向上查找CardBorder来获取item
            var cardBorder = FindParent<Microsoft.UI.Xaml.Controls.Border>(badge);
            if (cardBorder?.Tag is ItemMetaPayload item)
            {
                var state = ViewModel.GetItemState(item);
                var stateColor = ViewModel.GetItemStateColor(state);
                
                badge.Background = new SolidColorBrush(stateColor);
                
                // 查找StateText
                var stateText = FindVisualChild<Microsoft.UI.Xaml.Controls.TextBlock>(badge);
                if (stateText != null)
                {
                    stateText.Text = state;
                }
            }
        }
    }

    private static T? FindParent<T>(Microsoft.UI.Xaml.DependencyObject child) where T : Microsoft.UI.Xaml.DependencyObject
    {
        var parent = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(child);
        while (parent != null)
        {
            if (parent is T result)
                return result;
            parent = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(parent);
        }
        return null;
    }

    private void OnCardPointerEntered(object sender, PointerRoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder)
        {
            var hoverOverlay = FindVisualChildByName<Microsoft.UI.Xaml.Shapes.Rectangle>(cardBorder, "HoverOverlay");
            if (hoverOverlay != null)
            {
                hoverOverlay.Opacity = 1.0;
            }
        }
    }

    private void OnCardPointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder)
        {
            var hoverOverlay = FindVisualChildByName<Microsoft.UI.Xaml.Shapes.Rectangle>(cardBorder, "HoverOverlay");
            if (hoverOverlay != null)
            {
                hoverOverlay.Opacity = 0.0;
            }
        }
    }

    private void UpdateCardSelection()
    {
        // 更新所有卡片的选中状态
        // 遍历ItemsRepeater的所有子元素
        if (ContentScrollViewer?.Content is Microsoft.UI.Xaml.Controls.StackPanel stackPanel)
        {
            var itemsRepeater = FindVisualChild<Microsoft.UI.Xaml.Controls.ItemsRepeater>(stackPanel);
            if (itemsRepeater != null)
            {
                var children = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(itemsRepeater);
                for (int i = 0; i < children; i++)
                {
                    var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(itemsRepeater, i);
                    if (child is Microsoft.UI.Xaml.Controls.Border cardBorder)
                    {
                        UpdateCardSelection(cardBorder);
                    }
                }
            }
        }
    }

    private void UpdateCardSelection(Microsoft.UI.Xaml.Controls.Border border)
    {
        if (border.Tag is ItemMetaPayload item)
        {
            var isSelected = ViewModel.IsItemSelected(item.ItemId);
            
            // 查找内容 Grid，用于调整 Padding
            var contentGrid = FindVisualChildByName<Microsoft.UI.Xaml.Controls.Grid>(border, "ContentGrid");
            
            if (isSelected)
            {
                // 选中时：边框加粗（从 1px 变为 2px），颜色改为选中颜色
                border.BorderThickness = new Microsoft.UI.Xaml.Thickness(2);
                
                // 从 Page.Resources 获取边框颜色（资源在 Page.Resources 的 ThemeDictionaries 中）
                // 需要根据当前主题从 ThemeDictionaries 中获取
                var themeDictionaries = this.Resources.ThemeDictionaries;
                var currentTheme = this.ActualTheme == Microsoft.UI.Xaml.ElementTheme.Light ? "Light" : "Dark";
                if (themeDictionaries != null && themeDictionaries.TryGetValue(currentTheme, out var themeDict) && themeDict is Microsoft.UI.Xaml.ResourceDictionary themeResourceDict)
                {
                    if (themeResourceDict.TryGetValue("CardSelectedBorderBrush", out var resource) && resource is Microsoft.UI.Xaml.Media.SolidColorBrush themeBrush)
                    {
                        border.BorderBrush = themeBrush;
                    }
                }
                
                // 减少 Padding 来补偿边框占用的额外空间（边框从 1px 变为 2px，所以 Padding 减少 1px）
                if (contentGrid != null)
                {
                    contentGrid.Padding = new Microsoft.UI.Xaml.Thickness(11, 11, 11, 11);
                }
            }
            else
            {
                // 未选中时：恢复默认边框（1px），颜色使用 CardStrokeColorDefaultBrush
                border.BorderThickness = new Microsoft.UI.Xaml.Thickness(1);
                border.BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Microsoft.UI.Xaml.Application.Current.Resources["CardStrokeColorDefaultBrush"];
                
                if (contentGrid != null)
                {
                    contentGrid.Padding = new Microsoft.UI.Xaml.Thickness(12, 12, 12, 12);
                }
            }
        }
    }

    private static T? FindVisualChild<T>(Microsoft.UI.Xaml.DependencyObject parent) where T : Microsoft.UI.Xaml.DependencyObject
    {
        for (int i = 0; i < Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(parent); i++)
        {
            var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(parent, i);
            if (child is T result)
                return result;
            var childOfChild = FindVisualChild<T>(child);
            if (childOfChild != null)
                return childOfChild;
        }
        return null;
    }

    private void SafeDrawChart(Action drawAction, string chartName)
    {
        try
        {
            drawAction();
        }
        catch (Exception ex)
        {
            ShowError($"绘制{chartName}图表失败: {ex.Message}");
        }
    }

    private void ShowError(string message)
    {
        try
        {
            // 如果已经有错误栏，更新它；否则创建新的
            if (_errorInfoBar == null)
            {
                _errorInfoBar = new Microsoft.UI.Xaml.Controls.InfoBar
                {
                    IsOpen = true,
                    Severity = Microsoft.UI.Xaml.Controls.InfoBarSeverity.Error,
                    Title = "主页错误",
                    Message = message,
                    HorizontalAlignment = Microsoft.UI.Xaml.HorizontalAlignment.Stretch,
                    Margin = new Microsoft.UI.Xaml.Thickness(0, 0, 0, 12)
                };
                
                // 添加到ContentStackPanel的顶部
                if (ContentStackPanel != null)
                {
                    ContentStackPanel.Children.Insert(0, _errorInfoBar);
                }
            }
            else
            {
                _errorInfoBar.Message = message;
                _errorInfoBar.IsOpen = true;
            }
        }
        catch (Exception ex)
        {
            // Error display failed, ignore
        }
    }

    private void DrawCacheChart()
    {
        try
        {
            CacheChartCanvas.Children.Clear();
            
            var padding = 40.0;
            var width = CacheChartCanvas.ActualWidth > 0 ? CacheChartCanvas.ActualWidth : 600;
            var height = CacheChartCanvas.ActualHeight > 0 ? CacheChartCanvas.ActualHeight : 200;
            if (width <= padding * 2 || height <= padding * 2)
                return;

            if (ViewModel.CacheSeries.Count == 0)
            {
                // 显示"暂无数据"提示
                var textBlock = new Microsoft.UI.Xaml.Controls.TextBlock
                {
                    Text = "暂无数据",
                    FontSize = 14,
                    Foreground = new SolidColorBrush(Colors.Gray),
                    TextAlignment = Microsoft.UI.Xaml.TextAlignment.Center
                };
                Canvas.SetLeft(textBlock, (width - 60) / 2);
                Canvas.SetTop(textBlock, (height - 20) / 2);
                CacheChartCanvas.Children.Add(textBlock);
                return;
            }

            var chartWidth = width - padding * 2;
            var chartHeight = height - padding * 2;

            var maxValue = ViewModel.CacheSeries.Any() ? ViewModel.CacheSeries.Max(p => p.CacheBytes) : 1;
            var minValue = ViewModel.CacheSeries.Any() ? ViewModel.CacheSeries.Min(p => p.CacheBytes) : 0;
            var valueRange = maxValue - minValue;
            if (valueRange == 0)
                valueRange = 1;

            var points = new PointCollection();
            var count = ViewModel.CacheSeries.Count;
            for (int i = 0; i < count; i++)
            {
                var point = ViewModel.CacheSeries[i];
                var x = padding + (count > 1 ? (i / (double)(count - 1)) : 0.5) * chartWidth;
                var y = padding + chartHeight - ((point.CacheBytes - minValue) / valueRange) * chartHeight;
                points.Add(new Point(x, y));
            }

            if (points.Count > 0)
            {
                var polyline = new Polyline
                {
                    Points = points,
                    Stroke = new SolidColorBrush(Colors.Blue),
                    StrokeThickness = 2
                };
                CacheChartCanvas.Children.Add(polyline);
            }
        } catch (Exception ex)
        {
            throw;
        }
    }

    private void DrawNetworkChart()
    {
        try
        {
            NetworkChartCanvas.Children.Clear();
            
            var padding = 40.0;
            var width = NetworkChartCanvas.ActualWidth > 0 ? NetworkChartCanvas.ActualWidth : 600;
            var height = NetworkChartCanvas.ActualHeight > 0 ? NetworkChartCanvas.ActualHeight : 200;
            if (width <= padding * 2 || height <= padding * 2)
                return;

            if (ViewModel.NetworkSeries.Count == 0)
            {
                // 显示"暂无数据"提示
                var textBlock = new Microsoft.UI.Xaml.Controls.TextBlock
                {
                    Text = "暂无数据",
                    FontSize = 14,
                    Foreground = new SolidColorBrush(Colors.Gray),
                    TextAlignment = Microsoft.UI.Xaml.TextAlignment.Center
                };
                Canvas.SetLeft(textBlock, (width - 60) / 2);
                Canvas.SetTop(textBlock, (height - 20) / 2);
                NetworkChartCanvas.Children.Add(textBlock);
                return;
            }

            var chartWidth = width - padding * 2;
            var chartHeight = height - padding * 2;

            var maxValue = ViewModel.NetworkSeries.Any()
                ? ViewModel.NetworkSeries.Max(p => Math.Max(p.BytesSent, p.BytesRecv))
                : 1;
            var minValue = 0.0;
            var valueRange = maxValue - minValue;
            if (valueRange == 0)
                valueRange = 1;

            var count = ViewModel.NetworkSeries.Count;

            // Sent line
            var sentPoints = new PointCollection();
            for (int i = 0; i < count; i++)
            {
                var point = ViewModel.NetworkSeries[i];
                var x = padding + (count > 1 ? (i / (double)(count - 1)) : 0.5) * chartWidth;
                var y = padding + chartHeight - ((point.BytesSent - minValue) / valueRange) * chartHeight;
                sentPoints.Add(new Point(x, y));
            }

            if (sentPoints.Count > 0)
            {
                var sentLine = new Polyline
                {
                    Points = sentPoints,
                    Stroke = new SolidColorBrush(Colors.Green),
                    StrokeThickness = 2
                };
                NetworkChartCanvas.Children.Add(sentLine);
            }

            // Received line
            var recvPoints = new PointCollection();
            for (int i = 0; i < count; i++)
            {
                var point = ViewModel.NetworkSeries[i];
                var x = padding + (count > 1 ? (i / (double)(count - 1)) : 0.5) * chartWidth;
                var y = padding + chartHeight - ((point.BytesRecv - minValue) / valueRange) * chartHeight;
                recvPoints.Add(new Point(x, y));
            }

            if (recvPoints.Count > 0)
            {
                var recvLine = new Polyline
                {
                    Points = recvPoints,
                    Stroke = new SolidColorBrush(Colors.Orange),
                    StrokeThickness = 2
                };
                NetworkChartCanvas.Children.Add(recvLine);
            }
        } catch (Exception ex)
        {
            throw;
        }
    }

    private void DrawActivityChart()
    {
        try
        {
            ActivityChartCanvas.Children.Clear();
            
            var padding = 40.0;
            var width = ActivityChartCanvas.ActualWidth > 0 ? ActivityChartCanvas.ActualWidth : 600;
            var height = ActivityChartCanvas.ActualHeight > 0 ? ActivityChartCanvas.ActualHeight : 200;
            if (width <= padding * 2 || height <= padding * 2)
                return;

            if (ViewModel.ActivitySeries.Count == 0)
            {
                // 显示"暂无数据"提示
                var textBlock = new Microsoft.UI.Xaml.Controls.TextBlock
                {
                    Text = "暂无数据",
                    FontSize = 14,
                    Foreground = new SolidColorBrush(Colors.Gray),
                    TextAlignment = Microsoft.UI.Xaml.TextAlignment.Center
                };
                Canvas.SetLeft(textBlock, (width - 60) / 2);
                Canvas.SetTop(textBlock, (height - 20) / 2);
                ActivityChartCanvas.Children.Add(textBlock);
                return;
            }

            var chartWidth = width - padding * 2;
            var chartHeight = height - padding * 2;

            var maxValue = ViewModel.ActivitySeries.Any()
                ? Math.Max(
                    ViewModel.ActivitySeries.Max(p => p.TextCount),
                    Math.Max(
                        ViewModel.ActivitySeries.Max(p => p.ImageCount),
                        ViewModel.ActivitySeries.Max(p => p.FilesCount)
                    )
                )
                : 1;
            var minValue = 0.0;
            var valueRange = maxValue - minValue;
            if (valueRange == 0)
                valueRange = 1;

            var count = ViewModel.ActivitySeries.Count;

            // Text line
            var textPoints = new PointCollection();
            for (int i = 0; i < count; i++)
            {
                var point = ViewModel.ActivitySeries[i];
                var x = padding + (count > 1 ? (i / (double)(count - 1)) : 0.5) * chartWidth;
                var y = padding + chartHeight - ((point.TextCount - minValue) / valueRange) * chartHeight;
                textPoints.Add(new Point(x, y));
            }

            if (textPoints.Count > 0)
            {
                var textLine = new Polyline
                {
                    Points = textPoints,
                    Stroke = new SolidColorBrush(Colors.Blue),
                    StrokeThickness = 2
                };
                ActivityChartCanvas.Children.Add(textLine);
            }

            // Image line
            var imagePoints = new PointCollection();
            for (int i = 0; i < count; i++)
            {
                var point = ViewModel.ActivitySeries[i];
                var x = padding + (count > 1 ? (i / (double)(count - 1)) : 0.5) * chartWidth;
                var y = padding + chartHeight - ((point.ImageCount - minValue) / valueRange) * chartHeight;
                imagePoints.Add(new Point(x, y));
            }

            if (imagePoints.Count > 0)
            {
                var imageLine = new Polyline
                {
                    Points = imagePoints,
                    Stroke = new SolidColorBrush(Colors.Purple),
                    StrokeThickness = 2
                };
                ActivityChartCanvas.Children.Add(imageLine);
            }

            // Files line
            var filesPoints = new PointCollection();
            for (int i = 0; i < count; i++)
            {
                var point = ViewModel.ActivitySeries[i];
                var x = padding + (count > 1 ? (i / (double)(count - 1)) : 0.5) * chartWidth;
                var y = padding + chartHeight - ((point.FilesCount - minValue) / valueRange) * chartHeight;
                filesPoints.Add(new Point(x, y));
            }

            if (filesPoints.Count > 0)
            {
                var filesLine = new Polyline
                {
                    Points = filesPoints,
                    Stroke = new SolidColorBrush(Colors.Red),
                    StrokeThickness = 2
                };
                ActivityChartCanvas.Children.Add(filesLine);
            }
        } catch (Exception ex)
        {
            throw;
        }
    }
}
