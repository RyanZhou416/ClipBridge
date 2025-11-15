using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using System.Windows.Input;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using ClipBridge.Interop;
using ClipBridge.Models;
using CommunityToolkit.Mvvm.ComponentModel;

namespace ClipBridgeShell_CS.ViewModels;

public sealed class LogsViewModel : ObservableObject
{

    // 可绑定到 ListView / ItemsRepeater
    public ObservableCollection<LogRow> Items { get; } = new();

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
    public ICommand StartTailCmd  => new Relay(async _ => await StartTailAsync());
    public ICommand StopTailCmd   => new Relay(_ => StopTail());
    public ICommand ClearViewCmd  => new Relay(_ => { Items.Clear(); _lastId = 0; });
    public ICommand RefreshStatsCmd => new Relay(async _ => await RefreshStatsAsync());
    public ICommand QueryPageCmd  => new Relay(async _ => await QueryPageAsync());
    public ICommand DeleteBeforeCmd => new Relay(async _ =>
    {
        var cutoff = DateTimeOffset.Now.AddDays(-7).ToUnixTimeMilliseconds(); // 示例：保留 7 天
        var deleted = Native.LogsDeleteBefore(cutoff);
        await RefreshStatsAsync();
    });
    public ICommand ExportCsvCmd => new Relay(async _ => await ExportCsvAsync());

    public LogsViewModel()
    {
        // 默认启动 tail
        _ = StartTailAsync();
        _ = RefreshStatsAsync();
    }

    private async Task RefreshStatsAsync()
    {
        Stats = Native.LogsStats();
        await Task.CompletedTask;
    }

    // -------------------- Tail 轮询 --------------------
    private async Task StartTailAsync()
    {
        StopTail();
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
        try
        {
            var batch = Native.LogsQueryAfterId(_lastId, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, 500);
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
            // 你可以把错误也写入日志：Native.LogsWrite(4, "LogsViewModel", $"Tail error: {ex.Message}", ex.ToString());
        }
    }

    public event EventHandler? TailRequested; // 给页面滚动用

    // -------------------- 历史查询（分页） --------------------
    private async Task QueryPageAsync()
    {
        StopTail(); // 停止实时，以免干扰
        Items.Clear();
        var startMs = RangeStart.ToUnixTimeMilliseconds();
        var endMs = RangeEnd.ToUnixTimeMilliseconds();
        var list = Native.LogsQueryRange(startMs, endMs, LevelMin, string.IsNullOrWhiteSpace(FilterText) ? null : FilterText, PageSize, PageIndex * PageSize);
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
}

// 超轻量 Relay
public sealed class Relay : ICommand
{
    private readonly Action<object?> _a;
    private readonly Func<object?, bool>? _can;
    public Relay(Action<object?> a, Func<object?, bool>? can = null) { _a = a; _can = can; }
    public bool CanExecute(object? parameter) => _can?.Invoke(parameter) ?? true;
    public void Execute(object? parameter) => _a(parameter);
    public event EventHandler? CanExecuteChanged { add { } remove { } }
}
