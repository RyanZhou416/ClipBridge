using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
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
using Microsoft.Extensions.Logging;
using ClipBridgeShell_CS.Helpers;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace ClipBridgeShell_CS.ViewModels;

public sealed class LogsViewModel : ObservableObject
{
    private static int _instanceCounter = 0;
    private readonly int _instanceId;

    // 可绑定到 ListView / ItemsRepeater（保留用于兼容）
    public ObservableCollection<LogRow> Items { get; } = new();
    
    // 格式化文本（用于文本流显示，类似 Logcat）
    private readonly System.Text.StringBuilder _formattedText = new();
    private string _formattedTextContent = "";
    public string FormattedTextContent
    {
        get => _formattedTextContent;
        private set => SetProperty(ref _formattedTextContent, value);
    }
    
    // 列宽定义（用于格式化对齐）
    private const int TimeWidth = 23;      // yyyy-MM-dd HH:mm:ss.fff
    private const int LevelWidth = 8;      // Critical
    private const int ComponentWidth = 18; // 组件名（增加宽度以容纳更长的组件名如 LogsViewModel）
    private const int CategoryWidth = 20;  // 分类名
    
    /// <summary>
    /// 计算字符串在等宽字体中的显示宽度（中文字符占2个字符宽度）
    /// </summary>
    private static int GetDisplayWidth(string str)
    {
        if (string.IsNullOrEmpty(str)) return 0;
        int width = 0;
        foreach (char c in str)
        {
            // 判断是否为全角字符（中文、日文、韩文等）
            if (c >= 0x1100 && (c <= 0x115F || c >= 0x2E80 && c <= 0x9FFF || c >= 0xAC00 && c <= 0xD7AF || c >= 0xF900 && c <= 0xFAFF))
            {
                width += 2;
            }
            else
            {
                width += 1;
            }
        }
        return width;
    }
    
    /// <summary>
    /// 在等宽字体中对齐字符串（考虑中文字符宽度）
    /// </summary>
    private static string PadRightForDisplay(string str, int targetWidth)
    {
        if (string.IsNullOrEmpty(str))
        {
            return new string(' ', targetWidth);
        }
        
        int currentWidth = GetDisplayWidth(str);
        if (currentWidth >= targetWidth)
        {
            return str;
        }
        
        int paddingNeeded = targetWidth - currentWidth;
        return str + new string(' ', paddingNeeded);
    }
    
    private readonly ICoreHostService _coreHost;
    private readonly StashLogManager? _stashManager;
    private readonly Services.EventPumpService? _eventPump;
    private readonly ILocalSettingsService? _localSettings;
    private readonly Microsoft.Extensions.Logging.ILogger<LogsViewModel>? _logger;
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
    
    // 格式化单条日志为固定宽度文本行
    private string FormatLogLine(LogRow item)
    {
        var timeStr = PadRightForDisplay(item.TimeStrFull, TimeWidth);
        var levelStr = PadRightForDisplay(item.LevelName, LevelWidth);
        var componentStr = PadRightForDisplay(item.Component ?? "", ComponentWidth);
        var categoryStr = PadRightForDisplay(item.CategoryDisplay ?? "", CategoryWidth);
        var messageStr = item.DisplayMessage ?? "";
        var exceptionStr = !string.IsNullOrEmpty(item.Exception) ? $"{"LogsPage_ExceptionPrefix".GetLocalized()}{item.Exception}" : "";
        
        return $"{timeStr} {levelStr} {componentStr} {categoryStr} {messageStr}{exceptionStr}";
    }
    
    // 应用过滤条件（全量重建，用于初始加载或过滤条件变化时）
    private void ApplyFilters()
    {
        var filtered = _allItems.Where(item => MatchesFilter(item)).ToList();
        Items.Clear();
        _formattedText.Clear();
        
        foreach (var item in filtered)
        {
            Items.Add(item);
            _formattedText.AppendLine(FormatLogLine(item));
        }
        
        FormattedTextContent = _formattedText.ToString();
    }
    
