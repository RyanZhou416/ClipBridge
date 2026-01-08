using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using Windows.Storage;

namespace ClipBridgeShell_CS.Services.Logging;

/// <summary>
/// 暂存日志管理器，用于在核心未就绪时暂存日志
/// </summary>
public sealed class StashLogManager
{
    private readonly string _stashFilePath;
    private readonly object _lock = new();
    private readonly List<LogEntry> _inMemoryLogs = new();

    public StashLogManager()
    {
        var localFolder = ApplicationData.Current.LocalFolder.Path;
        var logsDir = Path.Combine(localFolder, "ClipBridge", "logs");
        Directory.CreateDirectory(logsDir);
        _stashFilePath = Path.Combine(logsDir, "stash.log");
    }

    /// <summary>
    /// 写入暂存日志（JSON Lines格式）
    /// </summary>
    public void WriteLog(LogEntry entry)
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[StashLogManager] WriteLog called: id={entry.Id}, message={entry.Message.Substring(0, Math.Min(50, entry.Message.Length))}");
        // #endregion
        lock (_lock)
        {
            // 添加到内存列表（用于立即显示）
            _inMemoryLogs.Add(entry);
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[StashLogManager] Added to memory: inMemoryCount={_inMemoryLogs.Count}");
            // #endregion

            // 写入文件（JSON Lines格式）
            try
            {
                // 使用默认命名策略（PascalCase），与LogEntry属性名匹配
                var json = JsonSerializer.Serialize(entry);
                File.AppendAllText(_stashFilePath, json + Environment.NewLine);
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[StashLogManager] Written to file: path={_stashFilePath}");
                // #endregion
            }
            catch (Exception ex)
            {
                // 如果写入失败，至少保留在内存中
                System.Diagnostics.Debug.WriteLine($"[StashLogManager] Failed to write to file: {ex.Message}");
            }
        }
    }

    /// <summary>
    /// 读取所有暂存日志（按时间戳排序）
    /// </summary>
    public List<LogEntry> ReadAllLogs()
    {
        lock (_lock)
        {
            var logs = new List<LogEntry>();

            // 从内存读取
            logs.AddRange(_inMemoryLogs);

            // 从文件读取（如果文件存在）
            if (File.Exists(_stashFilePath))
            {
                try
                {
                    var lines = File.ReadAllLines(_stashFilePath);
                    foreach (var line in lines)
                    {
                        if (string.IsNullOrWhiteSpace(line)) continue;
                        try
                        {
                            // 使用默认命名策略（PascalCase），与LogEntry属性名匹配
                            var entry = JsonSerializer.Deserialize<LogEntry>(line);
                            if (entry != null)
                            {
                                // 避免重复（内存中可能已有）
                                if (!logs.Any(l => l.Id == entry.Id && l.TsUtc == entry.TsUtc))
                                {
                                    logs.Add(entry);
                                }
                            }
                        }
                        catch
                        {
                            // 忽略无效行
                        }
                    }
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"[StashLogManager] Failed to read file: {ex.Message}");
                }
            }

            // 按时间戳排序
            return logs.OrderBy(l => l.TsUtc).ThenBy(l => l.Id).ToList();
        }
    }

    /// <summary>
    /// 清空暂存日志
    /// </summary>
    public void Clear()
    {
        lock (_lock)
        {
            _inMemoryLogs.Clear();
            try
            {
                if (File.Exists(_stashFilePath))
                {
                    File.Delete(_stashFilePath);
                }
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[StashLogManager] Failed to delete file: {ex.Message}");
            }
        }
    }

    /// <summary>
    /// 检查是否有暂存日志
    /// </summary>
    public bool HasStashedLogs()
    {
        lock (_lock)
        {
            return _inMemoryLogs.Count > 0 || (File.Exists(_stashFilePath) && new FileInfo(_stashFilePath).Length > 0);
        }
    }
}

/// <summary>
/// 暂存日志条目
/// </summary>
public sealed class LogEntry
{
    public long Id { get; set; }
    public long TsUtc { get; set; }
    public int Level { get; set; }
    public string Component { get; set; } = "";
    public string Category { get; set; } = "";
    public string Message { get; set; } = "";
    public string? Exception { get; set; }
    public string? PropsJson { get; set; }
}
