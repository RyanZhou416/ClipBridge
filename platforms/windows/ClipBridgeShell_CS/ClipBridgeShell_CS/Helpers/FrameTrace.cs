// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/FrameTrace.cs
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 运行时帧时序诊断（默认完全关闭，只有显式设置环境变量才生效）。
///
/// 开关：
/// - <c>CB_FRAME_TRACE=1</c> 启用（未设置时本类所有入口方法立即返回）；
/// - <c>CB_FRAME_TRACE_SCENARIO=idle|scroll|all</c> 选择场景，默认 <c>all</c>；
/// - <c>CB_FRAME_TRACE_SECONDS=&lt;n&gt;</c> 覆盖两个场景各自的时长（默认 idle 5 秒、scroll 6 秒）。
///
/// 采集三路数据，全部带场景标签：
/// 1) 帧间隔：订阅 <see cref="CompositionTarget.Rendering"/>，用 <see cref="Stopwatch.GetTimestamp"/>
///    （即 QueryPerformanceCounter）记录相邻两次回调的间隔 —— 这就是 XAML 渲染节拍间隔；
/// 2) UI 线程停顿：后台线程每约 16ms 通过 <see cref="DispatcherQueue.TryEnqueue"/> 投递一个最小
///    工作项，测量“投递到执行”的延迟与当时尚未执行的探测项数量（队列深度的近似值）。
///    这个延迟直接量化 UI 线程被占住的时间：同步 FFI、大批量集合更新都会在它上面冒尖，
///    即使没有真正掉帧；
/// 3) UI 线程操作：同步 FFI 调用（由 <c>CoreHostService.TryUseHandle</c> 打点）与主页面图表重绘
///    的耗时，超过 5ms 才落盘，用于给停顿样本提供“当时在干什么”的标签。
///
/// 输出（都写在 <c>%LOCALAPPDATA%\ClipBridge\</c>）：
/// - <c>frames-&lt;scenario&gt;-&lt;timestamp&gt;.csv</c>：<c>wallcloack_us,delta_us,scenario</c>；
/// - <c>stalls-&lt;scenario&gt;-&lt;timestamp&gt;.csv</c>：<c>wallcloack_us,stall_us,scenario,queue_depth</c>；
/// - <c>uiops-&lt;scenario&gt;-&lt;timestamp&gt;.csv</c>：<c>wallcloack_us,duration_us,label,scenario</c>；
/// - <c>summary-&lt;scenario&gt;-&lt;timestamp&gt;.txt</c>：统计汇总（同时写 stdout，需要调用方重定向）。
/// 落盘并 flush 之后，经 UI 线程调用 <see cref="Application.Exit"/> 让进程自行结束。
///
/// 注意：本类刻意不依赖任何 UI 类型以外的壳内部结构；滚动驱动由 MainPage 注册，
/// 这样诊断代码不需要知道 ItemsRepeater/ScrollViewer 的具体布局。
/// </summary>
internal static class FrameTrace
{
    /// <summary>同时未执行的探测项上限：UI 线程长时间卡死时不让探测自己的队列无限膨胀。</summary>
    private const int MaxPendingProbes = 32;

    /// <summary>FFI / UI 操作只有超过该阈值（微秒）才记录，避免噪声。</summary>
    private const double FfiSlowThresholdUs = 5000.0;

    private const double UiOpSlowThresholdUs = 5000.0;

    /// <summary>主页面就绪后先稳定这么久再开始 idle 场景，避开首帧/首绘的收尾工作。</summary>
    private const int SettleAfterMainPageMs = 500;

    /// <summary>等主页面就绪的上限；超时也继续（否则某个环境异常会让进程永远不退出）。</summary>
    private const int MainPageWaitTimeoutMs = 15000;

    /// <summary>场景阶段。数值会被写进样本，落盘时再映射成名字。</summary>
    private enum Phase
    {
        Warmup = 0,
        Idle = 1,
        Scroll = 2,
        Ended = 3
    }

    /// <summary>
    /// 是否启用。只在类型初始化时读一次环境变量；未设置时每个入口方法只是一次字段读取 + 返回，
    /// 不订阅事件、不起线程、不建文件，行为与没有这段诊断代码完全等价。
    /// </summary>
    internal static readonly bool Enabled =
        string.Equals(Environment.GetEnvironmentVariable("CB_FRAME_TRACE"), "1", StringComparison.Ordinal);

