using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Services;

public sealed class CoreConfigBuilder
{
    public sealed record Paths(
        string AppDataDir,
        string CoreDataDir,
        string DataDir,
        string CacheDir,
        string LogDir
    );

    public Paths BuildPaths(string appName = "ClipBridge")
    {
        // 真实布局（与 CoreHostService.InitializeAsync 传给 cb_init 的字段一致）：
        //   %LOCALAPPDATA%\<appName>\            ← data_dir：core.db / logs.db / tls/ / startup.log
        //   %LOCALAPPDATA%\<appName>\Cache\      ← cache_dir：blobs/ + tmp/
        //
        // 历史遗留：这里曾经创建 `<appName>\shell`、`<appName>\core\{data,cache,logs}`，
        // 但运行中的 core 从来没用过它们（data_dir 一直是根目录），结果是每次启动都
        // 留下几个空目录，误导用户和排障。现在只创建真正会用到的两个目录，并对旧布局
        // 的空目录做一次性清理（见 CoreHostService.TryRemoveEmptyLegacyDirs）。
        var root = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        var appRoot = Path.Combine(root, appName);
        var cacheDir = Path.Combine(appRoot, "Cache");

        Directory.CreateDirectory(appRoot);
        Directory.CreateDirectory(cacheDir);

        // LogDir 与 data_dir 同根：core 把 logs.db 写在 data_dir 下。
        return new Paths(appRoot, appRoot, appRoot, cacheDir, appRoot);
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
