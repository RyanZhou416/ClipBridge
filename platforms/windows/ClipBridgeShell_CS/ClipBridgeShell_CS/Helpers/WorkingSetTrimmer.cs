// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/WorkingSetTrimmer.cs
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;

using Microsoft.Extensions.Logging;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 应用不可见时（最小化 / 隐藏到托盘）按需裁剪本进程的工作集（working set）。
///
/// 背景：本进程常驻内存约 215MB，其中绝大多数不是托管堆（dotnet-counters 实测 GC 堆只有约 8MB），
/// 而是 WinUI3/WindowsAppSDK（self-contained）加载的原生模块、XAML 页内容、
/// 2880×1537 窗口的 D3D 交换链与 Mica 合成资源。窗口不可见时这些页面没有保留价值，
/// 交还给系统后 Task Manager 的内存读数会明显下降；用户切回来时再按需缺页换入 ——
/// 这正是轻量托盘应用的常规做法。
///
/// 触发策略（见 <see cref="OnWindowHidden"/> / <see cref="OnWindowVisible"/>）：
/// - 只在窗口“最小化”或“隐藏到托盘”后延迟 <see cref="TrimDelayMs"/> 毫秒裁剪一次：
///   给“最小化后立刻还原”留出取消窗口，避免用户为一次误触付缺页代价；
/// - 普通失焦 / 切到别的窗口不触发（窗口仍然可见，用户随时会看回来）；
/// - 还原 / 显示窗口时取消尚未触发的裁剪并复位幂等标记，下次隐藏可重新裁剪；
/// - 幂等：同一段隐藏期内只裁剪一次，重复的最小化/隐藏事件是空操作。
///
/// 安全：
/// - P/Invoke（EmptyWorkingSet 可能耗时数十毫秒）固定在线程池执行，绝不阻塞 UI 线程；
/// - 对外方法与日志路径全部 try/catch，异常不会逸出：App 已销毁、MainWindow 为 null 的
///   托盘-only 生命周期下调用同样安全；
/// - 只裁剪本进程，不修改权限、系统设置，也不影响其它进程。
///
/// 开关：<c>CB_NO_WS_TRIM=1</c> 关闭裁剪；未设置或非法值（空、0、false 等）均视为开启，
/// 与仓库内其它 <c>CB_*</c> 测量/降级开关的判定方式一致。
///
/// 不调用 GC.Collect：托管堆实测只有约 8MB，一次回收能归还的上限就那么点，
/// 而压缩/阻塞回收会在还原路径上引入额外停顿；因此这里只归还原生页，不主动 GC。
/// </summary>
internal static class WorkingSetTrimmer
{
    private const string DisableEnvVarName = "CB_NO_WS_TRIM";

    /// <summary>
    /// 可选开关：<c>CB_WS_TRIM_MINIMIZE=1</c> 时也把“最小化”当作可裁剪状态。
    ///
    /// 默认**不**裁剪最小化：实测裁剪后恢复需要 0.5~0.9 秒重绘（窗口 40ms 就响应，
    /// 但客户区内容要等换页 + 重新合成），而省下的工作集在 20 秒后又被后台活动涨回
    /// 78~86MB —— 收益短暂、代价可感。隐藏到托盘时用户看不到窗口，裁剪才划算。
    /// </summary>
    private const string TrimOnMinimizeEnvVarName = "CB_WS_TRIM_MINIMIZE";

    /// <summary>窗口不可见后延迟多久再裁剪（毫秒）：过滤“最小化后又马上还原”的场景。</summary>
    private const int TrimDelayMs = 3000;


    /// <summary>CB_NO_WS_TRIM=1 时关闭；其它值（含未设置、空、0、false）一律视为开启。</summary>
    private static readonly bool Enabled =
        !string.Equals(Environment.GetEnvironmentVariable(DisableEnvVarName)?.Trim(), "1", StringComparison.Ordinal);

    private static readonly object Gate = new();

    /// <summary>延迟裁剪用的定时器（进程生命周期内保持引用，避免被 GC 回收）。</summary>
    private static Timer? _timer;

