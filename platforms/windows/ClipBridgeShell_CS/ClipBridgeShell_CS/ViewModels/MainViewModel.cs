using System;
using System.Collections.ObjectModel;
// #region agent log
using System.IO;
using System.Text.Json;
// #endregion agent log
using System.Linq;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
namespace ClipBridgeShell_CS.ViewModels;

public partial class MainViewModel : ObservableRecipient
{
    // 持有 Store 的引用
    private readonly HistoryStore _historyStore;
    private readonly ICoreHostService _coreHost;
    private readonly PeerStore _peerStore;
    private readonly TransferStore _transferStore;
    private readonly ILocalSettingsService _localSettings;
    private readonly ClipboardApplyService? _clipboardApply;
    private DispatcherTimer? _statsTimer;
    private int _recentItemsCount = 10; // 默认10个
    private string? _localDeviceId; // 缓存本机device_id

    // #region agent log
    private async Task LogAsync(string hypothesisId, string location, string message, object data)
    {
        try
        {
            var logEntry = new
            {
                sessionId = "debug-session",
                runId = "run1",
                hypothesisId = hypothesisId,
                location = location,
                message = message,
                data = data,
                timestamp = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds()
            };
            var jsonLine = JsonSerializer.Serialize(logEntry);
            await File.AppendAllTextAsync("c:\\Project\\ClipBridge\\.cursor\\debug.log", jsonLine + Environment.NewLine);
        }
        catch { /* Ignore logging errors */ }
    }
    // #endregion agent log

    // 直接暴露 Store 的集合供 UI 绑定
    // 这样当 Store 更新时，UI 会自动刷新
    public ObservableCollection<ItemMetaPayload> HistoryItems => _historyStore.Items;

    // 最近N条历史（用于顶部卡片条），从Core数据库查询
    private readonly ObservableCollection<ItemMetaPayload> _recentItems = new();
    public ObservableCollection<ItemMetaPayload> RecentItems => _recentItems;

    // 选中的卡片ItemId
    private string? _selectedItemId;
    public string? SelectedItemId
    {
        get => _selectedItemId;
        set => SetProperty(ref _selectedItemId, value);
    }

    /// <summary>
    /// 获取本机设备ID
    /// </summary>
    public string? GetLocalDeviceId()
    {
        return _localDeviceId;
    }

    /// <summary>
    /// 根据 device_id 获取设备名称
    /// </summary>
    public string GetDeviceName(string deviceId)
    {
        if (string.IsNullOrEmpty(deviceId))
            return "Unknown";
        
        var peer = _peerStore.Peers.FirstOrDefault(p => p.DeviceId == deviceId);
        return peer?.Name ?? deviceId; // 如果找不到，返回 deviceId 作为后备
    }

    /// <summary>
    /// 判断item是否被选中
    /// </summary>
    public bool IsItemSelected(string itemId)
    {
        return SelectedItemId == itemId;
    }

    /// <summary>
    /// 获取item的状态
    /// </summary>
    public string GetItemState(ItemMetaPayload item)
    {
        // #region agent log
        _ = LogAsync("B,C", "MainViewModel.cs:GetItemState", "Entering GetItemState", new { ItemId = item?.ItemId, SourceDeviceId = item?.SourceDeviceId, LocalDeviceId = _localDeviceId });
        // #endregion agent log
        if (item == null) return "Unknown";

        // 检查是否有正在进行的传输
        var transfer = _transferStore.Transfers.FirstOrDefault(t => 
            t.ItemId == item.ItemId && 
            (t.State == "downloading" || t.State == "uploading"));
        
        if (transfer != null)
        {
            var progress = transfer.Progress;
            var state = progress > 0 ? $"Downloading {(int)(progress * 100)}%" : "Downloading";
            // #region agent log
            _ = LogAsync("B,C", "MainViewModel.cs:GetItemState", "Transfer in progress", new { ItemId = item.ItemId, State = state });
            // #endregion agent log
            return state;
        }

        // 检查是否有失败的传输
        var failedTransfer = _transferStore.Transfers.FirstOrDefault(t => 
            t.ItemId == item.ItemId && 
            t.State == "failed");
        
        if (failedTransfer != null)
        {
            var errorMsg = failedTransfer.Message ?? "Unknown error";
            if (errorMsg.Length > 20) errorMsg = errorMsg.Substring(0, 20) + "...";
            var state = $"Failed: {errorMsg}";
            // #region agent log
            _ = LogAsync("B,C", "MainViewModel.cs:GetItemState", "Transfer failed", new { ItemId = item.ItemId, State = state });
            // #endregion agent log
            return state;
        }

        // 判断是否来自本机
        // 如果source_device_id为空，可能是本机内容（旧数据或特殊情况）
        if (string.IsNullOrEmpty(item.SourceDeviceId))
        {
            // #region agent log
            _ = LogAsync("B", "MainViewModel.cs:GetItemState", "SourceDeviceId is empty, returning Ready", new { ItemId = item.ItemId, State = "Ready" });
            // #endregion agent log
            return "Ready"; // 假设是本机内容
        }

        // 如果source_device_id等于本机device_id，可能是Ready（本机内容）
        if (!string.IsNullOrEmpty(_localDeviceId) && item.SourceDeviceId == _localDeviceId)
        {
            // #region agent log
            _ = LogAsync("C", "MainViewModel.cs:GetItemState", "Item from local device, returning Ready", new { ItemId = item.ItemId, SourceDeviceId = item.SourceDeviceId, LocalDeviceId = _localDeviceId, State = "Ready" });
            // #endregion agent log
            return "Ready"; // 本机内容，假设已经在CAS中
        }

        // 如果source_device_id不等于本机device_id，说明来自其他设备
        // 默认应该是NeedsDownload（除非已经有CONTENT_CACHED事件，但我们在UI层面无法直接检查）
        // 为了简化，暂时假设来自其他设备的都是NeedsDownload
        // TODO: 可以通过检查HistoryStore中是否有对应的CONTENT_CACHED事件来判断是否已下载
        // #region agent log
        _ = LogAsync("A,C", "MainViewModel.cs:GetItemState", "Item from remote device, returning NeedsDownload", new { ItemId = item.ItemId, SourceDeviceId = item.SourceDeviceId, LocalDeviceId = _localDeviceId, State = "NeedsDownload" });
        // #endregion agent log
        return "NeedsDownload";
    }

