// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/StartupTrace.cs
using System;
using System.Diagnostics;
using System.IO;
using ClipBridgeShell_CS.Core.Services;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 启动耗时打点（默认关闭）。
///
/// 用法：设置环境变量 <c>CB_STARTUP_TRACE=1</c> 后启动应用，会把每个阶段的
/// “距进程创建”的毫秒数追加写入 &lt;dataRoot&gt;\startup.log。
/// dataRoot 由 <see cref="CoreConfigBuilder.ResolveDataRoot"/> 统一决定：
/// 默认 <c>%LOCALAPPDATA%\ClipBridge</c>，便携模式下为 <c>&lt;exeDir&gt;\data</c>。
///
/// 设计要点：
/// - 用 <see cref="Process.StartTime"/> 作为零点，因此能覆盖到 <c>App</c> 构造之前的
///   WinAppSDK bootstrap 开销（那是 WinUI3 冷启动里最容易被忽略的一段）。
/// - 未开启时全部方法立即返回，生产环境零成本。
/// - 整个进程只持有一个追加流并逐条 flush，避免打点本身引入明显 IO 噪声。
/// </summary>
public static class StartupTrace
{
    private static readonly bool Enabled =
        string.Equals(Environment.GetEnvironmentVariable("CB_STARTUP_TRACE"), "1", StringComparison.Ordinal);

    private static readonly Stopwatch Clock = new();
    private static readonly object Gate = new();
    private static StreamWriter? _writer;
    private static bool _initialized;

    private static void EnsureInitialized()
    {
        if (_initialized) return;
        _initialized = true;
        try
        {
            var dir = CoreConfigBuilder.ResolveDataRoot();
            Directory.CreateDirectory(dir);
            var path = Path.Combine(dir, "startup.log");
            // 每次进程启动写入分隔线，便于把多次运行的记录分开
            _writer = new StreamWriter(new FileStream(path, FileMode.Append, FileAccess.Write, FileShare.Read))
            {
                AutoFlush = true
            };
            _writer.WriteLine($"--- run {DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} pid={Environment.ProcessId} ---");
        }
        catch
        {
            _writer = null;
        }
    }

    /// <summary>记录一个阶段。phase 应保持稳定，便于聚合统计。</summary>
    public static void Mark(string phase)
    {
        if (!Enabled) return;
        try
        {
            EnsureInitialized();
            // 相对进程创建时刻的毫秒数（包含进程启动到本类首次使用的全部时间）
            var sinceProcessStartMs = (DateTime.Now - Process.GetCurrentProcess().StartTime).TotalMilliseconds;
            lock (Gate)
            {
                // 同时写入绝对时间戳：跨进程测量（例如第二实例重定向到主实例、
                // 主实例把窗口带前台）必须靠绝对时间才对得齐。
                _writer?.WriteLine($"{sinceProcessStartMs,9:F1}\t{DateTime.Now:HH:mm:ss.fff}\t{phase}");
            }
        }
        catch
        {
            // 打点绝不能影响启动
        }
    }

    /// <summary>记录一个自上次标记以来的耗时（用于测量单段代码）。</summary>
    public static void MarkElapsed(string phase)
    {
        if (!Enabled) return;
        try
        {
            Clock.Stop();
            var ms = Clock.Elapsed.TotalMilliseconds;
            Mark($"{phase} (segment={ms:F1}ms)");
            Clock.Restart();
        }
        catch
        {
        }
    }

    public static void RestartSegmentClock()
    {
        if (!Enabled) return;
        Clock.Restart();
    }
}
