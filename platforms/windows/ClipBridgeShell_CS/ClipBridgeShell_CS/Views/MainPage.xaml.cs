using System;
using System.Collections.Specialized;
using System.Linq;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.ViewModels;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Views;
using WinUI3Localizer;
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
using CommunityToolkit.WinUI;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml.Hosting;

namespace ClipBridgeShell_CS.Views;

public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel
    {
        get;
    } = null!;

    private Microsoft.UI.Xaml.Controls.InfoBar? _errorInfoBar;
    
    // 卡片选中状态管理（性能优化：只更新变化的卡片，而不是遍历所有卡片）
    private Microsoft.UI.Xaml.Controls.Border? _selectedBorder;

    private DispatcherTimer? _relativeTimeTimer;
    private readonly RelativeOrExactTimestampConverter _relativeTimeConverter = new();

    // 卡片右键菜单：打开时暂存当前项，菜单项点击时使用
    private ItemMetaPayload? _contextMenuCardItem;
    private Microsoft.UI.Xaml.Controls.Border? _contextMenuCardBorder;
    private Microsoft.UI.Xaml.Controls.MenuFlyout? _cardContextMenu;

    public MainPage()
    {
        StartupTrace.Mark("mainpage-ctor-enter");
        try
        {
            ViewModel = App.GetService<MainViewModel>();
            StartupTrace.Mark("mainpage-vm-resolved");
            InitializeComponent();
            StartupTrace.Mark("mainpage-xaml-inflated");
            InitCardContextMenu();
            StartupTrace.Mark("mainpage-ctor-exit");
            // 监听数据变化，更新图表（在Loaded之后）
            Loaded += OnPageLoaded;
            Unloaded += OnPageUnloaded;
        }
        catch (Exception ex)
        {
            ShowError($"初始化主页失败: {ex.Message}");
        }
    }

    private void InitCardContextMenu()
    {
        _cardContextMenu = new Microsoft.UI.Xaml.Controls.MenuFlyout();
        var loc = WinUI3Localizer.Localizer.Get();
        var deleteLocal = loc.GetLocalizedString("Main_ContextMenu_DeleteLocal");
        if (string.IsNullOrEmpty(deleteLocal) || deleteLocal == "Main_ContextMenu_DeleteLocal") deleteLocal = "Local delete";
        var deleteGlobal = loc.GetLocalizedString("Main_ContextMenu_GlobalDelete");
        if (string.IsNullOrEmpty(deleteGlobal) || deleteGlobal == "Main_ContextMenu_GlobalDelete") deleteGlobal = "Global delete";

        var itemDelete = new Microsoft.UI.Xaml.Controls.MenuFlyoutItem { Text = deleteLocal };
        itemDelete.Click += OnCardContextMenuDeleteClick;
        _cardContextMenu.Items.Add(itemDelete);

        var itemGlobalDelete = new Microsoft.UI.Xaml.Controls.MenuFlyoutItem { Text = deleteGlobal };
        itemGlobalDelete.Click += OnCardContextMenuGlobalDeleteClick;
        _cardContextMenu.Items.Add(itemGlobalDelete);
    }

    private void OnCardContextRequested(object sender, Microsoft.UI.Xaml.Input.ContextRequestedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border border && border.Tag is ItemMetaPayload item)
        {
            _contextMenuCardItem = item;
            _contextMenuCardBorder = border;
            _cardContextMenu?.ShowAt(border);
            e.Handled = true;
        }
    }

    // 图表重绘标记：数据变化时合并为一次低优先级重绘，不再定时轮询
    private bool _cacheChartDirty = false;
    private bool _networkChartDirty = false;
    private bool _activityChartDirty = false;
    private bool _redrawQueued = false;

    // 订阅状态：Transient 页面会多次 Loaded/Unloaded，必须成对订阅与解除订阅
    private bool _subscribed = false;

    // 启动优化：背景图加载 / 标题颜色检测 / 图表首绘都推迟到首帧之后
    private const string BackgroundImagePathSettingKey = "MainPage_BackgroundImagePath";
    private const string TitleColorCacheSettingKey = "MainPage_TitleColorCache";
    // 3840x2160 的原始背景图按 1600 宽解码（约 1/5 像素量）；hero 区约 600 高、通常 1200~1600 宽，
    // 1600 可避免 UniformToFill 放大后出现明显糊化。
    private const int BackgroundDecodePixelWidth = 1600;

    /// <summary>
    /// 视差速度：卡片区相对页面滚动多移动的比例。
    /// 1.1 表示页面滚 100px 时卡片区反向偏移 110px，形成"背景更慢、前景更快"的层感。
    /// 历史上曾被性能提交移除（担心 ViewChanged 高频回调），现按原值恢复；
    /// RenderTransform 不参与布局，单次仅一次 2D 平移。
    /// </summary>
    private const double ParallaxSpeed = 1.1;

    // 首绘是否还挂在合并重绘队列里（用于只打一次 mainpage-charts-deferred 标记）
    private bool _initialChartDrawPending;

    // 背景图源版本：换图时自增，用于丢弃上一张图迟到的颜色检测结果
    private int _backgroundImageGeneration;

    // 当前通过 SetSourceAsync 设置的自定义图片路径（BitmapImage.UriSource 在流模式下为空）
    private string? _lastBackgroundImagePath;

    // 标题颜色检测去重：同一张图（路径+最后写入时间）每次启动最多解码一次
    private string? _titleColorPendingKey;
    private string? _titleColorResolvedKey;
    private Color? _titleColorResolvedColor;

    private void OnPageLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        StartupTrace.Mark("mainpage-loaded");
        try
        {
            // 设置 BorderBrushConverter 的 Page 引用
            CardSelectionBorderBrushConverter.SetPageReference(this);

            if (!_subscribed)
            {
                _subscribed = true;

                ViewModel.PropertyChanged += OnViewModelPropertyChanged;

                // 图表数据变化时只标记并排队一次重绘（合并突发更新，避免重建可视树）
                ViewModel.CacheSeries.CollectionChanged += OnCacheSeriesChanged;
                ViewModel.NetworkSeries.CollectionChanged += OnNetworkSeriesChanged;
                ViewModel.ActivitySeries.CollectionChanged += OnActivitySeriesChanged;

                ViewModel.RecentItems.CollectionChanged += OnRecentItemsChanged;

                // 视差滚动：用 RenderTransform 驱动（不影响布局），在 OnPageUnloaded 里配对退订。
                // 注意：这里只订阅一次；重复进页面时先退订再订阅，避免重复回调。
                if (ContentScrollViewer != null && ParallaxContentContainer != null)
                {
                    ContentScrollViewer.ViewChanged -= OnScrollViewChanged;
                    ContentScrollViewer.ViewChanged += OnScrollViewChanged;
                }

                // 监听Canvas大小变化（重绘交由合并队列处理）
                CacheChartCanvas.SizeChanged += OnChartCanvasSizeChanged;
                NetworkChartCanvas.SizeChanged += OnChartCanvasSizeChanged;
                ActivityChartCanvas.SizeChanged += OnChartCanvasSizeChanged;

                // 背景图片颜色检测：首帧之后由 ImageOpened / 延迟加载触发。
                if (HeroImage != null)
                {
                    HeroImage.ImageOpened += OnHeroImageOpened;
                }

                // 监听设置变化，当背景图片改变时重新加载
                var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
                settingsService.SettingChanged += OnSettingChanged;

                // 监听主题变化，确保标题颜色不被主题覆盖
                this.ActualThemeChanged += OnPageThemeChanged;
            }

            StartRelativeTimeRefresh();

            // 运行时帧时序诊断（默认完全关闭）：把“逐帧滚动一步”的驱动交给 FrameTrace，
            // 并告知主页面已就绪（idle 场景会等它就绪后再开始）。未设置环境变量时两个调用立即返回。
            FrameTrace.RegisterScrollDriver(FrameTraceScrollTick);
            FrameTrace.NotifyMainPageLoaded();

            StartupTrace.Mark("ab-mode-loaded-deferred");
            // 首帧之后再绘制图表、加载/检测背景图，避免和第一帧抢 UI 线程
            DeferStartupWorkUntilAfterFirstFrame();
        }
        catch (Exception ex)
        {
            ShowError($"加载主页内容失败: {ex.Message}");
        }
    }

    // 帧时序诊断的 scroll 场景驱动状态（仅 CB_FRAME_TRACE=1 时使用）：
    // 优先横向滚动历史卡片条（ItemsRepeater 所在 ScrollViewer），它没有可滚动范围时退回整页纵向滚动。
    private ScrollViewer? _frameHistoryScroller;
    private double _frameScrollDirection = 1.0;
    private bool _frameScrollTargetNoted;

    /// <summary>
    /// scroll 场景：每个渲染节拍滚动一步（两个方向往复）。由 FrameTrace 在 scroll 阶段逐帧调用；
    /// 未开启诊断时本方法不会被注册，也不会被调用。
    /// </summary>
    private void FrameTraceScrollTick()
    {
        // 历史卡片条的 ScrollViewer 在 HorizontalScrollContainer 内部，首次调用时从可视树里取。
        var historyScroller = _frameHistoryScroller;
        if (historyScroller == null)
        {
            _frameHistoryScroller = historyScroller = FindVisualChild<ScrollViewer>(RecentlyVisitedContainer);
        }

        if (historyScroller != null && historyScroller.ScrollableWidth > 1)
        {
            if (!_frameScrollTargetNoted)
            {
                _frameScrollTargetNoted = true;
                FrameTrace.NoteScrollTarget("history-strip");
            }

            var horizontal = historyScroller.HorizontalOffset + (18.0 * _frameScrollDirection);
            if (horizontal <= 0)
            {
                horizontal = 0;
                _frameScrollDirection = 1;
            }
            else if (horizontal >= historyScroller.ScrollableWidth)
            {
                horizontal = historyScroller.ScrollableWidth;
                _frameScrollDirection = -1;
            }

            historyScroller.ChangeView(horizontal, null, null, true);
            return;
        }

        // 兜底：历史卡片条为空/不可横向滚动时，滚动整页（页面内容始终高于视口，保证有真实滚动）。
        var pageScroller = ContentScrollViewer;
        if (pageScroller == null)
        {
            return;
        }

        if (!_frameScrollTargetNoted)
        {
            _frameScrollTargetNoted = true;
            FrameTrace.NoteScrollTarget("page-vertical");
        }

        var vertical = pageScroller.VerticalOffset + (12.0 * _frameScrollDirection);
        if (vertical <= 0)
        {
            vertical = 0;
            _frameScrollDirection = 1;
        }
        else if (vertical >= pageScroller.ScrollableHeight)
        {
            vertical = pageScroller.ScrollableHeight;
            _frameScrollDirection = -1;
        }

        pageScroller.ChangeView(null, vertical, null, true);
    }

    private void OnPageUnloaded(object sender, RoutedEventArgs e)
    {
        try
        {
            _relativeTimeTimer?.Stop();
            _relativeTimeTimer = null;

            // 对称解除订阅，避免 Singleton ViewModel 上残留已卸载页面的 Handler（越用越慢）
            if (_subscribed)
            {
                _subscribed = false;

                ViewModel.PropertyChanged -= OnViewModelPropertyChanged;
                ViewModel.CacheSeries.CollectionChanged -= OnCacheSeriesChanged;
                ViewModel.NetworkSeries.CollectionChanged -= OnNetworkSeriesChanged;
                ViewModel.ActivitySeries.CollectionChanged -= OnActivitySeriesChanged;
                ViewModel.RecentItems.CollectionChanged -= OnRecentItemsChanged;

                // 视差订阅必须配对解除：ScrollViewer 的生命周期比页面长，
                // 留着会让已卸载页面继续收到 ViewChanged 回调（此前正是因此被移除）。
                if (ContentScrollViewer != null)
                {
                    ContentScrollViewer.ViewChanged -= OnScrollViewChanged;
                }

                CacheChartCanvas.SizeChanged -= OnChartCanvasSizeChanged;
                NetworkChartCanvas.SizeChanged -= OnChartCanvasSizeChanged;
                ActivityChartCanvas.SizeChanged -= OnChartCanvasSizeChanged;

                if (HeroImage != null)
                {
                    HeroImage.ImageOpened -= OnHeroImageOpened;
                }

                var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
                settingsService.SettingChanged -= OnSettingChanged;

                this.ActualThemeChanged -= OnPageThemeChanged;
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"页面卸载清理失败: {ex.Message}");
        }
    }

    private void OnRecentItemsChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        if (ViewModel.LockedItemId != null)
        {
            DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
                () => UpdateAllCardsLockIcon());
        }
    }

    /// <summary>
    /// 页面滚动时更新卡片区的反向位移，形成视差层感。
    ///
    /// 只写 RenderTransform（TranslateTransform），不触发 Measure/Arrange；
    /// 因此每帧成本是一次合成层平移，而不是重新布局。
    /// </summary>
    private void OnScrollViewChanged(object? sender, ScrollViewerViewChangedEventArgs e)
    {
        if (ContentScrollViewer is null || ParallaxContentContainer is null || ParallaxTransform is null)
        {
            return;
        }

        var parallaxOffset = ContentScrollViewer.VerticalOffset * ParallaxSpeed;
        ParallaxTransform.Y = -parallaxOffset;
    }

    private void OnCacheSeriesChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        _cacheChartDirty = true;
        QueueChartRedraw();
    }

    private void OnNetworkSeriesChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        _networkChartDirty = true;
        QueueChartRedraw();
    }

    private void OnActivitySeriesChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        _activityChartDirty = true;
        QueueChartRedraw();
    }

    private void OnChartCanvasSizeChanged(object sender, Microsoft.UI.Xaml.SizeChangedEventArgs e)
    {
        if (ReferenceEquals(sender, CacheChartCanvas))
        {
            _cacheChartDirty = true;
        }
        else if (ReferenceEquals(sender, NetworkChartCanvas))
        {
            _networkChartDirty = true;
        }
        else if (ReferenceEquals(sender, ActivityChartCanvas))
        {
            _activityChartDirty = true;
        }

        QueueChartRedraw();
    }

    // 合并重绘：一次排队只重绘被标记的图表，突发变化折叠为一次重绘
    private void QueueChartRedraw()
    {
        if (_redrawQueued)
        {
            return;
        }

        _redrawQueued = true;
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            // 帧时序诊断（默认关闭）：给图表重绘打点，超过 5ms 的重绘会带标签记入 trace，
            // 用来解释同一时刻 UI 线程停顿样本的成因。未开启时 BeginUiOp 返回 0，EndUiOp 立即返回。
            var traceToken = FrameTrace.BeginUiOp();
            _redrawQueued = false;

            if (_initialChartDrawPending)
            {
                _initialChartDrawPending = false;
                StartupTrace.Mark("mainpage-charts-deferred");
            }

            if (_cacheChartDirty)
            {
                _cacheChartDirty = false;
                SafeDrawChart(DrawCacheChart, "Cache");
            }

            if (_networkChartDirty)
            {
                _networkChartDirty = false;
                SafeDrawChart(DrawNetworkChart, "Network");
            }

            if (_activityChartDirty)
            {
                _activityChartDirty = false;
                SafeDrawChart(DrawActivityChart, "Activity");
            }

            FrameTrace.EndUiOp("mainpage-chart-redraw", traceToken);
        });
    }

    // 首帧之后再做的启动工作：图表首绘（复用合并重绘队列）与背景图加载/颜色检测。
    // 注意：这里不能 Activate 窗口、不能弹对话框，避免在测量时抢焦点。
    private void DeferStartupWorkUntilAfterFirstFrame()
    {
        // 图表首绘：只标记脏位，复用既有的 Low 优先级合并重绘队列（不新增定时器）
        _initialChartDrawPending = true;
        _cacheChartDirty = true;
        _networkChartDirty = true;
        _activityChartDirty = true;
        QueueChartRedraw();

        // 背景图片加载 + 标题颜色检测：等首帧提交后再执行，解码在后台线程
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            StartupTrace.Mark("mainpage-images-deferred");
            _ = LoadBackgroundImageAsync();
        });
    }

    private async void OnHeroImageOpened(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        await DetectBackgroundColorAndUpdateTitleAsync();
    }

    // 只负责解码 + 计算标题区域平均亮度，不碰 UI。（调用方决定放在 UI 线程还是后台线程）
    private static async Task<Color?> DecodeTitleColorFromFileAsync(Windows.Storage.StorageFile file)
    {
        try
        {
            using var stream = await file.OpenAsync(Windows.Storage.FileAccessMode.Read);
            var decoder = await BitmapDecoder.CreateAsync(stream);

            // 计算标题区域在图片中的位置（标题在图片的上方，大约在 48-100 像素的位置）
            // 采样标题区域的颜色（左上角 36, 48 到 200, 100 的区域）
            // 确保边界不超出图片范围
            var x = (uint)Math.Min(36, decoder.PixelWidth);
            var y = (uint)Math.Min(48, decoder.PixelHeight);
            var maxWidth = decoder.PixelWidth > x ? decoder.PixelWidth - x : 1;
            var maxHeight = decoder.PixelHeight > y ? decoder.PixelHeight - y : 1;
            var width = (uint)Math.Min(200, maxWidth);
            var height = (uint)Math.Min(100, maxHeight);

            // 如果图片太小，跳过颜色检测
            if (width == 0 || height == 0 || decoder.PixelWidth == 0 || decoder.PixelHeight == 0)
            {
                return null;
            }

            // 读取像素数据（只取标题区域，不整图解码到内存）
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
            var pixelCount = 0;

            for (var i = 0; i < bytes.Length; i += 4)
            {
                var r = bytes[i];
                var g = bytes[i + 1];
                var b = bytes[i + 2];
                // bytes[i + 3] 是 alpha，这里忽略

                totalR += r;
                totalG += g;
                totalB += b;
                pixelCount++;
            }

            if (pixelCount == 0)
            {
                return null;
            }

            var avgR = (int)(totalR / pixelCount);
            var avgG = (int)(totalG / pixelCount);
            var avgB = (int)(totalB / pixelCount);

            // 计算亮度（使用相对亮度公式：0.299*R + 0.587*G + 0.114*B）
            var luminance = 0.299 * avgR + 0.587 * avgG + 0.114 * avgB;

            // 如果亮度小于 128（深色背景），使用白色文字；否则使用黑色文字
            return luminance < 128 ? Colors.White : Colors.Black;
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"检测图片颜色失败: {ex.Message}");
            return null;
        }
    }

    // ---- 新行为：元数据/解码全部在后台线程；按“路径+最后写入时间”缓存；同一张图每次启动最多解码一次 ----
    private async Task DetectBackgroundColorAndUpdateTitleAsync()
    {
        try
        {
            if (HeroImage?.Source == null || TitleTextBlock == null)
                return;

            var generation = _backgroundImageGeneration;
            var (appxUri, customPath) = CaptureBackgroundImageLocator();

            var descriptor = await Task.Run(() => ResolveBackgroundImageDescriptorAsync(appxUri, customPath));
            if (descriptor == null || generation != _backgroundImageGeneration)
                return;

            var cacheKey = descriptor.CacheKey;

            // 本次启动内已经算过同一张图：只重新应用颜色，不再读缓存/解码
            if (cacheKey == _titleColorResolvedKey && _titleColorResolvedColor is Color resolvedColor)
            {
                UpdateTitleColor(resolvedColor);
                return;
            }

            // 同一张图已有检测在后台进行：合并为一次解码
            if (cacheKey == _titleColorPendingKey)
                return;

            // 持久化缓存命中（跨启动）：整次解码都跳过
            var cachedColor = await ReadCachedTitleColorAsync(cacheKey);
            if (cachedColor.HasValue)
            {
                if (generation == _backgroundImageGeneration)
                {
                    _titleColorResolvedKey = cacheKey;
                    _titleColorResolvedColor = cachedColor.Value;
                    UpdateTitleColor(cachedColor.Value);
                }
                return;
            }

            if (generation != _backgroundImageGeneration)
                return;

            _titleColorPendingKey = cacheKey;
            try
            {
                var detectedColor = await Task.Run(() => DecodeBackgroundTitleColorAsync(descriptor));
                if (!detectedColor.HasValue || generation != _backgroundImageGeneration)
                    return;

                _titleColorResolvedKey = cacheKey;
                _titleColorResolvedColor = detectedColor.Value;
                UpdateTitleColor(detectedColor.Value);
                await SaveCachedTitleColorAsync(cacheKey, detectedColor.Value);
            }
            finally
            {
                if (_titleColorPendingKey == cacheKey)
                    _titleColorPendingKey = null;
            }
        }
        catch (Exception ex)
        {
            // 如果检测失败，使用默认颜色
            System.Diagnostics.Debug.WriteLine($"检测背景颜色失败: {ex.Message}");
        }
    }

    // 在 UI 线程上捕获“当前显示的是哪张图”（纯字符串，可安全带到后台线程）
    private (string? AppxUri, string? CustomPath) CaptureBackgroundImageLocator()
    {
        string? appxUri = null;
        string? customPath = null;

        if (HeroImage?.Source is Microsoft.UI.Xaml.Media.Imaging.BitmapImage bitmapImage)
        {
            var uri = bitmapImage.UriSource;
            if (uri != null && uri.IsAbsoluteUri)
            {
                if (uri.Scheme == "ms-appx")
                {
                    appxUri = uri.AbsoluteUri;
                }
                else if (uri.IsFile)
                {
                    customPath = uri.LocalPath;
                }
            }
        }

        if (appxUri == null && customPath == null)
        {
            // 通过 SetSourceAsync 设置的自定义图没有 UriSource，与旧逻辑一致回落到设置里记录的路径
            customPath = _lastBackgroundImagePath;
        }

        return (appxUri, customPath);
    }

    private static async Task<BackgroundImageDescriptor?> ResolveBackgroundImageDescriptorAsync(string? appxUri, string? customPath)
    {
        var file = await TryOpenBackgroundImageFileAsync(appxUri, customPath);
        if (file == null)
            return null;

        long lastWriteUtcTicks = 0;
        try
        {
            var basicProperties = await file.GetBasicPropertiesAsync();
            lastWriteUtcTicks = basicProperties.DateModified.UtcDateTime.Ticks;
        }
        catch
        {
            // 拿不到时间戳时退化为仅用路径做键（仍能在本次启动内去重）
        }

        var path = string.IsNullOrEmpty(file.Path) ? (customPath ?? appxUri ?? string.Empty) : file.Path;
        return new BackgroundImageDescriptor(appxUri, customPath, $"{path}|{lastWriteUtcTicks}");
    }

    private static async Task<Color?> DecodeBackgroundTitleColorAsync(BackgroundImageDescriptor descriptor)
    {
        var file = await TryOpenBackgroundImageFileAsync(descriptor.AppxUri, descriptor.CustomPath);
        return file == null ? null : await DecodeTitleColorFromFileAsync(file);
    }

    private static async Task<Windows.Storage.StorageFile?> TryOpenBackgroundImageFileAsync(string? appxUri, string? customPath)
    {
        if (!string.IsNullOrEmpty(customPath))
        {
            try
            {
                return await Windows.Storage.StorageFile.GetFileFromPathAsync(customPath);
            }
            catch
            {
                // 自定义文件可能已被移动/删除，回落到默认图
            }
        }

        if (!string.IsNullOrEmpty(appxUri))
        {
            try
            {
                return await Windows.Storage.StorageFile.GetFileFromApplicationUriAsync(new Uri(appxUri));
            }
            catch
            {
            }
        }

        return null;
    }

    private async Task<Color?> ReadCachedTitleColorAsync(string cacheKey)
    {
        try
        {
            var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
            var raw = await settingsService.ReadSettingAsync<string>(TitleColorCacheSettingKey);
            if (string.IsNullOrEmpty(raw))
                return null;

            var separator = raw.IndexOf('\t');
            if (separator < 0)
                return null;

            if (!string.Equals(raw.Substring(0, separator), cacheKey, StringComparison.Ordinal))
                return null;

            return raw.Substring(separator + 1) switch
            {
                "white" => Colors.White,
                "black" => Colors.Black,
                _ => null
            };
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"读取标题颜色缓存失败: {ex.Message}");
            return null;
        }
    }

    private async Task SaveCachedTitleColorAsync(string cacheKey, Color color)
    {
        try
        {
            var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
            var value = cacheKey + "\t" + (color == Colors.White ? "white" : "black");
            await settingsService.SaveSettingAsync(TitleColorCacheSettingKey, value);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"写入标题颜色缓存失败: {ex.Message}");
        }
    }

    private sealed class BackgroundImageDescriptor
    {
        public BackgroundImageDescriptor(string? appxUri, string? customPath, string cacheKey)
        {
            AppxUri = appxUri;
            CustomPath = customPath;
            CacheKey = cacheKey;
        }

        public string? AppxUri { get; }

        public string? CustomPath { get; }

        public string CacheKey { get; }
    }
    
    // 加载背景图片（用户自定义或默认）。
    private async Task LoadBackgroundImageAsync()
    {
        try
        {
            if (HeroImage == null)
                return;

            var settingsService = App.GetService<ClipBridgeShell_CS.Contracts.Services.ILocalSettingsService>();
            var customImagePath = await settingsService.ReadSettingAsync<string?>(BackgroundImagePathSettingKey);

            Windows.Storage.StorageFile? imageFile = null;

            if (!string.IsNullOrEmpty(customImagePath))
            {
                try
                {
                    // 检查文件是否存在
                    imageFile = await Windows.Storage.StorageFile.GetFileFromPathAsync(customImagePath);
                }
                catch
                {
                    // 如果文件不存在或读取失败，使用默认图片
                    System.Diagnostics.Debug.WriteLine($"无法加载自定义背景图片: {customImagePath}");
                }
            }

            if (imageFile != null)
            {
                _lastBackgroundImagePath = string.IsNullOrEmpty(imageFile.Path) ? customImagePath : imageFile.Path;
                await ApplyLoadedBackgroundImageAsync(imageFile);
                return;
            }

            _lastBackgroundImagePath = null;
            await ApplyDefaultBackgroundImageAsync();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"加载背景图片失败: {ex.Message}");
            // 失败时使用默认图片
            if (HeroImage != null)
            {
                var fallbackBitmap = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage();
                fallbackBitmap.DecodePixelWidth = BackgroundDecodePixelWidth;
                fallbackBitmap.UriSource = new Uri("ms-appx:///Assets/background.jpg");

                _backgroundImageGeneration++;
                HeroImage.Source = fallbackBitmap;

                _ = DetectBackgroundColorAndUpdateTitleAsync();
            }
        }
    }

    private async Task ApplyLoadedBackgroundImageAsync(Windows.Storage.StorageFile imageFile)
    {
        var bitmapImage = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage();

        // 下采样解码：按显示所需宽度解码，避免整张 3840x2160 背景图占用 UI 线程时间与内存
        bitmapImage.DecodePixelWidth = BackgroundDecodePixelWidth;

        using (var stream = await imageFile.OpenAsync(Windows.Storage.FileAccessMode.Read))
        {
            await bitmapImage.SetSourceAsync(stream);
        }

        _backgroundImageGeneration++;
        HeroImage!.Source = bitmapImage;

        // 首帧之后异步检测；解码在后台线程，ImageOpened 触发的检测会被去重
        _ = DetectBackgroundColorAndUpdateTitleAsync();
    }

    private async Task ApplyDefaultBackgroundImageAsync()
    {
        var defaultUri = new Uri("ms-appx:///Assets/background.jpg");

        // XAML 已经声明了同一张默认图时不再重复解码，只补一次首帧后的颜色检测
        if (IsSourceTheDefaultBackground(defaultUri))
        {
            _ = DetectBackgroundColorAndUpdateTitleAsync();
            return;
        }

        var bitmapImage = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage();
        bitmapImage.DecodePixelWidth = BackgroundDecodePixelWidth;
        bitmapImage.UriSource = defaultUri;

        _backgroundImageGeneration++;
        HeroImage!.Source = bitmapImage;

        _ = DetectBackgroundColorAndUpdateTitleAsync();
    }

    private bool IsSourceTheDefaultBackground(Uri defaultUri)
    {
        return HeroImage?.Source is Microsoft.UI.Xaml.Media.Imaging.BitmapImage bitmapImage
            && bitmapImage.UriSource is Uri currentUri
            && currentUri.IsAbsoluteUri
            && string.Equals(currentUri.AbsoluteUri, defaultUri.AbsoluteUri, StringComparison.OrdinalIgnoreCase);
    }

    private async void OnSettingChanged(object? sender, string key)
    {
        // 当背景图片设置改变时，重新加载
        if (key == BackgroundImagePathSettingKey)
        {
            await LoadBackgroundImageAsync();
        }
    }


    // 主题变化时，重新检测背景颜色并更新标题颜色
    // 标题由 TextBlock 显示，颜色始终由代码控制，这里只需要重新检测背景颜色
    private async void OnPageThemeChanged(FrameworkElement sender, object args)
    {
        // 主题变化时，重新检测背景颜色并更新标题
        // 延迟一下，确保主题切换完成后再更新
        await Task.Delay(50);
        await DetectBackgroundColorAndUpdateTitleAsync();
    }

    // 更新标题颜色的辅助方法：直接设置 TextBlock 前景色，完全不受主题影响
    private void UpdateTitleColor(Color textColor)
    {
        if (TitleTextBlock == null)
            return;

        var dispatcherQueue = DispatcherQueue.GetForCurrentThread();

        // 使用 DispatcherQueueHandler 委托类型
        DispatcherQueueHandler updateHandler = () =>
        {
            if (TitleTextBlock != null)
            {
                TitleTextBlock.Foreground = new SolidColorBrush(textColor);
            }
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

    private void OnViewModelPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ViewModel.IsCopyStatusOpen))
        {
            if (ViewModel.IsCopyStatusOpen)
            {
                ShowCopyToast();
            }
            else
            {
                HideCopyToast();
            }
        }
        else if (e.PropertyName == nameof(ViewModel.DisableAcrylicOnCards))
        {
            UpdateAllCopyCardsBackground();
        }
        else if (e.PropertyName == nameof(ViewModel.SelectedItemId))
        {
            UpdateAllCardsSelection();
        }
        else if (e.PropertyName == nameof(ViewModel.LockedItemId))
        {
            UpdateAllCardsLockIcon();
        }
    }

    private Brush GetCardBackgroundBrush()
    {
        if (Resources["AcrylicBrushConverter"] is not AcrylicBrushConverter converter)
            return new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        return (Brush)converter.Convert(ViewModel.DisableAcrylicOnCards, typeof(Brush), null!, null!);
    }

    private void UpdateAllCopyCardsBackground()
    {
        if (RecentItemsRepeater == null || ViewModel?.RecentItems == null) return;
        var brush = GetCardBackgroundBrush();
        for (var i = 0; i < ViewModel.RecentItems.Count; i++)
        {
            var element = RecentItemsRepeater.TryGetElement(i);
            if (element == null) continue;
            var border = element as Microsoft.UI.Xaml.Controls.Border
                ?? FindVisualChildByName<Microsoft.UI.Xaml.Controls.Border>(element, "CardBorder");
            if (border != null)
                border.Background = brush;
        }
    }

    private void ShowCopyToast()
    {
        if (CopyToast == null) return;

        // E73E = checkmark, EA39 = dismiss/error
        CopyToastIcon.Glyph = ViewModel.IsCopySuccess ? "\uE73E" : "\uEA39";
        CopyToastIcon.Foreground = ViewModel.IsCopySuccess
            ? new SolidColorBrush(Microsoft.UI.Colors.LimeGreen)
            : new SolidColorBrush(Microsoft.UI.Colors.OrangeRed);

        CopyToast.Opacity = 1;
        CopyToast.Translation = new System.Numerics.Vector3(0, 0, 32);
    }

    private void HideCopyToast()
    {
        if (CopyToast == null) return;
        CopyToast.Opacity = 0;
        CopyToast.Translation = new System.Numerics.Vector3(0, 12, 32);
    }

    private async void OnCardLockClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Button button)
        {
            var cardBorder = FindParent<Microsoft.UI.Xaml.Controls.Border>(button);
            if (cardBorder?.Tag is ItemMetaPayload item)
            {
                await ViewModel.ToggleLockItemAsync(item);
                UpdateAllCardsLockIcon();
            }
        }
    }

    private async void OnCardDeleteClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Button button)
        {
            var cardBorder = FindParent<Microsoft.UI.Xaml.Controls.Border>(button);
            if (cardBorder?.Tag is ItemMetaPayload item)
            {
                if (_selectedBorder == cardBorder)
                {
                    _selectedBorder = null;
                }
                await ViewModel.DeleteRecentItemAsync(item);
            }
        }
    }

    private async void OnCardContextMenuDeleteClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        var item = _contextMenuCardItem;
        var cardBorder = _contextMenuCardBorder;
        _contextMenuCardItem = null;
        _contextMenuCardBorder = null;
        if (item == null) return;
        if (_selectedBorder == cardBorder)
            _selectedBorder = null;
        await ViewModel.DeleteRecentItemAsync(item);
    }

    private async void OnCardContextMenuGlobalDeleteClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        var item = _contextMenuCardItem;
        var cardBorder = _contextMenuCardBorder;
        _contextMenuCardItem = null;
        _contextMenuCardBorder = null;
        if (item == null) return;
        if (_selectedBorder == cardBorder)
            _selectedBorder = null;
        await ViewModel.GlobalDeleteItemAsync(item);
    }

    private void OnCardTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        // Skip if tap originated from a Button (lock/delete)
        if (e.OriginalSource is Microsoft.UI.Xaml.DependencyObject source &&
            FindParent<Microsoft.UI.Xaml.Controls.Button>(source) != null)
        {
            return;
        }

        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder && cardBorder.Tag is ItemMetaPayload item)
        {
            ViewModel.SelectItemCommand.Execute(item);
            
            if (_selectedBorder != null && _selectedBorder != cardBorder)
            {
                UpdateCardSelection(_selectedBorder, forceUnselected: true);
            }
            
            _selectedBorder = cardBorder;
            UpdateCardSelection(cardBorder);
        }
    }

    private void StartRelativeTimeRefresh()
    {
        if (_relativeTimeTimer != null) return;
        _relativeTimeTimer = new DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(30)
        };
        _relativeTimeTimer.Tick += (_, _) => RefreshCardRelativeTimes();
        _relativeTimeTimer.Start();
    }

    private void RefreshCardRelativeTimes()
    {
        if (RecentItemsRepeater == null || ViewModel?.RecentItems == null) return;
        var language = System.Globalization.CultureInfo.CurrentUICulture.Name;
        for (var i = 0; i < ViewModel.RecentItems.Count; i++)
        {
            var element = RecentItemsRepeater.TryGetElement(i);
            if (element == null) continue;
            Microsoft.UI.Xaml.Controls.Border? border = element as Microsoft.UI.Xaml.Controls.Border;
            border ??= FindVisualChildByName<Microsoft.UI.Xaml.Controls.Border>(element, "CardBorder");
            if (border?.Tag is ItemMetaPayload item)
            {
                var tb = FindVisualChildByName<Microsoft.UI.Xaml.Controls.TextBlock>(border, "CardRelativeTimeText");
                if (tb != null)
                    tb.Text = (string)_relativeTimeConverter.Convert(item.CreatedTsMs, typeof(string), null!, language);
            }
        }
    }

    private void UpdateAllCardsLockIcon()
    {
        if (RecentItemsRepeater == null || ViewModel?.RecentItems == null) return;
        for (var i = 0; i < ViewModel.RecentItems.Count; i++)
        {
            var element = RecentItemsRepeater.TryGetElement(i);
            if (element == null) continue;
            Microsoft.UI.Xaml.Controls.Border? border = element as Microsoft.UI.Xaml.Controls.Border;
            border ??= FindVisualChildByName<Microsoft.UI.Xaml.Controls.Border>(element, "CardBorder");
            if (border != null)
            {
                UpdateCardLockIcon(border);
            }
        }
    }

    private void UpdateCardLockIcon(Microsoft.UI.Xaml.Controls.Border cardBorder)
    {
        var lockIcon = FindVisualChildByName<Microsoft.UI.Xaml.Controls.FontIcon>(cardBorder, "LockIcon");
        if (lockIcon == null || cardBorder.Tag is not ItemMetaPayload item) return;

        var isLocked = ViewModel.IsItemLocked(item.ItemId);
        // E72E = Lock, E785 = Unlock
        lockIcon.Glyph = isLocked ? "\uE72E" : "\uE785";
        lockIcon.Foreground = isLocked
            ? new SolidColorBrush(Microsoft.UI.Colors.Orange)
            : (Microsoft.UI.Xaml.Media.Brush)Microsoft.UI.Xaml.Application.Current.Resources["TextFillColorPrimaryBrush"];
    }

    private void OnCardLoaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.Controls.Border cardBorder && cardBorder.Tag is ItemMetaPayload item)
        {
            cardBorder.Background = GetCardBackgroundBrush();
            UpdateCardSelection(cardBorder);
            UpdateCardLockIcon(cardBorder);
            
            if (ViewModel.IsItemSelected(item.ItemId))
            {
                _selectedBorder = cardBorder;
            }
            
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
        for (var i = 0; i < Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(parent); i++)
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

    /// <summary>
    /// 更新卡片选中状态（性能优化版本）
    /// 只更新之前选中的卡片和当前选中的卡片，而不是遍历所有卡片
    /// 注意：如果通过代码直接修改 SelectedItemId（而不是点击），仍需要查找对应的卡片
    /// </summary>
    private void UpdateAllCardsSelection()
    {
        if (ViewModel?.RecentItems == null) return;
        
        // 1. 取消之前选中卡片的选中状态
        if (_selectedBorder != null)
        {
            // 检查之前的卡片是否仍然存在且仍然被选中
            if (_selectedBorder.Tag is ItemMetaPayload oldItem && 
                ViewModel.IsItemSelected(oldItem.ItemId))
            {
                // 如果之前的卡片仍然是选中的，说明 SelectedItemId 没有改变，不需要更新
                return;
            }
            
            // 取消之前的选中状态
            UpdateCardSelection(_selectedBorder);
            _selectedBorder = null;
        }
        
        // 2. 找到并选中新的卡片（仅在通过代码修改 SelectedItemId 时才会执行到这里）
        if (!string.IsNullOrEmpty(ViewModel.SelectedItemId) && RecentItemsRepeater != null)
        {
            // 在 ItemsSource 中查找对应的项
            for (var i = 0; i < ViewModel.RecentItems.Count; i++)
            {
                var item = ViewModel.RecentItems[i];
                if (item.ItemId == ViewModel.SelectedItemId)
                {
                    // 获取对应的 UI 元素
                    var element = RecentItemsRepeater.TryGetElement(i);
                    if (element != null)
                    {
                        // 查找 Border（卡片）
                        Microsoft.UI.Xaml.Controls.Border? border;
                        if (element is Microsoft.UI.Xaml.Controls.Border b && b.Name == "CardBorder")
                        {
                            border = b;
                        }
                        else
                        {
                            border = FindVisualChildByName<Microsoft.UI.Xaml.Controls.Border>(element, "CardBorder");
                        }
                        
                        if (border != null)
                        {
                            _selectedBorder = border;
                            UpdateCardSelection(border);
                            break; // 找到后立即退出
                        }
                    }
                }
            }
        }
    }

    /// <summary>
    /// 更新单个卡片的选中状态（带动画）
    /// </summary>
    /// <param name="border">要更新的卡片 Border</param>
    /// <param name="forceUnselected">强制设置为未选中状态（用于在切换选中时取消之前的选中）</param>
    private void UpdateCardSelection(Microsoft.UI.Xaml.Controls.Border border, bool forceUnselected = false)
    {
        if (border.Tag is ItemMetaPayload item)
        {
            var isSelected = forceUnselected ? false : ViewModel.IsItemSelected(item.ItemId);
            
            // 查找内容 Grid，用于调整 Padding
            var contentGrid = FindVisualChildByName<Microsoft.UI.Xaml.Controls.Grid>(border, "ContentGrid");
            
            if (isSelected)
            {
                // 选中时：边框加粗（从 1px 变为 2px），颜色改为选中颜色
                border.BorderThickness = new Microsoft.UI.Xaml.Thickness(2);
                
                // 从 Page.Resources 获取边框颜色
                var themeDictionaries = this.Resources.ThemeDictionaries;
                var currentTheme = this.ActualTheme == Microsoft.UI.Xaml.ElementTheme.Light ? "Light" : "Dark";
                if (themeDictionaries != null && themeDictionaries.TryGetValue(currentTheme, out var themeDict) && themeDict is Microsoft.UI.Xaml.ResourceDictionary themeResourceDict)
                {
                    if (themeResourceDict.TryGetValue("CardSelectedBorderBrush", out var resource) && resource is Microsoft.UI.Xaml.Media.SolidColorBrush themeBrush)
                    {
                        border.BorderBrush = themeBrush;
                    }
                }
                
                // 减少 Padding 来补偿边框占用的额外空间
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
        for (var i = 0; i < Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(parent); i++)
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
        catch (Exception)
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

            // 优化：只遍历一次集合来计算最大值和最小值
            var count = ViewModel.CacheSeries.Count;
            if (count == 0)
            {
                return;
            }
            
            double maxValue = ViewModel.CacheSeries[0].CacheBytes;
            double minValue = ViewModel.CacheSeries[0].CacheBytes;
            for (var i = 1; i < count; i++)
            {
                var value = ViewModel.CacheSeries[i].CacheBytes;
                if (value > maxValue) maxValue = value;
                if (value < minValue) minValue = value;
            }
            
            var valueRange = maxValue - minValue;
            if (valueRange == 0)
                valueRange = 1;

            var points = new PointCollection();
            for (var i = 0; i < count; i++)
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
        } catch (Exception)
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

            // 优化：只遍历一次集合来计算最大值
            var count = ViewModel.NetworkSeries.Count;
            if (count == 0)
            {
                return;
            }
            
            double maxValue = Math.Max(ViewModel.NetworkSeries[0].BytesSent, ViewModel.NetworkSeries[0].BytesRecv);
            for (var i = 1; i < count; i++)
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
            for (var i = 0; i < count; i++)
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
            for (var i = 0; i < count; i++)
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
        } catch (Exception)
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
            for (var i = 1; i < count; i++)
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
            for (var i = 0; i < count; i++)
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
            for (var i = 0; i < count; i++)
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
            for (var i = 0; i < count; i++)
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
        } catch (Exception)
        {
            throw;
        }
    }

    private async void OnAvatarTapped(object sender, TappedRoutedEventArgs e)
    {
        var accountService = App.GetService<IAccountService>();
        var hasAccount = await accountService.HasAccountAsync();

        if (!hasAccount)
        {
            // 显示登录窗口
            var loginDialog = new LoginDialog(accountService);
            loginDialog.XamlRoot = this.XamlRoot;
            await loginDialog.ShowAsync();
        }
        else
        {
            // 显示账号信息和更换账号选项
            var account = await accountService.LoadAccountAsync();
            if (account.HasValue)
            {
                var deviceName = System.Environment.MachineName;
                
                // 创建内容面板
                var contentPanel = new StackPanel
                {
                    Spacing = 12
                };
                
                var loc = Localizer.Get();
                
                // 账号信息
                var accountInfo = new TextBlock
                {
                    Text = string.Format(loc.GetLocalizedString("AccountInfo_Account"), account.Value.username),
                    FontSize = 14,
                    Margin = new Thickness(0, 0, 0, 8)
                };
                contentPanel.Children.Add(accountInfo);
                
                // 设备信息
                var deviceInfo = new TextBlock
                {
                    Text = string.Format(loc.GetLocalizedString("AccountInfo_Device"), deviceName),
                    FontSize = 14
                };
                contentPanel.Children.Add(deviceInfo);
                
                var dialog = new ContentDialog
                {
                    Title = loc.GetLocalizedString("AccountInfo_Title"),
                    Content = contentPanel,
                    PrimaryButtonText = loc.GetLocalizedString("AccountInfo_SwitchAccount"),
                    CloseButtonText = loc.GetLocalizedString("AccountInfo_Close"),
                    XamlRoot = this.XamlRoot
                };
                
                var result = await dialog.ShowAsync();
                
                // 如果点击了"更换账号"
                if (result == ContentDialogResult.Primary)
                {
                    // 清除当前账号
                    await accountService.ClearAccountAsync();
                    
                    // 关闭核心（如果正在运行）
                    var coreHost = App.GetService<ICoreHostService>();
                    if (coreHost.State == CoreState.Ready || 
                        coreHost.State == CoreState.Loading)
                    {
                        await coreHost.ShutdownAsync();
                    }
                    
                    // 显示登录对话框
                    var loginDialog = new LoginDialog(accountService);
                    loginDialog.XamlRoot = this.XamlRoot;
                    await loginDialog.ShowAsync();
                }
            }
        }
    }
}