    /// <summary>
    /// 获取状态对应的颜色
    /// </summary>
    public Windows.UI.Color GetItemStateColor(string state)
    {
        if (string.IsNullOrEmpty(state)) return Windows.UI.Color.FromArgb(255, 128, 128, 128);

        if (state.StartsWith("Ready", StringComparison.OrdinalIgnoreCase))
            return Windows.UI.Color.FromArgb(255, 15, 123, 15); // 绿色
        else if (state.StartsWith("NeedsDownload", StringComparison.OrdinalIgnoreCase))
            return Windows.UI.Color.FromArgb(255, 255, 140, 0); // 橙色
        else if (state.StartsWith("Downloading", StringComparison.OrdinalIgnoreCase))
            return Windows.UI.Color.FromArgb(255, 0, 120, 215); // 蓝色
        else if (state.StartsWith("Failed", StringComparison.OrdinalIgnoreCase))
            return Windows.UI.Color.FromArgb(255, 196, 43, 28); // 红色
        else
            return Windows.UI.Color.FromArgb(255, 128, 128, 128); // 灰色
    }

    // 设备列表
    public ObservableCollection<PeerMetaPayload> Peers => _peerStore.Peers;

    // 格式化缓存大小
    public string CurrentCacheBytesFormatted
    {
        get
        {
            if (_currentCacheBytes < 1024)
                return $"{_currentCacheBytes} B";
            else if (_currentCacheBytes < 1024 * 1024)
                return $"{_currentCacheBytes / 1024.0:F2} KB";
            else if (_currentCacheBytes < 1024L * 1024 * 1024)
                return $"{_currentCacheBytes / (1024.0 * 1024):F2} MB";
            else
                return $"{_currentCacheBytes / (1024.0 * 1024 * 1024):F2} GB";
        }
    }

    // 统计属性
    private long _currentCacheBytes;
    public long CurrentCacheBytes
    {
        get => _currentCacheBytes;
        set
        {
            if (SetProperty(ref _currentCacheBytes, value))
            {
                OnPropertyChanged(nameof(CurrentCacheBytesFormatted));
            }
        }
    }

    private int _activeTransfersCount;
    public int ActiveTransfersCount
    {
        get => _activeTransfersCount;
        set => SetProperty(ref _activeTransfersCount, value);
    }

    private int _outboundPeerCount;
    public int OutboundPeerCount
    {
        get => _outboundPeerCount;
        set => SetProperty(ref _outboundPeerCount, value);
    }

    private int _inboundPeerCount;
    public int InboundPeerCount
    {
        get => _inboundPeerCount;
        set => SetProperty(ref _inboundPeerCount, value);
    }

    private bool _clipboardCaptureEnabled = true;
    public bool ClipboardCaptureEnabled
    {
        get => _clipboardCaptureEnabled;
        set => SetProperty(ref _clipboardCaptureEnabled, value);
    }

    private bool _clipboardSharingEnabled = true;
    public bool ClipboardSharingEnabled
    {
        get => _clipboardSharingEnabled;
        set => SetProperty(ref _clipboardSharingEnabled, value);
    }

