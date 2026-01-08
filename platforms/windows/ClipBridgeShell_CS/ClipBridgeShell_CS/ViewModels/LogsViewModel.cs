using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Input;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Models;
using SourceStats = ClipBridgeShell_CS.Interop.SourceStats;
using ClipBridgeShell_CS.Services.Logging;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.ViewModels;

public sealed class LogsViewModel : ObservableObject
{
    private static int _instanceCounter = 0;
    private readonly int _instanceId;

    // 可绑定到 ListView / ItemsRepeater
    public ObservableCollection<LogRow> Items { get; } = new();
    private readonly ICoreHostService _coreHost;
    private readonly StashLogManager? _stashManager;
    private readonly Services.EventPumpService? _eventPump;
    private readonly ILocalSettingsService? _localSettings;
    private string _currentLanguage = "en-US";
    private bool CanUseCore() => _coreHost.State == CoreState.Ready;
    
    /// <summary>
    /// 当前语言设置
    /// </summary>
    public string CurrentLanguage
    {
        get => _currentLanguage;
        private set
        {
            if (SetProperty(ref _currentLanguage, value))
            {
                // 语言变化时重新查询日志
                RestartTailNow();
            }
        }
    }
    // 过滤 & 状态
    private int _levelMin = 0;
    public int LevelMin
    {
        get => _levelMin;
        set => SetProperty(ref _levelMin, value);
    }

    private string? _filterText;
    public string? FilterText
    {
        get => _filterText;
        set
        {
            if (SetProperty(ref _filterText, value))
            {
                RestartTailNow();
            }
        }
    }
    
    // 高级过滤条件
    private DateTimeOffset? _filterStartTime;
    public DateTimeOffset? FilterStartTime
    {
        get => _filterStartTime;
        set
        {
            if (SetProperty(ref _filterStartTime, value))
            {
                ApplyFilters();
            }
        }
    }
    
    private DateTimeOffset? _filterEndTime;
    public DateTimeOffset? FilterEndTime
    {
        get => _filterEndTime;
        set
        {
            if (SetProperty(ref _filterEndTime, value))
            {
                ApplyFilters();
            }
        }
    }
    
    private HashSet<int> _filterLevels = new HashSet<int> { 0, 1, 2, 3, 4, 5 }; // 默认全部选中
    public HashSet<int> FilterLevels
    {
        get => _filterLevels;
        set
        {
            // 比较内容而不是引用
            if (value == null || !_filterLevels.SetEquals(value))
            {
                _filterLevels = value ?? new HashSet<int> { 0, 1, 2, 3, 4, 5 };
                OnPropertyChanged();
                ApplyFilters();
            }
        }
    }
    
    private HashSet<string> _filterComponents = new HashSet<string>(); // 空集合表示不过滤
    public HashSet<string> FilterComponents
    {
        get => _filterComponents;
        set
        {
            // 比较内容而不是引用
            if (value == null || !_filterComponents.SetEquals(value))
            {
                _filterComponents = value ?? new HashSet<string>();
                OnPropertyChanged();
                ApplyFilters();
            }
        }
    }
    
    private HashSet<string> _filterCategories = new HashSet<string>(); // 空集合表示不过滤
    public HashSet<string> FilterCategories
    {
        get => _filterCategories;
        set
        {
            // 比较内容而不是引用
            if (value == null || !_filterCategories.SetEquals(value))
            {
                _filterCategories = value ?? new HashSet<string>();
                OnPropertyChanged();
                ApplyFilters();
            }
        }
    }
    
    // 所有原始日志（未过滤）
    private ObservableCollection<LogRow> _allItems = new();
    