    /// <summary>Stopwatch 一个 tick 对应的微秒数（QueryPerformanceCounter 频率换算）。</summary>
    private static readonly double UsPerTick = 1_000_000.0 / Stopwatch.Frequency;

    /// <summary>样本列表的锁：写入来自 UI 线程（Rendering/探测回调/操作打点），落盘时从编排线程快照。</summary>
    private static readonly object RecordLock = new();

    private static readonly List<FrameSample> Frames = new();
    private static readonly List<StallSample> Stalls = new();
    private static readonly List<OpSample> Ops = new();

    private static DispatcherQueue? _queue;
    private static IntPtr _hwnd;
    private static int _uiThreadId;
    private static long _startTimestamp;
    private static long _startWallUs;
    private static long _prevFrameTimestamp;
    private static bool _hasPrevFrame;
    private static int _phase = (int)Phase.Warmup;
    private static Action? _scrollTick;
    private static bool _mainPageLoaded;
    private static bool _stop;
    private static int _pendingProbes;
    private static int _ffiCallCount;
    private static int _ffiUiThreadCallCount;
    private static int _ffiOverThresholdCount;
    private static int _scrollTickCount;
    private static Thread? _probeThread;
    private static string _scrollTarget = "(none)";
    private static string _runTag = "all";
    private static string _startedAtText = string.Empty;
    private static string _fileStamp = string.Empty;
    private static double _idleSeconds = 5.0;
    private static double _scrollSeconds = 6.0;
    private static bool _windowVisibleAtStart;
    private static bool _windowForegroundAtStart;

    /// <summary>开始采集。必须在 UI 线程调用（订阅 Rendering 要求线程亲和）。</summary>
    /// <param name="queue">窗口的 <see cref="DispatcherQueue"/>，用于探测 UI 线程响应。</param>
    /// <param name="hwnd">主窗口句柄，仅用于在汇总里记录“窗口可见但未获得焦点”这一运行条件。</param>
    internal static void StartAfterWindowVisible(DispatcherQueue? queue, IntPtr hwnd)
    {
        if (!Enabled || queue == null)
        {
            return;
        }

        _queue = queue;
        _hwnd = hwnd;
        _uiThreadId = Environment.CurrentManagedThreadId;
        _startTimestamp = Stopwatch.GetTimestamp();
        _startWallUs = (DateTime.UtcNow.Ticks - DateTime.UnixEpoch.Ticks) / 10;
        _startedAtText = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss.fff", CultureInfo.InvariantCulture);
        _fileStamp = DateTime.Now.ToString("yyyyMMdd-HHmmss-fff", CultureInfo.InvariantCulture);
        _windowVisibleAtStart = IsWindowVisible(hwnd);
        _windowForegroundAtStart = GetForegroundWindow() == hwnd;

        // 场景选择：显式写错时按 all 处理，保证“设了开关就一定有数据”。
        var scenario = (Environment.GetEnvironmentVariable("CB_FRAME_TRACE_SCENARIO") ?? "all").Trim().ToLowerInvariant();
        var doIdle = scenario is "idle" or "all" or "";
        var doScroll = scenario is "scroll" or "all" or "";
        if (!doIdle && !doScroll)
        {
            doIdle = true;
            doScroll = true;
        }
        _runTag = doIdle && doScroll ? "all" : (doIdle ? "idle" : "scroll");

        // 时长覆盖：一个变量同时改两个场景，保持“采样预算可调”的简单语义。
        var secondsText = Environment.GetEnvironmentVariable("CB_FRAME_TRACE_SECONDS");
        if (!string.IsNullOrWhiteSpace(secondsText)
            && double.TryParse(secondsText, NumberStyles.Float, CultureInfo.InvariantCulture, out var seconds)
            && seconds > 0.5)
        {
            _idleSeconds = seconds;
            _scrollSeconds = seconds;
        }

        // 帧间隔采集：Rendering 每个渲染节拍在 UI 线程回调一次。
        CompositionTarget.Rendering += OnRendering;

        // UI 线程停顿探测：独立线程，避免线程池饥饿把探测本身拖慢。
        _probeThread = new Thread(ProbeLoop)
        {
            IsBackground = true,
            Name = "CB.FrameTrace.Probe"
        };
        _probeThread.Start();

        // 场景编排：等主页面 → idle → scroll → 落盘 → 退出。用独立线程而不是线程池，
        // 保证即使 UI 线程完全卡死，它也能按时收尾并写文件。
        var planThread = new Thread(() => RunPlan(doIdle, doScroll))
        {
            IsBackground = true,
            Name = "CB.FrameTrace.Plan"
        };
        planThread.Start();
    }

