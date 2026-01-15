//platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Views/LogsPage.xaml.cs
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media.Animation;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml.Hosting;
using CommunityToolkit.WinUI.Animations;
using Microsoft.UI.Xaml.Controls.Primitives;
using System.Threading.Tasks;
using Windows.ApplicationModel.DataTransfer;
using Windows.UI.Core;
using Windows.UI;
using System.Linq;
using System;
using System.Collections.Specialized;
using System.Numerics;
using System.Collections.Generic;
using ClipBridgeShell_CS.Helpers;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsPage : Page
{
    public LogsViewModel VM { get; }
    private const double ScrollThreshold = 10.0; // 底部吸附阈值（像素）
    private bool _isLoadingOlderLogs = false; // 防止重复加载

    private bool _isFirstLayout = true; // 标记是否是首次布局
    private bool _hasScrolledToBottom = false; // 标记是否已经滚动到底部（防止重复滚动）
    private bool _userScrolledAway = false; // 标记用户是否主动向上滚动离开了底部
    private double _lastScrollOffset = 0; // 记录上次滚动位置，用于判断滚动方向
    private HashSet<long> _animatedItemIds = new(); // 记录已动画过的日志项ID，避免重复动画

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
        
        // 页面加载完成后设置初始状态
        Loaded += OnPageLoaded;
        
        // ScrollViewer 加载完成后，订阅滚动事件
        LogsScrollViewer.Loaded += OnScrollViewerLoaded;
        
        // ItemsRepeater 元素准备完成时，为新项添加动画
        LogsItemsRepeater.ElementPrepared += OnLogItemElementPrepared;
        
        // 监听 Items 集合变化，清空时重置动画记录
        VM.Items.CollectionChanged += OnItemsCollectionChanged;
    }
    
    private void InitializeFilterFlyouts()
    {
        // 设置各个过滤 Flyout 的 ViewModel
        LevelFilterFlyout.ViewModel = VM;
        ComponentFilterFlyout.ViewModel = VM;
        CategoryFilterFlyout.ViewModel = VM;
        TimestampFilterFlyout.ViewModel = VM;
        
    }
    
    private void OnResetFiltersClick(object sender, RoutedEventArgs e)
    {
        // 重置所有过滤条件
        VM.FilterStartTime = null;
        VM.FilterEndTime = null;
        VM.FilterLevels = new HashSet<int> { 0, 1, 2, 3, 4, 5 }; // 全部级别
        VM.FilterComponents = new HashSet<string>(); // 空集合表示不过滤
        VM.FilterCategories = new HashSet<string>(); // 空集合表示不过滤
        
        // 注意：各个 Flyout 会在下次打开时自动从 ViewModel 恢复状态
        // 因为它们都监听 ViewModel 的过滤属性变化
    }
    
    private void OnItemsCollectionChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        // 如果集合被清空，清空动画记录
        if (e.Action == NotifyCollectionChangedAction.Reset)
        {
            ClearAnimatedItems();
        }
    }
    
    private void OnLogItemElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        // 当新的日志项元素准备完成时，为其添加滑入动画
        if (args.Element is Border border && border.DataContext is Models.LogRow item)
        {
            // 检查是否已经动画过（避免重复动画）
            if (!_animatedItemIds.Contains(item.Id))
            {
                _animatedItemIds.Add(item.Id);
                
                // 只有在用户滚动到底部时才添加动画
                // 注意：初始加载的项不添加动画，只有新添加的项才动画
                if (!_userScrolledAway && VM.IsScrolledToBottom && !_isFirstLayout)
                {
                    ApplySlideInAnimation(border);
                }
            }
        }
    }
    
    private void ApplySlideInAnimation(Border element)
    {
        try
        {
            // 使用 CommunityToolkit 的隐式动画
            var showAnimations = new ImplicitAnimationSet();
            
            // 从下方滑入动画（Y轴从30到0）
            // 注意：TranslationAnimation 的 From 和 To 属性可能需要字符串格式
            var slideAnimation = new TranslationAnimation
            {
                From = "0,30,0", // 从下方30像素开始（字符串格式：X,Y,Z）
                To = "0,0,0", // 移动到最终位置
                Duration = TimeSpan.FromMilliseconds(300),
                EasingType = EasingType.Cubic,
                EasingMode = EasingMode.EaseOut
            };
            
            // 淡入动画
            var fadeAnimation = new OpacityAnimation
            {
                From = 0.0f,
                To = 1.0f,
                Duration = TimeSpan.FromMilliseconds(300)
            };
            
            showAnimations.Add(slideAnimation);
            showAnimations.Add(fadeAnimation);
            
            // 应用动画
            Implicit.SetShowAnimations(element, showAnimations);
        }
        catch
        {
            // 动画失败时静默处理
        }
    }
    
    private Color GetLevelColor(int level)
    {
        return level switch
        {
            0 => Color.FromArgb(255, 128, 128, 128),    // Trace: 灰色
            1 => Color.FromArgb(255, 0, 191, 255),       // Debug: 青色
            2 => Color.FromArgb(255, 30, 144, 255),      // Info: 蓝色
            3 => Color.FromArgb(255, 255, 140, 0),       // Warn: 橙色
            4 => Color.FromArgb(255, 220, 20, 60),       // Error: 红色
            5 => Color.FromArgb(255, 139, 0, 0),         // Critical: 深红色
            _ => Color.FromArgb(255, 128, 128, 128)
        };
    }
    
    // 清空已动画项记录（当清空日志时）
    private void ClearAnimatedItems()
    {
        _animatedItemIds.Clear();
    }
    
    private void OnPageLoaded(object sender, RoutedEventArgs e)
    {
        // 页面加载时，设置初始滚动状态为底部（显示最新日志）
        VM.IsScrolledToBottom = true;
        _userScrolledAway = false; // 初始状态：在底部，未离开
        
        // 初始化过滤 Flyout（在页面加载完成后）
        InitializeFilterFlyouts();
    }
    
    private void OnScrollViewerLoaded(object sender, RoutedEventArgs e)
    {
        // 订阅滚动事件
        LogsScrollViewer.ViewChanged += OnScrollViewChanged;
        
        // 首次加载时滚动到底部
        if (_isFirstLayout && VM.Items.Count > 0)
        {
            _isFirstLayout = false;
            // 延迟一点确保内容已渲染
            _ = Task.Run(async () =>
            {
                await Task.Delay(150);
                App.MainWindow.DispatcherQueue.TryEnqueue(() =>
                {
                    ScrollToBottom();
                    // 标记所有初始项已动画（避免初始加载时动画）
                    foreach (var item in VM.Items)
                    {
                        _animatedItemIds.Add(item.Id);
                    }
                });
            });
        }
    }
    
    private void OnTailRequested(object? sender, EventArgs e)
    {
        // 新日志追加到底部，只有在用户没有离开底部且开启了自动滚动时才滚动
        // VM.IsScrolledToBottom 现在只有在用户主动滚动到底部且没有离开时才为 true
        if (VM.IsScrolledToBottom && !_userScrolledAway && VM.Items.Count > 0)
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
        if (LogsScrollViewer == null) return;
        
        // 滚动到底部（最新日志在底部）
        if (LogsScrollViewer.ScrollableHeight > 0)
        {
            LogsScrollViewer.ChangeView(null, LogsScrollViewer.ScrollableHeight, null, disableAnimation: true);
        }
    }
    
    private void OnLoadOlderLogsRequested(object? sender, LoadOlderLogsEventArgs e)
    {
        // 加载更早的日志后，需要保持滚动位置
        // 由于是文本流，ScrollViewer 会自动保持滚动位置
        // 这里可以记录当前滚动位置，加载后恢复（如果需要的话）
    }
    
    private void OnScrollViewChanged(object? sender, ScrollViewerViewChangedEventArgs e)
    {
        if (sender is ScrollViewer scrollViewer)
        {
            double verticalOffset = scrollViewer.VerticalOffset;
            double scrollableHeight = scrollViewer.ScrollableHeight;
            double viewportHeight = scrollViewer.ViewportHeight;
            
            // 判断滚动方向：向上滚动（离开底部）还是向下滚动（接近底部）
            // 注意：需要排除首次调用（_lastScrollOffset 为 0 的情况）
            bool scrollingUp = _lastScrollOffset > 0 && verticalOffset < _lastScrollOffset;
            bool scrollingDown = _lastScrollOffset > 0 && verticalOffset > _lastScrollOffset;
            _lastScrollOffset = verticalOffset;
            
            // 判断是否在底部（使用更严格的阈值，只有真正在底部才认为在底部）
            bool isAtBottom;
            if (scrollableHeight == 0)
            {
                // 内容还没渲染完，默认认为在底部（首次加载时）
                isAtBottom = true;
                _userScrolledAway = false;
            }
            else
            {
                // 计算距离底部的距离
                double distanceFromBottom = scrollableHeight - (verticalOffset + viewportHeight);
                
                // 如果用户向上滚动（离开底部），立即标记为已离开，不再锁定
                if (scrollingUp && distanceFromBottom > 0)
                {
                    _userScrolledAway = true;
                    isAtBottom = false;
                }
                // 如果用户向下滚动到底部（距离底部 <= 阈值），且之前已离开，现在重新锁定
                else if (scrollingDown && distanceFromBottom <= ScrollThreshold)
                {
                    // 用户主动滚动到底部，重新锁定
                    _userScrolledAway = false;
                    isAtBottom = true;
                }
                // 如果已经在底部且没有离开过，保持锁定
                else if (!_userScrolledAway && distanceFromBottom <= ScrollThreshold)
                {
                    isAtBottom = true;
                }
                // 如果已经离开过底部，即使现在在阈值内，也不锁定（除非用户主动滚动回来）
                else if (_userScrolledAway)
                {
                    isAtBottom = false;
                }
                // 其他情况：不在底部
                else
                {
                    isAtBottom = false;
                }
            }
            
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
    
    // 注意：列宽调整、表头点击、选择相关的方法已移除，因为文本流显示不需要这些功能
    
    // 复制功能（复制所有格式化文本）
    private void OnCopyClick(object sender, RoutedEventArgs e)
    {
        if (VM.Items.Count == 0) return;
        
        // 构建格式化文本
        var sb = new System.Text.StringBuilder();
        foreach (var item in VM.Items)
        {
            var timeStr = item.TimeStrFull.PadRight(23);
            var levelStr = item.LevelName.PadRight(8);
            var componentStr = (item.Component ?? "").PadRight(12);
            var categoryStr = (item.CategoryDisplay ?? "").PadRight(20);
            var messageStr = item.DisplayMessage ?? "";
            var exceptionStr = !string.IsNullOrEmpty(item.Exception) ? $"{"LogsPage_ExceptionPrefix".GetLocalized()}{item.Exception}" : "";
            
            sb.AppendLine($"{timeStr} {levelStr} {componentStr} {categoryStr} {messageStr}{exceptionStr}");
        }
        
        // 复制到剪贴板
        var dataPackage = new DataPackage();
        dataPackage.SetText(sb.ToString());
        Clipboard.SetContent(dataPackage);
    }
}
