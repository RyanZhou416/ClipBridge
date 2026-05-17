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
                var netStats = CoreInterop.QueryNetStats(handle);
                App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
                {
                    NetworkSeries.Clear();
                    foreach (var point in netStats.Series)
                    {
                        NetworkSeries.Add(point);
                    }
                });

                // 更新活动统计
                var activityStats = CoreInterop.QueryActivityStats(handle);
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

    private void UnlockClipboard()
    {
        _clipboardService?.UnlockClipboard();
        _lockedItemMeta = null;
        LockedItemId = null;
        SelectedItemId = null;
        OnPropertyChanged(nameof(SelectedItemId));

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