    /// <summary>主页面 Loaded 时登记。用于把 idle 场景推迟到页面稳定之后。</summary>
    internal static void NotifyMainPageLoaded()
    {
        if (!Enabled)
        {
            return;
        }

        Volatile.Write(ref _mainPageLoaded, true);
    }

    /// <summary>
    /// 注册“逐帧滚动一步”的驱动。scroll 场景期间每个渲染节拍调用一次；驱动内部自行负责方向与边界。
    /// 未启用时立即返回，注册方不需要额外判断。
    /// </summary>
    internal static void RegisterScrollDriver(Action tick)
    {
        if (!Enabled)
        {
            return;
        }

        Volatile.Write(ref _scrollTick, tick);
    }

    /// <summary>记录滚动实际作用的对象（history-strip / page-vertical），写进汇总，避免误读数据。</summary>
    internal static void NoteScrollTarget(string target)
    {
        if (!Enabled)
        {
            return;
        }

        Volatile.Write(ref _scrollTarget, target);
    }

    /// <summary>UI 操作计时起点。未启用时返回 0，配合 <see cref="EndUiOp"/> 变成空操作。</summary>
    internal static long BeginUiOp()
    {
        return Enabled ? Stopwatch.GetTimestamp() : 0L;
    }

    /// <summary>UI 操作计时终点：只有超过 5ms 的操作才记入 trace。</summary>
    internal static void EndUiOp(string label, long startTimestamp)
    {
        if (!Enabled || startTimestamp == 0L)
        {
            return;
        }

        var durationUs = (Stopwatch.GetTimestamp() - startTimestamp) * UsPerTick;
        if (durationUs < UiOpSlowThresholdUs)
        {
            return;
        }

        RecordOp(WallUs(startTimestamp), durationUs, label, Volatile.Read(ref _phase));
    }

    /// <summary>同步 FFI 计时起点（在持有 Core 句柄锁、即将调用 cb_* 的位置使用）。</summary>
    internal static long BeginFfiCall()
    {
        return Enabled ? Stopwatch.GetTimestamp() : 0L;
    }

    /// <summary>
    /// 同步 FFI 计时终点：统计调用总数，并只把“发生在 UI 线程且超过 5ms”的调用记入 trace，
    /// 标签取 CoreHostService 里调用方方法名（例如 <c>SetPeerPolicy</c>），对应到具体的 cb_* 入口。
    /// </summary>
    internal static void EndFfiCall(long startTimestamp)
    {
        if (!Enabled || startTimestamp == 0L)
        {
            return;
        }

        var durationUs = (Stopwatch.GetTimestamp() - startTimestamp) * UsPerTick;
        Interlocked.Increment(ref _ffiCallCount);
        if (Environment.CurrentManagedThreadId != _uiThreadId)
        {
            return;
        }

        Interlocked.Increment(ref _ffiUiThreadCallCount);
        if (durationUs < FfiSlowThresholdUs)
        {
            return;
        }

        Interlocked.Increment(ref _ffiOverThresholdCount);
        RecordOp(WallUs(startTimestamp), durationUs, "ffi:" + ResolveCallerName(), Volatile.Read(ref _phase));
    }

    /// <summary>渲染节拍回调：记录帧间隔；scroll 阶段同时驱动一次滚动。</summary>
    private static void OnRendering(object? sender, object e)
    {
        var timestamp = Stopwatch.GetTimestamp();
        var phase = Volatile.Read(ref _phase);

        if (_hasPrevFrame)
        {
            var deltaUs = (timestamp - _prevFrameTimestamp) * UsPerTick;
            var sample = new FrameSample(WallUs(timestamp), deltaUs, phase);
            lock (RecordLock)
            {
                Frames.Add(sample);
            }
        }

        _prevFrameTimestamp = timestamp;
        _hasPrevFrame = true;

        if (phase == (int)Phase.Scroll && Volatile.Read(ref _scrollTick) is { } scrollTick)
        {
            Interlocked.Increment(ref _scrollTickCount);
            try
            {
                scrollTick();
            }
            catch
            {
                // 滚动驱动失败只影响该场景的可解释性，不能让整个测量崩掉。
            }
        }
    }