    /// <summary>已经为本次隐藏期排了一次裁剪（定时器在跑）。</summary>
    private static bool _trimScheduled;

    /// <summary>本次隐藏期已经裁剪过（幂等标记；窗口可见时复位）。</summary>
    private static bool _trimmed;

    /// <summary>调用方最后一次报告的状态是“不可见”；裁剪真正执行前会再核对一次。</summary>
    private static bool _hidden;

    private static ILogger? _logger;

    /// <summary>裁剪功能是否开启（只受 <c>CB_NO_WS_TRIM=1</c> 影响）。</summary>
    public static bool IsEnabled => Enabled;

    /// <summary>
    /// 是否也把“最小化”当作可裁剪状态（<c>CB_WS_TRIM_MINIMIZE=1</c> 时才为 true）。
    /// 默认 false：隐藏到托盘才裁剪，最小化保持秒开。
    /// </summary>
    public static bool TrimOnMinimize
    {
        get;
    } =
        string.Equals(Environment.GetEnvironmentVariable(TrimOnMinimizeEnvVarName)?.Trim(), "1", StringComparison.Ordinal);

    /// <summary>
    /// 窗口变为最小化 / 隐藏到托盘（或托盘-only 生命周期里根本没有窗口）时调用。
    /// 只排队一次延迟裁剪；重复调用不会重复裁剪，也不会把延迟不断后移。
    /// </summary>
    public static void OnWindowHidden()
    {
        if (!Enabled)
        {
            return;
        }

        try
        {
            lock (Gate)
            {
                _hidden = true;

                // 同一隐藏期已经裁剪过，或已经排在 3 秒延迟上：空操作（幂等）。
                if (_trimmed || _trimScheduled)
                {
                    return;
                }

                _timer ??= new Timer(static _ => TrimIfStillHidden(), null, Timeout.Infinite, Timeout.Infinite);
                _trimScheduled = true;
                _timer.Change(TrimDelayMs, Timeout.Infinite);
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[WorkingSetTrimmer] schedule failed: {ex}");
        }
    }

    /// <summary>
    /// 窗口重新可见（还原 / 从托盘显示 / 激活）时调用：取消尚未触发的裁剪并复位幂等标记。
    /// </summary>
    public static void OnWindowVisible()
    {
        if (!Enabled)
        {
            return;
        }

        try
        {
            lock (Gate)
            {
                _hidden = false;
                _trimmed = false;

                if (!_trimScheduled)
                {
                    return;
                }

                _trimScheduled = false;
                _timer?.Change(Timeout.Infinite, Timeout.Infinite);

                // CB_STARTUP_TRACE=1 时可复核“还原取消了一次待执行的裁剪”。
                StartupTrace.Mark("ws-trim-cancelled");
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[WorkingSetTrimmer] cancel failed: {ex}");
        }
    }

    /// <summary>
    /// 在线程池上裁剪本进程的工作集，返回表示后台工作的任务；调用方不需要等待。
    /// 窗口已经重新可见时什么都不做（只用于诊断/直接调用，正常触发路径是同类的定时器）。
    /// </summary>
    public static Task TrimAsync()
    {
        if (!Enabled)
        {
            return Task.CompletedTask;
        }

        return Task.Run(TrimCore);
    }

    private static void TrimIfStillHidden()
    {
        try
        {
            lock (Gate)
            {
                _trimScheduled = false;

                // 定时器触发前用户已经还原：直接放弃，不付裁剪 / 缺页代价。
                if (!_hidden || _trimmed)
                {
                    StartupTrace.Mark("ws-trim-abandoned");
                    return;
                }

                _trimmed = true;
            }

            _ = TrimAsync();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[WorkingSetTrimmer] timer dispatch failed: {ex}");
        }
    }

    private static void TrimCore()
    {
        long wsBefore;
        long wsAfter;
        long privateBefore;
        long privateAfter;

        try
        {
            lock (Gate)
            {
                // 排队之后、真正执行之前窗口又变可见（极窄的时间窗）：放弃，绝不裁可见窗口。
                if (!_hidden)
                {
                    return;
                }
            }

            var process = Process.GetCurrentProcess();
            wsBefore = process.WorkingSet64;
            privateBefore = process.PrivateMemorySize64;

            var hProcess = GetCurrentProcess();

            // 首选 EmptyWorkingSet（psapi.dll）；返回 false 或不可用时退回
            // SetProcessWorkingSetSize(hProcess, -1, -1)（kernel32，-1 表示尽量多归还）。
            var method = "EmptyWorkingSet";
            var trimmed = false;
            try
            {
                trimmed = EmptyWorkingSet(hProcess);
            }
            catch (Exception ex)
            {
                // 极少数精简/未来 Windows 版本可能没有这个导出：记录后退回备用 API。
                LogDebug($"EmptyWorkingSet unavailable ({ex.GetType().Name}); falling back to SetProcessWorkingSetSize.");
            }

            if (!trimmed)
            {
                method = "SetProcessWorkingSetSize";
                trimmed = SetProcessWorkingSetSize(hProcess, new IntPtr(-1), new IntPtr(-1));
            }

            process.Refresh();
            wsAfter = process.WorkingSet64;
            privateAfter = process.PrivateMemorySize64;

            if (!trimmed)
            {
                // 裁剪失败：允许下一次隐藏事件再试，不把失败记成“已裁剪”。
                lock (Gate)
                {
                    _trimmed = false;
                }
            }

            var summary =
                $"Working-set trim ({method}) {(trimmed ? "ok" : "failed")}: " +
                $"WS {ToMb(wsBefore)} MB -> {ToMb(wsAfter)} MB, " +
                $"private {ToMb(privateBefore)} MB -> {ToMb(privateAfter)} MB";
            LogDebug(summary);

            // 与其它测量打点共用 CB_STARTUP_TRACE=1 开关（未开启时不写文件）：
            // 用于在同一份构建上复核裁剪是否真的发生、裁剪了多少。
            StartupTrace.Mark(
                $"ws-trim method={method} ok={trimmed} ws-before-kb={wsBefore / 1024} ws-after-kb={wsAfter / 1024}");
        }
        catch (Exception ex)
        {
            lock (Gate)
            {
                _trimmed = false;
            }

            LogDebug($"Working-set trim failed ({ex.GetType().Name}): {ex.Message}");
            System.Diagnostics.Debug.WriteLine($"[WorkingSetTrimmer] trim failed: {ex}");
        }
    }

    /// <summary>
    /// 写一条 Debug 级日志。优先走应用已有的 ILogger 管线（诊断日志会进入核心日志库），
    /// App/DI 尚未就绪或已销毁时退化为 Debug 输出；任何日志失败都不影响裁剪本身。
    /// </summary>
    private static void LogDebug(string message)
    {
        System.Diagnostics.Debug.WriteLine($"[WorkingSetTrimmer] {message}");

        try
        {
            _logger ??= App.GetService<ILoggerFactory>().CreateLogger("WorkingSetTrimmer");
            _logger.LogDebug("{Message}", message);
        }
        catch
        {
            // 日志不是关键路径：App.Current 为空、服务容器已释放等情况下直接放弃。
        }
    }

    private static string ToMb(long bytes) => (bytes / (1024.0 * 1024.0)).ToString("F1");

    /// <summary>本进程的伪句柄（-1）；不需要关闭，且带有本进程的全部访问权限。</summary>
    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    /// <summary>回收 process 中尽可能多的物理内存页（psapi.dll）。失败返回 false。</summary>
    [DllImport("psapi.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EmptyWorkingSet(IntPtr hProcess);

    /// <summary>
    /// 设置工作集上下限。最小/最大值都传 -1 时表示“尽量腾出已不再使用的物理内存页”，
    /// 是 EmptyWorkingSet 不可用时的等价兜底（kernel32.dll）。
    /// </summary>
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetProcessWorkingSetSize(IntPtr hProcess, IntPtr dwMinimumWorkingSetSize, IntPtr dwMaximumWorkingSetSize);
}
