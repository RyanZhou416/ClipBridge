using System;
using System.Collections.Generic;
using Microsoft.Extensions.Logging;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;

namespace ClipBridgeShell_CS.Services.Logging;

/// <summary>
/// Core日志提供者，实现ILoggerProvider接口
/// </summary>
public sealed class CoreLoggerProvider : ILoggerProvider
{
    private readonly ICoreHostService _coreHost;
    private readonly CoreLogDispatcher _dispatcher;
    private readonly StashLogManager _stashManager;
    private readonly Dictionary<string, CoreLogger> _loggers = new();
    private readonly HashSet<long> _pendingStashedLogIds = new(); // 跟踪待写入的暂存日志ID
    private readonly object _pendingLock = new(); // 保护 _pendingStashedLogIds 的锁

    public CoreLoggerProvider(ICoreHostService coreHost, CoreLogDispatcher dispatcher, StashLogManager stashManager)
    {
        _coreHost = coreHost;
        _dispatcher = dispatcher;
        _stashManager = stashManager;

        // 监听核心状态变化
        _coreHost.StateChanged += OnCoreStateChanged;
        
        // 监听日志写入完成回调
        _dispatcher.BatchWritten += OnBatchWritten;
    }

    private void OnCoreStateChanged(CoreState state)
    {
        if (state == CoreState.Ready)
        {
            // 核心就绪，回写暂存日志
            _ = Task.Run(async () => await RewriteStashedLogsAsync());
        }
    }

    private async Task RewriteStashedLogsAsync()
    {
        if (_coreHost.State != CoreState.Ready)
        {
            return;
        }

        var stashedLogs = _stashManager.ReadAllLogs();
        
        if (stashedLogs.Count == 0)
        {
            return;
        }

        // 使用 CoreLogDispatcher 批量写入暂存日志（更高效）
        // 记录所有待写入的暂存日志ID，用于跟踪写入完成状态
        lock (_pendingLock)
        {
            _pendingStashedLogIds.Clear();
            foreach (var entry in stashedLogs)
            {
                _pendingStashedLogIds.Add(entry.Id);
            }
        }
        
        foreach (var entry in stashedLogs)
        {
            if (!_dispatcher.Enqueue(entry))
            {
                // 如果入队失败，从待写入列表中移除
                lock (_pendingLock)
                {
                    _pendingStashedLogIds.Remove(entry.Id);
                }
            }
        }
        
        // 注意：不再使用固定延迟，而是通过 OnBatchWritten 回调来检查所有暂存日志是否已写入
        // 如果所有暂存日志都已写入，OnBatchWritten 会自动清空暂存文件
    }

    public ILogger CreateLogger(string categoryName)
    {
        if (!_loggers.TryGetValue(categoryName, out var logger))
        {
            logger = new CoreLogger(categoryName, _coreHost, _dispatcher, _stashManager);
            _loggers[categoryName] = logger;
        }
        return logger;
    }

    /// <summary>
    /// 当一批日志写入完成时触发
    /// </summary>
    private void OnBatchWritten(List<LogEntry> writtenEntries)
    {
        lock (_pendingLock)
        {
            if (_pendingStashedLogIds.Count == 0)
            {
                // 没有待写入的暂存日志，无需处理
                return;
            }
            
            // 从待写入列表中移除已写入的日志
            foreach (var entry in writtenEntries)
            {
                _pendingStashedLogIds.Remove(entry.Id);
            }
            
            // 如果所有暂存日志都已写入，清空暂存文件
            if (_pendingStashedLogIds.Count == 0)
            {
                _stashManager.Clear();
            }
        }
    }

    public void Dispose()
    {
        _coreHost.StateChanged -= OnCoreStateChanged;
        if (_dispatcher != null)
        {
            _dispatcher.BatchWritten -= OnBatchWritten;
            _dispatcher.Dispose();
        }
        _loggers.Clear();
        lock (_pendingLock)
        {
            _pendingStashedLogIds.Clear();
        }
    }
}

/// <summary>
/// Core日志记录器
/// </summary>
internal sealed class CoreLogger : ILogger
{
    private readonly string _categoryName;
    private readonly ICoreHostService _coreHost;
    private readonly CoreLogDispatcher _dispatcher;
    private readonly StashLogManager _stashManager;
    private static long _nextId = 1;
    private static readonly object _idLock = new();

    public CoreLogger(string categoryName, ICoreHostService coreHost, CoreLogDispatcher dispatcher, StashLogManager stashManager)
    {
        _categoryName = categoryName;
        _coreHost = coreHost;
        _dispatcher = dispatcher;
        _stashManager = stashManager;
    }

    public IDisposable? BeginScope<TState>(TState state) where TState : notnull => null;

    public bool IsEnabled(LogLevel logLevel) => true; // 所有级别都启用

    public void Log<TState>(
        LogLevel logLevel,
        EventId eventId,
        TState state,
        Exception? exception,
        Func<TState, Exception?, string> formatter)
    {
        if (formatter == null)
        {
            return;
        }

        var message = formatter(state, exception);
        if (string.IsNullOrEmpty(message) && exception == null)
        {
            return;
        }

        var level = MapLogLevel(logLevel);
        var component = "Shell";
        var category = _categoryName;
        var exceptionStr = exception?.ToString();
        var propsJson = SerializeProperties(state);

        // 获取多语言消息（从翻译器获取中英文版本）
        var (messageEn, messageZhCn) = LogMessageTranslator.GetTranslated(message);

        var entry = new LogEntry
        {
            Id = GetNextId(),
            TsUtc = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            Level = level,
            Component = component,
            Category = category,
            Message = message, // 向后兼容
            MessageEn = messageEn,
            MessageZhCn = messageZhCn,
            Exception = exceptionStr,
            PropsJson = propsJson
        };

        // 根据核心状态选择写入路径
        if (_coreHost.State == CoreState.Ready)
        {
            // 核心就绪，通过dispatcher写入
            _dispatcher.Enqueue(entry);
        }
        else
        {
            // 核心未就绪，写入暂存日志
            _stashManager.WriteLog(entry);
        }
    }

    private static int MapLogLevel(LogLevel logLevel) => logLevel switch
    {
        LogLevel.Trace => 0,
        LogLevel.Debug => 1,
        LogLevel.Information => 2,
        LogLevel.Warning => 3,
        LogLevel.Error => 4,
        LogLevel.Critical => 5,
        _ => 2
    };

    private static string? SerializeProperties<TState>(TState state)
    {
        try
        {
            if (state == null) return null;
            return System.Text.Json.JsonSerializer.Serialize(state);
        }
        catch
        {
            return null;
        }
    }

    private static long GetNextId()
    {
        lock (_idLock)
        {
            return _nextId++;
        }
    }
}
