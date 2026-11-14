using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Services;

/// <summary>数据根目录的两种来源。</summary>
public enum DataRootMode
{
    /// <summary>默认：<c>%LOCALAPPDATA%\&lt;appName&gt;</c>。</summary>
    LocalAppData = 0,

    /// <summary>便携：exe 旁存在 <c>ClipBridge.portable</c> 且 <c>&lt;exeDir&gt;\data</c> 可写。</summary>
    Portable = 1,
}

/// <summary>
/// 数据根目录的解析结果。
/// </summary>
/// <param name="Root">最终使用的数据根目录（core.db / tls / Cache / logs 都在它下面）。</param>
/// <param name="Mode">根目录的来源。</param>
/// <param name="PortableMarkerPath">检测到的 marker 路径；未请求便携模式时为 null。</param>
/// <param name="PortableFallbackReason">
/// 便携模式被请求但不可用时，回退到 <see cref="DataRootMode.LocalAppData"/> 的原因；
/// 正常情况（没有 marker，或便携目录可用）为 null。调用方必须把非 null 值提示给用户/写进日志，
/// 不允许静默回退。
/// </param>
public sealed record DataRootResolution(
    string Root,
    DataRootMode Mode,
    string? PortableMarkerPath,
    string? PortableFallbackReason)
{
    public bool IsPortable => Mode == DataRootMode.Portable;

    public string ModeText => IsPortable ? "portable" : "localappdata";
}

public sealed class CoreConfigBuilder
{
    /// <summary>便携模式标记文件（空文件，放在 exe 同目录）。</summary>
    public const string PortableMarkerFileName = "ClipBridge.portable";

    /// <summary>便携模式下的数据目录名（位于 exe 目录下）。</summary>
    public const string PortableDataDirectoryName = "data";

    /// <summary>回退原因记录文件名（写在回退目标根目录下，便于排障）。</summary>
    private const string FallbackLogFileName = "portable-fallback.log";

    /// <summary>
    /// 解析结果按 appName 缓存：同一进程内所有调用方（core 路径、日志、诊断、UI）必须看到
    /// 同一个根目录，不能在两次调用之间因为目录状态变化而“半分裂”。
    /// </summary>
    private static readonly ConcurrentDictionary<string, DataRootResolution> ResolutionCache =
        new(StringComparer.OrdinalIgnoreCase);

    public sealed record Paths(
        string AppDataDir,
        string CoreDataDir,
        string DataDir,
        string CacheDir,
        string LogDir
    );

    /// <summary>
    /// 返回数据根目录。这是全仓库唯一的解析入口：
    /// <list type="number">
    /// <item>exe 旁存在 <c>ClipBridge.portable</c>（且 <c>&lt;exeDir&gt;\data</c> 可写）→ 便携根；</item>
    /// <item>否则/回退 → <c>%LOCALAPPDATA%\&lt;appName&gt;</c>。</item>
    /// </list>
    /// 需要回退原因时用 <see cref="ResolveDataRootInfo"/>。
    /// </summary>
    public static string ResolveDataRoot(string appName = "ClipBridge")
        => ResolveDataRootInfo(appName).Root;

    /// <summary>
    /// 同 <see cref="ResolveDataRoot"/>，但返回完整解析结果（来源、marker、回退原因）。
    /// </summary>
    public static DataRootResolution ResolveDataRootInfo(string appName = "ClipBridge")
    {
        appName = string.IsNullOrWhiteSpace(appName) ? "ClipBridge" : appName.Trim();
        return ResolutionCache.GetOrAdd(appName, static name => ResolveDataRootCore(name));
    }

    private static DataRootResolution ResolveDataRootCore(string appName)
    {
        var localAppDataRoot = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            appName);

        var exeDir = AppContext.BaseDirectory;
        var markerPath = Path.Combine(exeDir, PortableMarkerFileName);
        if (!File.Exists(markerPath))
        {
            return new DataRootResolution(localAppDataRoot, DataRootMode.LocalAppData, null, null);
        }

