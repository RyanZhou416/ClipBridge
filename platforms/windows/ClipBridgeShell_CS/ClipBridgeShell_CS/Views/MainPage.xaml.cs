using System;
using System.Linq;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.ViewModels;
using ClipBridgeShell_CS.Helpers;
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
using Microsoft.UI.Xaml;
using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Text;
using Microsoft.Graphics.Canvas.UI.Xaml;
using System.Numerics;
using Windows.UI.Text;
using CommunityToolkit.WinUI;

namespace ClipBridgeShell_CS.Views;

public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel
    {
        get;
    }

    private Microsoft.UI.Xaml.Controls.InfoBar? _errorInfoBar;
    
    // Win2D Canvas 相关（仅用于标题绘制）
    private CanvasTextLayout? _titleTextLayout;
    private Color _titleTextColor = Colors.White; // 默认白色，确保在深色背景上可见
    private string _titleText = string.Empty;

    public MainPage()
    {
        try
        {
            ViewModel = App.GetService<MainViewModel>();
            InitializeComponent();
            
            // 监听数据变化，更新图表（在Loaded之后）
            Loaded += OnPageLoaded;
            Unloaded += OnPageUnloaded;
        }
        catch (Exception ex)
        {
            ShowError($"初始化主页失败: {ex.Message}");
        }
    }

    private DispatcherTimer? _chartUpdateTimer;
    private bool _cacheChartDirty = false;
    private bool _networkChartDirty = false;
    private bool _activityChartDirty = false;

    private void OnPageLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        try
        {
            // 设置 BorderBrushConverter 的 Page 引用
            CardSelectionBorderBrushConverter.SetPageReference(this);
            
            // 使用节流机制优化图表绘制性能
            _chartUpdateTimer = new DispatcherTimer
            {
                Interval = TimeSpan.FromMilliseconds(100) // 100ms 节流，避免频繁重绘
            };
            _chartUpdateTimer.Tick += (s, args) =>
            {
                if (_cacheChartDirty) { SafeDrawChart(DrawCacheChart, "Cache"); _cacheChartDirty = false; }
                if (_networkChartDirty) { SafeDrawChart(DrawNetworkChart, "Network"); _networkChartDirty = false; }
                if (_activityChartDirty) { SafeDrawChart(DrawActivityChart, "Activity"); _activityChartDirty = false; }
            };
            _chartUpdateTimer.Start();
            
            // 监听数据变化，标记为脏（而不是立即绘制）
            ViewModel.CacheSeries.CollectionChanged += (s, args) => _cacheChartDirty = true;
            ViewModel.NetworkSeries.CollectionChanged += (s, args) => _networkChartDirty = true;
            ViewModel.ActivitySeries.CollectionChanged += (s, args) => _activityChartDirty = true;
            
            // 初始绘制
            SafeDrawChart(DrawCacheChart, "Cache");
            SafeDrawChart(DrawNetworkChart, "Network");
            SafeDrawChart(DrawActivityChart, "Activity");

            // 监听Canvas大小变化（也需要节流，但可以立即更新）
            CacheChartCanvas.SizeChanged += (s, args) => _cacheChartDirty = true;
            NetworkChartCanvas.SizeChanged += (s, args) => _networkChartDirty = true;
            ActivityChartCanvas.SizeChanged += (s, args) => _activityChartDirty = true;

            // 监听RecentItems和选中状态变化
            ViewModel.RecentItems.CollectionChanged += (s, e) => 
            {
                // 当RecentItems变化时，可能需要更新UI
                // 由于ItemsRepeater会自动更新，这里可以留空或添加其他逻辑
            };
            // 注意：已移除 PropertyChanged 中对 UpdateCardSelection() 的调用
            // 现在使用数据绑定自动更新卡片选中状态
            
            // 设置视差滚动效果（使用 RenderTransform，不影响布局）
            if (ContentScrollViewer != null && ParallaxContentContainer != null)
            {
                ContentScrollViewer.ViewChanged += OnScrollViewChanged;
            }
            
            // 初始化 Win2D Canvas 标题绘制
            InitializeCanvasTitle();

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

            // 加载用户自定义背景图片
            _ = LoadBackgroundImageAsync();

            // 监听设置变化，当背景图片改变时重新加载
            var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
            settingsService.SettingChanged += OnSettingChanged;

            // 监听主题变化，确保标题颜色不被主题覆盖（Canvas 不受主题影响，但需要重新绘制）
            this.ActualThemeChanged += OnPageThemeChanged;
        }
        catch (Exception ex)
        {
            ShowError($"加载主页内容失败: {ex.Message}");
        }
    }

    private void OnPageUnloaded(object sender, RoutedEventArgs e)
    {
        try
        {
            // 停止定时器
            _chartUpdateTimer?.Stop();
            
            // 清理标题文本布局
            _titleTextLayout?.Dispose();
            _titleTextLayout = null;
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"页面卸载清理失败: {ex.Message}");
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
            
            Windows.Storage.StorageFile? imageFile = null;
            
            // 获取图片源
            var imageSource = HeroImage.Source;
            if (imageSource is Microsoft.UI.Xaml.Media.Imaging.BitmapImage bitmapImage)
            {
                // 尝试从 Uri 读取（默认图片）
                var uri = bitmapImage.UriSource;
                if (uri != null && uri.Scheme == "ms-appx")
                {
                    try
                    {
                        imageFile = await Windows.Storage.StorageFile.GetFileFromApplicationUriAsync(uri);
                    }
                    catch
                    {
                        // Uri 方式失败，尝试从路径读取（自定义图片）
                    }
                }
            }
            
            // 如果 Uri 方式失败，尝试从设置中读取自定义图片路径
            if (imageFile == null)
            {
                var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
                var customImagePath = await settingsService.ReadSettingAsync<string?>("MainPage_BackgroundImagePath");
                if (!string.IsNullOrEmpty(customImagePath))
                {
                    try
                    {
                        imageFile = await Windows.Storage.StorageFile.GetFileFromPathAsync(customImagePath);
                    }
                    catch
                    {
                        // 读取失败
                    }
                }
            }
            
            if (imageFile != null)
            {
                await DetectImageColorAndUpdateTitle(imageFile);
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
                    
                    // 更新标题颜色（使用辅助方法，确保不受主题影响）
                    UpdateTitleColor(textColor);
                }
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"检测图片颜色失败: {ex.Message}");
        }
    }
    
    private const double ParallaxSpeed = 1.1;


    // 加载背景图片（用户自定义或默认）
    private async Task LoadBackgroundImageAsync()
    {
        try
        {
            if (HeroImage == null)
                return;

            var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
            var customImagePath = await settingsService.ReadSettingAsync<string?>("MainPage_BackgroundImagePath");

            Windows.Storage.StorageFile? imageFile = null;

            if (!string.IsNullOrEmpty(customImagePath))
            {
                try
                {
                    // 检查文件是否存在
                    imageFile = await Windows.Storage.StorageFile.GetFileFromPathAsync(customImagePath);
                    if (imageFile != null)
                    {
                        var bitmapImage = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage();
                        await bitmapImage.SetSourceAsync(await imageFile.OpenAsync(Windows.Storage.FileAccessMode.Read));
                        HeroImage.Source = bitmapImage;
                        
                        // 加载自定义图片后，检测颜色并更新标题
                        await DetectImageColorAndUpdateTitle(imageFile);
                        return;
                    }
                }
                catch
                {
                    // 如果文件不存在或读取失败，使用默认图片
                    System.Diagnostics.Debug.WriteLine($"无法加载自定义背景图片: {customImagePath}");
                }
            }

            // 使用默认图片
            var defaultUri = new Uri("ms-appx:///Assets/background.jpg");
            HeroImage.Source = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(defaultUri);
            
            // 加载默认图片后，检测颜色并更新标题
            try
            {
                imageFile = await Windows.Storage.StorageFile.GetFileFromApplicationUriAsync(defaultUri);
                if (imageFile != null)
                {
                    await DetectImageColorAndUpdateTitle(imageFile);
                }
            }
            catch
            {
                // 如果检测失败，使用默认颜色
                System.Diagnostics.Debug.WriteLine("无法检测默认背景图片颜色");
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"加载背景图片失败: {ex.Message}");
            // 失败时使用默认图片
            if (HeroImage != null)
            {
                HeroImage.Source = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri("ms-appx:///Assets/background.jpg"));
            }
        }
    }

    private async void OnSettingChanged(object? sender, string key)
    {
        // 当背景图片设置改变时，重新加载
        if (key == "MainPage_BackgroundImagePath")
        {
            await LoadBackgroundImageAsync();
        }
    }


    // 主题变化时，重新检测背景颜色并更新标题颜色
    // Canvas 绘制不受主题影响，但需要重新检测背景颜色
    private async void OnPageThemeChanged(FrameworkElement sender, object args)
    {
        // 主题变化时，重新检测背景颜色并更新标题
        // 延迟一下，确保主题切换完成后再更新
        await Task.Delay(50);
        await DetectBackgroundColorAndUpdateTitle();
    }

    // 初始化 Win2D Canvas 标题绘制
    private void InitializeCanvasTitle()
    {
        try
        {
            if (TitleCanvas == null || TitleTextBlock == null)
                return;

            // 等待 Canvas 创建资源
            TitleCanvas.CreateResources += (s, e) =>
            {
                // Canvas 资源创建后，更新文本
                UpdateCanvasTitleText();
            };

            // 等待 TextBlock 加载完成，获取文本内容
            TitleTextBlock.Loaded += (s, e) =>
            {
                // 延迟一下，确保 Canvas 设备已就绪
                var dispatcherQueue = DispatcherQueue.GetForCurrentThread();
                dispatcherQueue?.TryEnqueue(DispatcherQueuePriority.Normal, () =>
                {
                    UpdateCanvasTitleText();
                });
            };

            // 如果已经加载，延迟更新
            if (TitleTextBlock.IsLoaded)
            {
                var dispatcherQueue = DispatcherQueue.GetForCurrentThread();
                dispatcherQueue?.TryEnqueue(DispatcherQueuePriority.Normal, () =>
                {
                    UpdateCanvasTitleText();
                });
            }

            // 监听文本变化
            TitleTextBlock.RegisterPropertyChangedCallback(TextBlock.TextProperty, (s, dp) =>
            {
                UpdateCanvasTitleText();
            });
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"初始化 Canvas 标题失败: {ex.Message}");
        }
    }

    // 更新 Canvas 标题文本
    private void UpdateCanvasTitleText()
    {
        try
        {
            if (TitleCanvas == null || TitleTextBlock == null)
                return;

            // 等待 Canvas 设备就绪
            if (TitleCanvas.Device == null)
            {
                // 如果设备未就绪，延迟重试
                TitleCanvas.CreateResources += (s, e) =>
                {
                    UpdateCanvasTitleText();
                };
                return;
            }

            _titleText = TitleTextBlock.Text ?? string.Empty;
            if (string.IsNullOrEmpty(_titleText))
            {
                // 如果文本为空，尝试使用默认文本
                _titleText = "ClipBridge";
            }

            // 释放旧的布局
            _titleTextLayout?.Dispose();

            // 创建文本格式
            var textFormat = new CanvasTextFormat
            {
                FontSize = 36, // 增大字体
                FontWeight = new Windows.UI.Text.FontWeight { Weight = 300 }, // Light = 300，更细
                WordWrapping = CanvasWordWrapping.NoWrap,
                FontFamily = "Segoe UI"
            };

            // 创建文本布局（用于测量和绘制）
            _titleTextLayout = new CanvasTextLayout(
                TitleCanvas.Device,
                _titleText,
                textFormat,
                float.MaxValue,
                float.MaxValue
            );

            // 更新 Canvas 大小（增加高度以容纳更大的字体）
            TitleCanvas.Width = Math.Max((float)_titleTextLayout.LayoutBounds.Width + 20, 200);
            TitleCanvas.Height = Math.Max((float)_titleTextLayout.LayoutBounds.Height + 10, 60);

            // 触发重绘
            TitleCanvas.Invalidate();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"更新 Canvas 标题文本失败: {ex.Message}");
        }
    }

    // Canvas 绘制事件
    private void OnTitleCanvasDraw(CanvasControl sender, CanvasDrawEventArgs args)
    {
        try
        {
            // 如果文本布局未创建，尝试创建
            if (_titleTextLayout == null)
            {
                UpdateCanvasTitleText();
                // 如果仍然为空，直接绘制文本
                if (_titleTextLayout == null && !string.IsNullOrEmpty(_titleText))
                {
                    var textFormat = new CanvasTextFormat
                    {
                        FontSize = 36, // 增大字体
                        FontWeight = new Windows.UI.Text.FontWeight { Weight = 300 }, // Light = 300，更细
                        WordWrapping = CanvasWordWrapping.NoWrap,
                        FontFamily = "Segoe UI"
                    };
                    args.DrawingSession.DrawText(
                        _titleText,
                        new Vector2(0, 0),
                        _titleTextColor,
                        textFormat
                    );
                    return;
                }
            }

            if (_titleTextLayout == null || string.IsNullOrEmpty(_titleText))
                return;

            // 绘制文本，使用固定颜色（完全不受主题影响）
            args.DrawingSession.DrawTextLayout(
                _titleTextLayout,
                new Vector2(0, 0),
                _titleTextColor
            );
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"Canvas 绘制失败: {ex.Message}");
            // 如果绘制失败，尝试直接绘制文本
            if (!string.IsNullOrEmpty(_titleText))
            {
                try
                {
                    var textFormat = new CanvasTextFormat
                    {
                        FontSize = 36, // 增大字体
                        FontWeight = new Windows.UI.Text.FontWeight { Weight = 300 }, // Light = 300，更细
                        WordWrapping = CanvasWordWrapping.NoWrap,
                        FontFamily = "Segoe UI"
                    };
                    args.DrawingSession.DrawText(
                        _titleText,
                        new Vector2(0, 0),
                        _titleTextColor,
                        textFormat
                    );
                }
                catch { }
            }
        }
    }

    // 更新标题颜色的辅助方法，使用 Win2D Canvas 确保颜色固定，完全不受主题影响
    private void UpdateTitleColor(Color textColor)
    {
        _titleTextColor = textColor;

        if (TitleCanvas == null)
            return;

        var dispatcherQueue = DispatcherQueue.GetForCurrentThread();
        
        // 使用 DispatcherQueueHandler 委托类型
        DispatcherQueueHandler updateHandler = () =>
        {
            // Canvas 绘制完全由代码控制，不受主题影响
            // 只需要触发重绘即可
            TitleCanvas?.Invalidate();
        };

        if (dispatcherQueue != null && !dispatcherQueue.HasThreadAccess)
        {
            dispatcherQueue.TryEnqueue(DispatcherQueuePriority.High, updateHandler);
        }
        else
        {
            // 如果已经在 UI 线程，直接执行
            updateHandler();
        }
    }

    private void OnScrollViewChanged(object? sender, Microsoft.UI.Xaml.Controls.ScrollViewerViewChangedEventArgs e)
    {
        if (ContentScrollViewer == null || ParallaxContentContainer == null)
            return;

        var scrollOffset = ContentScrollViewer.VerticalOffset;
        var parallaxOffset = scrollOffset * ParallaxSpeed;

        // 直接更新 Transform（RenderTransform 不影响布局，性能足够好）
        if (ParallaxTransform != null)
        {
            ParallaxTransform.Y = -parallaxOffset;
        }
    }

    private void OnCardTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder && cardBorder.Tag is ItemMetaPayload item)
        {
            ViewModel.SelectItemCommand.Execute(item);
            // 注意：不再需要手动调用 UpdateCardSelection，数据绑定会自动更新
        }
    }

    private void OnCardLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder && cardBorder.Tag is ItemMetaPayload item)
        {
            // 注意：不再需要调用 UpdateCardSelection，数据绑定会自动更新
            
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

    // 注意：UpdateCardSelection() 无参数方法已移除
    // 现在使用数据绑定自动更新卡片选中状态，性能从 O(n) 优化到 O(1)
    // 保留此方法作为后备（如果绑定失败时使用）
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
            // 优化：临时禁用缓存，更新内容，然后重新启用
            if (CacheChartCanvas.CacheMode != null)
            {
                CacheChartCanvas.CacheMode = null;
            }
            
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

            // 优化：只遍历一次集合来计算最大值和最小值
            var count = ViewModel.CacheSeries.Count;
            if (count == 0)
            {
                return;
            }
            
            double maxValue = ViewModel.CacheSeries[0].CacheBytes;
            double minValue = ViewModel.CacheSeries[0].CacheBytes;
            for (int i = 1; i < count; i++)
            {
                var value = ViewModel.CacheSeries[i].CacheBytes;
                if (value > maxValue) maxValue = value;
                if (value < minValue) minValue = value;
            }
            
            var valueRange = maxValue - minValue;
            if (valueRange == 0)
                valueRange = 1;

            var points = new PointCollection();
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
            
            // 重新启用缓存
            CacheChartCanvas.CacheMode = new Microsoft.UI.Xaml.Media.BitmapCache();
        } catch (Exception ex)
        {
            throw;
        }
    }

    private void DrawNetworkChart()
    {
        try
        {
            // 优化：临时禁用缓存，更新内容，然后重新启用
            if (NetworkChartCanvas.CacheMode != null)
            {
                NetworkChartCanvas.CacheMode = null;
            }
            
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

            // 优化：只遍历一次集合来计算最大值
            var count = ViewModel.NetworkSeries.Count;
            if (count == 0)
            {
                return;
            }
            
            double maxValue = Math.Max(ViewModel.NetworkSeries[0].BytesSent, ViewModel.NetworkSeries[0].BytesRecv);
            for (int i = 1; i < count; i++)
            {
                var point = ViewModel.NetworkSeries[i];
                var pointMax = Math.Max(point.BytesSent, point.BytesRecv);
                if (pointMax > maxValue) maxValue = pointMax;
            }
            
            var minValue = 0.0;
            var valueRange = maxValue - minValue;
            if (valueRange == 0)
                valueRange = 1;

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
            
            // 重新启用缓存
            NetworkChartCanvas.CacheMode = new Microsoft.UI.Xaml.Media.BitmapCache();
        } catch (Exception ex)
        {
            throw;
        }
    }

    private void DrawActivityChart()
    {
        try
        {
            // 优化：临时禁用缓存，更新内容，然后重新启用
            if (ActivityChartCanvas.CacheMode != null)
            {
                ActivityChartCanvas.CacheMode = null;
            }
            
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

            // 优化：只遍历一次集合来计算最大值
            var count = ViewModel.ActivitySeries.Count;
            if (count == 0)
            {
                return;
            }
            
            double maxValue = Math.Max(
                Math.Max(ViewModel.ActivitySeries[0].TextCount, ViewModel.ActivitySeries[0].ImageCount),
                ViewModel.ActivitySeries[0].FilesCount
            );
            for (int i = 1; i < count; i++)
            {
                var point = ViewModel.ActivitySeries[i];
                var pointMax = Math.Max(Math.Max(point.TextCount, point.ImageCount), point.FilesCount);
                if (pointMax > maxValue) maxValue = pointMax;
            }
            
            var minValue = 0.0;
            var valueRange = maxValue - minValue;
            if (valueRange == 0)
                valueRange = 1;

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
            
            // 重新启用缓存
            ActivityChartCanvas.CacheMode = new Microsoft.UI.Xaml.Media.BitmapCache();
        } catch (Exception ex)
        {
            throw;
        }
    }
}