    // 图表数据
    public ObservableCollection<CacheStatsPoint> CacheSeries { get; } = new();
    public ObservableCollection<NetworkStatsPoint> NetworkSeries { get; } = new();
    public ObservableCollection<ActivityStatsPoint> ActivitySeries { get; } = new();

    // [测试用] 手动模拟 Core 发事件的命令
    public IRelayCommand TestAddEventCommand { get; }

    // 命令
    public IRelayCommand ClearCacheCommand { get; }
    public IRelayCommand ToggleCaptureCommand { get; }
    public IRelayCommand ToggleSharingCommand { get; }
    public IRelayCommand NavigateToHistoryCommand { get; }
    public IRelayCommand<ItemMetaPayload> SelectItemCommand { get; }

    // 构造函数注入
    public MainViewModel(
        HistoryStore historyStore,
        Services.EventPumpService pump,
        ICoreHostService coreHost,
        PeerStore peerStore,
        TransferStore transferStore,
        INavigationService navigationService,
        ILocalSettingsService localSettings,
        Services.ClipboardApplyService? clipboardApply = null)
    {
        _historyStore = historyStore;
        _coreHost = coreHost;
        _peerStore = peerStore;
        _transferStore = transferStore;
        _localSettings = localSettings;
        _clipboardApply = clipboardApply;

        // 创建一个测试按钮命令：点击后模拟 Core 发来一条数据
        TestAddEventCommand = new RelayCommand(() =>
        {
            var randomId = (ulong)Random.Shared.Next(1000, 9999);
            var json = $$"""
            {
                "type": "item_added",
                "payload": {
                    "id": {{randomId}},
                    "timestamp": {{DateTimeOffset.UtcNow.ToUnixTimeMilliseconds()}},
                    "mime_type": "text/plain",
                    "preview_text": "Simulated Item #{{randomId}}",
                    "device_id": "myself"
                }
            }
            """;
            // 直接往泵里灌数据，假装是 Core 发来的
            pump.Enqueue(json);
        });

        // 初始化命令
        ClearCacheCommand = new AsyncRelayCommand(ClearCacheAsync, () => _coreHost.State == CoreState.Ready);
        ToggleCaptureCommand = new RelayCommand(() => ClipboardCaptureEnabled = !ClipboardCaptureEnabled);
        ToggleSharingCommand = new RelayCommand(() => ClipboardSharingEnabled = !ClipboardSharingEnabled);
        NavigateToHistoryCommand = new RelayCommand(() =>
        {
            navigationService.NavigateTo(typeof(HistoryViewModel).FullName!);
        });
        SelectItemCommand = new AsyncRelayCommand<ItemMetaPayload>(SelectItemAsync);

        // 监听核心状态变化
        _coreHost.StateChanged += OnCoreStateChanged;
        _coreHost.StateChanged += (state) => ClearCacheCommand.NotifyCanExecuteChanged();

        // 监听设置变化
        _localSettings.SettingChanged += OnSettingChanged;

        // 监听历史变化，当有新项目添加时重新查询RecentItems
        _historyStore.Items.CollectionChanged += (s, e) =>
        {
            // 如果有新项目添加，重新查询RecentItems（确保显示最新的）
            if (e.NewItems != null && e.NewItems.Count > 0)
            {
                _ = LoadRecentItemsFromCoreAsync();
            }
        };

        // 加载设置并初始化RecentItems
        _ = LoadRecentItemsCountAndRefreshAsync();

        // 启动统计更新定时器
        StartStatsTimer();

        // 获取本机device_id（异步，不阻塞）
        _ = LoadLocalDeviceIdAsync();
    }

    private async Task LoadLocalDeviceIdAsync()
    {
        try
        {
            // 从LocalSettings读取本机device_id
            const string Key = "Core_DeviceId";
            _localDeviceId = await _localSettings.ReadSettingAsync<string>(Key);
            // #region agent log
            _ = LogAsync("A", "MainViewModel.cs:LoadLocalDeviceIdAsync", "Local Device ID loaded", new { LocalDeviceId = _localDeviceId, IsEmpty = string.IsNullOrEmpty(_localDeviceId) });
            // #endregion agent log
        }
        catch (Exception ex)
        {
            // 忽略错误
            // #region agent log
            _ = LogAsync("A", "MainViewModel.cs:LoadLocalDeviceIdAsync", "Failed to load Local Device ID", new { Error = ex.Message });
            // #endregion agent log
        }
    }

    private async Task LoadRecentItemsCountAndRefreshAsync()
    {
        // 从设置读取RecentItemsCount，默认10
        const string SettingKey = "MainPage_RecentItemsCount";
        var count = await _localSettings.ReadSettingAsync<int>(SettingKey);
        if (count <= 0) count = 10; // 默认10个
        _recentItemsCount = count;

        // 如果Core已就绪，立即加载历史记录
        if (_coreHost.State == CoreState.Ready)
        {
            await LoadRecentItemsFromCoreAsync();
        }
    }

