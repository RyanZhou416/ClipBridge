using System;
using System.Collections.ObjectModel;
using System.Collections.Specialized;
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
    private readonly HistoryStore _historyStore;
    private readonly ICoreHostService _coreHost;
    private readonly PeerStore _peerStore;
    private readonly TransferStore _transferStore;
    private readonly ILocalSettingsService _localSettings;
    private readonly ClipboardApplyService? _clipboardApply;
    private readonly IClipboardService? _clipboardService;
    private DispatcherTimer? _statsTimer;
    private int _recentItemsCount = 10;
    private string? _localDeviceId;
    private bool _enableAcrylicOnCards = true;
    private bool _enableAcrylicOnStatsCards = true;

    // 直接暴露 Store 的集合供 UI 绑定
    // 这样当 Store 更新时，UI 会自动刷新
    public ObservableCollection<ItemMetaPayload> HistoryItems => _historyStore.Items;

    // 最近N条历史（用于顶部卡片条），从Core数据库查询
    private readonly ObservableCollection<ItemMetaPayload> _recentItems = new();
    public ObservableCollection<ItemMetaPayload> RecentItems => _recentItems;

    private string? _selectedItemId;
    public string? SelectedItemId
    {
        get => _selectedItemId;
        set => SetProperty(ref _selectedItemId, value);
    }

    private string? _lockedItemId;
    public string? LockedItemId
    {
        get => _lockedItemId;
        set => SetProperty(ref _lockedItemId, value);
    }

    private ItemMetaPayload? _lockedItemMeta;
    public ItemMetaPayload? LockedItemMeta => _lockedItemMeta;

    public bool IsItemLocked(string itemId) => LockedItemId == itemId;

    /// <summary>是否关闭首页最近条目卡片的亚克力（= !EnableAcrylicOnCards，供绑定用）</summary>
    public bool DisableAcrylicOnCards
    {
        get => !_enableAcrylicOnCards;
        set
        {
            var newEnable = !value;
            if (_enableAcrylicOnCards != newEnable)
            {
                _enableAcrylicOnCards = newEnable;
                OnPropertyChanged(nameof(DisableAcrylicOnCards));
            }
        }
    }

    /// <summary>是否关闭首页统计信息卡片的亚克力（= !EnableAcrylicOnStatsCards，供绑定用）</summary>
    public bool DisableAcrylicOnStatsCards
    {
        get => !_enableAcrylicOnStatsCards;
        set
        {
            var newEnable = !value;
            if (_enableAcrylicOnStatsCards != newEnable)
            {
                _enableAcrylicOnStatsCards = newEnable;
                OnPropertyChanged(nameof(DisableAcrylicOnStatsCards));
            }
        }
    }

    // 复制状态提示
    private string? _copyStatusMessage;
    public string? CopyStatusMessage
    {
        get => _copyStatusMessage;
        set => SetProperty(ref _copyStatusMessage, value);
    }

    private bool _isCopyStatusOpen;
    public bool IsCopyStatusOpen
    {
        get => _isCopyStatusOpen;
        set => SetProperty(ref _isCopyStatusOpen, value);
    }

    private bool _isCopySuccess;
    public bool IsCopySuccess
    {
        get => _isCopySuccess;
        set => SetProperty(ref _isCopySuccess, value);
    }

    private void ShowCopyStatus(string message, bool success)
    {
        IsCopySuccess = success;
        CopyStatusMessage = message;
        IsCopyStatusOpen = true;
        _ = HideCopyStatusAfterDelayAsync();
    }

    private async Task HideCopyStatusAfterDelayAsync()
    {
        await Task.Delay(2000);
        IsCopyStatusOpen = false;
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
        if (item == null) return "Unknown";

        // 单次遍历传输列表：进行中的传输优先于失败传输，避免每张卡片两次线性扫描
        TransferUpdatePayload? transfer = null;
        TransferUpdatePayload? failedTransfer = null;
        foreach (var t in _transferStore.Transfers)
        {
            if (t.ItemId != item.ItemId) continue;

            if (t.State == "downloading" || t.State == "uploading")
            {
                transfer = t;
                break;
            }

            if (failedTransfer == null && t.State == "failed")
            {
                failedTransfer = t;
            }
        }

        if (transfer != null)
        {
            var progress = transfer.Progress;
            return progress > 0 ? $"Downloading {(int)(progress * 100)}%" : "Downloading";
        }

        if (failedTransfer != null)
        {
            var errorMsg = failedTransfer.Message ?? "Unknown error";
            if (errorMsg.Length > 20) errorMsg = errorMsg.Substring(0, 20) + "...";
            return $"Failed: {errorMsg}";
        }

        // 判断是否来自本机
        // 如果source_device_id为空，可能是本机内容（旧数据或特殊情况）
        if (string.IsNullOrEmpty(item.SourceDeviceId))
        {
            return "Ready"; // 假设是本机内容
        }

        // 如果source_device_id等于本机device_id，可能是Ready（本机内容）
        if (!string.IsNullOrEmpty(_localDeviceId) && item.SourceDeviceId == _localDeviceId)
        {
            return "Ready"; // 本机内容，假设已经在CAS中
        }

        // 如果source_device_id不等于本机device_id，说明来自其他设备
        // 默认应该是NeedsDownload（除非已经有CONTENT_CACHED事件，但我们在UI层面无法直接检查）
        // 为了简化，暂时假设来自其他设备的都是NeedsDownload
        // TODO: 可以通过检查HistoryStore中是否有对应的CONTENT_CACHED事件来判断是否已下载
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
    public IAsyncRelayCommand<ItemMetaPayload> DeleteRecentItemCommand { get; }
    public IAsyncRelayCommand<ItemMetaPayload> GlobalDeleteItemCommand { get; }

    public MainViewModel(
        HistoryStore historyStore,
        Services.EventPumpService pump,
        ICoreHostService coreHost,
        PeerStore peerStore,
        TransferStore transferStore,
        INavigationService navigationService,
        ILocalSettingsService localSettings,
        Services.ClipboardApplyService? clipboardApply = null,
        IClipboardService? clipboardService = null)
    {
        _historyStore = historyStore;
        _coreHost = coreHost;
        _peerStore = peerStore;
        _transferStore = transferStore;
        _localSettings = localSettings;
        _clipboardApply = clipboardApply;
        _clipboardService = clipboardService;

        // 统一解锁入口由 ClipboardService 发出：历史页删除锁定项等外部入口解锁后，
        // 首页也必须同步清空锁定态，否则 UI 继续显示锁定并拦截复制。
        if (_clipboardService != null)
            _clipboardService.ClipboardLockChanged += OnClipboardLockChanged;

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
        DeleteRecentItemCommand = new AsyncRelayCommand<ItemMetaPayload>(DeleteRecentItemAsync);
        GlobalDeleteItemCommand = new AsyncRelayCommand<ItemMetaPayload>(GlobalDeleteItemAsync);

        // 监听核心状态变化
        _coreHost.StateChanged += OnCoreStateChanged;
        _coreHost.StateChanged += (state) => ClearCacheCommand.NotifyCanExecuteChanged();

        // 监听设置变化
        _localSettings.SettingChanged += OnSettingChanged;

        // 监听历史变化：只做增量插入/移除，不再每次都向 Core 重查并重建整个列表
        _historyStore.Items.CollectionChanged += OnHistoryItemsChanged;

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
        }
        catch (Exception)
        {
            // 忽略错误
        }
    }

    private async Task LoadRecentItemsCountAndRefreshAsync()
    {
        // 从设置读取RecentItemsCount，默认10
        const string SettingKey = "MainPage_RecentItemsCount";
        var count = await _localSettings.ReadSettingAsync<int>(SettingKey);
        if (count <= 0) count = 10; // 默认10个
        _recentItemsCount = count;

        await LoadPerformanceSettingsAsync();

        // 如果Core已就绪，立即加载历史记录
        if (_coreHost.State == CoreState.Ready)
        {
            await LoadRecentItemsFromCoreAsync();
        }
    }

    private async Task LoadPerformanceSettingsAsync()
    {
        const string KeyCards = "Performance_EnableAcrylicOnCards";
        const string KeyStats = "Performance_EnableAcrylicOnStatsCards";
        var cards = await _localSettings.ReadSettingAsync<bool?>(KeyCards);
        var stats = await _localSettings.ReadSettingAsync<bool?>(KeyStats);
        App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
        {
            _enableAcrylicOnCards = cards ?? true;
            _enableAcrylicOnStatsCards = stats ?? true;
            OnPropertyChanged(nameof(DisableAcrylicOnCards));
            OnPropertyChanged(nameof(DisableAcrylicOnStatsCards));
        });
    }

    private void OnSettingChanged(object? sender, string key)
    {
        if (key == "MainPage_RecentItemsCount")
        {
            _ = LoadRecentItemsCountAndRefreshAsync();
        }
        else if (key == "Performance_EnableAcrylicOnCards" || key == "Performance_EnableAcrylicOnStatsCards")
        {
            _ = LoadPerformanceSettingsAsync();
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
            
            App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
            {
                _recentItems.Clear();

                // Pin locked item to top
                if (_lockedItemMeta != null)
                {
                    var freshLocked = page.Items.FirstOrDefault(i => i.ItemId == _lockedItemMeta.ItemId);
                    _recentItems.Add(freshLocked ?? _lockedItemMeta);
                }

                foreach (var item in page.Items)
                {
                    if (_lockedItemMeta != null && item.ItemId == _lockedItemMeta.ItemId)
                        continue;
                    _recentItems.Add(item);
                }
            });
        }
        catch
        {
            // 忽略错误
        }
    }

    /// <summary>
    /// 历史集合变化：只做增量插入/移除/原地替换，避免每次新增都向 Core 重查并重建整个列表。
    /// 首次加载、锁定态变化等需要权威数据的场景仍走 LoadRecentItemsFromCoreAsync。
    /// </summary>
    private void OnHistoryItemsChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        switch (e.Action)
        {
            case NotifyCollectionChangedAction.Add when e.NewItems != null:
                foreach (ItemMetaPayload item in e.NewItems)
                {
                    InsertRecentItem(item);
                }
                break;

            case NotifyCollectionChangedAction.Remove when e.OldItems != null:
                foreach (ItemMetaPayload item in e.OldItems)
                {
                    RemoveRecentItemById(item.ItemId);
                }
                break;

            case NotifyCollectionChangedAction.Replace when e.NewItems != null:
                foreach (ItemMetaPayload item in e.NewItems)
                {
                    var index = IndexOfRecentItem(item.ItemId);
                    if (index >= 0)
                    {
                        _recentItems[index] = item;
                    }
                }
                break;

            default:
                // Reset / Move 等少见操作：只做一次本地重排，不触发 FFI 查询
                RelayoutRecentItemsFromStore();
                break;
        }
    }

    private int IndexOfRecentItem(string? itemId)
    {
        if (string.IsNullOrEmpty(itemId)) return -1;
        for (var i = 0; i < _recentItems.Count; i++)
        {
            if (_recentItems[i].ItemId == itemId) return i;
        }
        return -1;
    }

    // 锁定项额外占用第 0 位，与 LoadRecentItemsFromCoreAsync 的“锁定项置顶”容量一致
    private int RecentItemsCapacity => _recentItemsCount + (_lockedItemMeta != null ? 1 : 0);

    private void TrimRecentItems()
    {
        var capacity = Math.Max(1, RecentItemsCapacity);
        while (_recentItems.Count > capacity)
        {
            _recentItems.RemoveAt(_recentItems.Count - 1);
        }
    }

    /// <summary>
    /// 按 CreatedTsMs 降序插入单条历史项；锁定项固定在第 0 位且不重复。
    /// </summary>
    private void InsertRecentItem(ItemMetaPayload? item)
    {
        if (item == null || string.IsNullOrEmpty(item.ItemId)) return;

        // 已存在则原地更新，保持现有位置
        var existingIndex = IndexOfRecentItem(item.ItemId);
        if (existingIndex >= 0)
        {
            _recentItems[existingIndex] = item;
            return;
        }

        var lockedId = _lockedItemMeta?.ItemId;
        if (!string.IsNullOrEmpty(lockedId) && item.ItemId == lockedId)
        {
            // 锁定项固定在索引 0
            _lockedItemMeta = item;
            _recentItems.Insert(0, item);
        }
        else
        {
            var startIndex = !string.IsNullOrEmpty(lockedId)
                             && _recentItems.Count > 0
                             && _recentItems[0].ItemId == lockedId
                ? 1
                : 0;

            var insertIndex = _recentItems.Count;
            for (var i = startIndex; i < _recentItems.Count; i++)
            {
                if (_recentItems[i].CreatedTsMs < item.CreatedTsMs)
                {
                    insertIndex = i;
                    break;
                }
            }

            _recentItems.Insert(insertIndex, item);
        }

        TrimRecentItems();
    }

    private void RemoveRecentItemById(string? itemId)
    {
        var index = IndexOfRecentItem(itemId);
        if (index >= 0)
        {
            _recentItems.RemoveAt(index);
        }
    }

    /// <summary>
    /// Reset/Move 场景下按内存中的 HistoryStore 做一次本地重排（不触发 FFI 查询）。
    /// </summary>
    private void RelayoutRecentItemsFromStore()
    {
        var capacity = Math.Max(1, RecentItemsCapacity);
        var ordered = _historyStore.Items
            .OrderByDescending(i => i.CreatedTsMs)
            .Take(capacity)
            .ToList();

        _recentItems.Clear();

        // Pin locked item to top
        if (_lockedItemMeta != null)
        {
            var freshLocked = ordered.FirstOrDefault(i => i.ItemId == _lockedItemMeta.ItemId);
            _recentItems.Add(freshLocked ?? _lockedItemMeta);
        }

        foreach (var item in ordered)
        {
            if (_recentItems.Count >= capacity) break;
            if (_lockedItemMeta != null && item.ItemId == _lockedItemMeta.ItemId)
                continue;
            _recentItems.Add(item);
        }
    }

    private async Task ClearCacheAsync()
    {
        if (_coreHost.State != CoreState.Ready) return;
        await _coreHost.ClearCacheAsync();
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

        await Task.Run(async () =>
        {
            try
            {
                // 更新缓存统计
                var cacheStats = await _coreHost.QueryCacheStatsAsync();
                App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
                {
                    CurrentCacheBytes = cacheStats.CurrentCacheBytes;
                    // 使用批量更新，减少 CollectionChanged 事件触发次数
                    // 先暂停通知，然后批量更新
                    var oldItems = CacheSeries.ToList();
                    CacheSeries.Clear();
                    foreach (var point in cacheStats.Series)
                    {
                        CacheSeries.Add(point);
                    }
                });

                // 更新网络统计
                var netStats = await _coreHost.QueryNetStatsAsync();
                App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
                {
                    NetworkSeries.Clear();
                    foreach (var point in netStats.Series)
                    {
                        NetworkSeries.Add(point);
                    }
                });

                // 更新活动统计
                var activityStats = await _coreHost.QueryActivityStatsAsync();
                App.MainWindow?.DispatcherQueue?.TryEnqueue(() =>
                {
                    ActivitySeries.Clear();
                    foreach (var point in activityStats.Series)
                    {
                        ActivitySeries.Add(point);
                    }
                });
            }
            catch (Exception)
            {
                // Log error if needed
            }
        });

        // 更新设备统计（从Store）- 优化：减少LINQ操作
        App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
        {
            var peers = _peerStore.Peers;
            var transfers = _transferStore.Transfers;
            
            // 只遍历一次集合来计算统计
            var outboundCount = 0;
            var inboundCount = 0;
            var activeTransfers = 0;
            
            foreach (var peer in peers)
            {
                if (peer.IsOnline && peer.IsAllowed)
                {
                    outboundCount++;
                    inboundCount++;
                }
            }
            
            foreach (var transfer in transfers)
            {
                if (transfer.State == "downloading" || transfer.State == "uploading")
                {
                    activeTransfers++;
                }
            }
            
            OutboundPeerCount = outboundCount;
            InboundPeerCount = inboundCount;
            ActiveTransfersCount = activeTransfers;
        });
    }

    /// <summary>
    /// 仅从 UI 移除（不调用核心）。保留用于兼容或内部使用。
    /// </summary>
    public void RemoveRecentItem(ItemMetaPayload item)
    {
        if (LockedItemId == item.ItemId)
        {
            UnlockClipboard();
        }
        _recentItems.Remove(item);
        if (SelectedItemId == item.ItemId)
        {
            SelectedItemId = null;
        }

        var loc = WinUI3Localizer.Localizer.Get();
        var msg = loc.GetLocalizedString("Main_ItemRemoved");
        ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_ItemRemoved" ? "Removed" : msg, true);
    }

    /// <summary>
    /// 本地删除：调用核心删除后从 UI 移除并提示。
    /// </summary>
    public async Task DeleteRecentItemAsync(ItemMetaPayload? item)
    {
        if (item == null) return;
        try
        {
            await _coreHost.DeleteItemLocalAsync(item.ItemId);
            if (LockedItemId == item.ItemId)
                UnlockClipboard();
            _recentItems.Remove(item);
            if (SelectedItemId == item.ItemId)
                SelectedItemId = null;

            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_ItemDeleted");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_ItemDeleted" ? "Deleted" : msg, true);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[MainViewModel] DeleteRecentItemAsync failed: {ex}");
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_DeleteFailed");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_DeleteFailed" ? "Delete failed" : msg, false);
        }
    }

    /// <summary>
    /// 全局删除：从所有设备删除后从 UI 移除并提示。
    /// </summary>
    public async Task GlobalDeleteItemAsync(ItemMetaPayload? item)
    {
        if (item == null) return;
        try
        {
            await _coreHost.DeleteItemGlobalAsync(item.ItemId);
            if (LockedItemId == item.ItemId)
                UnlockClipboard();
            _recentItems.Remove(item);
            if (SelectedItemId == item.ItemId)
                SelectedItemId = null;

            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_ItemDeletedGlobal");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_ItemDeletedGlobal" ? "Deleted from all devices" : msg, true);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[MainViewModel] GlobalDeleteItemAsync failed: {ex}");
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_DeleteFailed");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_DeleteFailed" ? "Delete failed" : msg, false);
        }
    }

    public async Task ToggleLockItemAsync(ItemMetaPayload item)
    {
        if (_clipboardService == null || _clipboardApply == null) return;

        if (LockedItemId == item.ItemId)
        {
            UnlockClipboard();
            _ = LoadRecentItemsFromCoreAsync();
            return;
        }

        try
        {
            await _clipboardApply.ApplyMetaToSystemClipboardAsync(item);

            var text = await _clipboardService.GetTextAsync();
            if (string.IsNullOrEmpty(text))
            {
                var loc = WinUI3Localizer.Localizer.Get();
                var msg = loc.GetLocalizedString("Main_LockFailed");
                ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_LockFailed" ? "Lock failed" : msg, false);
                return;
            }

            _clipboardService.LockClipboard(text, item.ItemId);
            _lockedItemMeta = item;
            LockedItemId = item.ItemId;
            SelectedItemId = item.ItemId;
            OnPropertyChanged(nameof(SelectedItemId));
            _ = LoadRecentItemsFromCoreAsync();

            var loc2 = WinUI3Localizer.Localizer.Get();
            var msg2 = loc2.GetLocalizedString("Main_ClipboardLocked");
            ShowCopyStatus(string.IsNullOrEmpty(msg2) || msg2 == "Main_ClipboardLocked" ? "Clipboard locked" : msg2, true);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[MainViewModel] ToggleLockItem failed: {ex}");
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_LockFailed");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_LockFailed" ? "Lock failed" : msg, false);
        }
    }

    /// <summary>
    /// ClipboardService 的统一解锁通知：任何入口解锁后同步清空首页锁定态
    /// （历史页删除锁定项也会走这里，避免首页仍拦截复制）。
    /// </summary>
    private void OnClipboardLockChanged(object? sender, bool isLocked)
    {
        if (isLocked) return;
        ClearLockedState();
    }

    /// <summary>解锁后的统一状态复位（锁定文本已由 ClipboardService 清空）。</summary>
    private void ClearLockedState()
    {
        _lockedItemMeta = null;
        LockedItemId = null;
        SelectedItemId = null;
        OnPropertyChanged(nameof(SelectedItemId));
    }

    private void UnlockClipboard()
    {
        _clipboardService?.UnlockClipboard();

        // 没有 ClipboardService（降级/测试环境）时不会触发解锁事件，这里兜底复位
        if (_clipboardService == null)
            ClearLockedState();

        var loc = WinUI3Localizer.Localizer.Get();
        var msg = loc.GetLocalizedString("Main_ClipboardUnlocked");
        ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_ClipboardUnlocked" ? "Clipboard unlocked" : msg, true);
    }

    private async Task SelectItemAsync(ItemMetaPayload? item)
    {
        if (item == null) return;
        if (_clipboardApply == null) return;

        if (_clipboardService != null && _clipboardService.IsClipboardLocked && item.ItemId != _lockedItemId)
        {
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_ClipboardIsLocked");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_ClipboardIsLocked" ? "Clipboard is locked" : msg, false);
            return;
        }

        SelectedItemId = item.ItemId;
        OnPropertyChanged(nameof(SelectedItemId));

        try
        {
            await _clipboardApply.ApplyMetaToSystemClipboardAsync(item);
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_CopySuccess");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_CopySuccess" ? "Copied!" : msg, true);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[MainViewModel] ApplyMetaToSystemClipboardAsync failed: {ex}");
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_CopyFailed");
            ShowCopyStatus(string.IsNullOrEmpty(msg) || msg == "Main_CopyFailed" ? "Copy failed" : msg, false);
        }
    }
}