    /// <summary>探测线程：每约 16ms 投递一个最小工作项；UI 线程忙时延迟会直接变大。</summary>
    private static void ProbeLoop()
    {
        var queue = _queue;
        if (queue == null)
        {
            return;
        }

        while (!Volatile.Read(ref _stop))
        {
            if (Volatile.Read(ref _pendingProbes) < MaxPendingProbes)
            {
                var postedTimestamp = Stopwatch.GetTimestamp();
                Interlocked.Increment(ref _pendingProbes);
                var accepted = queue.TryEnqueue(() => OnProbeExecuted(postedTimestamp));
                if (!accepted)
                {
                    Interlocked.Decrement(ref _pendingProbes);
                }
            }

            // Thread.Sleep(16) 在本机默认计时精度下实际约 15.6ms，采样密度足够刻画 16ms 级停顿。
            Thread.Sleep(16);
        }
    }

    /// <summary>探测工作项在 UI 线程上真正执行时回调：延迟即 UI 线程被占用的时间。</summary>
    private static void OnProbeExecuted(long postedTimestamp)
    {
        var executedTimestamp = Stopwatch.GetTimestamp();
        var latencyUs = (executedTimestamp - postedTimestamp) * UsPerTick;
        var depth = Volatile.Read(ref _pendingProbes) - 1;
        if (depth < 0)
        {
            depth = 0;
        }

        var sample = new StallSample(WallUs(postedTimestamp), latencyUs, Volatile.Read(ref _phase), depth);
        lock (RecordLock)
        {
            Stalls.Add(sample);
        }

        Interlocked.Decrement(ref _pendingProbes);
    }

    /// <summary>场景编排：等主页面 → idle → scroll → 收尾。</summary>
    private static void RunPlan(bool doIdle, bool doScroll)
    {
        try
        {
            var waitStart = Stopwatch.GetTimestamp();
            while (!Volatile.Read(ref _mainPageLoaded)
                   && (Stopwatch.GetTimestamp() - waitStart) * UsPerTick / 1000.0 < MainPageWaitTimeoutMs)
            {
                Thread.Sleep(20);
            }

            // 页面刚 Loaded 时还有首绘图、背景图解码、图表首绘等收尾工作，先稳定下来再测 idle，
            // 否则 idle 里混进启动收尾，会把“稳态空闲”的整体水平抬高。
            Thread.Sleep(SettleAfterMainPageMs);

            if (doIdle)
            {
                SetPhase(Phase.Idle);
                SleepSeconds(_idleSeconds);
            }

            if (doScroll)
            {
                SetPhase(Phase.Scroll);
                SleepSeconds(_scrollSeconds);
            }

            SetPhase(Phase.Ended);
            Thread.Sleep(120);
        }
        finally
        {
            Finish();
        }
    }

    private static void SetPhase(Phase phase)
    {
        Volatile.Write(ref _phase, (int)phase);
    }

    /// <summary>分段睡眠：保证阶段切换足够及时，即使时长被 CB_FRAME_TRACE_SECONDS 调大。</summary>
    private static void SleepSeconds(double seconds)
    {
        var start = Stopwatch.GetTimestamp();
        var targetUs = seconds * 1_000_000.0;
        while ((Stopwatch.GetTimestamp() - start) * UsPerTick < targetUs)
        {
            Thread.Sleep(50);
        }
    }

    /// <summary>把单调时间戳换算成“自 Unix 纪元起的微秒”，三份 CSV 因此可以直接按时间对齐。</summary>
    private static long WallUs(long timestamp)
    {
        return _startWallUs + (long)((timestamp - _startTimestamp) * UsPerTick);
    }

