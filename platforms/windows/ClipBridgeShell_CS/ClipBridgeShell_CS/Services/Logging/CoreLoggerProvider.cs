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

    public CoreLoggerProvider(ICoreHostService coreHost, CoreLogDispatcher dispatcher, StashLogManager stashManager)
    {
        _coreHost = coreHost;
        _dispatcher = dispatcher;
        _stashManager = stashManager;

        // 监听核心状态变化
        _coreHost.StateChanged += OnCoreStateChanged;
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
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] RewriteStashedLogsAsync called, CoreState={_coreHost.State}");
        // #endregion
        
        if (_coreHost.State != CoreState.Ready)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] RewriteStashedLogsAsync skipped: Core not ready");
            // #endregion
            return;
        }

        var stashedLogs = _stashManager.ReadAllLogs();
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] Found {stashedLogs.Count} stashed logs to rewrite");
        // #endregion
        
        if (stashedLogs.Count == 0)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] No stashed logs to rewrite");
            // #endregion
            return;
        }

        // 使用 CoreLogDispatcher 批量写入暂存日志（更高效）
        int enqueuedCount = 0;
        foreach (var entry in stashedLogs)
        {
            if (_dispatcher.Enqueue(entry))
            {
                enqueuedCount++;
            }
            else
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] Failed to enqueue stashed log: id={entry.Id}");
                // #endregion
            }
        }
        
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] Enqueued {enqueuedCount}/{stashedLogs.Count} stashed logs to dispatcher");
        // #endregion

        // 等待一小段时间，让dispatcher处理一些日志
        await Task.Delay(500);

        // 清空暂存日志（即使部分日志还在队列中，也会被dispatcher处理）
        _stashManager.Clear();
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] Stashed logs cleared after enqueueing");
        // #endregion
    }

    public ILogger CreateLogger(string categoryName)
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] CreateLogger called for category: {categoryName}, CoreState: {_coreHost.State}");
        // #endregion
        if (!_loggers.TryGetValue(categoryName, out var logger))
        {
            logger = new CoreLogger(categoryName, _coreHost, _dispatcher, _stashManager);
            _loggers[categoryName] = logger;
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLoggerProvider] Created new CoreLogger for category: {categoryName}");
            // #endregion
        }
        return logger;
    }

    public void Dispose()
    {
        _coreHost.StateChanged -= OnCoreStateChanged;
        _dispatcher?.Dispose();
        _loggers.Clear();
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
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLogger] Log called: category={_categoryName}, level={logLevel}, CoreState={_coreHost.State}");
        // #endregion
        if (formatter == null)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLogger] Log skipped: formatter is null");
            // #endregion
            return;
        }

        var message = formatter(state, exception);
        if (string.IsNullOrEmpty(message) && exception == null)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLogger] Log skipped: message is empty and no exception");
            // #endregion
            return;
        }

        var level = MapLogLevel(logLevel);
        var component = "Shell";
        var category = _categoryName;
        var exceptionStr = exception?.ToString();
        var propsJson = SerializeProperties(state);

        var entry = new LogEntry
        {
            Id = GetNextId(),
            TsUtc = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            Level = level,
            Component = component,
            Category = category,
            Message = message,
            Exception = exceptionStr,
            PropsJson = propsJson
        };

        // 根据核心状态选择写入路径
        if (_coreHost.State == CoreState.Ready)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLogger] Enqueueing log to dispatcher: id={entry.Id}, message={message.Substring(0, Math.Min(50, message.Length))}");
            // #endregion
            // 核心就绪，通过dispatcher写入
            _dispatcher.Enqueue(entry);
        }
        else
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLogger] Writing to stash: id={entry.Id}, message={message.Substring(0, Math.Min(50, message.Length))}");
            // #endregion
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
