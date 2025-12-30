using System.Threading.Channels;
using System.Text.Json;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Stores;

namespace ClipBridgeShell_CS.Services;

public class EventPumpService
{
    private readonly HistoryStore _historyStore;

    // 无锁队列：生产者是 Core FFI 回调，消费者是 ProcessEventsAsync
    private readonly Channel<string> _channel;
    private readonly CancellationTokenSource _cts = new();

    public EventPumpService(HistoryStore historyStore)
    {
        _historyStore = historyStore;

        // 创建无界通道（v1 简单处理，避免丢消息）
        _channel = Channel.CreateUnbounded<string>();

        // 启动后台消费者线程
        _ = Task.Run(ProcessEventsAsync);
    }

    /// <summary>
    /// 供 CoreHostService 的 FFI 回调调用
    /// </summary>
    public void Enqueue(string jsonEvent)
    {
        _channel.Writer.TryWrite(jsonEvent);
    }

    private async Task ProcessEventsAsync()
    {
        var reader = _channel.Reader;
        try
        {
            while (await reader.WaitToReadAsync(_cts.Token))
            {
                while (reader.TryRead(out var json))
                {
                    try
                    {
                        ParseAndDispatch(json);
                    }
                    catch (Exception ex)
                    {
                        // 记录日志，但不崩溃
                        System.Diagnostics.Debug.WriteLine($"[EventPump] Parse Error: {ex.Message}");
                    }
                }
            }
        }
        catch (OperationCanceledException) { }
    }

    private void ParseAndDispatch(string json)
    {
        // 1. 初步解析获取 type
        var envelope = JsonSerializer.Deserialize<CoreEventEnvelope>(json);
        if (envelope == null) return;

        // 2. 根据 type 路由到不同的 Store
        switch (envelope.Type)
        {
            case "item_meta_added": // 兼容旧命名
            case "item_added":
                // 二次解析 Payload
                var meta = envelope.Payload.Deserialize<ItemMetaPayload>();
                if (meta != null) _historyStore.Upsert(meta);
                break;

            // TODO: 这里后续添加 "transfer_progress", "peer_status" 等分支

            default:
                System.Diagnostics.Debug.WriteLine($"[EventPump] Unhandled event: {envelope.Type}");
                break;
        }
    }

    public void Shutdown()
    {
        _cts.Cancel();
    }
}