    /// <summary>收尾：停探测线程 → 在 UI 线程退订 Rendering → 落盘 → 打印 → 退出进程。</summary>
    private static void Finish()
    {
        try
        {
            Volatile.Write(ref _stop, true);
            _probeThread?.Join(1500);

            // 退订必须回到 UI 线程；用事件等它执行完，之后样本列表不再增长，可以安全快照。
            using (var stopped = new ManualResetEventSlim(false))
            {
                var accepted = _queue?.TryEnqueue(() =>
                {
                    try
                    {
                        CompositionTarget.Rendering -= OnRendering;
                    }
                    catch
                    {
                        // 退订失败也要放行收尾，顶多丢掉最后几个样本。
                    }
                    finally
                    {
                        stopped.Set();
                    }
                }) ?? false;
                if (accepted)
                {
                    stopped.Wait(2000);
                }
            }

            // 再排一个低优先级空工作项，把已经排在队列里的图表重绘之类跑完，减少“收尾阶段”的截断误差。
            using (var drained = new ManualResetEventSlim(false))
            {
                var accepted = _queue?.TryEnqueue(DispatcherQueuePriority.Low, () => drained.Set()) ?? false;
                if (accepted)
                {
                    drained.Wait(1000);
                }
            }

            var snapshot = TakeSnapshot();
            var framePaths = WriteOutputs(snapshot);
            var summary = BuildSummary(snapshot, framePaths);

            try
            {
                var dir = TraceDirectory();
                var summaryPath = Path.Combine(dir, $"summary-{_runTag}-{_fileStamp}.txt");
                File.WriteAllText(summaryPath, summary, new UTF8Encoding(false));
            }
            catch
            {
                // 汇总文件写不出来时 stdout 仍是唯一出口，不能因此影响退出。
            }

            WriteConsole(summary);
        }
        catch
        {
            // 诊断代码绝不能把进程留在“跑完但没退出”的状态。
        }
        finally
        {
            ExitApplication();
        }
    }

    private static Samples TakeSnapshot()
    {
        lock (RecordLock)
        {
            return new Samples
            {
                FrameSamples = Frames.ToArray(),
                StallSamples = Stalls.ToArray(),
                OpSamples = Ops.ToArray()
            };
        }
    }

    /// <summary>写三份 CSV；返回文件路径（用于汇总里报告落盘位置）。</summary>
    private static string[] WriteOutputs(Samples snapshot)
    {
        var dir = TraceDirectory();
        Directory.CreateDirectory(dir);

        var framesPath = Path.Combine(dir, $"frames-{_runTag}-{_fileStamp}.csv");
        var stallsPath = Path.Combine(dir, $"stalls-{_runTag}-{_fileStamp}.csv");
        var opsPath = Path.Combine(dir, $"uiops-{_runTag}-{_fileStamp}.csv");

        using (var writer = new StreamWriter(new FileStream(framesPath, FileMode.Create, FileAccess.Write, FileShare.Read), new UTF8Encoding(false)))
        {
            writer.WriteLine("wallcloack_us,delta_us,scenario");
            foreach (var sample in snapshot.FrameSamples)
            {
                writer.Write(sample.WallUs.ToString(CultureInfo.InvariantCulture));
                writer.Write(',');
                writer.Write(sample.DeltaUs.ToString("F1", CultureInfo.InvariantCulture));
                writer.Write(',');
                writer.WriteLine(PhaseName(sample.Phase));
            }
        }

        using (var writer = new StreamWriter(new FileStream(stallsPath, FileMode.Create, FileAccess.Write, FileShare.Read), new UTF8Encoding(false)))
        {
            writer.WriteLine("wallcloack_us,stall_us,scenario,queue_depth");
            foreach (var sample in snapshot.StallSamples)
            {
                writer.Write(sample.WallUs.ToString(CultureInfo.InvariantCulture));
                writer.Write(',');
                writer.Write(sample.LatencyUs.ToString("F1", CultureInfo.InvariantCulture));
                writer.Write(',');
                writer.Write(PhaseName(sample.Phase));
                writer.Write(',');
                writer.WriteLine(sample.Depth.ToString(CultureInfo.InvariantCulture));
            }
        }

        using (var writer = new StreamWriter(new FileStream(opsPath, FileMode.Create, FileAccess.Write, FileShare.Read), new UTF8Encoding(false)))
        {
            writer.WriteLine("wallcloack_us,duration_us,label,scenario");
            foreach (var sample in snapshot.OpSamples)
            {
                writer.Write(sample.WallUs.ToString(CultureInfo.InvariantCulture));
                writer.Write(',');
                writer.Write(sample.DurationUs.ToString("F1", CultureInfo.InvariantCulture));
                writer.Write(',');
                writer.Write(EscapeCsv(sample.Label));
                writer.Write(',');
                writer.WriteLine(PhaseName(sample.Phase));
            }
        }

        return new[] { framesPath, stallsPath, opsPath };
    }

