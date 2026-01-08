using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using System.Windows.Input;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Models;
using ClipBridgeShell_CS.Services.Logging;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace ClipBridgeShell_CS.ViewModels;

public sealed class LogsViewModel : ObservableObject
{

    // 可绑定到 ListView / ItemsRepeater
    public ObservableCollection<LogRow> Items { get; } = new();
    private readonly ICoreHostService _coreHost;
    private readonly StashLogManager? _stashManager;
    private readonly Services.EventPumpService? _eventPump;
    private bool CanUseCore() => _coreHost.State == CoreState.Ready;
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
    private bool _autoScroll = true;
    public bool AutoScroll
    {
        get => _autoScroll;
        set => SetProperty(ref _autoScroll, value);
    }
    private long _lastId = 0; // tail 用
    private DispatcherTimer? _timer;
    private int _emptyQueryCount = 0; // 连续空查询次数，用于退避策略

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

    // 命令
    public IRelayCommand StartTailCmd { get; }
    public IRelayCommand StopTailCmd { get; }
    public IRelayCommand ClearViewCmd { get; }
    public IRelayCommand RefreshStatsCmd { get; }
    public IRelayCommand QueryPageCmd { get; }
    public IRelayCommand DeleteBeforeCmd { get; }
    public IRelayCommand ExportCsvCmd { get; }

    public LogsViewModel(ICoreHostService coreHost, StashLogManager? stashManager = null, Services.EventPumpService? eventPump = null)
    {
        _coreHost = coreHost;
        _stashManager = stashManager;
        _eventPump = eventPump;

        // 订阅日志写入事件
        if (_eventPump != null)
        {
            _eventPump.LogWritten += OnLogWritten;
        }

        // 1. 初始化命令，绑定 CanExecute (CanUseCore)
        StartTailCmd = new RelayCommand(async () => await StartTailAsync(), CanUseCore);

        // Stop 和 Clear 不需要 Core，随时可用
        StopTailCmd = new RelayCommand(StopTail);
        ClearViewCmd = new RelayCommand(() => { Items.Clear(); _lastId = 0; });

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

        // 2. 订阅状态变化，以便自动刷新按钮状态
        _coreHost.StateChanged += OnCoreStateChanged;

        // 3. 如果核心未就绪，加载暂存日志
        if (!CanUseCore() && _stashManager != null)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: Core not ready, loading stashed logs");
            // #endregion
            LoadStashedLogs();
        }
        // 4. 如果核心就绪，尝试启动
        else if (CanUseCore())
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Constructor: Core ready, starting tail and refresh");
            // #endregion
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
            Items.Add(new LogRow
            {
                Id = entry.Id,
                Time_Unix = entry.TsUtc,
                Level = entry.Level,
                Component = entry.Component,
                Category = entry.Category,
                Message = entry.Message,
                Exception = entry.Exception,
                Props_Json = entry.PropsJson
            });
        }
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
        _lastId = 0; Items.Clear();
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
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] TickOnce stopped: Core not ready");
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
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Querying logs: afterId={_lastId}, levelMin={LevelMin}, filter={FilterText}");
            // #endregion
            var batch = CoreInterop.LogsQueryAfterId(handle, _lastId, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, 500);
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
            foreach (var row in batch)
            {
                Items.Add(row);
                _lastId = row.Id;
            }
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[LogsViewModel] Added {batch.Count} items, newLastId={_lastId}, newItemsCount={Items.Count}");
            // #endregion
            if (AutoScroll) TailRequested?.Invoke(this, EventArgs.Empty);
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
        Items.Clear();
        var handle = _coreHost.GetHandle();
        if (handle == IntPtr.Zero) return;

        var startMs = RangeStart.ToUnixTimeMilliseconds();
        var endMs = RangeEnd.ToUnixTimeMilliseconds();
        var list = CoreInterop.LogsQueryRange(handle, startMs, endMs, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, PageSize, PageIndex * PageSize);

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

        foreach (var row in list) Items.Add(row);
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

    // [新增] 状态变更处理
    private void OnCoreStateChanged(CoreState state)
    {
        // 确保在 UI 线程刷新命令状态
        App.MainWindow.DispatcherQueue.TryEnqueue(() =>
        {
            StartTailCmd.NotifyCanExecuteChanged();
            RefreshStatsCmd.NotifyCanExecuteChanged();
            QueryPageCmd.NotifyCanExecuteChanged();
            DeleteBeforeCmd.NotifyCanExecuteChanged();

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