    private void OnSettingChanged(object? sender, string key)
    {
        if (key == "MainPage_RecentItemsCount")
        {
            _ = LoadRecentItemsCountAndRefreshAsync();
        }
    }

    private async Task LoadRecentItemsFromCoreAsync()
    {
        if (_coreHost.State != CoreState.Ready) return;

        try
        {
            var query = new HistoryQuery
            {
                Limit = _recentItemsCount,
                Cursor = null // 第一页，获取最新的
            };

            var page = await _coreHost.ListHistoryAsync(query);
            
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                _recentItems.Clear();
                foreach (var item in page.Items)
                {
                    _recentItems.Add(item);
                }
            });
        }
        catch
        {
            // 忽略错误
        }
    }

    private async Task ClearCacheAsync()
    {
        if (_coreHost.State != CoreState.Ready) return;
        var handle = _coreHost.GetHandle();
        if (handle == IntPtr.Zero) return;

        await Task.Run(() =>
        {
            ClipBridgeShell_CS.Interop.CoreInterop.ClearCache(handle);
        });
    }

    private void OnCoreStateChanged(CoreState state)
    {
        if (state == CoreState.Ready)
        {
            StartStatsTimer();
            // Core就绪后，加载历史记录
            _ = LoadRecentItemsFromCoreAsync();
        }
        else
        {
            StopStatsTimer();
        }
    }

    private void StartStatsTimer()
    {
        StopStatsTimer();
        if (_coreHost.State != CoreState.Ready) return;

        _statsTimer = new DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(5)
        };
        _statsTimer.Tick += async (_, __) => await RefreshStatsAsync();
        _statsTimer.Start();
        _ = RefreshStatsAsync(); // 立即执行一次
    }

    private void StopStatsTimer()
    {
        if (_statsTimer != null)
        {
            _statsTimer.Stop();
            _statsTimer = null;
        }
    }

    private async Task RefreshStatsAsync()
    {
        if (_coreHost.State != CoreState.Ready) return;

        var handle = _coreHost.GetHandle();
        if (handle == IntPtr.Zero) return;

        await Task.Run(() =>
        {
            try
            {
                // 更新缓存统计
                var cacheStats = CoreInterop.QueryCacheStats(handle);
                App.MainWindow.DispatcherQueue.TryEnqueue(() =>
                {
                    CurrentCacheBytes = cacheStats.CurrentCacheBytes;
                    CacheSeries.Clear();
                    foreach (var point in cacheStats.Series)
                    {
                        CacheSeries.Add(point);
                    }
                });

                // 更新网络统计
                var netStats = CoreInterop.QueryNetStats(handle);
                App.MainWindow.DispatcherQueue.TryEnqueue(() =>
                {
                    NetworkSeries.Clear();
                    foreach (var point in netStats.Series)
                    {
                        NetworkSeries.Add(point);
                    }
                });

                // 更新活动统计
                var activityStats = CoreInterop.QueryActivityStats(handle);
                App.MainWindow.DispatcherQueue.TryEnqueue(() =>
                {
                    ActivitySeries.Clear();
                    foreach (var point in activityStats.Series)
                    {
                        ActivitySeries.Add(point);
                    }
                });
            }
            catch (Exception ex)
            {
                // Log error if needed
            }
        });

        // 更新设备统计（从Store）
        App.MainWindow.DispatcherQueue.TryEnqueue(() =>
        {
            var peers = _peerStore.Peers;
            // 当前PeerMetaPayload模型没有区分Outbound/Inbound，使用IsOnline和IsAllowed
            // Outbound: 在线且允许的设备数（可共享到的设备）
            OutboundPeerCount = peers.Count(p => p.IsOnline && p.IsAllowed);
            // Inbound: 在线且允许的设备数（可接收的设备，当前模型相同）
            InboundPeerCount = peers.Count(p => p.IsOnline && p.IsAllowed);
            ActiveTransfersCount = _transferStore.Transfers.Count(t => t.State == "downloading" || t.State == "uploading");
        });
    }

    private async Task SelectItemAsync(ItemMetaPayload? item)
    {
        if (item == null) return;

        // 更新选中状态
        SelectedItemId = item.ItemId;
        OnPropertyChanged(nameof(SelectedItemId));

        // 如果ClipboardApplyService可用，调用它来写入剪切板
        if (_clipboardApply != null)
        {
            try
            {
                await _clipboardApply.ApplyMetaToSystemClipboardAsync(item);
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[MainViewModel] ApplyMetaToSystemClipboardAsync failed: {ex}");
                // TODO: 显示错误提示
            }
        }
    }
}