    /// <summary>汇总文本：统计数字 + 运行条件 + 最慢停顿/操作清单。stdout 与 summary 文件内容一致。</summary>
    private static string BuildSummary(Samples snapshot, string[] paths)
    {
        var sb = new StringBuilder();
        sb.AppendLine("=== ClipBridge FrameTrace summary ===");
        sb.AppendLine(FormattableString.Invariant(
            $"run={_runTag} started={_startedAtText} pid={Environment.ProcessId} scenario={_runTag} idle={_idleSeconds:F1}s scroll={_scrollSeconds:F1}s"));
        sb.AppendLine(FormattableString.Invariant(
            $"frames={snapshot.FrameSamples.Length} stalls={snapshot.StallSamples.Length} uiops={snapshot.OpSamples.Length} ffi_calls={Volatile.Read(ref _ffiCallCount)} ffi_on_ui_thread={Volatile.Read(ref _ffiUiThreadCallCount)} ffi_ui_over_5ms={Volatile.Read(ref _ffiOverThresholdCount)} scroll_ticks={Volatile.Read(ref _scrollTickCount)} scroll_target={_scrollTarget}"));
        sb.AppendLine(FormattableString.Invariant(
            $"machine={Environment.MachineName} cpus={Environment.ProcessorCount} os={Environment.OSVersion.VersionString}"));
        sb.AppendLine(FormattableString.Invariant(
            $"window_visible_start={_windowVisibleAtStart} window_foreground_start={_windowForegroundAtStart} window_visible_end={IsWindowVisible(_hwnd)} window_foreground_end={GetForegroundWindow() == _hwnd}"));
        sb.AppendLine(FormattableString.Invariant(
            $"window_rect={DescribeWindowRect()} screen={GetSystemMetrics(SmCxScreen)}x{GetSystemMetrics(SmCyScreen)} dpi={GetDpiForWindow(_hwnd)} display_refresh_hz={ReadDisplayRefreshHz()}"));
        sb.AppendLine(FormattableString.Invariant($"page_ready_wait_timeout_ms={MainPageWaitTimeoutMs} settle_ms={SettleAfterMainPageMs}"));
        sb.AppendLine();

        // 帧间隔：按场景分组统计（warmup 也列出，便于判断场景边界的污染程度）。
        sb.AppendLine("--- frame intervals (ms) ---");
        foreach (var phase in new[] { (int)Phase.Warmup, (int)Phase.Idle, (int)Phase.Scroll, (int)Phase.Ended })
        {
            var deltas = new List<double>();
            foreach (var sample in snapshot.FrameSamples)
            {
                if (sample.Phase == phase)
                {
                    deltas.Add(sample.DeltaUs);
                }
            }

            if (deltas.Count > 0)
            {
                sb.AppendLine(StatsLine($"scenario={PhaseName(phase)}", deltas));
            }
        }

        sb.AppendLine();
        sb.AppendLine("--- ui-thread stall latency (ms; time from DispatcherQueue post to execution) ---");
        foreach (var phase in new[] { (int)Phase.Warmup, (int)Phase.Idle, (int)Phase.Scroll, (int)Phase.Ended })
        {
            var latencies = new List<double>();
            var maxDepth = 0;
            foreach (var sample in snapshot.StallSamples)
            {
                if (sample.Phase == phase)
                {
                    latencies.Add(sample.LatencyUs);
                    if (sample.Depth > maxDepth)
                    {
                        maxDepth = sample.Depth;
                    }
                }
            }

            if (latencies.Count > 0)
            {
                sb.AppendLine(StatsLine($"scenario={PhaseName(phase)}", latencies) + FormattableString.Invariant($" max_queue_depth={maxDepth}"));
            }
        }

        sb.AppendLine();
        sb.AppendLine("--- top 10 ui-thread stalls ---");
        var topStalls = new List<StallSample>(snapshot.StallSamples);
        topStalls.Sort((a, b) => b.LatencyUs.CompareTo(a.LatencyUs));
        for (var i = 0; i < Math.Min(10, topStalls.Count); i++)
        {
            var sample = topStalls[i];
            sb.AppendLine(FormattableString.Invariant(
                $"#{i + 1} stall_ms={sample.LatencyUs / 1000.0:F2} scenario={PhaseName(sample.Phase)} wall_us={sample.WallUs} depth={sample.Depth}"));
        }

        sb.AppendLine();
        sb.AppendLine("--- ui-thread ops > 5ms (ffi = synchronous cb_* inside CoreHostService) ---");
        var topOps = new List<OpSample>(snapshot.OpSamples);
        topOps.Sort((a, b) => b.DurationUs.CompareTo(a.DurationUs));
        if (topOps.Count == 0)
        {
            sb.AppendLine("(none recorded)");
        }

        foreach (var sample in topOps)
        {
            sb.AppendLine(FormattableString.Invariant(
                $"op_ms={sample.DurationUs / 1000.0:F2} label={sample.Label} scenario={PhaseName(sample.Phase)} wall_us={sample.WallUs}"));
        }

        sb.AppendLine();
        sb.AppendLine("--- files ---");
        foreach (var path in paths)
        {
            sb.AppendLine(path);
        }

        return sb.ToString();
    }