        var portableRoot = Path.Combine(exeDir, PortableDataDirectoryName);
        var createdByUs = !Directory.Exists(portableRoot);
        try
        {
            Directory.CreateDirectory(portableRoot);
            ProbeWritable(portableRoot);
            return new DataRootResolution(portableRoot, DataRootMode.Portable, markerPath, null);
        }
        catch (Exception ex)
        {
            // 便携目录不可用（典型：解压到 Program Files / 只读 U 盘 / 受控目录）。
            // 绝不能“一部分数据写便携目录、一部分写 LocalAppData”：整体回退，并记录原因。
            var reason = $"无法写入便携数据目录 {portableRoot}（{ex.GetType().Name}: {ex.Message}）";
            TryDeleteIfEmpty(portableRoot, createdByUs);
            TryRecordFallback(localAppDataRoot, markerPath, portableRoot, reason, ex);
            return new DataRootResolution(
                localAppDataRoot,
                DataRootMode.LocalAppData,
                markerPath,
                reason);
        }
    }

    /// <summary>
    /// 用一次真实的写文件探测目录是否可写：目录已存在但 ACL 只读时，
    /// <see cref="Directory.CreateDirectory(string)"/> 会静默成功，只有真正写文件才能发现。
    /// 探测文件用 <see cref="FileOptions.DeleteOnClose"/>，关闭即删除，不留残留。
    /// </summary>
    private static void ProbeWritable(string dir)
    {
        var probePath = Path.Combine(dir, $".clipbridge-write-{Guid.NewGuid():N}.tmp");
        using var probe = new FileStream(
            probePath,
            FileMode.CreateNew,
            FileAccess.Write,
            FileShare.None,
            bufferSize: 1,
            FileOptions.DeleteOnClose);
    }

    /// <summary>便携目录是本次调用新建的、且回退时仍为空 → 删掉，避免留下误导性的空 <c>data</c>。</summary>
    private static void TryDeleteIfEmpty(string dir, bool createdByUs)
    {
        if (!createdByUs)
        {
            return;
        }

        try
        {
            if (Directory.Exists(dir) && !Directory.EnumerateFileSystemEntries(dir).Any())
            {
                Directory.Delete(dir, recursive: false);
            }
        }
        catch
        {
            // 清理属于尽力而为；删不掉也不影响回退结果。
        }
    }

    /// <summary>
    /// 把回退原因追加到回退目标根目录下的日志文件，保证即使用户忽略了启动提示，
    /// 事后也能从磁盘上看到为什么没有使用便携目录。
    /// </summary>
    private static void TryRecordFallback(
        string localAppDataRoot,
        string markerPath,
        string portableRoot,
        string reason,
        Exception ex)
    {
        try
        {
            Directory.CreateDirectory(localAppDataRoot);
            var logPath = Path.Combine(localAppDataRoot, FallbackLogFileName);
            var line =
                $"{DateTimeOffset.Now:yyyy-MM-dd HH:mm:ss.fff zzz}\tpid={Environment.ProcessId}" +
                $"\tmarker={markerPath}\tportable={portableRoot}\tfallback={localAppDataRoot}" +
                $"\treason={reason}{Environment.NewLine}";
            File.AppendAllText(logPath, line);
        }
        catch
        {
            // 连回退位置都写不了时只能放弃落盘，异常类型仍会通过 Debug 输出到调试器。
        }

        System.Diagnostics.Debug.WriteLine(
            $"[CoreConfigBuilder] portable data dir unavailable, falling back to {localAppDataRoot}: {reason}");
        System.Diagnostics.Debug.WriteLine(ex);
    }

    public Paths BuildPaths(string appName = "ClipBridge")
    {
        // 真实布局（与 CoreHostService.InitializeAsync 传给 cb_init 的字段一致）：
        //   <dataRoot>\            ← data_dir：core.db / logs.db / tls/ / startup.log
        //   <dataRoot>\Cache\      ← cache_dir：blobs/ + tmp/
        //
        // <dataRoot> 由 ResolveDataRoot 统一决定：
        //   - 默认：%LOCALAPPDATA%\<appName>
        //   - 便携：exe 旁有 ClipBridge.portable 且 <exeDir>\data 可写 → <exeDir>\data
        //   - 便携目录不可写：回退 %LOCALAPPDATA%\<appName>（原因见 DataRootResolution）
        //
        // 历史遗留：这里曾经创建 `<appName>\shell`、`<appName>\core\{data,cache,logs}`，
        // 但运行中的 core 从来没用过它们（data_dir 一直是根目录），结果是每次启动都
        // 留下几个空目录，误导用户和排障。现在只创建真正会用到的两个目录，并对旧布局
        // 的空目录做一次性清理（见 CoreHostService.TryRemoveEmptyLegacyDirs）。
        var root = ResolveDataRoot(appName);
        var cacheDir = Path.Combine(root, "Cache");

        Directory.CreateDirectory(root);
        Directory.CreateDirectory(cacheDir);

        // LogDir 与 data_dir 同根：core 把 logs.db 写在 data_dir 下。
        return new Paths(root, root, root, cacheDir, root);
    }

    public string BuildConfigJson(Paths p)
    {
        // 对齐文档 4.8.8.1 的“cb_init(config_json)”口径（字段可逐步补齐）
        // 关键是 data_dir / cache_dir / log_dir 必须正确。
        var obj = new Dictionary<string, object?>
        {
            ["type"] = "CoreConfig",
            ["data_dir"] = p.DataDir.Replace("\\", "/"),
            ["cache_dir"] = p.CacheDir.Replace("\\", "/"),
            ["log_dir"] = p.LogDir.Replace("\\", "/"),

            // 先给最小可运行默认值；后续接入账号/设备体系再补齐
            ["device_name"] = Environment.MachineName,
            ["log_level"] = "info",

            // limits：你文档里已经有整体策略；这里先留结构位
            ["limits"] = new Dictionary<string, object?>
            {
                ["text_max_bytes"] = 1_000_000,
                ["image_max_bytes"] = 30_000_000,
                ["file_max_bytes"] = 200_000_000,
            }
        };

        return JsonSerializer.Serialize(obj);
    }
}
