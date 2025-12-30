using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using System.Windows.Input;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Models;
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

    public LogsViewModel(ICoreHostService coreHost)
    {
        _coreHost = coreHost;

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
                CoreInterop.LogsDeleteBefore(cutoff);
                await RefreshStatsAsync();
            }
            catch (Exception ex) { /* TODO: Show error */ System.Diagnostics.Debug.WriteLine(ex); }
        }, CanUseCore);

        ExportCsvCmd = new RelayCommand(async () => await ExportCsvAsync()); // 导出现有数据不需要 Core

        // 2. 订阅状态变化，以便自动刷新按钮状态
        _coreHost.StateChanged += OnCoreStateChanged;

        // 3. 尝试启动
        if (CanUseCore())
        {
            _ = StartTailAsync();
            _ = RefreshStatsAsync();
        }
    }

    private async Task RefreshStatsAsync()
    {
        if (!CanUseCore()) return;
        Stats = CoreInterop.LogsStats();
        await Task.CompletedTask;
    }

    // -------------------- Tail 轮询 --------------------
    private async Task StartTailAsync()
    {
        StopTail();
        if (!CanUseCore()) return; // 双重保险

        _timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(200) };
        _timer.Tick += (_, __) => TickOnce();
        _timer.Start();
        await Task.CompletedTask;
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
        // 如果运行中 Core 突然挂了，停止 Timer
        if (!CanUseCore())
        {
            StopTail();
            return;
        }

        try
        {
            var batch = CoreInterop.LogsQueryAfterId(_lastId, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, 500);
            if (batch.Count == 0) return;
            foreach (var row in batch)
            {
                Items.Add(row);
                _lastId = row.Id;
            }
            if (AutoScroll) TailRequested?.Invoke(this, EventArgs.Empty);
        }
        catch (Exception ex)
        {
            // 如果出错（例如 DLL 调用失败），停止 Tail
            StopTail();
            System.Diagnostics.Debug.WriteLine($"Tail error: {ex.Message}");
        }
    }

    public event EventHandler? TailRequested; // 给页面滚动用

    // -------------------- 历史查询（分页） --------------------
    private async Task QueryPageAsync()
    {
        if (!CanUseCore()) return;
        StopTail(); // 停止实时，以免干扰
        Items.Clear();
        var startMs = RangeStart.ToUnixTimeMilliseconds();
        var endMs = RangeEnd.ToUnixTimeMilliseconds();
        var list = CoreInterop.LogsQueryRange(startMs, endMs, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, PageSize, PageIndex * PageSize);
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

            // 如果 Core 挂了，强制停止 Tail 防止报错
            if (state != CoreState.Ready) StopTail();
        });
    }
}