    /// <summary>计算 count/mean/P50/P90/P99/max 与 &gt;33ms / &gt;50ms 个数（百分位用 nearest-rank）。</summary>
    private static string StatsLine(string prefix, List<double> samples)
    {
        if (samples.Count == 0)
        {
            return prefix + " n=0";
        }

        var sorted = samples.ToArray();
        Array.Sort(sorted);
        var sum = 0.0;
        var over33 = 0;
        var over50 = 0;
        foreach (var value in sorted)
        {
            sum += value;
            if (value > 33_000.0)
            {
                over33++;
            }

            if (value > 50_000.0)
            {
                over50++;
            }
        }

        var mean = sum / sorted.Length;
        return FormattableString.Invariant(
            $"{prefix} n={sorted.Length} mean={Ms(mean)} p50={Ms(Percentile(sorted, 50))} p90={Ms(Percentile(sorted, 90))} p99={Ms(Percentile(sorted, 99))} max={Ms(sorted[^1])} >33ms={over33} >50ms={over50}");
    }

    /// <summary>nearest-rank 百分位：第 ceil(p%*n) 个排序值（1 基）。</summary>
    private static double Percentile(double[] sorted, double percentile)
    {
        var rank = (int)Math.Ceiling(percentile / 100.0 * sorted.Length);
        if (rank < 1)
        {
            rank = 1;
        }

        if (rank > sorted.Length)
        {
            rank = sorted.Length;
        }

        return sorted[rank - 1];
    }

    private static string Ms(double us)
    {
        return (us / 1000.0).ToString("F2", CultureInfo.InvariantCulture) + "ms";
    }

    private static string PhaseName(int phase)
    {
        return phase switch
        {
            (int)Phase.Warmup => "warmup",
            (int)Phase.Idle => "idle",
            (int)Phase.Scroll => "scroll",
            (int)Phase.Ended => "ended",
            _ => "unknown"
        };
    }

    /// <summary>取 CoreHostService 里紧邻 TryUseHandle 的调用方方法名，作为 cb_* 的操作标签。</summary>
    private static string ResolveCallerName()
    {
        try
        {
            // 跳过 ResolveCallerName / EndFfiCall / TryUseHandle 三层，栈顶即 CoreHostService 的方法
            // （通常是一个 lambda，名字形如 "<ListHistoryAsync>b__19_0"，下面会把方法名剥出来）。
            var trace = new StackTrace(3, false);
            var method = trace.GetFrame(0)?.GetMethod();
            var name = method?.Name ?? "cb_call";
            if (name.Length > 2 && name[0] == '<')
            {
                var end = name.IndexOf('>');
                if (end > 1)
                {
                    name = name.Substring(1, end - 1);
                }
            }

            return name;
        }
        catch
        {
            return "cb_call";
        }
    }