    // 应用过滤条件
    private void ApplyFilters()
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] ApplyFilters called: _allItems.Count={_allItems.Count}, FilterLevels.Count={_filterLevels.Count}, FilterComponents.Count={_filterComponents.Count}, FilterCategories.Count={_filterCategories.Count}");
        var separatorCount = _allItems.Count(item => item.Id < 0 && item.Category == "Separator");
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] ApplyFilters: separator count in _allItems={separatorCount}");
        // #endregion
        var filtered = _allItems.Where(item => MatchesFilter(item)).ToList();
        // #region agent log
        var separatorCountInFiltered = filtered.Count(item => item.Id < 0 && item.Category == "Separator");
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] ApplyFilters: filtered.Count={filtered.Count}, separator count in filtered={separatorCountInFiltered}, before clear Items.Count={Items.Count}");
        // #endregion
        Items.Clear();
        foreach (var item in filtered)
        {
            Items.Add(item);
        }
        // #region agent log
        var separatorCountInItems = Items.Count(item => item.Id < 0 && item.Category == "Separator");
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] ApplyFilters: after add Items.Count={Items.Count}, separator count in Items={separatorCountInItems}");
        // #endregion
    }
    
    // 检查日志项是否匹配过滤条件
    private bool MatchesFilter(LogRow item)
    {
        // 分隔符始终显示，不受过滤条件影响
        if (item.Id < 0 && item.Category == "Separator")
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] MatchesFilter: separator item {item.Id} always passes filter");
            // #endregion
            return true;
        }
        
        // 时间范围过滤
        if (_filterStartTime.HasValue && item.Time < _filterStartTime.Value)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] MatchesFilter: item {item.Id} filtered by startTime");
            // #endregion
            return false;
        }
        if (_filterEndTime.HasValue && item.Time > _filterEndTime.Value)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] MatchesFilter: item {item.Id} filtered by endTime");
            // #endregion
            return false;
        }
        
        // 级别过滤
        if (!_filterLevels.Contains(item.Level))
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] MatchesFilter: item {item.Id} filtered by level {item.Level}, FilterLevels={string.Join(",", _filterLevels)}");
            // #endregion
            return false;
        }
        
        // 组件过滤
        if (_filterComponents.Count > 0 && !_filterComponents.Contains(item.Component))
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] MatchesFilter: item {item.Id} filtered by component {item.Component}, FilterComponents={string.Join(",", _filterComponents)}");
            // #endregion
            return false;
        }
        
        // 分类过滤
        if (_filterCategories.Count > 0 && !_filterCategories.Contains(item.Category))
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] MatchesFilter: item {item.Id} filtered by category {item.Category}, FilterCategories={string.Join(",", _filterCategories)}");
            // #endregion
            return false;
        }
        
        return true;
    }
    private bool _autoScroll = true;
    public bool AutoScroll
    {
        get => _autoScroll;
        set => SetProperty(ref _autoScroll, value);
    }
    
    // 滚动状态管理
    private bool _isScrolledToBottom = true; // 默认滚动到底部
    public bool IsScrolledToBottom
    {
        get => _isScrolledToBottom;
        set => SetProperty(ref _isScrolledToBottom, value);
    }
    
    private long _lastId = 0; // tail 用（最新日志的 ID）
    private long _firstId = 0; // 当前已加载的最早日志的 ID（用于按需加载）
    private DispatcherTimer? _timer;
    private int _emptyQueryCount = 0; // 连续空查询次数，用于退避策略
    private CoreState _previousCoreState = CoreState.NotInitialized; // 跟踪上一个核心状态，用于插入分隔符
    private long _separatorIdCounter = -1; // 分隔符ID计数器（使用负数，避免与真实日志ID冲突）
    private HashSet<long> _processedInitLogIds = new(); // 跟踪已处理过的"Core initializing"日志ID，避免重复插入分隔符

    // 历史查询（分页）
    private DateTimeOffset _rangeStart = DateTimeOffset.Now.AddHours(-1);
    public DateTimeOffset RangeStart
    {
        get => _rangeStart;
        set => SetProperty(ref _rangeStart, value);
    }

    private DateTimeOffset _rangeEnd = DateTimeOffset.Now;
    public DateTimeOffset RangeEnd
    {
        get => _rangeEnd;
        set => SetProperty(ref _rangeEnd, value);
    }

    public TimeSpan RangeStartTime
    {
        get => RangeStart.TimeOfDay;
        set => RangeStart = new DateTimeOffset(RangeStart.Date + value, RangeStart.Offset);
    }

    public TimeSpan RangeEndTime
    {
        get => RangeEnd.TimeOfDay;
        set => RangeEnd = new DateTimeOffset(RangeEnd.Date + value, RangeEnd.Offset);
    }

    public int PageSize { get; set; } = 500;
    public int PageIndex { get; set; } = 0;

    // 统计
    private LogStats _stats = new();
    public LogStats Stats
    {
        get => _stats;
        private set => SetProperty(ref _stats, value);
    }

    // 来源统计
    private SourceStats? _sourceStats;
    public SourceStats? SourceStats
    {
        get => _sourceStats;
        private set => SetProperty(ref _sourceStats, value);
    }
    
    // 从现有日志统计组件和分类
    public SourceStats GetSourceStatsFromItems()
    {
        var stats = new SourceStats();
        foreach (var item in _allItems)
        {
            // 统计组件
            if (!string.IsNullOrEmpty(item.Component))
            {
                if (stats.Components.ContainsKey(item.Component))
                {
                    stats.Components[item.Component]++;
                }
                else
                {
                    stats.Components[item.Component] = 1;
                }
            }
            
            // 统计分类
            if (!string.IsNullOrEmpty(item.Category))
            {
                if (stats.Categories.ContainsKey(item.Category))
                {
                    stats.Categories[item.Category]++;
                }
                else
                {
                    stats.Categories[item.Category] = 1;
                }
            }
        }
        return stats;
    }

    // 选择模式
    private bool _isSelectionMode = false;
    public bool IsSelectionMode
    {
        get => _isSelectionMode;
        set => SetProperty(ref _isSelectionMode, value);
    }

    // 选中的日志 ID 列表
    public ObservableCollection<long> SelectedLogIds { get; } = new();
    
    // 列宽（用于同步表头和内容区域）
    private double _col0Width = 30;
    public double Col0Width
    {
        get => _col0Width;
        set => SetProperty(ref _col0Width, value);
    }
    
    private double _col1Width = 200;
    public double Col1Width
    {
        get => _col1Width;
        set => SetProperty(ref _col1Width, value);
    }
    
    private double _col2Width = 100;
    public double Col2Width
    {
        get => _col2Width;
        set => SetProperty(ref _col2Width, value);
    }
    
    private double _col3Width = 100;
    public double Col3Width
    {
        get => _col3Width;
        set => SetProperty(ref _col3Width, value);
    }
    
    private double _col4Width = 150;
    public double Col4Width
    {
        get => _col4Width;
        set => SetProperty(ref _col4Width, value);
    }
    
    private double _col6Width = 250;
    public double Col6Width
    {
        get => _col6Width;
        set => SetProperty(ref _col6Width, value);
    }

    // 命令
    public IRelayCommand StartTailCmd { get; }
    public IRelayCommand StopTailCmd { get; }
    public IRelayCommand ClearViewCmd { get; }
    public IRelayCommand RefreshStatsCmd { get; }
    public IRelayCommand QueryPageCmd { get; }
    public IRelayCommand DeleteBeforeCmd { get; }
    public IRelayCommand ExportCsvCmd { get; }
    public IRelayCommand DeleteSelectedCmd { get; }
    public IRelayCommand DeleteAllCmd { get; }
    public IRelayCommand SelectAllCmd { get; }
    public IRelayCommand DeselectAllCmd { get; }
    public IRelayCommand RefreshSourceStatsCmd { get; }

    public LogsViewModel(ICoreHostService coreHost, StashLogManager? stashManager = null, Services.EventPumpService? eventPump = null, ILocalSettingsService? localSettings = null)
    {
        _instanceId = Interlocked.Increment(ref _instanceCounter);
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] Constructor: creating new instance");
        // #endregion
        
        _coreHost = coreHost;
        _stashManager = stashManager;
        _eventPump = eventPump;
        _localSettings = localSettings;

        // 初始化语言设置（同步初始化，确保在查询日志前完成）
        // 注意：这里不能使用 await，因为构造函数不能是 async
        // 所以先同步获取语言，异步部分稍后执行
        try
        {
            var loc = WinUI3Localizer.Localizer.Get();
            var locLang = loc.GetCurrentLanguage();
            CurrentLanguage = NormalizeLanguageTag(locLang);
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: initial CurrentLanguage={CurrentLanguage} from WinUI3Localizer");
            // #endregion
        }
        catch
        {
            CurrentLanguage = "en-US";
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: failed to get language, defaulting to en-US");
            // #endregion
        }
        
        // 异步部分：从设置中读取保存的语言（如果有）
        _ = InitializeLanguageAsync();

        // 订阅日志写入事件
        if (_eventPump != null)
        {
            _eventPump.LogWritten += OnLogWritten;
        }

        // 1. 初始化命令，绑定 CanExecute (CanUseCore)
        StartTailCmd = new RelayCommand(async () => await StartTailAsync(), CanUseCore);

        // Stop 和 Clear 不需要 Core，随时可用
        StopTailCmd = new RelayCommand(StopTail);
        ClearViewCmd = new RelayCommand(() => { _allItems.Clear(); Items.Clear(); _lastId = 0; _firstId = 0; _processedInitLogIds.Clear(); });

        RefreshStatsCmd = new RelayCommand(async () => await RefreshStatsAsync(), CanUseCore);
        QueryPageCmd = new RelayCommand(async () => await QueryPageAsync(), CanUseCore);

        DeleteBeforeCmd = new RelayCommand(async () =>
        {
            var cutoff = DateTimeOffset.Now.AddDays(-7).ToUnixTimeMilliseconds();
            try
            {
                var handle = _coreHost.GetHandle();
                if (handle != IntPtr.Zero)
                {
                    CoreInterop.LogsDeleteBefore(handle, cutoff);
                    await RefreshStatsAsync();
                }
            }
            catch (Exception ex) { /* TODO: Show error */ System.Diagnostics.Debug.WriteLine(ex); }
        }, CanUseCore);

        ExportCsvCmd = new RelayCommand(async () => await ExportCsvAsync()); // 导出现有数据不需要 Core

        DeleteSelectedCmd = new RelayCommand(async () => await DeleteSelectedAsync(), () => SelectedLogIds.Count > 0);
        DeleteAllCmd = new RelayCommand(async () => await DeleteAllAsync(), CanUseCore);
        SelectAllCmd = new RelayCommand(() => 
        {
            SelectedLogIds.Clear();
            foreach (var item in Items)
            {
                item.IsSelected = true;
                SelectedLogIds.Add(item.Id);
            }
        }, () => Items.Count > 0);
        DeselectAllCmd = new RelayCommand(() => 
        {
            foreach (var item in Items)
            {
                item.IsSelected = false;
            }
            SelectedLogIds.Clear();
        }, () => SelectedLogIds.Count > 0);
        RefreshSourceStatsCmd = new RelayCommand(async () => await RefreshSourceStatsAsync(), CanUseCore);

        // 2. 订阅状态变化，以便自动刷新按钮状态
        _coreHost.StateChanged += OnCoreStateChanged;
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: subscribed to StateChanged event");
        // #endregion

        // 初始化上一个核心状态
        var initialState = _coreHost.State;
        _previousCoreState = initialState;
        
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: initial CoreState={initialState}, _previousCoreState={_previousCoreState}");
        // #endregion

        // 3. 如果核心未就绪，加载暂存日志
        if (!CanUseCore() && _stashManager != null)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] Constructor: Core not ready, loading stashed logs");
            // #endregion
            LoadStashedLogs();
            
            // 如果核心状态是 NotLoaded 或 ShuttingDown，且数据库中有核心初始化的日志（说明之前核心是 Ready 的），
            // 应该在加载日志后插入"核心关"分隔符
            // 注意：这里需要异步查询数据库，所以延迟到 StartTailAsync 中处理
            // 但为了确保分隔符在正确的位置，我们在 StartTailAsync 中检查并插入
        }
        // 4. 如果核心就绪，先加载暂存日志（如果有），然后启动tail
        else if (CanUseCore())
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: Core ready, checking for stashed logs before starting tail");
            // #endregion
            
            // 即使核心就绪，也先加载暂存日志（如果有）
            // 这样可以显示在核心启动之前产生的日志
            // 注意：暂存日志可能已经被回写到核心，但为了确保显示，我们仍然从暂存中加载
            // 如果暂存日志已经被回写，它们会在核心查询结果中出现，但通过ID去重可以避免重复显示
            if (_stashManager != null && _stashManager.HasStashedLogs())
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: Core ready but stashed logs exist, loading them first");
                // #endregion
                LoadStashedLogs();
            }
            
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: Core ready, starting tail and refresh");
            // #endregion
            
            // 不在这里插入分隔符，让TickOnce在检测到"Core initializing"日志时插入
            // 这样可以避免重复插入
            
            _ = StartTailAsync();
            _ = RefreshStatsAsync();
        }
        else
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: Core not ready and no stash manager");
            // #endregion
        }
    }

    private void LoadStashedLogs()
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] LoadStashedLogs called: stashManager={_stashManager != null}");
        // #endregion
        if (_stashManager == null)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] LoadStashedLogs skipped: stashManager is null");
            // #endregion
            return;
        }
        var stashedLogs = _stashManager.ReadAllLogs();
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] LoadStashedLogs: found {stashedLogs.Count} stashed logs");
        // #endregion
        foreach (var entry in stashedLogs)
        {
            // 根据当前语言选择消息
            string displayMessage;
            if (CurrentLanguage.StartsWith("zh", StringComparison.OrdinalIgnoreCase))
            {
                // 中文：优先使用 MessageZhCn，否则使用 Message
                displayMessage = !string.IsNullOrEmpty(entry.GetMessageZhCn()) 
                    ? entry.GetMessageZhCn() 
                    : entry.Message;
            }
            else
            {
                // 英文：优先使用 MessageEn，否则使用 Message
                displayMessage = entry.GetMessageEn();
            }
            
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] LoadStashedLogs: loading entry id={entry.Id}, ts_utc={entry.TsUtc}, CurrentLanguage={CurrentLanguage}, MessageEn={entry.GetMessageEn()?.Substring(0, Math.Min(50, entry.GetMessageEn()?.Length ?? 0))}, MessageZhCn={entry.GetMessageZhCn()?.Substring(0, Math.Min(50, entry.GetMessageZhCn()?.Length ?? 0))}, displayMessage={displayMessage.Substring(0, Math.Min(50, displayMessage.Length))}");
            // #endregion
            
            // 检查是否已存在（避免重复，如果暂存日志已经被回写到核心）
            bool alreadyExists = _allItems.Any(item => item.Id == entry.Id || (item.Time_Unix == entry.TsUtc && item.Component == entry.Component && item.Category == entry.Category && item.Message == displayMessage));
            if (alreadyExists)
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel] LoadStashedLogs: skipping duplicate entry id={entry.Id}");
                // #endregion
                continue;
            }
            
            _allItems.Add(new LogRow
            {
                Id = entry.Id,
                Time_Unix = entry.TsUtc,
                Level = entry.Level,
                Component = entry.Component,
                Category = entry.Category,
                Message = displayMessage, // 使用本地化后的消息
                Exception = entry.Exception,
                Props_Json = entry.PropsJson
            });
        }
        
        // 按时间戳和ID排序，确保暂存日志在正确的位置
        var sortedItems = _allItems.OrderBy(item => item.Time_Unix).ThenBy(item => item.Id).ToList();
        _allItems.Clear();
        foreach (var item in sortedItems)
        {
            _allItems.Add(item);
        }
        ApplyFilters();
        if (stashedLogs.Count > 0)
        {
            _lastId = stashedLogs.Max(e => e.Id);
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] LoadStashedLogs: loaded {stashedLogs.Count} logs, lastId={_lastId}, itemsCount={Items.Count}");
            // #endregion
        }
    }

    private async Task RefreshStatsAsync()
    {
        if (!CanUseCore()) return;
        var handle = _coreHost.GetHandle();
        if (handle == IntPtr.Zero) return;
        Stats = CoreInterop.LogsStats(handle);
        await Task.CompletedTask;
    }

    // -------------------- Tail 回调 + 后备轮询 --------------------
    private async Task StartTailAsync()
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] StartTailAsync called: CanUseCore={CanUseCore()}, CoreState={_coreHost.State}");
        // #endregion
        StopTail();
        if (!CanUseCore())
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] StartTailAsync skipped: Core not ready");
            // #endregion
            return; // 双重保险
        }

        // 立即查询一次
        _ = Task.Run(() => TickOnce());

        // 启动后备轮询定时器（降低频率到10秒，作为事件丢失的后备）
        _timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(10) }; // 后备间隔10秒
        _timer.Tick += (_, __) => TickOnce();
        _timer.Start();
        _emptyQueryCount = 0; // 重置空查询计数
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Tail started: callback-based with 10s fallback polling, lastId={_lastId}");
        // #endregion
        await Task.CompletedTask;
    }

    /// <summary>
    /// 当核心发送日志写入事件时触发
    /// </summary>
    private void OnLogWritten(object? sender, EventArgs e)
    {
        // 在后台线程执行查询，避免阻塞事件处理
        _ = Task.Run(() => TickOnce());
    }

    private void StopTail()
    {
        if (_timer is not null) { _timer.Stop(); _timer = null; }
    }

    private void RestartTailNow()
    {
        if (_timer is null) return;
        _lastId = 0; 
        _allItems.Clear(); 
        Items.Clear();
        _processedInitLogIds.Clear(); // 清空已处理的日志ID，以便重新检测分隔符
    }

    private void TickOnce()
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] TickOnce called: lastId={_lastId}, CanUseCore={CanUseCore()}");
        // #endregion
        // 如果运行中 Core 突然挂了，停止 Timer
        if (!CanUseCore())
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] TickOnce stopped: Core not ready");
            // #endregion
            StopTail();
            return;
        }

        try
        {
            var handle = _coreHost.GetHandle();
            if (handle == IntPtr.Zero)
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel] TickOnce skipped: handle is zero");
                // #endregion
                return;
            }

            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Querying logs: afterId={_lastId}, levelMin={LevelMin}, filter={FilterText}, lang={CurrentLanguage}");
            // #endregion
            var batch = CoreInterop.LogsQueryAfterId(handle, _lastId, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, 500, CurrentLanguage);
            // #region agent log
            if (batch.Count > 0)
            {
                var firstMsg = batch[0].Message;
                var firstId = batch[0].Id;
                var firstTs = batch[0].Time_Unix;
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel] First log message sample (first 100 chars): {firstMsg.Substring(0, Math.Min(100, firstMsg.Length))}, isJson={firstMsg.TrimStart().StartsWith("{")}, id={firstId}, ts_utc={firstTs}");
                if (batch.Count > 1)
                {
                    var lastId = batch[batch.Count - 1].Id;
                    var lastTs = batch[batch.Count - 1].Time_Unix;
                    System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Last log in batch: id={lastId}, ts_utc={lastTs}");
                }
            }
            // #endregion
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Query returned: batchCount={batch.Count}, currentItemsCount={Items.Count}");
            // #endregion
            if (batch.Count == 0)
            {
                // 后备轮询：空查询时不调整间隔（保持10秒）
                // 因为主要依赖事件回调，轮询只是后备
                return;
            }
            
            // 有新日志，重置空查询计数
            _emptyQueryCount = 0;
            
            // 在UI线程更新集合
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                // #region agent log
                var separatorCountBefore = _allItems.Count(item => item.Id < 0 && item.Category == "Separator");
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] TickOnce: before adding batch, _allItems.Count={_allItems.Count}, separatorCount={separatorCountBefore}");
                // #endregion
                
                foreach (var row in batch)
                {
                    // 检查是否已存在（避免重复添加，特别是暂存日志可能已经被回写到核心）
                    bool alreadyExists = _allItems.Any(item => item.Id == row.Id);
                    if (alreadyExists)
                    {
                        // #region agent log
                        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] TickOnce: skipping duplicate log id={row.Id}");
                        // #endregion
                        // 更新_lastId，但不添加重复项
                        if (row.Id > _lastId)
                        {
                            _lastId = row.Id;
                        }
                        continue;
                    }
                    
                    // #region agent log
                    // 在添加日志前检查是否匹配初始化日志条件
                    bool isInitLog = row.Component == "Core" && 
                                     row.Category == "Init" && 
                                     row.Message != null && 
                                     (row.Message.Contains("Core initializing", StringComparison.OrdinalIgnoreCase) ||
                                      row.Message.Contains("核心正在初始化", StringComparison.OrdinalIgnoreCase) ||
                                      row.Message.Contains("核心初始化", StringComparison.OrdinalIgnoreCase) ||
                                      (row.Message.Contains("初始化", StringComparison.OrdinalIgnoreCase) && row.Message.Contains("设备 ID", StringComparison.OrdinalIgnoreCase)));
                    bool alreadyProcessed = _processedInitLogIds.Contains(row.Id);
                    if (isInitLog)
                    {
                        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] TickOnce: checking init log: id={row.Id}, Component={row.Component}, Category={row.Category}, Message='{row.Message.Substring(0, Math.Min(80, row.Message.Length))}...', isInitLog={isInitLog}, alreadyProcessed={alreadyProcessed}");
                    }
                    // #endregion
                    
                    _allItems.Add(row);
                    _lastId = row.Id;
                    
                    // 标记已处理的初始化日志（分隔符现在从数据库读取，不需要在这里插入）
                    if (isInitLog && !alreadyProcessed)
                    {
                        // #region agent log
                        System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] TickOnce: detected 'Core initializing' log with id={row.Id}, message='{row.Message.Substring(0, Math.Min(50, row.Message.Length))}...'");
                        // #endregion
                        _processedInitLogIds.Add(row.Id);
                    }
                }
                
                // #region agent log
                var separatorCountAfter = _allItems.Count(item => item.Id < 0 && item.Category == "Separator");
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] TickOnce: after adding batch, _allItems.Count={_allItems.Count}, separatorCount={separatorCountAfter}");
                // #endregion
                
                // 应用过滤
                ApplyFilters();
                // 如果这是第一次加载，设置 firstId
                if (_firstId == 0 && _allItems.Count > 0)
                {
                    // 找到第一个非分隔符项作为 firstId
                    var firstNonSeparator = _allItems.FirstOrDefault(item => item.Id >= 0);
                    if (firstNonSeparator != null)
                    {
                        _firstId = firstNonSeparator.Id;
                    }
                }
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Added {batch.Count} items, newLastId={_lastId}, newItemsCount={Items.Count}, _allItemsCount={_allItems.Count}");
                // #endregion
                // 只有在滚动到底部时才自动滚动
                if (AutoScroll && IsScrolledToBottom)
                {
                    TailRequested?.Invoke(this, EventArgs.Empty);
                }
            });
        }
        catch (Exception ex)
        {
            // 如果出错（例如 DLL 调用失败），停止 Tail
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] TickOnce error: {ex.Message}, stopping tail");
            // #endregion
            StopTail();
            System.Diagnostics.Debug.WriteLine($"Tail error: {ex.Message}");
        }
    }

    public event EventHandler? TailRequested; // 给页面滚动用
    public event EventHandler? ScrollToBottomRequested; // 请求滚动到底部
    public event EventHandler<LoadOlderLogsEventArgs>? LoadOlderLogsRequested; // 请求加载更早的日志
    
    /// <summary>
    /// 加载更早的日志（向上滚动时按需加载）
    /// </summary>
    public async Task LoadOlderLogsAsync()
    {
        if (!CanUseCore() || _firstId == 0) return;
        
        try
        {
            var handle = _coreHost.GetHandle();
            if (handle == IntPtr.Zero) return;

            var batch = CoreInterop.LogsQueryBeforeId(handle, _firstId, LevelMin, 
                string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, 500, CurrentLanguage);
            
            if (batch.Count == 0) return; // 没有更早的日志了
            
            // 在 UI 线程插入到列表开头
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                // 记录当前第一个元素的 ID，用于保持滚动位置
                long? oldFirstId = Items.Count > 0 ? Items[0].Id : null;
                
                // 将新日志插入到列表开头（按时间升序，所以是倒序插入）
                for (int i = batch.Count - 1; i >= 0; i--)
                {
                    Items.Insert(0, batch[i]);
                }
                
                _firstId = batch[0].Id; // 更新最早日志 ID
                
                // 通知页面需要保持滚动位置
                LoadOlderLogsRequested?.Invoke(this, new LoadOlderLogsEventArgs 
                { 
                    OldFirstId = oldFirstId,
                    NewFirstId = _firstId,
                    AddedCount = batch.Count
                });
            });
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"LoadOlderLogs error: {ex.Message}");
        }
        
        await Task.CompletedTask;
    }
    
    /// <summary>
    /// 滚动到底部并启用吸附
    /// </summary>
    public void ScrollToBottom()
    {
        IsScrolledToBottom = true;
        ScrollToBottomRequested?.Invoke(this, EventArgs.Empty);
    }

    // -------------------- 历史查询（分页） --------------------
    private async Task QueryPageAsync()
    {
        if (!CanUseCore())
        {
            // 核心未就绪，只显示暂存日志
            if (_stashManager != null)
            {
                Items.Clear();
                LoadStashedLogs();
            }
            return;
        }
        StopTail(); // 停止实时，以免干扰
        _allItems.Clear();
        Items.Clear();
        var handle = _coreHost.GetHandle();
        if (handle == IntPtr.Zero) return;

        var startMs = RangeStart.ToUnixTimeMilliseconds();
        var endMs = RangeEnd.ToUnixTimeMilliseconds();
        var list = CoreInterop.LogsQueryRange(handle, startMs, endMs, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, PageSize, PageIndex * PageSize, CurrentLanguage);

        // 合并暂存日志（如果存在）
        if (_stashManager != null)
        {
            var stashedLogs = _stashManager.ReadAllLogs()
                .Where(e => e.TsUtc >= startMs && e.TsUtc <= endMs)
                .Select(e => new LogRow
                {
                    Id = e.Id,
                    Time_Unix = e.TsUtc,
                    Level = e.Level,
                    Component = e.Component,
                    Category = e.Category,
                    Message = e.Message,
                    Exception = e.Exception,
                    Props_Json = e.PropsJson
                });
            list = list.Concat(stashedLogs).OrderBy(r => r.Time_Unix).ThenBy(r => r.Id).ToList();
        }

        foreach (var row in list) _allItems.Add(row);
        ApplyFilters();
        await Task.CompletedTask;
    }

    // -------------------- 导出 CSV（简单版） --------------------
    private async Task ExportCsvAsync()
    {
        // 简化：导出当前 Items
        var sb = new System.Text.StringBuilder();
        sb.AppendLine("id,time,level,category,message,exception");
        foreach (var r in Items)
        {
            var line = string.Join(",", new[]
            {
                r.Id.ToString(),
                r.Time.ToString("yyyy-MM-dd HH:mm:ss.fff"),
                r.LevelName,
                CsvEscape(r.Category),
                CsvEscape(r.Message),
                CsvEscape(r.Exception ?? "")
            });
            sb.AppendLine(line);
        }

        var path = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Desktop), $"logs_{DateTime.Now:yyyyMMdd_HHmmss}.csv");
        await File.WriteAllTextAsync(path, sb.ToString());
        // 你也可以在 UI 做个 toast 提示
    }

    private static string CsvEscape(string s)
    {
        if (s.Contains('"') || s.Contains(',') || s.Contains('\n') || s.Contains('\r'))
        {
            return "\"" + s.Replace("\"", "\"\"") + "\"";
        }
        return s;
    }

    // -------------------- 删除功能 --------------------
    private async Task DeleteSelectedAsync()
    {
        if (SelectedLogIds.Count == 0 || !CanUseCore()) return;
        
        try
        {
            var handle = _coreHost.GetHandle();
            if (handle == IntPtr.Zero) return;
            
            var ids = SelectedLogIds.ToArray();
            var deleted = CoreInterop.LogsDeleteByIds(handle, ids);
            
            // 从 Items 中移除已删除的项
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                var itemsToRemove = Items.Where(item => ids.Contains(item.Id)).ToList();
                foreach (var item in itemsToRemove)
                {
                    Items.Remove(item);
                }
                SelectedLogIds.Clear();
                _ = RefreshStatsAsync();
            });
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"DeleteSelected error: {ex.Message}");
        }
        
        await Task.CompletedTask;
    }
    
    private async Task DeleteAllAsync()
    {
        if (!CanUseCore()) return;
        
        try
        {
            var handle = _coreHost.GetHandle();
            if (handle == IntPtr.Zero) return;
            
            CoreInterop.ClearLogsDb(handle);
            
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                Items.Clear();
                SelectedLogIds.Clear();
                _lastId = 0;
                _firstId = 0;
                _ = RefreshStatsAsync();
            });
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"DeleteAll error: {ex.Message}");
        }
        
        await Task.CompletedTask;
    }
    
    // -------------------- 来源统计 --------------------
    private async Task RefreshSourceStatsAsync()
    {
        if (!CanUseCore()) return;
        var handle = _coreHost.GetHandle();
        if (handle == IntPtr.Zero) return;
        
        try
        {
            SourceStats = CoreInterop.LogsSourceStats(handle);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"RefreshSourceStats error: {ex.Message}");
        }
        
        await Task.CompletedTask;
    }

    // [新增] 状态变更处理
    private void OnCoreStateChanged(CoreState state)
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] OnCoreStateChanged: called with state={state}, _previousCoreState={_previousCoreState}");
        // #endregion
        
        // 确保在 UI 线程刷新命令状态
        App.MainWindow.DispatcherQueue.TryEnqueue(() =>
        {
            // 更新状态（分隔符现在从数据库读取，不需要在这里插入）
            if (_previousCoreState != state)
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[LogsViewModel#{_instanceId}] OnCoreStateChanged: state changed from {_previousCoreState} to {state}");
                // #endregion
                _previousCoreState = state;
            }

            StartTailCmd.NotifyCanExecuteChanged();
            RefreshStatsCmd.NotifyCanExecuteChanged();
            QueryPageCmd.NotifyCanExecuteChanged();
            DeleteBeforeCmd.NotifyCanExecuteChanged();
            DeleteAllCmd.NotifyCanExecuteChanged();
            RefreshSourceStatsCmd.NotifyCanExecuteChanged();

            if (state == CoreState.Ready)
            {
                // 核心就绪，合并暂存日志并启动tail
                if (_stashManager != null && _stashManager.HasStashedLogs())
                {
                    // 暂存日志会由CoreLoggerProvider自动回写，这里只需要刷新显示
                    LoadStashedLogs();
                }
                _ = StartTailAsync();
                _ = RefreshStatsAsync();
            }
            else
            {
                // 如果 Core 挂了，强制停止 Tail 防止报错
                StopTail();
            }
        });
    }

    /// <summary>
    /// 初始化语言设置（异步部分：从设置中读取保存的语言）
    /// </summary>
    private async Task InitializeLanguageAsync()
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] InitializeLanguageAsync called, current CurrentLanguage={CurrentLanguage}");
        // #endregion
        
        if (_localSettings != null)
        {
            var savedLang = await _localSettings.ReadSettingAsync<string>("PreferredLanguage");
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] InitializeLanguageAsync: savedLang={savedLang}");
            // #endregion
            if (!string.IsNullOrEmpty(savedLang))
            {
                var newLang = NormalizeLanguageTag(savedLang);
                if (newLang != CurrentLanguage)
                {
                    CurrentLanguage = newLang;
                    // #region agent log
                    System.Diagnostics.Debug.WriteLine($"[LogsViewModel] InitializeLanguageAsync: updated CurrentLanguage={CurrentLanguage} from savedLang");
                    // #endregion
                }
            }
        }
        
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[LogsViewModel] InitializeLanguageAsync: final CurrentLanguage={CurrentLanguage}");
        // #endregion
    }

    /// <summary>
    /// 规范化语言标签
    /// </summary>
    private static string NormalizeLanguageTag(string? lang)
    {
        if (string.IsNullOrWhiteSpace(lang))
            return "en-US";
        lang = lang.Trim();
        if (lang.Equals("en", StringComparison.OrdinalIgnoreCase))
            return "en-US";
        if (lang.Equals("zh", StringComparison.OrdinalIgnoreCase) || lang.Equals("zh-Hans", StringComparison.OrdinalIgnoreCase))
            return "zh-CN";
        return lang;
    }

    /// <summary>
    /// 清理资源，取消事件订阅
    /// </summary>
    public void Dispose()
    {
        if (_eventPump != null)
        {
            _eventPump.LogWritten -= OnLogWritten;
        }
        StopTail();
    }
}

/// <summary>
/// 加载更早日志的事件参数
/// </summary>
public class LoadOlderLogsEventArgs : EventArgs
{
    public long? OldFirstId { get; set; }
    public long NewFirstId { get; set; }
    public int AddedCount { get; set; }
}
