using System;
using System.Linq;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;

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
        }
        catch (Exception ex)
        {
            ShowError($"加载主页内容失败: {ex.Message}");
        }
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