    private static string TraceDirectory()
    {
        return Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "ClipBridge");
    }

    private static string EscapeCsv(string value)
    {
        if (value.IndexOfAny(new[] { ',', '"', '\r', '\n' }) < 0)
        {
            return value;
        }

        return "\"" + value.Replace("\"", "\"\"") + "\"";
    }

    private static string DescribeWindowRect()
    {
        try
        {
            if (GetWindowRect(_hwnd, out var rect))
            {
                return FormattableString.Invariant($"{rect.Right - rect.Left}x{rect.Bottom - rect.Top} at ({rect.Left},{rect.Top})");
            }
        }
        catch
        {
            // 拿不到窗口矩形不影响统计。
        }

        return "unknown";
    }

    /// <summary>读取主显示器刷新率（GDI GetDeviceCaps VREFRESH）；0 表示拿不到。</summary>
    private static int ReadDisplayRefreshHz()
    {
        var dc = IntPtr.Zero;
        try
        {
            dc = GetDC(IntPtr.Zero);
            return dc == IntPtr.Zero ? 0 : GetDeviceCaps(dc, VRefresh);
        }
        catch
        {
            return 0;
        }
        finally
        {
            if (dc != IntPtr.Zero)
            {
                _ = ReleaseDC(IntPtr.Zero, dc);
            }
        }
    }

    private static void WriteConsole(string text)
    {
        try
        {
            Console.Out.Write(text);
            Console.Out.Flush();
        }
        catch
        {
            // WinExe 没有控制台且调用方没重定向时，stdout 不可写；summary 文件已经落盘。
        }
    }

    /// <summary>
    /// 退出：走应用自己的退出路径（<see cref="Application.Exit"/>，必须在 UI 线程调用）。
    /// 另外挂一个看门狗线程：若窗口关闭钩子把退出取消掉，超时后强制结束进程，
    /// 保证脚本化连续运行不会卡住（只在诊断模式下存在）。
    /// </summary>
    private static void ExitApplication()
    {
        var queue = _queue;
        if (queue != null)
        {
            queue.TryEnqueue(() =>
            {
                try
                {
                    Application.Current.Exit();
                }
                catch
                {
                    // 退出路径本身异常时交给下面的看门狗。
                }
            });
        }

        var watchdog = new Thread(() =>
        {
            Thread.Sleep(8000);
            Environment.Exit(0);
        })
        {
            IsBackground = true,
            Name = "CB.FrameTrace.ExitWatchdog"
        };
        watchdog.Start();
    }

    private static void RecordOp(long wallUs, double durationUs, string label, int phase)
    {
        var sample = new OpSample(wallUs, durationUs, label, phase);
        lock (RecordLock)
        {
            Ops.Add(sample);
        }
    }

    private readonly struct FrameSample
    {
        public FrameSample(long wallUs, double deltaUs, int phase)
        {
            WallUs = wallUs;
            DeltaUs = deltaUs;
            Phase = phase;
        }

        public long WallUs { get; }

        public double DeltaUs { get; }

        public int Phase { get; }
    }

    private readonly struct StallSample
    {
        public StallSample(long wallUs, double latencyUs, int phase, int depth)
        {
            WallUs = wallUs;
            LatencyUs = latencyUs;
            Phase = phase;
            Depth = depth;
        }

        public long WallUs { get; }

        public double LatencyUs { get; }

        public int Phase { get; }

        public int Depth { get; }
    }

    private readonly struct OpSample
    {
        public OpSample(long wallUs, double durationUs, string label, int phase)
        {
            WallUs = wallUs;
            DurationUs = durationUs;
            Label = label;
            Phase = phase;
        }

        public long WallUs { get; }

        public double DurationUs { get; }

        public string Label { get; }

        public int Phase { get; }
    }

    private sealed class Samples
    {
        public FrameSample[] FrameSamples { get; set; } = Array.Empty<FrameSample>();

        public StallSample[] StallSamples { get; set; } = Array.Empty<StallSample>();

        public OpSample[] OpSamples { get; set; } = Array.Empty<OpSample>();
    }

    // ---- Win32：只用于记录运行条件（窗口可见/前台、屏幕、DPI、刷新率），不改变任何窗口行为 ----
    private const int SmCxScreen = 0;
    private const int SmCyScreen = 1;
    private const int VRefresh = 116;

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(IntPtr hWnd, out Rect rect);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int nIndex);

    [DllImport("user32.dll")]
    private static extern IntPtr GetDC(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern int ReleaseDC(IntPtr hWnd, IntPtr hDc);

    [DllImport("gdi32.dll")]
    private static extern int GetDeviceCaps(IntPtr hdc, int nIndex);

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }
}
