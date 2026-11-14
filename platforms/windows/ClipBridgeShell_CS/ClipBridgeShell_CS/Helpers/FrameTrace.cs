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

using ClipBridgeShell_CS.Core.Services;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 运行时帧时序诊断（默认完全关闭，只有显式设置环境变量才生效）。
///
/// 开关：
/// - <c>CB_FRAME_TRACE=1</c> 启用（未设置时本类所有入口方法立即返回）；
/// - <c>CB_FRAME_TRACE_SCENARIO=idle|scroll|drag|manual|all</c> 选择场景，默认 <c>all</c>；
/// - <c>CB_FRAME_TRACE_SECONDS=&lt;n&gt;</c> 覆盖各场景时长（默认 idle 3 秒、scroll 6 秒、drag 3 秒、manual 5 秒）；
/// - <c>CB_FRAME_TRACE_READY_SECONDS=&lt;n&gt;</c> manual 场景开始记录前的准备时长，默认 2 秒
///   （留给操作者把手放到窗口上；0 表示立刻开始）；
/// - <c>CB_FRAME_TRACE_ACRYLIC=0|1</c> 在场景开始前把首页卡片的亚克力开关切到指定状态
///   （未设置时保持用户当前设置；只改运行时绑定的 ViewModel 属性，不落盘，进程退出即失效）；
/// - <c>CB_FRAME_TRACE_MICA=0|1</c> 在场景开始前把主窗口的 <see cref="Microsoft.UI.Xaml.Window.SystemBackdrop"/>
///   在 <see cref="MicaBackdrop"/> 与 <c>null</c> 之间切换（未设置时保持现状；复用 MainWindow 构造里
///   创建的同一个 MicaBackdrop 实例，不新增第二套 Mica 实现；请求状态与实际状态都写进汇总和 [cost] 行）。
///
/// drag 场景：先把窗口临时缩到约 900x600 并挪到工作区右上角（留出 ±60px 轨迹边距），
/// 再由 UI 线程在每个渲染节拍调用 <c>SetWindowPos</c>
/// （<c>SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE</c>）让窗口做 ±60px 的往复轨迹，
/// 使 DWM 每帧都要重新合成（亚克力/背景采样也随之每帧重做）。窗口始终留在工作区内、
/// 不抢焦点；场景结束后还原原始位置与尺寸。帧间隔仍来自 <see cref="CompositionTarget.Rendering"/>，
/// 因此能反映“拖拽窗口时合成是否把渲染节拍拖慢”。
///
/// manual 场景：应用完全不碰窗口 —— 不移动、不缩放、不激活、不重定位，也不调用
/// <c>SetWindowPos</c>/<c>Activate()</c>；由人在准备提示后手动拖动窗口，我们只记录帧时序。
/// 顺序为：打印准备提示 → 等待 <c>CB_FRAME_TRACE_READY_SECONDS</c>（默认 2 秒，带整秒倒计时）
/// → 打印“记录中” → 采集 <c>CB_FRAME_TRACE_SECONDS</c>（默认 5 秒）→ 打印“记录结束”
/// → 落盘汇总 → <see cref="Application.Exit"/>。它是判断“拖拽时应用自己是否慢”的唯一不被
/// UI 线程 <c>SetWindowPos</c> 开销污染的口径（drag 场景的移动由本类驱动，只用于合成压力对照）。
///
/// 采集四路数据，全部带场景标签：
/// 1) 帧间隔：订阅 <see cref="CompositionTarget.Rendering"/>，用 <see cref="Stopwatch.GetTimestamp"/>
///    （即 QueryPerformanceCounter）记录相邻两次回调的间隔 —— 这就是 XAML 渲染节拍间隔；
/// 2) UI 线程停顿：后台线程每约 16ms 通过 <see cref="DispatcherQueue.TryEnqueue"/> 投递一个最小
///    工作项，测量“投递到执行”的延迟与当时尚未执行的探测项数量（队列深度的近似值）。
///    这个延迟直接量化 UI 线程被占住的时间：同步 FFI、大批量集合更新都会在它上面冒尖，
///    即使没有真正掉帧；
/// 3) UI 线程操作：同步 FFI 调用（由 <c>CoreHostService.TryUseHandle</c> 打点）与主页面图表重绘
///    的耗时，超过 5ms 才落盘，用于给停顿样本提供“当时在干什么”的标签；
/// 4) 每帧实际成本（本文件新增的核心指标）：在场景起点、每个整秒、场景终点采样 UI 线程 CPU
///    （GetThreadTimes，句柄在 UI 线程上获取）、进程 CPU（GetProcessTimes）、本进程 GPU 引擎
///    利用率与显存占用（PDH）以及渲染回调数。本机 GetThreadTimes 对高频唤醒线程的记账偏差
///    很大（同一区间可比 cycle 计数少记数倍），因此额外用 QueryThreadCycleTime 标定出细粒度
///    CPU 毫秒，作为 UI/进程 CPU 的主口径；GetThreadTimes 原值保留为对照字段。
///    帧间隔只能证明“节拍没掉”，成本采样才能回答“这一帧到底花了多少个 6.25ms 预算里的毫秒”。
///
/// 输出（都写在 &lt;dataRoot&gt;，由 <see cref="CoreConfigBuilder.ResolveDataRoot"/> 决定：
/// 默认 <c>%LOCALAPPDATA%\ClipBridge</c>，便携模式下为 <c>&lt;exeDir&gt;\data</c>）：
/// - <c>frames-&lt;scenario&gt;-&lt;timestamp&gt;.csv</c>：<c>wallcloack_us,delta_us,scenario</c>；
/// - <c>stalls-&lt;scenario&gt;-&lt;timestamp&gt;.csv</c>：<c>wallcloack_us,stall_us,scenario,queue_depth</c>；
/// - <c>uiops-&lt;scenario&gt;-&lt;timestamp&gt;.csv</c>：<c>wallcloack_us,duration_us,label,scenario</c>；
/// - <c>cost-&lt;scenario&gt;-&lt;timestamp&gt;.csv</c>：每秒成本时间序列
///   <c>wallcloack_us,ui_cpu_ms_delta,proc_cpu_ms_delta,gpu_util_pct,frames</c>；
/// - <c>summary-&lt;scenario&gt;-&lt;timestamp&gt;.txt</c>：统计汇总 + <c>[cost]</c> 成本块
///   + 1%/0.1% low 帧时间与超预算帧分布 + 停顿探针的 1%/0.1% low（同时写 stdout，需要调用方重定向）。
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

    /// <summary>一帧的预算：160Hz 显示器即 6.25ms。成本占比都以它为分母。</summary>
    private const double FrameBudgetMs = 6.25;

    /// <summary>成本采样周期（秒）。</summary>
    private const double CostSampleIntervalS = 1.0;

    /// <summary>drag 场景轨迹的水平/垂直最大振幅（像素）。</summary>
    private const double DragAmplitudePx = 60.0;

    /// <summary>drag 场景临时把窗口缩到的小尺寸，避免测量时挡住屏幕中央。</summary>
    private const int DragWindowWidthPx = 900;

    private const int DragWindowHeightPx = 600;

    /// <summary>drag 轨迹靠屏幕右上角时的边距：至少留出振幅 + 24px，保证整段轨迹都在屏内。</summary>
    private const int DragCornerMarginPx = 84;

    /// <summary>缩窗/挪位后留给 XAML 重新布局的时间（毫秒），不计入场景采样。</summary>
    private const int DragResizeSettleMs = 300;

    /// <summary>drag 场景一个完整往复的周期（秒）。</summary>
    private const double DragPeriodSeconds = 1.2;

    /// <summary>drag 场景目标移动频率上限（Hz）；实际频率受渲染节拍限制，会记录在汇总里。</summary>
    private const int DragTicksPerSecond = 120;

    /// <summary>切换亚克力后留出的重绘稳定时间（毫秒），不属于任何场景的采样区间。</summary>
    private const int AcrylicSettleMs = 350;

    /// <summary>切换 Mica 后留出的重绘稳定时间（毫秒），不属于任何场景的采样区间。</summary>
    private const int MicaSettleMs = 350;

    /// <summary>退出看门狗的基础等待：Exit 路径被窗口关闭钩子取消时，最多等这么久就强制结束进程。</summary>
    private const int ExitWatchdogBaseMs = 8000;

    /// <summary>退出看门狗在“准备 + 记录”之外的余量（毫秒）。</summary>
    private const int ExitWatchdogMarginMs = 5000;

    /// <summary>SetWindowPos 只改位置：不改尺寸、不改 Z 序、不激活窗口。</summary>
    private const uint SwpNoSize = 0x0001;

    private const uint SwpNoZOrder = 0x0004;

    private const uint SwpNoActivate = 0x0010;

    /// <summary>SystemParametersInfo 的 SPI_GETWORKAREA：取主显示器工作区（不含任务栏）。</summary>
    private const uint SpiGetWorkArea = 0x0030;

    /// <summary>GetThreadTimes/GetProcessTimes 的单位是 100ns，换算成毫秒要除以 10000。</summary>
    private const double MsPerCpuTick = 1.0 / 10_000.0;

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
        Drag = 3,
        Ended = 4,

        /// <summary>drag 结束后还原窗口尺寸/位置的收尾区间：单独标记，不计入 drag 的帧统计。</summary>
        Teardown = 5,

        /// <summary>manual 场景：应用不碰窗口，由人手动拖动，只记录帧时序。</summary>
        Manual = 6
    }

    /// <summary>
    /// 是否启用。只在类型初始化时读一次环境变量；未设置时每个入口方法只是一次字段读取 + 返回，
    /// 不订阅事件、不起线程、不建文件，行为与没有这段诊断代码完全等价。
    /// </summary>
    internal static readonly bool Enabled =
        string.Equals(Environment.GetEnvironmentVariable("CB_FRAME_TRACE"), "1", StringComparison.Ordinal);

    /// <summary>Stopwatch 一个 tick 对应的微秒数（QueryPerformanceCounter 频率换算）。</summary>
    private static readonly double UsPerTick = 1_000_000.0 / Stopwatch.Frequency;

    /// <summary>逻辑处理器数，用于把进程 CPU 归一化成“整机百分比”。</summary>
    private static readonly int LogicalProcessors = Math.Max(1, Environment.ProcessorCount);

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
    private static double _idleSeconds = 3.0;
    private static double _scrollSeconds = 6.0;
    private static double _dragSeconds = 3.0;
    private static double _manualSeconds = 5.0;
    private static double _readySeconds = 2.0;
    private static bool _doManual;
    private static bool _windowVisibleAtStart;
    private static bool _windowForegroundAtStart;

    /// <summary>CB_FRAME_TRACE_ACRYLIC 的原始值（unset / 0 / 1 / 其他非法值），写进汇总。</summary>
    private static string _acrylicRequested = "unset";

    /// <summary>实际生效的亚克力状态（on / off / unknown / failed:...），写进 [cost] 与汇总。</summary>
    private static string _acrylicEffective = "unknown";

    /// <summary>亚克力状态的来源，便于之后判断这轮用的到底是哪个开关。</summary>
    private static string _acrylicSource = "(not-read)";

    /// <summary>CB_FRAME_TRACE_MICA 的原始值（unset / 0 / 1 / 其他非法值），写进汇总。</summary>
    private static string _micaRequested = "unset";

    /// <summary>实际生效的 Mica 状态（on / off / unknown / failed:...），写进 [cost] 与汇总。</summary>
    private static string _micaEffective = "unknown";

    /// <summary>Mica 状态的来源，便于之后判断这轮用的到底是哪个开关。</summary>
    private static string _micaSource = "(not-read)";

    /// <summary>
    /// MainWindow 构造期创建的那个 MicaBackdrop 实例。A/B 关掉时先把实例存在这里，
    /// 再打开时复用同一个对象（不在本类里新建第二套 Mica 实现）。只在 UI 线程上读写。
    /// </summary>
    private static SystemBackdrop? _micaOriginalBackdrop;

    // ---- drag 场景状态（StartWindowDrag 在 phase 翻到 Drag 之前写好，OnRendering 逐帧读取） ----
    private static bool _dragActive;
    private static long _dragStartTimestamp;
    private static long _dragLastMoveTimestamp;
    private static int _dragMoveCount;
    private static long _dragMoveTotalTicks;
    private static double _dragMoveAvgMs = double.NaN;
    private static int _dragMoves;
    private static int _dragAmplitudeX;
    private static int _dragAmplitudeY;
    private static int _dragBaseX;
    private static int _dragBaseY;
    private static int _dragWorkLeft;
    private static int _dragWorkTop;
    private static int _dragWorkRight;
    private static int _dragWorkBottom;
    private static int _dragWindowWidth;
    private static int _dragWindowHeight;
    private static double _dragActualSeconds;
    private static double _dragActualHz = double.NaN;
    private static string _dragStatus = "not-run";
    private static string _dragRectStart = "(none)";
    private static string _dragRectEnd = "(none)";
    private static Rect _dragOriginalRect;
    private static bool _dragOriginalRectValid;

    /// <summary>UI 线程的原生线程句柄（在 UI 线程上 GetCurrentThread + DuplicateHandle 得到）。</summary>
    private static IntPtr _uiThreadHandle;

    /// <summary>UI 线程的原生线程 id，仅在 DuplicateHandle 失败时用于兜底 OpenThread。</summary>
    private static uint _uiThreadNativeId;

    /// <summary>渲染回调累计计数；成本采样用相邻差值得到每个区间内的帧数。</summary>
    private static int _renderingCount;

    /// <summary>CB_FRAME_TRACE_MINIMIZE=1：场景开始前最小化窗口，作为“隐藏窗口”对照组。</summary>
    private static bool _minimizeAfterStart;

    /// <summary>主显示器刷新率，启动时读一次，避免汇总时再走 GDI。</summary>
    private static int _displayRefreshHz;

    /// <summary>每秒成本样本（只在场景编排线程上读写，不需要加锁）。</summary>
    private static readonly List<CostSample> CostSamples = new();

    /// <summary>每个场景的成本汇总（只在场景编排线程上读写）。</summary>
    private static readonly List<CostScenario> CostScenarios = new();

    /// <summary>PDH GPU 探针；初始化失败时为 null，成本汇总里如实标注“不可用”。</summary>
    private static GpuProbe? _gpuProbe;

    /// <summary>
    /// 桌面合成器（dwm.exe）的 GPU 探针。亚克力/毛玻璃是 DWM 侧的合成工作，
    /// 它的 GPU 成本不会记在本进程头上，所以 drag A/B 必须单独看这一路。
    /// </summary>
    private static GpuProbe? _dwmProbe;

    /// <summary>dwm.exe 的进程 id（当前会话）；0 表示没找到。</summary>
    private static int _dwmPid;

    /// <summary>
    /// QueryThreadCycleTime 的“每秒 cycle 数”，启动时用忙等标定（约 150ms）。
    /// 系统时钟节拍让 GetThreadTimes 只有 ~15.6ms 粒度，短场景读不到真实成本；
    /// cycle 计数是 TSC 级细粒度，除以该标定值即可换算成 CPU 毫秒。
    /// </summary>
    private static double _cyclesPerSecond;

    /// <summary>cycle 标定状态，写进汇总，避免把标定失败误读成“0 成本”。</summary>
    private static string _cyclesStatus = "not-calibrated";

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
        _displayRefreshHz = ReadDisplayRefreshHz();

        // 成本采样：必须在 UI 线程上拿线程句柄（GetCurrentThread 只代表“调用线程”）。
        // 复制成真实句柄后，场景编排线程才能随时读 GetThreadTimes。
        _uiThreadNativeId = GetCurrentThreadId();
        _uiThreadHandle = CaptureUiThreadHandle();

        // 隐藏窗口对照组：只有显式设置 CB_FRAME_TRACE_MINIMIZE=1 才最小化，正常行为不变。
        _minimizeAfterStart = string.Equals(
            Environment.GetEnvironmentVariable("CB_FRAME_TRACE_MINIMIZE"), "1", StringComparison.Ordinal);

        // 场景选择：显式写错时按 all 处理，保证“设了开关就一定有数据”。
        var scenario = (Environment.GetEnvironmentVariable("CB_FRAME_TRACE_SCENARIO") ?? "all").Trim().ToLowerInvariant();
        var doIdle = scenario is "idle" or "all" or "";
        var doScroll = scenario is "scroll" or "all" or "";
        var doDrag = scenario is "drag" or "all" or "";
        var doManual = scenario is "manual" or "all" or "";
        if (!doIdle && !doScroll && !doDrag && !doManual)
        {
            doIdle = true;
            doScroll = true;
            doDrag = true;
            doManual = true;
        }
        _doManual = doManual;
        _runTag = doIdle && doScroll && doDrag && doManual
            ? "all"
            : (doIdle ? "idle" : (doScroll ? "scroll" : (doDrag ? "drag" : "manual")));

        // 亚克力 A/B：只认 0/1；其他值（含未设置）一律不改变当前设置，但原始值会写进汇总。
        var acrylicText = (Environment.GetEnvironmentVariable("CB_FRAME_TRACE_ACRYLIC") ?? string.Empty).Trim();
        _acrylicRequested = acrylicText.Length == 0 ? "unset" : acrylicText;

        // Mica A/B：同样只认 0/1；未设置时只读当前状态，不改变任何东西。
        var micaText = (Environment.GetEnvironmentVariable("CB_FRAME_TRACE_MICA") ?? string.Empty).Trim();
        _micaRequested = micaText.Length == 0 ? "unset" : micaText;

        // 时长覆盖：一个变量同时改所有场景，保持“采样预算可调”的简单语义。
        var secondsText = Environment.GetEnvironmentVariable("CB_FRAME_TRACE_SECONDS");
        if (!string.IsNullOrWhiteSpace(secondsText)
            && double.TryParse(secondsText, NumberStyles.Float, CultureInfo.InvariantCulture, out var seconds)
            && seconds > 0.5)
        {
            _idleSeconds = seconds;
            _scrollSeconds = seconds;
            _dragSeconds = seconds;
            _manualSeconds = seconds;
        }

        // manual 的准备时长单独可调：它不参与任何采样，只是留给操作者握住窗口的时间。
        var readySecondsText = Environment.GetEnvironmentVariable("CB_FRAME_TRACE_READY_SECONDS");
        if (!string.IsNullOrWhiteSpace(readySecondsText)
            && double.TryParse(readySecondsText, NumberStyles.Float, CultureInfo.InvariantCulture, out var readySeconds)
            && readySeconds >= 0.0)
        {
            _readySeconds = readySeconds;
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

        // 场景编排：等主页面 → 应用亚克力/Mica A/B → idle → scroll → drag → manual → 落盘 → 退出。
        // 用独立线程而不是线程池，保证即使 UI 线程完全卡死，它也能按时收尾并写文件。
        var planThread = new Thread(() => RunPlan(doIdle, doScroll, doDrag, doManual))
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
        Interlocked.Increment(ref _renderingCount);

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

        // drag 场景：每个渲染节拍在 UI 线程推进一次窗口位置（与 scroll 相同的节拍来源），
        // 窗口移动本身的开销也计入 UI 线程；移动失败只影响轨迹，不影响帧时序采集。
        if (phase == (int)Phase.Drag && Volatile.Read(ref _dragActive))
        {
            try
            {
                DragStep();
            }
            catch
            {
                // 与滚动驱动一致：单个场景的驱动异常不能让整个测量崩掉。
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

    /// <summary>场景编排：等主页面 → 应用亚克力/Mica A/B → idle → scroll → drag → manual → 收尾。</summary>
    private static void RunPlan(bool doIdle, bool doScroll, bool doDrag, bool doManual)
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

            // 成本探针在场景开始前初始化；PDH 失败也只降级，不影响帧时序采集。
            _gpuProbe = new GpuProbe(Environment.ProcessId);
            _gpuProbe.TryOpen();

            // DWM 的 GPU 引擎利用率：亚克力合成发生在桌面合成器里，必须单独采一路。
            _dwmPid = FindDwmProcessId();
            if (_dwmPid > 0)
            {
                _dwmProbe = new GpuProbe(_dwmPid);
                _dwmProbe.TryOpen();
            }

            // 标定 cycle 时钟：GetThreadTimes 的记账粒度是系统时钟节拍（本机约 15.6ms），
            // 不足以刻画 6.25ms 帧预算；cycle 计数可以给出亚毫秒级的真实 UI 线程消耗。
            CalibrateCycleClock();

            // 亚克力 A/B 必须落在所有场景开始之前：改的是运行时绑定属性（立即换画刷），
            // 并留出重绘稳定时间，避免把切换本身的成本算进第一个场景。
            ApplyAcrylicSetting();

            // Mica A/B 同理：只换主窗口的 SystemBackdrop，失败也只降级为“unknown/failed”，不打断测量。
            ApplyMicaSetting();

            if (_minimizeAfterStart)
            {
                // 隐藏窗口对照：只在本环境变量显式开启时最小化，正常行为完全不变。
                _ = ShowWindow(_hwnd, SwMinimize);
            }

            if (doIdle)
            {
                RunScenarioWithCost(Phase.Idle, _idleSeconds);
            }

            if (doScroll)
            {
                RunScenarioWithCost(Phase.Scroll, _scrollSeconds);
            }

            if (doDrag)
            {
                RunScenarioWithCost(Phase.Drag, _dragSeconds);
            }

            // manual 放最后：drag 的收尾（还原窗口）已经完成，操作者拿到的是一个静止、可拖的窗口。
            if (doManual)
            {
                RunManualScenario();
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

    /// <summary>
    /// 把 CB_FRAME_TRACE_ACRYLIC 请求的状态落到主页 ViewModel 的绑定属性上，并读回实际状态。
    /// 未设置该变量时只读取当前状态（不改任何东西），让每轮数据都能自描述。
    /// </summary>
    private static void ApplyAcrylicSetting()
    {
        if (_acrylicRequested is "0" or "1")
        {
            var enable = _acrylicRequested == "1";
            var effective = SetAcrylicState(enable, out var error);
            if (effective != enable)
            {
                // 绑定的首次求值/异步设置加载可能刚好覆盖我们的写入；稍等后重试一次。
                Thread.Sleep(150);
                effective = SetAcrylicState(enable, out error);
            }

            _acrylicEffective = effective switch
            {
                true => "on",
                false => "off",
                null => "failed:" + (string.IsNullOrEmpty(error) ? "unknown" : error)
            };
            _acrylicSource = "MainViewModel.DisableAcrylicOnCards+DisableAcrylicOnStatsCards";
            Thread.Sleep(AcrylicSettleMs);
            return;
        }

        var current = ReadAcrylicState(out var readError);
        _acrylicEffective = current switch
        {
            true => "on",
            false => "off",
            null => "unknown:" + (string.IsNullOrEmpty(readError) ? "unavailable" : readError)
        };
        _acrylicSource = current == null ? "(not-read)" : "MainViewModel.DisableAcrylicOnCards+DisableAcrylicOnStatsCards";
    }

    /// <summary>在 UI 线程上写入两个亚克力开关并读回是否生效（true=亚克力开启）。</summary>
    private static bool? SetAcrylicState(bool enable, out string error)
    {
        return RunOnUiThread(
            () =>
            {
                var viewModel = ClipBridgeShell_CS.App.GetService<ClipBridgeShell_CS.ViewModels.MainViewModel>();
                // 实际驱动首页绑定的是 MainViewModel 的“关闭亚克力”反向属性；
                // SettingsViewModel.EnableAcrylicOn* 只负责持久化，不参与首页绑定，改它需要重启。
                viewModel.DisableAcrylicOnCards = !enable;
                viewModel.DisableAcrylicOnStatsCards = !enable;
                return ReadAcrylicStateFrom(viewModel);
            },
            out error);
    }

    /// <summary>只读当前亚克力状态（true=开启）；VM 不可达时返回 null 并给出原因。</summary>
    private static bool? ReadAcrylicState(out string error)
    {
        return RunOnUiThread(
            () => ReadAcrylicStateFrom(ClipBridgeShell_CS.App.GetService<ClipBridgeShell_CS.ViewModels.MainViewModel>()),
            out error);
    }

    /// <summary>
    /// 读取两处绑定源的合并状态：两处都为“启用”才是 on，都为“关闭”才是 off，
    /// 不一致时返回 null（汇总里会标成 failed:state-mixed，避免把混合状态当成单值）。
    /// </summary>
    private static bool? ReadAcrylicStateFrom(ClipBridgeShell_CS.ViewModels.MainViewModel viewModel)
    {
        var cardsEnabled = !viewModel.DisableAcrylicOnCards;
        var statsEnabled = !viewModel.DisableAcrylicOnStatsCards;
        return cardsEnabled == statsEnabled ? cardsEnabled : null;
    }

    /// <summary>把委托投递到 UI 线程执行并等待结果；超时/异常都转为 null + 原因，不抛出。</summary>
    private static bool? RunOnUiThread(Func<bool?> action, out string error)
    {
        var queue = _queue;
        error = string.Empty;
        if (queue == null)
        {
            error = "no-queue";
            return null;
        }

        bool? result = null;
        var errorText = string.Empty;
        using var done = new ManualResetEventSlim(false);
        var accepted = queue.TryEnqueue(() =>
        {
            try
            {
                result = action();
            }
            catch (Exception ex)
            {
                errorText = ex.GetType().Name;
            }
            finally
            {
                done.Set();
            }
        });

        if (!accepted)
        {
            error = "enqueue-rejected";
            return null;
        }

        if (!done.Wait(3000))
        {
            error = "ui-thread-timeout";
            return null;
        }

        if (errorText.Length > 0)
        {
            error = errorText;
            return null;
        }

        if (result == null)
        {
            error = "state-mixed";
            return null;
        }

        return result;
    }

    /// <summary>
    /// 把 CB_FRAME_TRACE_MICA 请求的状态落到主窗口的 SystemBackdrop 上，并读回实际状态。
    /// 只复用 MainWindow 构造期创建的同一个 MicaBackdrop 实例（见 MainWindow.xaml.cs 的
    /// <c>SystemBackdrop = new MicaBackdrop()</c>），不在本类里新增第二套 Mica 实现。
    /// 未设置该变量时只读取当前状态，不改任何东西；整段包在 try/catch 里，
    /// 切换失败只记录并继续，绝不让 A/B 开关打断测量。
    /// </summary>
    private static void ApplyMicaSetting()
    {
        try
        {
            if (_micaRequested is "0" or "1")
            {
                var enable = _micaRequested == "1";
                var effective = SetMicaState(enable, out var error);
                if (effective != enable)
                {
                    // 与亚克力一致：窗口/合成器可能刚完成上一轮设置，稍等后重试一次。
                    Thread.Sleep(150);
                    effective = SetMicaState(enable, out error);
                }

                _micaEffective = effective switch
                {
                    true => "on",
                    false => "off",
                    null => "failed:" + (string.IsNullOrEmpty(error) ? "unknown" : error)
                };
                _micaSource = "MainWindow.SystemBackdrop (MicaBackdrop)";
                if (effective == null)
                {
                    WriteStatusLine(FormattableString.Invariant($"[mica] 切换失败，继续记录：{_micaEffective}"));
                }
                else
                {
                    Thread.Sleep(MicaSettleMs);
                }

                return;
            }

            var current = ReadMicaState(out var readError);
            _micaEffective = current switch
            {
                true => "on",
                false => "off",
                null => "unknown:" + (string.IsNullOrEmpty(readError) ? "unavailable" : readError)
            };
            _micaSource = current == null ? "(not-read)" : "MainWindow.SystemBackdrop (MicaBackdrop)";
        }
        catch (Exception ex)
        {
            // 兜底：即使读状态本身抛异常，也只降级为 failed 并继续。
            _micaEffective = "failed:" + ex.GetType().Name;
            _micaSource = "MainWindow.SystemBackdrop (MicaBackdrop)";
            WriteStatusLine(FormattableString.Invariant($"[mica] 切换异常，继续记录：{ex.GetType().Name}"));
        }
    }

    /// <summary>在 UI 线程上把主窗口的 SystemBackdrop 切到 Mica 或 null，并读回是否处于 Mica 状态。</summary>
    private static bool? SetMicaState(bool enable, out string error)
    {
        return RunOnUiThread(
            () =>
            {
                var window = ClipBridgeShell_CS.App.MainWindow;
                if (window == null)
                {
                    throw new InvalidOperationException("no-main-window");
                }

                if (enable)
                {
                    // 切走前先记下 MainWindow 构造期创建的那个 MicaBackdrop；切回时复用同一个对象。
                    _micaOriginalBackdrop ??= window.SystemBackdrop as MicaBackdrop;
                    if (_micaOriginalBackdrop == null)
                    {
                        throw new InvalidOperationException("no-mica-backdrop");
                    }

                    window.SystemBackdrop = _micaOriginalBackdrop;
                }
                else
                {
                    if (window.SystemBackdrop is MicaBackdrop mica)
                    {
                        _micaOriginalBackdrop = mica;
                    }

                    window.SystemBackdrop = null;
                }

                return window.SystemBackdrop is MicaBackdrop;
            },
            out error);
    }

    /// <summary>只读当前 Mica 状态（true=Mica 生效）；主窗口不可达时返回 null 并给出原因。</summary>
    private static bool? ReadMicaState(out string error)
    {
        return RunOnUiThread(
            () =>
            {
                var window = ClipBridgeShell_CS.App.MainWindow;
                if (window == null)
                {
                    throw new InvalidOperationException("no-main-window");
                }

                return window.SystemBackdrop is MicaBackdrop;
            },
            out error);
    }

    /// <summary>
    /// manual 场景：应用完全不碰窗口。先打印准备提示并倒计时，让操作者握住窗口；
    /// 再按 CB_FRAME_TRACE_SECONDS（默认 5 秒）采集帧时序。期间不调用任何
    /// SetWindowPos/Activate/缩放/重定位，窗口的位置与状态完全由人控制。
    /// </summary>
    private static void RunManualScenario()
    {
        WriteStatusLine(FormattableString.Invariant(
            $"[manual] 准备 {_readySeconds:0.#} 秒后开始记录：请开始拖动窗口"));
        WaitWithCountdown(_readySeconds);
        WriteStatusLine(FormattableString.Invariant($"[manual] 记录中 {_manualSeconds:0.#} 秒…"));
        RunScenarioWithCost(Phase.Manual, _manualSeconds);
        WriteStatusLine("[manual] 记录结束");
    }

    /// <summary>准备阶段的等待：按整秒打印倒计时，结束时正好进入记录区间。</summary>
    private static void WaitWithCountdown(double seconds)
    {
        if (seconds <= 0.0)
        {
            return;
        }

        var start = Stopwatch.GetTimestamp();
        var lastRemaining = -1;
        while (true)
        {
            var remaining = seconds - ((Stopwatch.GetTimestamp() - start) / (double)Stopwatch.Frequency);
            if (remaining <= 0.0)
            {
                return;
            }

            var shown = (int)Math.Ceiling(remaining);
            if (shown != lastRemaining)
            {
                WriteStatusLine(FormattableString.Invariant($"[manual] 倒计时 {shown}"));
                lastRemaining = shown;
            }

            Thread.Sleep(50);
        }
    }

    /// <summary>
    /// 跑一个场景：在场景起点、每个整秒、场景终点各采一次成本。采样点同时写入
    /// cost CSV 的逐秒时间序列与场景汇总。睡眠仍按 ≤50ms 分段，保证阶段切换足够及时。
    /// drag 场景在该区间内由渲染回调逐帧移动窗口，采样口径与其它场景完全一致；
    /// manual 场景不驱动任何窗口动作，只把人的手动拖拽原样记录下来。
    /// </summary>
    private static void RunScenarioWithCost(Phase phase, double seconds)
    {
        // drag 的轨迹基准/振幅必须在 phase 翻到 Drag 之前写好，OnRendering 才能直接使用。
        if (phase == Phase.Drag)
        {
            StartWindowDrag();
        }

        SetPhase(phase);

        var scenarioSampleStart = CostSamples.Count;
        var startTimestamp = Stopwatch.GetTimestamp();
        var targetUs = seconds * 1_000_000.0;

        // GPU 利用率需要两次 PdhCollectQueryData 才有区间值：场景起点先采一次作基线。
        _gpuProbe?.Prime();
        _dwmProbe?.Prime();

        var prevTimestamp = startTimestamp;
        var prevUiTicks = ReadUiThreadCpuTicks();
        var prevProcTicks = ReadProcessCpuTicks();
        var prevUiCycles = ReadUiThreadCycles();
        var prevProcCycles = ReadProcessCycles();
        var prevFrames = Volatile.Read(ref _renderingCount);
        var nextBoundaryUs = CostSampleIntervalS * 1_000_000.0;

        try
        {
            while (true)
            {
                var now = Stopwatch.GetTimestamp();
                var elapsedUs = (now - startTimestamp) * UsPerTick;
                if (elapsedUs >= targetUs)
                {
                    // 场景终点：补最后一个区间；刚采过样时不再补一个 0 长度样本。
                    if ((now - prevTimestamp) * UsPerTick >= 250_000.0)
                    {
                        SampleCost(phase, now, ref prevTimestamp, ref prevUiTicks, ref prevProcTicks, ref prevUiCycles, ref prevProcCycles, ref prevFrames);
                    }

                    break;
                }

                var remainingUs = nextBoundaryUs - elapsedUs;
                if (remainingUs <= 0)
                {
                    SampleCost(phase, now, ref prevTimestamp, ref prevUiTicks, ref prevProcTicks, ref prevUiCycles, ref prevProcCycles, ref prevFrames);
                    do
                    {
                        nextBoundaryUs += CostSampleIntervalS * 1_000_000.0;
                    }
                    while (nextBoundaryUs <= elapsedUs);

                    continue;
                }

                // 50ms 上限保证阶段边界与采样点都足够准，同时不忙等。
                Thread.Sleep((int)Math.Clamp(remainingUs / 1000.0, 1.0, 50.0));
            }
        }
        finally
        {
            if (phase == Phase.Drag)
            {
                // 还原窗口尺寸/位置会触发一次重排，可能产生大帧间隔；单独标成 teardown，
                // 不让收尾成本污染 drag 的尾部指标（CSV 里仍能看到这些样本）。
                SetPhase(Phase.Teardown);
                StopWindowDrag();
            }
        }

        BuildCostScenario(phase, scenarioSampleStart, seconds);
    }

    /// <summary>
    /// 准备 drag 场景：把窗口临时缩到约 900x600 并停在主屏右上角（留出边距），
    /// 再在该点周围做水平/垂直 ±60px 的往复轨迹（三角波，x/y 相位差 90°，形成斜向折返）。
    /// 真正的移动由 <see cref="OnRendering"/> 在 UI 线程逐帧驱动（和 scroll 场景同样的节拍来源），
    /// 保证每个渲染周期窗口都换位置、DWM 必须重新合成。全程只用 SWP_NOZORDER|SWP_NOACTIVATE
    /// （不抢焦点、不改 Z 序），轨迹始终留在工作区内；场景结束后窗口还原到原始位置与尺寸。
    /// </summary>
    private static void StartWindowDrag()
    {
        Interlocked.Exchange(ref _dragMoveCount, 0);
        Interlocked.Exchange(ref _dragMoveTotalTicks, 0L);
        _dragMoves = 0;
        _dragActualSeconds = 0.0;
        _dragActualHz = double.NaN;
        _dragMoveAvgMs = double.NaN;
        _dragRectStart = "(none)";
        _dragRectEnd = "(none)";
        _dragOriginalRectValid = false;
        _dragStatus = "started";
        Volatile.Write(ref _dragActive, false);

        if (_hwnd == IntPtr.Zero)
        {
            _dragStatus = "unavailable:no-hwnd";
            return;
        }

        if (!GetWindowRect(_hwnd, out var rect) || rect.Right <= rect.Left || rect.Bottom <= rect.Top)
        {
            _dragStatus = "unavailable:getwindowrect";
            return;
        }

        var originalWidth = rect.Right - rect.Left;
        var originalHeight = rect.Bottom - rect.Top;

        // 工作区（不含任务栏）；SystemParametersInfo 失败时退回整屏，仍然保证不会离屏。
        var workLeft = 0;
        var workTop = 0;
        var workRight = Math.Max(1, GetSystemMetrics(SmCxScreen));
        var workBottom = Math.Max(1, GetSystemMetrics(SmCyScreen));
        if (SystemParametersInfoW(SpiGetWorkArea, 0, out var work, 0)
            && work.Right > work.Left
            && work.Bottom > work.Top)
        {
            workLeft = work.Left;
            workTop = work.Top;
            workRight = work.Right;
            workBottom = work.Bottom;
        }

        // 临时缩到小尺寸并停在右上角：不挡屏幕中央，同时留出 ±振幅的边距保证整段轨迹都在屏内。
        // 窗口尺寸只在 drag 场景内改动，结束后连同位置一起还原。
        var targetWidth = Math.Min(DragWindowWidthPx, Math.Max(320, workRight - workLeft));
        var targetHeight = Math.Min(DragWindowHeightPx, Math.Max(240, workBottom - workTop));
        var maxX = Math.Max(workLeft, workRight - targetWidth);
        var maxY = Math.Max(workTop, workBottom - targetHeight);
        var baseX = Math.Max(workLeft, maxX - DragCornerMarginPx);
        var baseY = Math.Min(maxY, workTop + DragCornerMarginPx);

        var amplitudeX = (int)Math.Max(0.0, Math.Min(DragAmplitudePx, Math.Min(baseX - workLeft, maxX - baseX)));
        var amplitudeY = (int)Math.Max(0.0, Math.Min(DragAmplitudePx, Math.Min(baseY - workTop, maxY - baseY)));

        _dragOriginalRect = rect;
        _dragOriginalRectValid = true;
        _dragAmplitudeX = amplitudeX;
        _dragAmplitudeY = amplitudeY;
        _dragBaseX = baseX;
        _dragBaseY = baseY;
        _dragWorkLeft = workLeft;
        _dragWorkTop = workTop;
        _dragWorkRight = workRight;
        _dragWorkBottom = workBottom;
        _dragWindowWidth = targetWidth;
        _dragWindowHeight = targetHeight;
        _dragRectStart = DescribeRect(rect);

        if (amplitudeX <= 0 && amplitudeY <= 0)
        {
            _dragStatus = "no-move:window-fills-workarea";
            return;
        }

        // 位置+尺寸一次改完；缩窗会触发 XAML 重新布局，留出一小段稳定时间（尚不属于任何场景采样）。
        if (targetWidth != originalWidth
            || targetHeight != originalHeight
            || baseX != rect.Left
            || baseY != rect.Top)
        {
            _ = SetWindowPos(
                _hwnd,
                IntPtr.Zero,
                baseX,
                baseY,
                targetWidth,
                targetHeight,
                SwpNoZOrder | SwpNoActivate);
            Thread.Sleep(DragResizeSettleMs);
        }

        // 起点/振幅在 phase 翻到 Drag 之前就写好（StartWindowDrag 在 SetPhase 之前调用），
        // OnRendering 只读这些已经稳定的值。
        _dragStartTimestamp = Stopwatch.GetTimestamp();
        _dragLastMoveTimestamp = 0L;
        Volatile.Write(ref _dragActive, true);
    }

    /// <summary>
    /// 每个渲染节拍在 UI 线程推进一次 drag 轨迹（目标 ~120Hz，显示刷新更快时按最小间隔节流）。
    /// 移动与渲染回调同线程，所以“移动窗口”本身的耗时也计入本场景的 UI CPU。
    /// </summary>
    private static void DragStep()
    {
        var now = Stopwatch.GetTimestamp();
        if (_dragLastMoveTimestamp != 0L
            && (now - _dragLastMoveTimestamp) / (double)Stopwatch.Frequency < 1.0 / DragTicksPerSecond)
        {
            return;
        }

        _dragLastMoveTimestamp = now;

        var elapsedS = (now - _dragStartTimestamp) / (double)Stopwatch.Frequency;
        var phase = 2.0 * Math.PI * elapsedS / DragPeriodSeconds;

        var x = _dragBaseX + (int)Math.Round(_dragAmplitudeX * TriangleWave(phase));
        var y = _dragBaseY + (int)Math.Round(_dragAmplitudeY * TriangleWave(phase + (Math.PI / 2.0)));

        // 轨迹本身已在工作区内；这里再夹一次，避免 DPI 取整把窗口边缘推出屏幕。
        x = Math.Clamp(x, _dragWorkLeft, Math.Max(_dragWorkLeft, _dragWorkRight - _dragWindowWidth));
        y = Math.Clamp(y, _dragWorkTop, Math.Max(_dragWorkTop, _dragWorkBottom - _dragWindowHeight));

        var moveStart = Stopwatch.GetTimestamp();
        if (SetWindowPos(_hwnd, IntPtr.Zero, x, y, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate))
        {
            Interlocked.Increment(ref _dragMoveCount);
        }

        Interlocked.Add(ref _dragMoveTotalTicks, Stopwatch.GetTimestamp() - moveStart);
    }

    /// <summary>结束 drag：停止逐帧移动、统计实际移动频率、把窗口还原到起始位置。</summary>
    private static void StopWindowDrag()
    {
        Volatile.Write(ref _dragActive, false);

        _dragActualSeconds = (Stopwatch.GetTimestamp() - _dragStartTimestamp) / (double)Stopwatch.Frequency;
        _dragMoves = Volatile.Read(ref _dragMoveCount);
        var moveTotalTicks = Interlocked.Read(ref _dragMoveTotalTicks);
        _dragMoveAvgMs = _dragMoves > 0 ? moveTotalTicks * UsPerTick / 1000.0 / _dragMoves : double.NaN;
        _dragActualHz = _dragActualSeconds > 0 ? _dragMoves / _dragActualSeconds : double.NaN;
        if (_dragMoves <= 0 && _dragStatus == "started")
        {
            _dragStatus = "no-move:setwindowpos-failed";
        }

        // 还原原始位置与尺寸（drag 场景临时缩成了小窗）；程序随后就会退出，这里主要是保持窗口状态干净。
        if (_dragOriginalRectValid)
        {
            _ = SetWindowPos(
                _hwnd,
                IntPtr.Zero,
                _dragOriginalRect.Left,
                _dragOriginalRect.Top,
                _dragOriginalRect.Right - _dragOriginalRect.Left,
                _dragOriginalRect.Bottom - _dragOriginalRect.Top,
                SwpNoZOrder | SwpNoActivate);
            _dragOriginalRectValid = false;
        }

        _dragRectEnd = DescribeWindowRect();
        if (_dragStatus == "started" && _dragMoves > 0)
        {
            _dragStatus = "moved";
        }
    }

    /// <summary>
    /// 相位到 [-1,1] 的三角波：phase=0 时为 0，随后向负方向折返。
    /// 用折返而不是正弦，轨迹速度恒定，更接近用户持续拖拽时窗口的移动方式。
    /// </summary>
    private static double TriangleWave(double phase)
    {
        var t = (phase / (2.0 * Math.PI)) + 0.25;
        t -= Math.Floor(t);
        return (4.0 * Math.Abs(t - 0.5)) - 1.0;
    }

    private static string DescribeRect(Rect rect)
    {
        return FormattableString.Invariant($"{rect.Right - rect.Left}x{rect.Bottom - rect.Top} at ({rect.Left},{rect.Top})");
    }

    /// <summary>找当前会话的桌面合成器进程（dwm.exe）。找不到时返回 0，DWM 指标降级为不可用。</summary>
    private static int FindDwmProcessId()
    {
        try
        {
            var sessionId = Process.GetCurrentProcess().SessionId;
            foreach (var process in Process.GetProcessesByName("dwm"))
            {
                try
                {
                    if (process.SessionId == sessionId)
                    {
                        return process.Id;
                    }
                }
                catch
                {
                    // 单个进程读不到 SessionId 时跳过即可。
                }
            }
        }
        catch
        {
            // 枚举失败只影响 DWM 指标，不影响主测量。
        }

        return 0;
    }

    /// <summary>
    /// 采一个成本样本：UI 线程 CPU（两种口径）、进程 CPU、渲染回调数、GPU 利用率/显存。
    /// ref 参数保存上一采样点；所有累计量都是 100ns / cycle 单位，差值是本区间的真实消耗。
    /// </summary>
    private static void SampleCost(
        Phase phase,
        long nowTimestamp,
        ref long prevTimestamp,
        ref long prevUiTicks,
        ref long prevProcTicks,
        ref long prevUiCycles,
        ref long prevProcCycles,
        ref int prevFrames)
    {
        var uiTicks = ReadUiThreadCpuTicks();
        var procTicks = ReadProcessCpuTicks();
        var uiCycles = ReadUiThreadCycles();
        var procCycles = ReadProcessCycles();
        var frames = Volatile.Read(ref _renderingCount);
        var gpu = _gpuProbe?.Read() ?? GpuReading.Unavailable();
        var dwmGpu = _dwmProbe?.Read() ?? GpuReading.Unavailable();

        var wallDeltaS = (nowTimestamp - prevTimestamp) / (double)Stopwatch.Frequency;
        var uiDeltaMs = prevUiTicks >= 0 && uiTicks >= 0 ? (uiTicks - prevUiTicks) * MsPerCpuTick : -1.0;
        var procDeltaMs = prevProcTicks >= 0 && procTicks >= 0 ? (procTicks - prevProcTicks) * MsPerCpuTick : -1.0;
        var uiCycleMs = CyclesToMs(prevUiCycles, uiCycles);
        var procCycleMs = CyclesToMs(prevProcCycles, procCycles);

        CostSamples.Add(new CostSample(
            WallUs(nowTimestamp),
            (int)phase,
            wallDeltaS,
            uiDeltaMs,
            procDeltaMs,
            frames - prevFrames,
            gpu,
            dwmGpu,
            uiCycleMs,
            procCycleMs));

        prevTimestamp = nowTimestamp;
        prevUiTicks = uiTicks;
        prevProcTicks = procTicks;
        prevUiCycles = uiCycles;
        prevProcCycles = procCycles;
        prevFrames = frames;
    }

    /// <summary>把两个 cycle 计数换算成本区间的 CPU 毫秒；缺任何一端就返回 -1。</summary>
    private static double CyclesToMs(long previousCycles, long currentCycles)
    {
        if (previousCycles < 0 || currentCycles < 0 || _cyclesPerSecond <= 0)
        {
            return -1.0;
        }

        return (currentCycles - previousCycles) / _cyclesPerSecond * 1000.0;
    }

    /// <summary>把一个场景的逐秒样本汇总成一条成本记录（wall/frames/CPU/GPU）。</summary>
    private static void BuildCostScenario(Phase phase, int sampleStart, double requestedSeconds)
    {
        var wallS = 0.0;
        var frames = 0;
        var samples = 0;
        var uiMs = 0.0;
        var procMs = 0.0;
        var uiCycleMs = 0.0;
        var procCycleMs = 0.0;
        var uiKnown = true;
        var procKnown = true;
        var uiCycleKnown = true;
        var procCycleKnown = true;
        var gpuSamples = 0;
        var gpuSumPct = 0.0;
        var gpuMaxPct = double.NaN;
        var engineMax = new Dictionary<string, double>(StringComparer.OrdinalIgnoreCase);
        var dedicatedBytes = -1L;
        var committedBytes = -1L;
        var dwmSamples = 0;
        var dwmSumPct = 0.0;
        var dwmMaxPct = double.NaN;
        var dwmEngineMax = new Dictionary<string, double>(StringComparer.OrdinalIgnoreCase);

        for (var i = sampleStart; i < CostSamples.Count; i++)
        {
            var sample = CostSamples[i];
            if (sample.Phase != (int)phase)
            {
                continue;
            }

            samples++;
            wallS += sample.WallDeltaS;
            frames += sample.FramesDelta;

            if (sample.UiCpuMsDelta >= 0)
            {
                uiMs += sample.UiCpuMsDelta;
            }
            else
            {
                uiKnown = false;
            }

            if (sample.ProcCpuMsDelta >= 0)
            {
                procMs += sample.ProcCpuMsDelta;
            }
            else
            {
                procKnown = false;
            }

            if (sample.UiCycleMsDelta >= 0)
            {
                uiCycleMs += sample.UiCycleMsDelta;
            }
            else
            {
                uiCycleKnown = false;
            }

            if (sample.ProcCycleMsDelta >= 0)
            {
                procCycleMs += sample.ProcCycleMsDelta;
            }
            else
            {
                procCycleKnown = false;
            }

            var gpu = sample.Gpu;
            if (gpu.EngineAvailable)
            {
                gpuSamples++;
                gpuSumPct += gpu.SumPct;
                gpuMaxPct = double.IsNaN(gpuMaxPct) ? gpu.MaxPct : Math.Max(gpuMaxPct, gpu.MaxPct);
                foreach (var pair in gpu.EnginePct)
                {
                    if (!engineMax.TryGetValue(pair.Key, out var existing) || pair.Value > existing)
                    {
                        engineMax[pair.Key] = pair.Value;
                    }
                }
            }

            if (gpu.DedicatedBytes >= 0)
            {
                dedicatedBytes = gpu.DedicatedBytes;
            }

            if (gpu.CommittedBytes >= 0)
            {
                committedBytes = gpu.CommittedBytes;
            }

            // DWM：亚克力合成发生在桌面合成器里，本进程的 GPU 计数看不到这部分。
            var dwmGpu = sample.DwmGpu;
            if (dwmGpu.EngineAvailable)
            {
                dwmSamples++;
                dwmSumPct += dwmGpu.SumPct;
                dwmMaxPct = double.IsNaN(dwmMaxPct) ? dwmGpu.MaxPct : Math.Max(dwmMaxPct, dwmGpu.MaxPct);
                foreach (var pair in dwmGpu.EnginePct)
                {
                    if (!dwmEngineMax.TryGetValue(pair.Key, out var existing) || pair.Value > existing)
                    {
                        dwmEngineMax[pair.Key] = pair.Value;
                    }
                }
            }
        }

        CostScenarios.Add(new CostScenario
        {
            PhaseName = PhaseName((int)phase),
            RequestedSeconds = requestedSeconds,
            WallS = wallS,
            Frames = frames,
            Samples = samples,
            UiCpuMs = uiKnown ? uiMs : double.NaN,
            ProcCpuMs = procKnown ? procMs : double.NaN,
            UiCycleCpuMs = uiCycleKnown ? uiCycleMs : double.NaN,
            ProcCycleCpuMs = procCycleKnown ? procCycleMs : double.NaN,
            GpuSampleCount = gpuSamples,
            GpuSumMeanPct = gpuSamples > 0 ? gpuSumPct / gpuSamples : double.NaN,
            GpuMaxPct = gpuMaxPct,
            GpuEngineMaxPct = engineMax,
            GpuDedicatedBytes = dedicatedBytes,
            GpuCommittedBytes = committedBytes,
            DwmGpuSampleCount = dwmSamples,
            DwmGpuSumMeanPct = dwmSamples > 0 ? dwmSumPct / dwmSamples : double.NaN,
            DwmGpuMaxPct = dwmMaxPct,
            DwmGpuEngineMaxPct = dwmEngineMax,
            DisplayRefreshHz = _displayRefreshHz,
            AcrylicRequested = _acrylicRequested,
            AcrylicEffective = _acrylicEffective,
            AcrylicSource = _acrylicSource,
            MicaRequested = _micaRequested,
            MicaEffective = _micaEffective,
            MicaSource = _micaSource,
            DragStatus = phase == Phase.Drag ? _dragStatus : "not-run",
            DragMoves = phase == Phase.Drag ? _dragMoves : 0,
            DragActualHz = phase == Phase.Drag ? _dragActualHz : double.NaN,
            DragMoveAvgMs = phase == Phase.Drag ? _dragMoveAvgMs : double.NaN,
            DragAmplitudeX = phase == Phase.Drag ? _dragAmplitudeX : 0,
            DragAmplitudeY = phase == Phase.Drag ? _dragAmplitudeY : 0,
            DragRectStart = phase == Phase.Drag ? _dragRectStart : "(none)",
            DragRectEnd = phase == Phase.Drag ? _dragRectEnd : "(none)"
        });
    }

    /// <summary>把单调时间戳换算成“自 Unix 纪元起的微秒”，四份 CSV 因此可以直接按时间对齐。</summary>
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

            // 释放诊断句柄/PDH 查询；即使进程马上退出，也保持资源释放的对称性。
            _gpuProbe?.Dispose();
            _gpuProbe = null;
            _dwmProbe?.Dispose();
            _dwmProbe = null;
            if (_uiThreadHandle != IntPtr.Zero)
            {
                _ = CloseHandle(_uiThreadHandle);
                _uiThreadHandle = IntPtr.Zero;
            }
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
                OpSamples = Ops.ToArray(),
                CostSeries = CostSamples.ToArray(),
                CostScenarioSummaries = CostScenarios.ToArray()
            };
        }
    }

    /// <summary>写四份 CSV；返回文件路径（用于汇总里报告落盘位置）。</summary>
    private static string[] WriteOutputs(Samples snapshot)
    {
        var dir = TraceDirectory();
        Directory.CreateDirectory(dir);

        var framesPath = Path.Combine(dir, $"frames-{_runTag}-{_fileStamp}.csv");
        var stallsPath = Path.Combine(dir, $"stalls-{_runTag}-{_fileStamp}.csv");
        var opsPath = Path.Combine(dir, $"uiops-{_runTag}-{_fileStamp}.csv");
        var costPath = Path.Combine(dir, $"cost-{_runTag}-{_fileStamp}.csv");

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

        // 每秒成本时间序列：列名按需求固定，GPU 不可用时写 -1（不伪造 0 会让“没采到”和“真 0”混淆）。
        // ui_cpu_ms_delta / proc_cpu_ms_delta 优先用 cycle 口径的细粒度值；本机 GetThreadTimes
        // 对高频唤醒线程记账偏差很大（逐线程外部交叉验证过），cycle 不可用时才退回。
        using (var writer = new StreamWriter(new FileStream(costPath, FileMode.Create, FileAccess.Write, FileShare.Read), new UTF8Encoding(false)))
        {
            writer.WriteLine("wallcloack_us,ui_cpu_ms_delta,proc_cpu_ms_delta,gpu_util_pct,frames");
            foreach (var sample in snapshot.CostSeries)
            {
                var uiCpuMs = sample.UiCycleMsDelta >= 0 ? sample.UiCycleMsDelta : sample.UiCpuMsDelta;
                var procCpuMs = sample.ProcCycleMsDelta >= 0 ? sample.ProcCycleMsDelta : sample.ProcCpuMsDelta;
                writer.Write(sample.WallUs.ToString(CultureInfo.InvariantCulture));
                writer.Write(',');
                writer.Write(FormatCostNumber(uiCpuMs, "F3"));
                writer.Write(',');
                writer.Write(FormatCostNumber(procCpuMs, "F3"));
                writer.Write(',');
                writer.Write(sample.Gpu.EngineAvailable ? sample.Gpu.SumPct.ToString("F2", CultureInfo.InvariantCulture) : "-1");
                writer.Write(',');
                writer.WriteLine(sample.FramesDelta.ToString(CultureInfo.InvariantCulture));
            }
        }

        return new[] { framesPath, stallsPath, opsPath, costPath };
    }

    /// <summary>汇总文本：统计数字 + 运行条件 + 最慢停顿/操作清单。stdout 与 summary 文件内容一致。</summary>
    private static string BuildSummary(Samples snapshot, string[] paths)
    {
        var sb = new StringBuilder();
        sb.AppendLine("=== ClipBridge FrameTrace summary ===");
        sb.AppendLine(FormattableString.Invariant(
            $"run={_runTag} started={_startedAtText} pid={Environment.ProcessId} scenario={_runTag} idle={_idleSeconds:F1}s scroll={_scrollSeconds:F1}s drag={_dragSeconds:F1}s manual={_manualSeconds:F1}s ready={_readySeconds:F1}s"));
        sb.AppendLine(FormattableString.Invariant(
            $"frames={snapshot.FrameSamples.Length} stalls={snapshot.StallSamples.Length} uiops={snapshot.OpSamples.Length} ffi_calls={Volatile.Read(ref _ffiCallCount)} ffi_on_ui_thread={Volatile.Read(ref _ffiUiThreadCallCount)} ffi_ui_over_5ms={Volatile.Read(ref _ffiOverThresholdCount)} scroll_ticks={Volatile.Read(ref _scrollTickCount)} scroll_target={_scrollTarget}"));
        // 亚克力/Mica A/B 与 drag 轨迹的自描述信息：看 [cost] 之前先能确认这轮到底测的是什么。
        sb.AppendLine(FormattableString.Invariant(
            $"acrylic requested={_acrylicRequested} effective={_acrylicEffective} source={_acrylicSource}"));
        sb.AppendLine(FormattableString.Invariant(
            $"mica requested={_micaRequested} effective={_micaEffective} source={_micaSource}"));
        sb.AppendLine(FormattableString.Invariant(
            $"drag path=zigzag target_hz={DragTicksPerSecond} period_s={DragPeriodSeconds:F2} amplitude_px={_dragAmplitudeX}x{_dragAmplitudeY} window_px={_dragWindowWidth}x{_dragWindowHeight} base=({_dragBaseX},{_dragBaseY}) status={_dragStatus} moves={_dragMoves} actual_hz={FormatOrNa(_dragActualHz, "F1")} move_avg_ms={FormatOrNa(_dragMoveAvgMs, "F3")} rect_start={_dragRectStart} rect_end={_dragRectEnd}"));
        sb.AppendLine(FormattableString.Invariant(
            $"machine={Environment.MachineName} cpus={Environment.ProcessorCount} os={Environment.OSVersion.VersionString}"));
        sb.AppendLine(FormattableString.Invariant(
            $"window_visible_start={_windowVisibleAtStart} window_foreground_start={_windowForegroundAtStart} window_visible_end={IsWindowVisible(_hwnd)} window_foreground_end={GetForegroundWindow() == _hwnd} minimize_after_start={_minimizeAfterStart}"));
        sb.AppendLine(FormattableString.Invariant(
            $"window_rect={DescribeWindowRect()} screen={GetSystemMetrics(SmCxScreen)}x{GetSystemMetrics(SmCyScreen)} dpi={GetDpiForWindow(_hwnd)} display_refresh_hz={_displayRefreshHz}"));
        sb.AppendLine(FormattableString.Invariant($"page_ready_wait_timeout_ms={MainPageWaitTimeoutMs} settle_ms={SettleAfterMainPageMs}"));
        sb.AppendLine();

        // 成本块：这才是“每帧实际花了多少时间”的答案；帧间隔只能说明节拍没掉。
        sb.AppendLine("--- per-scenario cost (UI thread / process CPU, GPU engines) ---");
        sb.AppendLine(FormattableString.Invariant(
            $"note: frame_budget_ms={FrameBudgetMs:F2} (160Hz) ui_cpu_ms_per_frame=ui_cpu_ms/frames pct=ui_cpu_ms_per_frame/{FrameBudgetMs:F2}ms*100 logical_cpus={LogicalProcessors}"));
        sb.AppendLine("note: proc_cpu_pct_onecore=proc_cpu_ms/wall_ms*100 (1 logical core = 100%); proc_cpu_pct_allcores=proc_cpu_ms/(wall_ms*logical_cpus)*100 (whole package = 100%)");
        sb.AppendLine("note: ui_cpu_* / proc_cpu_* are the calibrated QueryThreadCycleTime measurement (fine-grained, use these for per-frame cost); ui_getthreadtimes_* are the same intervals read with GetThreadTimes/GetProcessTimes, which on this machine mis-account frequently-waking threads, so treat them only as a noisy cross-check");
        sb.AppendLine("note: in cost-*.csv the ui_cpu_ms_delta / proc_cpu_ms_delta columns use the calibrated cycle values when available (fallback: GetThreadTimes)");
        sb.AppendLine("note: gpu_util_max_pct=max single-engine utilization over the 1s samples; gpu_util_sum_mean_pct=mean of summed per-engine utilization (can exceed 100)");
        sb.AppendLine("note: gpu_* only counts this process' PDH instances; dwm_gpu_* counts the desktop compositor (dwm.exe) of this session, where acrylic/mica composition actually happens. dwm_gpu is shared by every window on the desktop, so compare acrylic on/off within the same scenario rather than as an absolute cost");
        sb.AppendLine("note: drag = the window is temporarily shrunk to ~900x600 in the top-right corner and moved on the UI thread from the CompositionTarget.Rendering callback via SetWindowPos(SWP_NOZORDER|SWP_NOACTIVATE) at up to 120Hz, zig-zag +/-60px; every rendering tick that moves the window forces DWM to recompose, and the move call itself is included in drag_move_avg_ms / ui_cpu");
        sb.AppendLine("note: manual = the app does NOT touch the window (no SetWindowPos/Activate/resize/reposition); after a ready countdown the human drags the window for requested_s, so frame timing is free of harness-driven window moves. ready_s comes from CB_FRAME_TRACE_READY_SECONDS (default 2s)");
        if (snapshot.CostScenarioSummaries.Length == 0)
        {
            sb.AppendLine("(no cost samples recorded)");
        }

        foreach (var scenario in snapshot.CostScenarioSummaries)
        {
            sb.AppendLine(scenario.FormatLine());
        }

        sb.AppendLine(FormattableString.Invariant(
            $"[cost] cycles_calibration={_cyclesStatus} gpu_probe_status={_gpuProbe?.Status ?? "unavailable"} dwm_pid={_dwmPid} dwm_probe_status={(_dwmProbe?.Status ?? "unavailable")} ui_thread_native_id={_uiThreadNativeId} ui_thread_handle_tid={GetThreadId(_uiThreadHandle)}"));

        sb.AppendLine();

        // 帧间隔：按场景分组统计（warmup 也列出，便于判断场景边界的污染程度）。
        sb.AppendLine("--- frame intervals (ms) ---");
        foreach (var phase in new[] { (int)Phase.Warmup, (int)Phase.Idle, (int)Phase.Scroll, (int)Phase.Drag, (int)Phase.Manual, (int)Phase.Ended })
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
        sb.AppendLine("--- frame tail / over-budget metrics (6.25ms budget at 160Hz) ---");
        sb.AppendLine("note: p1_low_ms = mean of the slowest ceil(n*1%) frame deltas (not a percentile); p01_low_ms = mean of slowest ceil(n*0.1%); worst_ms = worst single delta; over_* = count (share) of frames slower than 2/3/4 frames or 50ms");
        sb.AppendLine("note: tail sample counts are shown as k1/k01; with ~800 frames in a 5s scenario 1% is only 8 frames and 0.1% is 1 frame, so treat these numbers as indicative, not precise");
        foreach (var phase in new[] { (int)Phase.Warmup, (int)Phase.Idle, (int)Phase.Scroll, (int)Phase.Drag, (int)Phase.Manual, (int)Phase.Ended })
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
                sb.AppendLine(FrameTailLine($"scenario={PhaseName(phase)}", deltas));
            }
        }

        sb.AppendLine();
        sb.AppendLine("--- ui-thread stall latency (ms; time from DispatcherQueue post to execution) ---");
        foreach (var phase in new[] { (int)Phase.Warmup, (int)Phase.Idle, (int)Phase.Scroll, (int)Phase.Drag, (int)Phase.Manual, (int)Phase.Ended })
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
        sb.AppendLine("--- ui-thread stall tail metrics (slowest 1% / 0.1% of post-to-execution latencies) ---");
        sb.AppendLine("note: same tail definition as frame p1_low/p01_low; k1/k01 are the number of samples averaged");
        foreach (var phase in new[] { (int)Phase.Warmup, (int)Phase.Idle, (int)Phase.Scroll, (int)Phase.Drag, (int)Phase.Manual, (int)Phase.Ended })
        {
            var latencies = new List<double>();
            foreach (var sample in snapshot.StallSamples)
            {
                if (sample.Phase == phase)
                {
                    latencies.Add(sample.LatencyUs);
                }
            }

            if (latencies.Count > 0)
            {
                sb.AppendLine(StallTailLine($"scenario={PhaseName(phase)}", latencies));
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

    /// <summary>
    /// 帧时间的尾部指标：p1_low / p01_low 分别是“最慢 ceil(n*1%) / ceil(n*0.1%) 帧”的均值
    /// （按需求用均值而不是中位数），另给 worst 与超过 2/3/4 帧、50ms 的帧数与占比。
    /// </summary>
    private static string FrameTailLine(string prefix, List<double> samplesUs)
    {
        var n = samplesUs.Count;
        if (n == 0)
        {
            return prefix + " n=0";
        }

        var sorted = samplesUs.ToArray();
        Array.Sort(sorted);
        var sortedDescending = (double[])sorted.Clone();
        Array.Reverse(sortedDescending);

        var k1 = TailCount(n, 0.01);
        var k01 = TailCount(n, 0.001);
        var over2Frames = 0;
        var over3Frames = 0;
        var over4Frames = 0;
        var over50Ms = 0;
        foreach (var value in sorted)
        {
            if (value > FrameBudgetMs * 2.0 * 1000.0)
            {
                over2Frames++;
            }

            if (value > FrameBudgetMs * 3.0 * 1000.0)
            {
                over3Frames++;
            }

            if (value > FrameBudgetMs * 4.0 * 1000.0)
            {
                over4Frames++;
            }

            if (value > 50_000.0)
            {
                over50Ms++;
            }
        }

        return FormattableString.Invariant(
            $"{prefix} n={n} p50={Ms(Percentile(sorted, 50))} p99={Ms(Percentile(sorted, 99))} p1_low={Ms(MeanTop(sortedDescending, k1))} (k1={k1}) p01_low={Ms(MeanTop(sortedDescending, k01))} (k01={k01}) worst={Ms(sorted[^1])} over_12.5ms={over2Frames}({Pct(over2Frames, n)}) over_18.75ms={over3Frames}({Pct(over3Frames, n)}) over_25ms={over4Frames}({Pct(over4Frames, n)}) over_50ms={over50Ms}({Pct(over50Ms, n)})");
    }

    /// <summary>停顿探针的尾部指标：最慢 ceil(n*1%) / ceil(n*0.1%) 个 post-to-execution 延迟的均值 + worst。</summary>
    private static string StallTailLine(string prefix, List<double> samplesUs)
    {
        var n = samplesUs.Count;
        if (n == 0)
        {
            return prefix + " n=0";
        }

        var sorted = samplesUs.ToArray();
        Array.Sort(sorted);
        var sortedDescending = (double[])sorted.Clone();
        Array.Reverse(sortedDescending);
        var k1 = TailCount(n, 0.01);
        var k01 = TailCount(n, 0.001);
        return FormattableString.Invariant(
            $"{prefix} n={n} p1_low={Ms(MeanTop(sortedDescending, k1))} (k1={k1}) p01_low={Ms(MeanTop(sortedDescending, k01))} (k01={k01}) worst={Ms(sorted[^1])}");
    }

    /// <summary>“最慢的百分之一”要平均多少个样本：ceil(n*fraction)，至少 1 个。</summary>
    private static int TailCount(int n, double fraction)
    {
        return Math.Max(1, (int)Math.Ceiling(n * fraction));
    }

    /// <summary>对降序数组的前 count 个值取均值（count 已保证 ≥ 1 且 ≤ 数组长度）。</summary>
    private static double MeanTop(double[] sortedDescending, int count)
    {
        var take = Math.Min(count, sortedDescending.Length);
        var sum = 0.0;
        for (var i = 0; i < take; i++)
        {
            sum += sortedDescending[i];
        }

        return take == 0 ? double.NaN : sum / take;
    }

    private static string Pct(int part, int total)
    {
        return total <= 0 ? "n/a" : (part * 100.0 / total).ToString("F2", CultureInfo.InvariantCulture) + "%";
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
            (int)Phase.Drag => "drag",
            (int)Phase.Ended => "ended",
            (int)Phase.Teardown => "teardown",
            (int)Phase.Manual => "manual",
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
        return CoreConfigBuilder.ResolveDataRoot();
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

    /// <summary>读 UI 线程累计 CPU（kernel+user，100ns）。句柄不可用时返回 -1。</summary>
    private static long ReadUiThreadCpuTicks()
    {
        if (_uiThreadHandle == IntPtr.Zero)
        {
            return -1;
        }

        return GetThreadTimes(_uiThreadHandle, out _, out _, out var kernel, out var user)
            ? kernel + user
            : -1;
    }

    /// <summary>读本进程累计 CPU（所有线程 kernel+user，100ns）。</summary>
    private static long ReadProcessCpuTicks()
    {
        return GetProcessTimes(GetCurrentProcess(), out _, out _, out var kernel, out var user)
            ? kernel + user
            : -1;
    }

    /// <summary>读 UI 线程累计执行 cycle 数（QueryThreadCycleTime，细粒度）；失败返回 -1。</summary>
    private static long ReadUiThreadCycles()
    {
        if (_uiThreadHandle == IntPtr.Zero)
        {
            return -1;
        }

        return QueryThreadCycleTime(_uiThreadHandle, out var cycles) ? (long)cycles : -1;
    }

    /// <summary>读整进程累计执行 cycle 数（QueryProcessCycleTime，细粒度）；失败返回 -1。</summary>
    private static long ReadProcessCycles()
    {
        return QueryProcessCycleTime(GetCurrentProcess(), out var cycles) ? (long)cycles : -1;
    }

    /// <summary>
    /// 标定“每秒 cycle 数”：在场景编排线程上忙等约 150ms，同时用 QPC 计时，
    /// 得到 cycle 与时间的换算比例。QueryThreadCycleTime 基于 TSC，短区间内误差很小。
    /// </summary>
    private static void CalibrateCycleClock()
    {
        try
        {
            var startQpc = Stopwatch.GetTimestamp();
            if (!QueryThreadCycleTime(GetCurrentThread(), out var startCycles))
            {
                _cyclesStatus = "unavailable:QueryThreadCycleTime";
                return;
            }

            // 忙等标定：这段发生在场景开始前，不会污染任何场景的 CPU 统计。
            var warmup = Stopwatch.StartNew();
            while (warmup.ElapsedTicks * 1000.0 / Stopwatch.Frequency < 150.0)
            {
                Thread.SpinWait(64);
            }

            if (!QueryThreadCycleTime(GetCurrentThread(), out var endCycles))
            {
                _cyclesStatus = "unavailable:QueryThreadCycleTime";
                return;
            }

            var seconds = (Stopwatch.GetTimestamp() - startQpc) / (double)Stopwatch.Frequency;
            if (seconds <= 0 || endCycles <= startCycles)
            {
                _cyclesStatus = "unavailable:no-cycle-delta";
                return;
            }

            _cyclesPerSecond = (endCycles - startCycles) / seconds;
            _cyclesStatus = FormattableString.Invariant($"ok:{_cyclesPerSecond / 1_000_000_000.0:F2}Gcyc/s");
        }
        catch
        {
            _cyclesStatus = "unavailable:exception";
        }
    }

    /// <summary>
    /// 在 UI 线程上抓取本线程句柄：GetCurrentThread 返回的是“伪句柄”，只对调用线程有效，
    /// 因此立刻 DuplicateHandle 成真实句柄，供场景编排线程随后读取 GetThreadTimes。
    /// </summary>
    private static IntPtr CaptureUiThreadHandle()
    {
        try
        {
            var process = GetCurrentProcess();
            if (DuplicateHandle(process, GetCurrentThread(), process, out var handle, ThreadQueryInformation, false, 0))
            {
                return handle;
            }

            // 兜底：按原生线程 id 打开（同样只需要查询权限）。
            return OpenThread(ThreadQueryInformation, false, _uiThreadNativeId);
        }
        catch
        {
            return IntPtr.Zero;
        }
    }

    /// <summary>成本 CSV 里的数值格式化：不可用（负数）写 -1 而不是 0，避免“没采到”被当成“真的 0”。</summary>
    private static string FormatCostNumber(double value, string format)
    {
        return value < 0 ? "-1" : value.ToString(format, CultureInfo.InvariantCulture);
    }

    /// <summary>汇总里的数值格式化：NaN 写 n/a。</summary>
    private static string FormatOrNa(double value, string format)
    {
        return double.IsNaN(value) ? "n/a" : value.ToString(format, CultureInfo.InvariantCulture);
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

    /// <summary>打印一行人工可读的进度（未重定向时安静失败，不影响测量）。</summary>
    private static void WriteStatusLine(string line)
    {
        WriteConsole(line + Environment.NewLine);
    }

    /// <summary>
    /// 退出看门狗的实际等待时长。基础是 8 秒；manual 场景额外覆盖
    /// “准备 + 记录 + 5 秒余量”，使强制退出的计时预算绝不短于一次合法的人工拖拽记录
    /// （否则一旦退出路径没有立即返回，新场景就会在记录中途被强杀）。
    /// 未跑 manual 时保持原来的 8 秒，行为与改动前完全一致。
    /// </summary>
    private static int ExitWatchdogMs()
    {
        if (!_doManual)
        {
            return ExitWatchdogBaseMs;
        }

        var manualWindowMs = (int)Math.Ceiling((_readySeconds + _manualSeconds) * 1000.0) + ExitWatchdogMarginMs;
        return Math.Max(ExitWatchdogBaseMs, manualWindowMs);
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
            Thread.Sleep(ExitWatchdogMs());
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

    /// <summary>一个成本采样区间（默认 1 秒）的增量数据。</summary>
    private readonly struct CostSample
    {
        public CostSample(
            long wallUs,
            int phase,
            double wallDeltaS,
            double uiCpuMsDelta,
            double procCpuMsDelta,
            int framesDelta,
            GpuReading gpu,
            GpuReading dwmGpu,
            double uiCycleMsDelta,
            double procCycleMsDelta)
        {
            WallUs = wallUs;
            Phase = phase;
            WallDeltaS = wallDeltaS;
            UiCpuMsDelta = uiCpuMsDelta;
            ProcCpuMsDelta = procCpuMsDelta;
            FramesDelta = framesDelta;
            Gpu = gpu;
            DwmGpu = dwmGpu;
            UiCycleMsDelta = uiCycleMsDelta;
            ProcCycleMsDelta = procCycleMsDelta;
        }

        public long WallUs { get; }

        public int Phase { get; }

        public double WallDeltaS { get; }

        /// <summary>UI 线程本区间 CPU 毫秒（GetThreadTimes；受 ~15.6ms 记账节拍限制）；-1 表示没采到。</summary>
        public double UiCpuMsDelta { get; }

        /// <summary>整进程本区间 CPU 毫秒（GetProcessTimes；同样受记账节拍限制）；-1 表示没采到。</summary>
        public double ProcCpuMsDelta { get; }

        public int FramesDelta { get; }

        public GpuReading Gpu { get; }

        /// <summary>本区间 DWM（桌面合成器）的 GPU 读数；不可用时 EngineAvailable=false。</summary>
        public GpuReading DwmGpu { get; }

        /// <summary>UI 线程本区间 CPU 毫秒（QueryThreadCycleTime 标定换算，细粒度）；-1 表示没采到。</summary>
        public double UiCycleMsDelta { get; }

        /// <summary>整进程本区间 CPU 毫秒（QueryProcessCycleTime 标定换算，细粒度）；-1 表示没采到。</summary>
        public double ProcCycleMsDelta { get; }
    }

    /// <summary>一次 PDH GPU 采样结果（只统计本进程实例）。不可用时 EngineAvailable=false。</summary>
    private sealed class GpuReading
    {
        private static readonly Dictionary<string, double> EmptyEngines = new(StringComparer.OrdinalIgnoreCase);

        public GpuReading(
            bool engineAvailable,
            double sumPct,
            double maxPct,
            Dictionary<string, double> enginePct,
            long dedicatedBytes,
            long committedBytes)
        {
            EngineAvailable = engineAvailable;
            SumPct = sumPct;
            MaxPct = maxPct;
            EnginePct = enginePct;
            DedicatedBytes = dedicatedBytes;
            CommittedBytes = committedBytes;
        }

        public static GpuReading Unavailable()
        {
            return new GpuReading(false, -1, -1, EmptyEngines, -1, -1);
        }

        /// <summary>GPU Engine 计数器可用且本次 collect 成功。</summary>
        public bool EngineAvailable { get; }

        /// <summary>本进程所有引擎实例利用率之和（%）。</summary>
        public double SumPct { get; }

        /// <summary>本进程单个引擎实例利用率的峰值（%）。</summary>
        public double MaxPct { get; }

        /// <summary>按引擎类型聚合的峰值（engtype_3d / engtype_copy / ...）。</summary>
        public Dictionary<string, double> EnginePct { get; }

        public long DedicatedBytes { get; }

        public long CommittedBytes { get; }
    }

    /// <summary>一个场景的成本汇总；<see cref="FormatLine"/> 生成 summary/stdout 里的 [cost] 行。</summary>
    private sealed class CostScenario
    {
        public string PhaseName { get; set; } = string.Empty;

        public double RequestedSeconds { get; set; }

        public double WallS { get; set; }

        public int Frames { get; set; }

        public int Samples { get; set; }

        public double UiCpuMs { get; set; }

        public double ProcCpuMs { get; set; }

        /// <summary>细粒度 UI 线程 CPU（cycle 标定换算）；GetThreadTimes 口径见 <see cref="UiCpuMs"/>。</summary>
        public double UiCycleCpuMs { get; set; } = double.NaN;

        /// <summary>细粒度整进程 CPU（cycle 标定换算）。</summary>
        public double ProcCycleCpuMs { get; set; } = double.NaN;

        public int GpuSampleCount { get; set; }

        public double GpuSumMeanPct { get; set; } = double.NaN;

        public double GpuMaxPct { get; set; } = double.NaN;

        public Dictionary<string, double> GpuEngineMaxPct { get; set; } = new(StringComparer.OrdinalIgnoreCase);

        public long GpuDedicatedBytes { get; set; } = -1;

        public long GpuCommittedBytes { get; set; } = -1;

        /// <summary>DWM（桌面合成器）GPU 引擎采样次数。</summary>
        public int DwmGpuSampleCount { get; set; }

        public double DwmGpuSumMeanPct { get; set; } = double.NaN;

        public double DwmGpuMaxPct { get; set; } = double.NaN;

        public Dictionary<string, double> DwmGpuEngineMaxPct { get; set; } = new(StringComparer.OrdinalIgnoreCase);

        public int DisplayRefreshHz { get; set; }

        /// <summary>本轮 CB_FRAME_TRACE_ACRYLIC 的原始值（unset/0/1）。</summary>
        public string AcrylicRequested { get; set; } = "unset";

        /// <summary>本轮实际生效的亚克力状态（on/off/unknown/failed:...）。</summary>
        public string AcrylicEffective { get; set; } = "unknown";

        /// <summary>亚克力状态来自哪个属性。</summary>
        public string AcrylicSource { get; set; } = "(not-read)";

        /// <summary>本轮 CB_FRAME_TRACE_MICA 的原始值（unset/0/1）。</summary>
        public string MicaRequested { get; set; } = "unset";

        /// <summary>本轮实际生效的 Mica 状态（on/off/unknown/failed:...）。</summary>
        public string MicaEffective { get; set; } = "unknown";

        /// <summary>Mica 状态来自哪个对象。</summary>
        public string MicaSource { get; set; } = "(not-read)";

        /// <summary>drag 场景执行状态（moved / no-move:... / unavailable:... / not-run）。</summary>
        public string DragStatus { get; set; } = "not-run";

        /// <summary>drag 场景实际成功的 SetWindowPos 次数。</summary>
        public int DragMoves { get; set; }

        /// <summary>drag 场景实测移动频率（Hz），moves / 场景持续时长。</summary>
        public double DragActualHz { get; set; } = double.NaN;

        /// <summary>单次 SetWindowPos 的平均耗时（毫秒）；用于从 UI CPU 里分离“移动窗口”本身的开销。</summary>
        public double DragMoveAvgMs { get; set; } = double.NaN;

        public int DragAmplitudeX { get; set; }

        public int DragAmplitudeY { get; set; }

        public string DragRectStart { get; set; } = "(none)";

        public string DragRectEnd { get; set; } = "(none)";

        public string FormatLine()
        {
            // 首选口径：cycle 标定值（细粒度且本机实测稳定）。GetThreadTimes/GetProcessTimes
            // 对高频唤醒线程记账偏差很大（外部逐线程交叉验证），只作为对照字段保留。
            var uiCpuMs = double.IsNaN(UiCycleCpuMs) ? UiCpuMs : UiCycleCpuMs;
            var procCpuMs = double.IsNaN(ProcCycleCpuMs) ? ProcCpuMs : ProcCycleCpuMs;
            var framesPerS = WallS > 0 ? Frames / WallS : double.NaN;
            var uiPerS = WallS > 0 ? uiCpuMs / WallS : double.NaN;
            var uiPerFrame = Frames > 0 ? uiCpuMs / Frames : double.NaN;
            var uiPct = double.IsNaN(uiPerFrame) ? double.NaN : uiPerFrame / FrameBudgetMs * 100.0;
            var procPerS = WallS > 0 ? procCpuMs / WallS : double.NaN;
            var procOneCore = WallS > 0 ? procCpuMs / (WallS * 1000.0) * 100.0 : double.NaN;
            var procAllCores = WallS > 0 ? procCpuMs / (WallS * 1000.0 * LogicalProcessors) * 100.0 : double.NaN;

            var gtPerFrame = Frames > 0 ? UiCpuMs / Frames : double.NaN;
            var gtPct = double.IsNaN(gtPerFrame) ? double.NaN : gtPerFrame / FrameBudgetMs * 100.0;
            var gtProcOneCore = WallS > 0 ? ProcCpuMs / (WallS * 1000.0) * 100.0 : double.NaN;

            var sb = new StringBuilder();
            sb.Append(FormattableString.Invariant(
                $"[cost] scenario={PhaseName} wall_s={WallS:F2} requested_s={RequestedSeconds:F1} frames={Frames} samples={Samples} render_cb_per_s={FormatOrNa(framesPerS, "F1")} display_refresh_hz={DisplayRefreshHz}"));
            sb.Append(FormattableString.Invariant(
                $" ui_cpu_ms={FormatOrNa(uiCpuMs, "F3")} ui_cpu_ms_per_s={FormatOrNa(uiPerS, "F3")} ui_cpu_ms_per_frame={FormatOrNa(uiPerFrame, "F4")} ui_pct_of_6.25ms={FormatOrNa(uiPct, "F3")}"));
            sb.Append(FormattableString.Invariant(
                $" ui_getthreadtimes_ms={FormatOrNa(UiCpuMs, "F3")} ui_getthreadtimes_ms_per_frame={FormatOrNa(gtPerFrame, "F4")} ui_gt_pct_of_6.25ms={FormatOrNa(gtPct, "F3")}"));
            sb.Append(FormattableString.Invariant(
                $" proc_cpu_ms={FormatOrNa(procCpuMs, "F3")} proc_cpu_ms_per_s={FormatOrNa(procPerS, "F3")} proc_cpu_pct_onecore={FormatOrNa(procOneCore, "F2")} proc_cpu_pct_allcores={FormatOrNa(procAllCores, "F3")} proc_getthreadtimes_ms={FormatOrNa(ProcCpuMs, "F3")} proc_gt_pct_onecore={FormatOrNa(gtProcOneCore, "F2")}"));
            sb.Append(FormattableString.Invariant(
                $" gpu_available={GpuSampleCount > 0} gpu_util_max_pct={FormatOrNa(GpuMaxPct, "F2")} gpu_util_sum_mean_pct={FormatOrNa(GpuSumMeanPct, "F2")} gpu_engines={FormatEngines(GpuEngineMaxPct, GpuSampleCount)} gpu_mem_dedicated_mb={FormatBytes(GpuDedicatedBytes)} gpu_mem_committed_mb={FormatBytes(GpuCommittedBytes)}"));
            // DWM 侧：亚克力/毛玻璃由桌面合成器完成，本进程的 GPU 计数不包含它。
            sb.Append(FormattableString.Invariant(
                $" dwm_gpu_available={DwmGpuSampleCount > 0} dwm_gpu_util_max_pct={FormatOrNa(DwmGpuMaxPct, "F2")} dwm_gpu_util_sum_mean_pct={FormatOrNa(DwmGpuSumMeanPct, "F2")} dwm_gpu_engines={FormatEngines(DwmGpuEngineMaxPct, DwmGpuSampleCount)}"));
            // 亚克力/Mica 状态每轮都要自描述：A/B 结果必须能直接看出这一轮到底开没开。
            sb.Append(FormattableString.Invariant(
                $" acrylic_requested={AcrylicRequested} acrylic_effective={AcrylicEffective} acrylic_source={AcrylicSource}"));
            sb.Append(FormattableString.Invariant(
                $" mica_requested={MicaRequested} mica_effective={MicaEffective} mica_source={MicaSource}"));
            if (string.Equals(PhaseName, "drag", StringComparison.Ordinal))
            {
                sb.Append(FormattableString.Invariant(
                    $" drag_status={DragStatus} drag_moves={DragMoves} drag_hz={FormatOrNa(DragActualHz, "F1")} drag_amp_px={DragAmplitudeX}x{DragAmplitudeY} drag_move_avg_ms={FormatOrNa(DragMoveAvgMs, "F3")} drag_rect_start={DragRectStart} drag_rect_end={DragRectEnd}"));
            }

            return sb.ToString();
        }

        private static string FormatEngines(Dictionary<string, double> engineMap, int sampleCount)
        {
            if (engineMap.Count == 0)
            {
                return sampleCount > 0 ? "(none-matched)" : "(unavailable)";
            }

            var keys = new List<string>(engineMap.Keys);
            keys.Sort(StringComparer.OrdinalIgnoreCase);
            var parts = new List<string>(keys.Count);
            foreach (var key in keys)
            {
                parts.Add(key + "=" + engineMap[key].ToString("F2", CultureInfo.InvariantCulture));
            }

            return string.Join("|", parts);
        }

        private static string FormatBytes(long bytes)
        {
            return bytes < 0 ? "n/a" : (bytes / (1024.0 * 1024.0)).ToString("F1", CultureInfo.InvariantCulture);
        }
    }

    /// <summary>
    /// 通过 PDH 读取“本进程”的 GPU 引擎利用率与显存占用。只在场景编排线程上使用；
    /// 任何一步失败都只降级为“不可用”，绝不让整个测量失败。
    /// </summary>
    private sealed class GpuProbe : IDisposable
    {
        private const string EngineCounterPath = @"\GPU Engine(*)\Utilization Percentage";
        private const string DedicatedCounterPath = @"\GPU Process Memory(*)\Dedicated Usage";
        private const string CommittedCounterPath = @"\GPU Process Memory(*)\Total Committed";

        private const uint PdhFmtDouble = 0x00000200;
        private const uint PdhFmtNoCap100 = 0x00008000;
        private const uint PdhMoreData = 0x800007D2;
        private const uint PdhCStatusValidData = 0x00000000;
        private const uint PdhCStatusNewData = 0x00000001;

        private readonly int _pid;
        private IntPtr _query;
        private IntPtr _engineCounter;
        private IntPtr _dedicatedCounter;
        private IntPtr _committedCounter;
        private bool _engineAvailable;
        private string _status = "not-initialized";

        public GpuProbe(int pid)
        {
            _pid = pid;
        }

        public string Status => _status;

        public void TryOpen()
        {
            try
            {
                var status = PdhOpenQueryW(null, IntPtr.Zero, out _query);
                if (status != 0)
                {
                    _query = IntPtr.Zero;
                    _status = FormattableString.Invariant($"PdhOpenQuery failed status=0x{status:X8}");
                    return;
                }

                _engineCounter = AddCounter(EngineCounterPath);
                _engineAvailable = _engineCounter != IntPtr.Zero;
                _dedicatedCounter = AddCounter(DedicatedCounterPath);
                _committedCounter = AddCounter(CommittedCounterPath);

                // 第一次 collect 只用于建立基线：利用率计数器需要两次采样才有区间值。
                _ = PdhCollectQueryData(_query);

                var missing = new List<string>();
                if (!_engineAvailable)
                {
                    missing.Add("gpu_engine");
                }

                if (_dedicatedCounter == IntPtr.Zero)
                {
                    missing.Add("gpu_dedicated");
                }

                if (_committedCounter == IntPtr.Zero)
                {
                    missing.Add("gpu_committed");
                }

                _status = missing.Count == 0 ? "ok" : "unavailable:" + string.Join("+", missing);
            }
            catch (Exception ex)
            {
                _status = "exception:" + ex.GetType().Name;
                Dispose();
            }
        }

        /// <summary>场景起点调用：为下一次 <see cref="Read"/> 建立采样基线。</summary>
        public void Prime()
        {
            if (_query != IntPtr.Zero)
            {
                _ = PdhCollectQueryData(_query);
            }
        }

        /// <summary>collect 一次并解析本进程的 GPU 利用率/显存。失败返回不可用读数而不是抛异常。</summary>
        public GpuReading Read()
        {
            if (_query == IntPtr.Zero)
            {
                return GpuReading.Unavailable();
            }

            var status = PdhCollectQueryData(_query);
            if (status != 0)
            {
                return GpuReading.Unavailable();
            }

            var sumPct = -1.0;
            var maxPct = -1.0;
            var engines = new Dictionary<string, double>(StringComparer.OrdinalIgnoreCase);
            if (_engineAvailable && TryReadInstances(_engineCounter, PdhFmtDouble, out var items))
            {
                sumPct = 0.0;
                maxPct = 0.0;
                foreach (var item in items)
                {
                    if (!NameMatchesPid(item.Name, _pid))
                    {
                        continue;
                    }

                    sumPct += item.Value;
                    maxPct = Math.Max(maxPct, item.Value);
                    var engine = EngineTypeOf(item.Name);
                    engines[engine] = engines.TryGetValue(engine, out var existing) ? Math.Max(existing, item.Value) : item.Value;
                }
            }

            var dedicated = -1L;
            if (_dedicatedCounter != IntPtr.Zero
                && TryReadInstances(_dedicatedCounter, PdhFmtDouble | PdhFmtNoCap100, out var dedicatedItems))
            {
                dedicated = SumForPid(dedicatedItems, _pid);
            }

            var committed = -1L;
            if (_committedCounter != IntPtr.Zero
                && TryReadInstances(_committedCounter, PdhFmtDouble | PdhFmtNoCap100, out var committedItems))
            {
                committed = SumForPid(committedItems, _pid);
            }

            return new GpuReading(
                _engineAvailable && sumPct >= 0,
                sumPct,
                maxPct,
                engines,
                dedicated,
                committed);
        }

        public void Dispose()
        {
            if (_query != IntPtr.Zero)
            {
                _ = PdhCloseQuery(_query);
                _query = IntPtr.Zero;
                _engineCounter = IntPtr.Zero;
                _dedicatedCounter = IntPtr.Zero;
                _committedCounter = IntPtr.Zero;
                _engineAvailable = false;
            }
        }

        private static long SumForPid(List<(string Name, double Value)> items, int pid)
        {
            var sum = 0L;
            foreach (var item in items)
            {
                if (NameMatchesPid(item.Name, pid))
                {
                    sum += (long)item.Value;
                }
            }

            return sum;
        }

        private IntPtr AddCounter(string path)
        {
            if (_query == IntPtr.Zero)
            {
                return IntPtr.Zero;
            }

            return PdhAddEnglishCounterW(_query, path, IntPtr.Zero, out var counter) == 0 ? counter : IntPtr.Zero;
        }

        /// <summary>枚举某个多实例计数器的全部实例值。PDH 的缓冲区布局由 PdhFmtCounterValueItem 描述。</summary>
        private static bool TryReadInstances(IntPtr counter, uint format, out List<(string Name, double Value)> items)
        {
            items = new List<(string Name, double Value)>();
            var bufferSize = 0u;
            var itemCount = 0u;
            var status = PdhGetFormattedCounterArrayW(counter, format, ref bufferSize, ref itemCount, IntPtr.Zero);
            if (status != PdhMoreData || bufferSize == 0)
            {
                // 没有实例时 PDH 成功返回 0 且 size=0。
                return status == 0;
            }

            // 实例数在两次调用之间可能变化，最多重试 3 次。
            for (var attempt = 0; attempt < 3; attempt++)
            {
                items.Clear();
                var buffer = Marshal.AllocHGlobal((int)bufferSize);
                try
                {
                    var size = bufferSize;
                    var count = itemCount;
                    status = PdhGetFormattedCounterArrayW(counter, format, ref size, ref count, buffer);
                    if (status == PdhMoreData)
                    {
                        bufferSize = size;
                        itemCount = count;
                        continue;
                    }

                    if (status != 0)
                    {
                        return false;
                    }

                    var itemSize = Marshal.SizeOf<PdhFmtCounterValueItem>();
                    for (var i = 0; i < count; i++)
                    {
                        var item = Marshal.PtrToStructure<PdhFmtCounterValueItem>(IntPtr.Add(buffer, i * itemSize));
                        if (item.SzName == IntPtr.Zero)
                        {
                            continue;
                        }

                        // 只接受有效数据，跳过 No Data / Invalid Data 等状态。
                        if (item.FmtValue.CStatus != PdhCStatusValidData && item.FmtValue.CStatus != PdhCStatusNewData)
                        {
                            continue;
                        }

                        var name = Marshal.PtrToStringUni(item.SzName);
                        if (name != null)
                        {
                            items.Add((name, item.FmtValue.DoubleValue));
                        }
                    }

                    return true;
                }
                finally
                {
                    Marshal.FreeHGlobal(buffer);
                }
            }

            return false;
        }

        /// <summary>GPU 实例名形如 pid_1234_luid_..._engtype_3d，按 pid_&lt;本进程&gt;_ 过滤（大小写不敏感）。</summary>
        private static bool NameMatchesPid(string instanceName, int pid)
        {
            var needle = "pid_" + pid.ToString(CultureInfo.InvariantCulture) + "_";
            return instanceName.IndexOf(needle, StringComparison.OrdinalIgnoreCase) >= 0;
        }

        /// <summary>从实例名里取出 engtype_ 之后到下一个下划线之前的引擎类型。</summary>
        private static string EngineTypeOf(string instanceName)
        {
            const string marker = "engtype_";
            var index = instanceName.IndexOf(marker, StringComparison.OrdinalIgnoreCase);
            if (index < 0)
            {
                return "unknown";
            }

            var start = index + marker.Length;
            var end = instanceName.IndexOf('_', start);
            return end < 0 ? instanceName[start..] : instanceName[start..end];
        }
    }

    private sealed class Samples
    {
        public FrameSample[] FrameSamples { get; set; } = Array.Empty<FrameSample>();

        public StallSample[] StallSamples { get; set; } = Array.Empty<StallSample>();

        public OpSample[] OpSamples { get; set; } = Array.Empty<OpSample>();

        public CostSample[] CostSeries { get; set; } = Array.Empty<CostSample>();

        public CostScenario[] CostScenarioSummaries { get; set; } = Array.Empty<CostScenario>();
    }

    // ---- Win32：记录运行条件（窗口可见/前台、屏幕、DPI、刷新率）与 drag 场景的窗口移动 ----
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

    /// <summary>drag 场景专用：只改窗口位置（不改尺寸/Z 序/激活状态）。</summary>
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint uFlags);

    /// <summary>取主显示器工作区（不含任务栏），用于保证 drag 轨迹不把窗口推出屏幕。</summary>
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SystemParametersInfoW(uint uiAction, uint uiParam, out Rect pvParam, uint fWinIni);

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

    // ---- Win32：CPU 计时（GetThreadTimes/GetProcessTimes）与隐藏窗口对照 ----
    private const int ThreadQueryInformation = 0x0040;
    private const int SwMinimize = 6;

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentThread();

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint GetThreadId(IntPtr hThread);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DuplicateHandle(
        IntPtr hSourceProcessHandle,
        IntPtr hSourceHandle,
        IntPtr hTargetProcessHandle,
        out IntPtr lpTargetHandle,
        uint dwDesiredAccess,
        [MarshalAs(UnmanagedType.Bool)] bool bInheritHandle,
        uint dwOptions);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenThread(uint dwDesiredAccess, [MarshalAs(UnmanagedType.Bool)] bool bInheritHandle, uint dwThreadId);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetThreadTimes(
        IntPtr hThread,
        out long lpCreationTime,
        out long lpExitTime,
        out long lpKernelTime,
        out long lpUserTime);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetProcessTimes(
        IntPtr hProcess,
        out long lpCreationTime,
        out long lpExitTime,
        out long lpKernelTime,
        out long lpUserTime);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryThreadCycleTime(IntPtr hThread, out ulong cycles);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryProcessCycleTime(IntPtr hProcess, out ulong cycles);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(IntPtr hObject);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    // ---- PDH：GPU 引擎利用率与显存占用（只用系统自带的 pdh.dll，不引 NuGet） ----
    [DllImport("pdh.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
    private static extern uint PdhOpenQueryW(string? dataSource, IntPtr userData, out IntPtr query);

    [DllImport("pdh.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
    private static extern uint PdhAddEnglishCounterW(IntPtr query, string fullCounterPath, IntPtr userData, out IntPtr counter);

    [DllImport("pdh.dll", ExactSpelling = true)]
    private static extern uint PdhCollectQueryData(IntPtr query);

    [DllImport("pdh.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
    private static extern uint PdhGetFormattedCounterArrayW(
        IntPtr counter,
        uint format,
        ref uint bufferSize,
        ref uint itemCount,
        IntPtr buffer);

    [DllImport("pdh.dll", ExactSpelling = true)]
    private static extern uint PdhCloseQuery(IntPtr query);

    /// <summary>
    /// PDH_FMT_COUNTERVALUE：DWORD 状态 + 8 字节 union（这里只用到 double）。
    /// x64 下 double 需要 8 字节对齐，因此结构体是 16 字节，与原生布局一致。
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct PdhFmtCounterValue
    {
        public uint CStatus;
        public double DoubleValue;
    }

    /// <summary>PDH_FMT_COUNTERVALUE_ITEM_W：实例名指针 + 格式化值。</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct PdhFmtCounterValueItem
    {
        public IntPtr SzName;
        public PdhFmtCounterValue FmtValue;
    }
}
