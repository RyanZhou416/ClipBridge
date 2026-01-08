using System;
using System.Collections.Concurrent;
using System.Threading;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Interop;

namespace ClipBridgeShell_CS.Services.Logging;

/// <summary>
/// 后台批量写入日志到核心的调度器
/// </summary>
public sealed class CoreLogDispatcher : IDisposable
{
    private readonly ICoreHostService _coreHost;
    private readonly ConcurrentQueue<LogEntry> _queue = new();
    private readonly SemaphoreSlim _semaphore = new(0);
    private readonly CancellationTokenSource _cts = new();
    private readonly Task _backgroundTask;
    private const int BatchSize = 50;
    private const int QueueCapacity = 1000;

    public CoreLogDispatcher(ICoreHostService coreHost)
    {
        _coreHost = coreHost;
        _backgroundTask = Task.Run(ProcessLogsAsync);
    }

    /// <summary>
    /// 入队日志（如果队列满，丢弃Trace/Debug级别）
    /// </summary>
    public bool Enqueue(LogEntry entry)
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Enqueue called: id={entry.Id}, queueCount={_queue.Count}, CoreState={_coreHost.State}");
        // #endregion
        if (_queue.Count >= QueueCapacity)
        {
            // 仅允许丢弃Trace/Debug级别
            if (entry.Level <= 1) // Trace=0, Debug=1
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Entry dropped: queue full and level is Trace/Debug");
                // #endregion
                return false; // 丢弃
            }
            // 对于更高级别的日志，尝试移除一个Trace/Debug级别的日志
            if (!TryRemoveTraceOrDebug())
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Entry dropped: queue full and cannot remove Trace/Debug");
                // #endregion
                return false; // 无法移除，丢弃
            }
        }

        _queue.Enqueue(entry);
        _semaphore.Release();
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Entry enqueued successfully: id={entry.Id}, newQueueCount={_queue.Count}");
        // #endregion
        return true;
    }

    private bool TryRemoveTraceOrDebug()
    {
        // 尝试从队列中移除一个Trace或Debug级别的日志
        var items = new System.Collections.Generic.List<LogEntry>();
        while (_queue.TryDequeue(out var item))
        {
            if (item.Level <= 1)
            {
                // 找到Trace/Debug，丢弃它
                return true;
            }
            items.Add(item);
        }
        // 重新入队
        foreach (var item in items)
        {
            _queue.Enqueue(item);
        }
        return false;
    }

    private async Task ProcessLogsAsync()
    {
        var batch = new System.Collections.Generic.List<LogEntry>();
        while (!_cts.Token.IsCancellationRequested)
        {
            try
            {
                // 等待信号或超时（100ms）
                await _semaphore.WaitAsync(100, _cts.Token);
            }
            catch (OperationCanceledException)
            {
                break;
            }

            // 收集一批日志
            while (batch.Count < BatchSize && _queue.TryDequeue(out var entry))
            {
                batch.Add(entry);
            }

            // 如果核心就绪且有日志，批量写入
            if (batch.Count > 0 && _coreHost.State == CoreState.Ready)
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Processing batch: count={batch.Count}, CoreState={_coreHost.State}");
                // #endregion
                await WriteBatchAsync(batch);
                batch.Clear();
            }
            else if (batch.Count > 0)
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Batch waiting: count={batch.Count}, CoreState={_coreHost.State}");
                // #endregion
            }
            else if (batch.Count > 0)
            {
                // 核心未就绪，保留在批次中等待
                // 但避免批次过大
                if (batch.Count >= BatchSize * 2)
                {
                    batch.RemoveRange(0, BatchSize); // 丢弃最旧的
                }
            }
        }

        // 清理：尝试写入剩余的日志
        if (batch.Count > 0 && _coreHost.State == CoreState.Ready)
        {
            await WriteBatchAsync(batch);
        }
    }

    private async Task WriteBatchAsync(System.Collections.Generic.List<LogEntry> batch)
    {
        // #region agent log
        System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] WriteBatchAsync called: batchCount={batch.Count}, CoreState={_coreHost.State}");
        // #endregion
        if (_coreHost.State != CoreState.Ready)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] WriteBatchAsync skipped: CoreState is not Ready");
            // #endregion
            return;
        }

        var handle = _coreHost.GetHandle();
        if (handle == IntPtr.Zero)
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] WriteBatchAsync skipped: handle is zero");
            // #endregion
            return;
        }

        await Task.Run(() =>
        {
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Writing {batch.Count} entries to core");
            // #endregion
            foreach (var entry in batch)
            {
                try
                {
                    var id = CoreInterop.LogsWrite(
                        handle,
                        entry.Level,
                        entry.Category,
                        entry.Message,
                        entry.Exception,
                        entry.PropsJson
                    );
                    // #region agent log
                    System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Log written to core: entryId={entry.Id}, coreId={id}");
                    // #endregion
                }
                catch (Exception ex)
                {
                    // 写入失败，记录但不阻塞
                    System.Diagnostics.Debug.WriteLine($"[CoreLogDispatcher] Failed to write log: {ex.Message}");
                }
            }
        });
    }

    public void Dispose()
    {
        _cts.Cancel();
        _backgroundTask.Wait(TimeSpan.FromSeconds(5));
        _cts.Dispose();
        _semaphore.Dispose();
    }
}