    // 增量应用过滤条件（只添加新项，用于新日志追加时）
    // 新日志已经按时间顺序排序，直接追加到底部即可
    private void ApplyFiltersIncremental(IEnumerable<LogRow> newItems)
    {
        // 使用 HashSet 快速检查已存在的 ID，避免重复添加
        var existingIds = new HashSet<long>(Items.Select(item => item.Id));
        
        foreach (var item in newItems)
        {
            // 检查是否匹配过滤条件且未添加过
            if (MatchesFilter(item) && !existingIds.Contains(item.Id))
            {
                // 新日志按时间顺序追加到底部（最新的在底部）
                Items.Add(item);
                existingIds.Add(item.Id); // 更新已存在的 ID 集合
                
                // 追加到格式化文本
                _formattedText.AppendLine(FormatLogLine(item));
            }
        }
        
        // 更新格式化文本内容（触发 UI 更新）
        FormattedTextContent = _formattedText.ToString();
    }
    
    // 检查日志项是否匹配过滤条件
    private bool MatchesFilter(LogRow item)
    {
        // 分隔符始终显示，不受过滤条件影响
        if (item.Id < 0 && item.Category == "Separator")
        {
            return true;
        }
        
        // 时间范围过滤
        if (_filterStartTime.HasValue && item.Time < _filterStartTime.Value)
        {
            return false;
        }
        if (_filterEndTime.HasValue && item.Time > _filterEndTime.Value)
        {
            return false;
        }
        
        // 级别过滤
        if (!_filterLevels.Contains(item.Level))
        {
            return false;
        }
        
        // 组件过滤
        if (_filterComponents.Count > 0 && !_filterComponents.Contains(item.Component))
        {
            return false;
        }
        
        // 分类过滤
        if (_filterCategories.Count > 0 && !_filterCategories.Contains(item.Category))
        {
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
    private bool _isScrolledToBottom = true; // 默认滚动到底部（显示最新日志）
    public bool IsScrolledToBottom
    {
        get => _isScrolledToBottom;
        set => SetProperty(ref _isScrolledToBottom, value);
    }
    
    private long _lastId = 0; // tail 用（最新日志的 ID，用于查询新日志）
    private long _firstId = 0; // 当前已加载的最早日志的 ID（用于向上滚动加载更早的日志）
    
    // 线程安全地读取 _lastId
    private long GetLastId() => System.Threading.Interlocked.Read(ref _lastId);
    
    // 线程安全地更新 _lastId（只在更大时更新）
    private void UpdateLastId(long newId)
    {
        long currentId;
        do
        {
            currentId = System.Threading.Interlocked.Read(ref _lastId);
            if (newId <= currentId) return; // 不需要更新
        } while (System.Threading.Interlocked.CompareExchange(ref _lastId, newId, currentId) != currentId);
    }
    private DispatcherTimer? _testLogTimer; // 测试日志定时器，每秒打印一条日志
    private readonly object _tickLock = new(); // 防止 TickOnce 并发执行
    private bool _isTickRunning = false; // 标记 TickOnce 是否正在执行
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
    public IRelayCommand RefreshLogsCmd { get; } // 强制刷新日志
    public IRelayCommand AddTestLogCmd { get; } // 添加测试日志（用于验证动画效果）

    public LogsViewModel(ICoreHostService coreHost, StashLogManager? stashManager = null, Services.EventPumpService? eventPump = null, ILocalSettingsService? localSettings = null, Microsoft.Extensions.Logging.ILoggerFactory? loggerFactory = null)
    {
        _instanceId = Interlocked.Increment(ref _instanceCounter);
        
        _coreHost = coreHost;
        _stashManager = stashManager;
        _eventPump = eventPump;
        _localSettings = localSettings;
        _logger = loggerFactory?.CreateLogger<LogsViewModel>();

        // 初始化语言设置（同步初始化，确保在查询日志前完成）
        // 注意：这里不能使用 await，因为构造函数不能是 async
        // 所以先同步获取语言，异步部分稍后执行
        try
        {
            var loc = WinUI3Localizer.Localizer.Get();
            var locLang = loc.GetCurrentLanguage();
            CurrentLanguage = NormalizeLanguageTag(locLang);
        }
        catch
        {
            CurrentLanguage = "en-US";
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
        ClearViewCmd = new RelayCommand(() => 
        { 
            _allItems.Clear(); 
            Items.Clear(); 
            _formattedText.Clear();
            FormattedTextContent = "";
            System.Threading.Interlocked.Exchange(ref _lastId, 0); 
            _firstId = 0; 
            _processedInitLogIds.Clear(); 
        });

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
        
        // 强制刷新日志命令（用于调试）
        RefreshLogsCmd = new RelayCommand(() =>
        {
            // 强制触发一次查询
            _ = Task.Run(() => TickOnce());
        }, CanUseCore);
        
        // 添加测试日志命令（用于验证动画效果）
        // 注意：使用 ILogger 而不是直接调用 CoreInterop.LogsWrite，这样会经过完整的日志系统
        AddTestLogCmd = new RelayCommand(() =>
        {
            if (!CanUseCore()) return;
            
            try
            {
                // 如果定时器已经在运行，停止它；否则启动它
                if (_testLogTimer != null && _testLogTimer.IsEnabled)
                {
                    _testLogTimer.Stop();
                    _testLogTimer = null;
                }
                else
                {
                    // 启动测试日志定时器，每秒打印一条日志
                    _testLogTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
                    _testLogTimer.Tick += (s, e) =>
                    {
                        if (!CanUseCore())
                        {
                            _testLogTimer?.Stop();
                            _testLogTimer = null;
                            return;
                        }
                        
                        var testMessage = $"测试日志 - {DateTimeOffset.Now:yyyy-MM-dd HH:mm:ss.fff}";
                        // 使用 ILogger 写入日志，这样会经过完整的日志系统（CoreLogger -> CoreLogDispatcher -> 事件）
                        _logger?.LogInformation(testMessage);
                    };
                    _testLogTimer.Start();
                }
            }
            catch (Exception ex)
            {
                // 测试日志命令错误，静默处理
            }
        }, CanUseCore);

        // 2. 订阅状态变化，以便自动刷新按钮状态
        _coreHost.StateChanged += OnCoreStateChanged;

        // 初始化上一个核心状态
        var initialState = _coreHost.State;
        _previousCoreState = initialState;

        // 3. 如果核心未就绪，加载暂存日志
        if (!CanUseCore() && _stashManager != null)
        {
            LoadStashedLogs();
            
            // 如果核心状态是 NotLoaded 或 ShuttingDown，且数据库中有核心初始化的日志（说明之前核心是 Ready 的），
            // 应该在加载日志后插入"核心关"分隔符
            // 注意：这里需要异步查询数据库，所以延迟到 StartTailAsync 中处理
            // 但为了确保分隔符在正确的位置，我们在 StartTailAsync 中检查并插入
        }
        // 4. 如果核心就绪，先加载暂存日志（如果有），然后启动tail
        else if (CanUseCore())
        {
            // 即使核心就绪，也先加载暂存日志（如果有）
            // 这样可以显示在核心启动之前产生的日志
            // 注意：暂存日志可能已经被回写到核心，但为了确保显示，我们仍然从暂存中加载
            // 如果暂存日志已经被回写，它们会在核心查询结果中出现，但通过ID去重可以避免重复显示
            if (_stashManager != null && _stashManager.HasStashedLogs())
            {
                LoadStashedLogs();
            }
            
            // 不在这里插入分隔符，让TickOnce在检测到"Core initializing"日志时插入
            // 这样可以避免重复插入
            
            _ = StartTailAsync();
            _ = RefreshStatsAsync();
        }
    }

    private void LoadStashedLogs()
    {
        if (_stashManager == null)
        {
            return;
        }
        var stashedLogs = _stashManager.ReadAllLogs();
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
            
            // 检查是否已存在（避免重复，如果暂存日志已经被回写到核心）
            bool alreadyExists = _allItems.Any(item => item.Id == entry.Id || (item.Time_Unix == entry.TsUtc && item.Component == entry.Component && item.Category == entry.Category && item.Message == displayMessage));
            if (alreadyExists)
            {
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
        
        // 按时间戳和ID从旧到新排序（ASC），确保最新日志在底部
        var sortedItems = _allItems.OrderBy(item => item.Time_Unix).ThenBy(item => item.Id).ToList();
        _allItems.Clear();
        foreach (var item in sortedItems)
        {
            _allItems.Add(item);
        }
        ApplyFilters();
        if (stashedLogs.Count > 0)
        {
            var maxId = stashedLogs.Max(e => e.Id);
            System.Threading.Interlocked.Exchange(ref _lastId, maxId);
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
        StopTail();
        if (!CanUseCore())
        {
            return; // 双重保险
        }

        // 初始加载：查询最新的日志（从新到旧）
        _ = Task.Run(() => LoadLatestLogs());

        // 不再使用轮询定时器，完全依赖事件驱动
        // 事件通过 EventPumpService.LogWritten 触发 OnLogWritten -> TickOnce
        
        await Task.CompletedTask;
    }

    /// <summary>
    /// 加载最新的日志（从新到旧排序，用于初始加载）
    /// </summary>
    private void LoadLatestLogs()
    {
        if (!CanUseCore())
        {
            return;
        }

        try
        {
            var handle = _coreHost.GetHandle();
            if (handle == IntPtr.Zero)
            {
                return;
            }

            // 查询最新的500条日志（从新到旧）
            var batch = CoreInterop.LogsQueryLatest(handle, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, 500, CurrentLanguage);
            
            if (batch.Count == 0)
            {
                return;
            }

            // 在UI线程更新集合
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                _allItems.Clear();
                Items.Clear();
                _formattedText.Clear();
                
                // batch是从新到旧排序（DESC），但显示时需要从旧到新（最新的在底部）
                // 所以需要反转顺序
                var reversedBatch = batch.ToList();
                reversedBatch.Reverse();
                
                foreach (var row in reversedBatch)
                {
                    _allItems.Add(row);
                }
                
                // 设置ID范围
                if (batch.Count > 0)
                {
                    System.Threading.Interlocked.Exchange(ref _lastId, batch[0].Id); // 最新的ID（batch的第一个）
                    _firstId = batch[batch.Count - 1].Id; // 最旧的ID（batch的最后一个）
                }
                
                // 应用过滤（会更新格式化文本）
                ApplyFilters();
                
                // 加载完成后，请求滚动到底部
                ScrollToBottomRequested?.Invoke(this, EventArgs.Empty);
            });
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] LoadLatestLogs error: {ex.Message}");
        }
    }

    /// <summary>
    /// 当核心发送日志写入事件时触发（事件驱动，不轮询）
    /// </summary>
    private void OnLogWritten(object? sender, EventArgs e)
    {
        // 事件驱动：当有新日志写入时，立即查询并更新UI
        lock (_tickLock)
        {
            if (_isTickRunning)
            {
                return;
            }
            _isTickRunning = true;
        }
        
        // 在后台线程执行查询，避免阻塞事件处理
        _ = Task.Run(() =>
        {
            try
            {
                TickOnce();
            }
            finally
            {
                lock (_tickLock)
                {
                    _isTickRunning = false;
                }
            }
        });
    }

    private void StopTail()
    {
        if (_testLogTimer is not null) { _testLogTimer.Stop(); _testLogTimer = null; }
    }

    private void RestartTailNow()
    {
        System.Threading.Interlocked.Exchange(ref _lastId, 0); 
        _allItems.Clear(); 
        Items.Clear();
        _formattedText.Clear();
        FormattedTextContent = "";
        _processedInitLogIds.Clear(); // 清空已处理的日志ID，以便重新检测分隔符
    }

    private void TickOnce()
    {
        // 使用线程安全的方式读取 _lastId
        var currentLastId = GetLastId();
        
        // 如果运行中 Core 突然挂了，停止 Timer
        if (!CanUseCore())
        {
            StopTail();
            return;
        }

        try
        {
            var handle = _coreHost.GetHandle();
            if (handle == IntPtr.Zero)
            {
                return;
            }

            var batch = CoreInterop.LogsQueryAfterId(handle, currentLastId, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, 500, CurrentLanguage);
            
            if (batch.Count == 0)
            {
                // 没有新日志，直接返回（事件驱动，不需要轮询）
                return;
            }
            
            // 在UI线程更新集合
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                // 新日志按ID从大到小排序（最新的在前），插入到列表顶部
                var newLogs = new List<LogRow>();
                foreach (var row in batch)
                {
                    // 检查是否已存在（避免重复添加，特别是暂存日志可能已经被回写到核心）
                    bool alreadyExists = _allItems.Any(item => item.Id == row.Id);
                    if (alreadyExists)
                    {
                        // 更新_lastId，但不添加重复项
                        UpdateLastId(row.Id);
                        continue;
                    }
                    
                    // 在添加日志前检查是否匹配初始化日志条件
                    bool isInitLog = row.Component == "Core" && 
                                     row.Category == "Init" && 
                                     row.Message != null && 
                                     (row.Message.Contains("Core initializing", StringComparison.OrdinalIgnoreCase) ||
                                      row.Message.Contains("核心正在初始化", StringComparison.OrdinalIgnoreCase) ||
                                      row.Message.Contains("核心初始化", StringComparison.OrdinalIgnoreCase) ||
                                      (row.Message.Contains("初始化", StringComparison.OrdinalIgnoreCase) && row.Message.Contains("设备 ID", StringComparison.OrdinalIgnoreCase)));
                    bool alreadyProcessed = _processedInitLogIds.Contains(row.Id);
                    
                    newLogs.Add(row);
                    
                    // 标记已处理的初始化日志（分隔符现在从数据库读取，不需要在这里插入）
                    if (isInitLog && !alreadyProcessed)
                    {
                        _processedInitLogIds.Add(row.Id);
                    }
                }
                
                // 将新日志追加到底部（最新的在底部）
                if (newLogs.Count > 0)
                {
                    // 按ID从小到大排序（旧的在前，新的在后），追加到底部
                    newLogs.Sort((a, b) => a.Id.CompareTo(b.Id));
                    var maxIdInBatch = newLogs[newLogs.Count - 1].Id; // 批次中的最大ID
                    
                    // 先添加到 _allItems
                    foreach (var row in newLogs)
                    {
                        _allItems.Add(row); // 追加到底部
                    }
                    
                    // 确保 _lastId 更新为批次中的最大ID（这是关键！）
                    UpdateLastId(maxIdInBatch);
                    
                    // 使用增量过滤，只添加匹配过滤条件的新项（避免全屏闪动）
                    ApplyFiltersIncremental(newLogs);
                }
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
                // 新日志追加到底部，如果开启了自动滚动，滚动到底部
                if (AutoScroll)
                {
                    TailRequested?.Invoke(this, EventArgs.Empty);
                }
            });
        }
        catch (Exception ex)
        {
            // 如果出错（例如 DLL 调用失败），停止 Tail
            StopTail();
            System.Diagnostics.Debug.WriteLine($"Tail error: {ex.Message}");
        }
    }

