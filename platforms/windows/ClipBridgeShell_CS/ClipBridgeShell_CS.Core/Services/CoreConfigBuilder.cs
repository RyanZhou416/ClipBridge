using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using System.Text.Json;

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
        // MSIX 与非打包：你现在 LocalSettingsService 已区分了 RuntimeHelper.IsMSIX（可复用同策略）
        // 这里用最保守、可落地的路径：LocalAppData\<appName>\
        var root = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        var appDataDir = Path.Combine(root, appName, "shell");
        var coreRoot = Path.Combine(root, appName, "core");  // core_data_dir（总根）
        var dataDir = Path.Combine(coreRoot, "data");       // SQLite + 其它持久
        var cacheDir = Path.Combine(coreRoot, "cache");      // CAS + tmp
        var logDir = Path.Combine(coreRoot, "logs");

        Directory.CreateDirectory(appDataDir);
        Directory.CreateDirectory(coreRoot);
        Directory.CreateDirectory(dataDir);
        Directory.CreateDirectory(cacheDir);
        Directory.CreateDirectory(logDir);

        return new Paths(appDataDir, coreRoot, dataDir, cacheDir, logDir);
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