    public event EventHandler? TailRequested; // 给页面滚动用
    public event EventHandler? ScrollToBottomRequested; // 请求滚动到底部
    public event EventHandler<LoadOlderLogsEventArgs>? LoadOlderLogsRequested; // 请求加载更早的日志
    
    /// <summary>
    /// 加载更早的日志（滚动到顶部时按需加载）
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
            
            // 在 UI 线程插入到列表顶部（更早的日志在顶部）
            App.MainWindow.DispatcherQueue.TryEnqueue(() =>
            {
                // 记录当前第一个元素的 ID，用于保持滚动位置
                long? oldFirstId = Items.Count > 0 ? Items[0].Id : null;
                
                // 将更早的日志插入到 _allItems 顶部（batch已经是按时间升序排列的，从旧到新）
                var newItems = new List<LogRow>();
                foreach (var row in batch)
                {
                    // 检查是否已存在
                    if (!_allItems.Any(item => item.Id == row.Id))
                    {
                        _allItems.Insert(0, row); // 插入到顶部（更早的日志）
                        newItems.Add(row);
                    }
                }
                
                // 更新最早日志 ID
                if (batch.Count > 0)
                {
                    _firstId = batch[0].Id; // batch是升序，第一个是最旧的
                }
                
                // 重新构建格式化文本（因为顺序改变了，需要全量重建）
                ApplyFilters();
                
                // 通知页面需要保持滚动位置
                LoadOlderLogsRequested?.Invoke(this, new LoadOlderLogsEventArgs 
                { 
                    OldFirstId = oldFirstId,
                    NewFirstId = _firstId,
                    AddedCount = newItems.Count
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
    /// 滚动到底部并启用吸附（显示最新日志）
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

    // -------------------- 导出 CSV（改进版：使用文件选择器） --------------------
    private async Task ExportCsvAsync()
    {
        try
        {
            // 获取主窗口
            var mainWindow = App.MainWindow;
            if (mainWindow == null)
            {
                _logger?.LogWarning("MainWindow is null, cannot show file picker");
                return;
            }

            // 创建文件保存选择器
            var savePicker = new FileSavePicker();
            
            // 使用 InitializeWithWindow 初始化
            var windowHandle = WindowNative.GetWindowHandle(mainWindow);
            InitializeWithWindow.Initialize(savePicker, windowHandle);
            
            savePicker.SuggestedStartLocation = PickerLocationId.DocumentsLibrary;
            savePicker.SuggestedFileName = $"logs_{DateTime.Now:yyyyMMdd_HHmmss}";
            savePicker.FileTypeChoices.Add("CSV Files", new List<string>() { ".csv" });
            savePicker.FileTypeChoices.Add("All Files", new List<string>() { "." });

            // 显示文件保存对话框
            var file = await savePicker.PickSaveFileAsync();
            if (file == null)
            {
                // 用户取消了选择
                return;
            }

            // 构建 CSV 内容
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

            // 写入文件（使用 UTF-8 编码，带 BOM 以便 Excel 正确识别）
            var utf8WithBom = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: true);
            await File.WriteAllTextAsync(file.Path, sb.ToString(), utf8WithBom);
            
            _logger?.LogInformation($"Logs exported to: {file.Path}");
        }
        catch (Exception ex)
        {
            _logger?.LogError(ex, "Failed to export logs to CSV");
        }
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
                System.Threading.Interlocked.Exchange(ref _lastId, 0);
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
        // 确保在 UI 线程刷新命令状态
        App.MainWindow.DispatcherQueue.TryEnqueue(() =>
        {
            // 更新状态（分隔符现在从数据库读取，不需要在这里插入）
            if (_previousCoreState != state)
            {
                _previousCoreState = state;
            }

            StartTailCmd.NotifyCanExecuteChanged();
            RefreshStatsCmd.NotifyCanExecuteChanged();
            QueryPageCmd.NotifyCanExecuteChanged();
            DeleteBeforeCmd.NotifyCanExecuteChanged();
            DeleteAllCmd.NotifyCanExecuteChanged();
            RefreshSourceStatsCmd.NotifyCanExecuteChanged();
            RefreshLogsCmd.NotifyCanExecuteChanged();
            AddTestLogCmd.NotifyCanExecuteChanged();

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
        if (_localSettings != null)
        {
            var savedLang = await _localSettings.ReadSettingAsync<string>("PreferredLanguage");
            if (!string.IsNullOrEmpty(savedLang))
            {
                var newLang = NormalizeLanguageTag(savedLang);
                if (newLang != CurrentLanguage)
                {
                    CurrentLanguage = newLang;
                }
            }
        }
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
