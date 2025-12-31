using System.Threading.Channels;
using System.Text.Json;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Stores;

namespace ClipBridgeShell_CS.Services;

public class EventPumpService
{
    private readonly HistoryStore _historyStore;
    private readonly PeerStore _peerStore;
    private readonly TransferStore _transferStore;

    // 无锁队列：生产者是 Core FFI 回调，消费者是 ProcessEventsAsync
    private readonly Channel<string> _channel;
    private readonly CancellationTokenSource _cts = new();

    public EventPumpService(HistoryStore historyStore, PeerStore peerStore, TransferStore transferStore)
    {
        _historyStore = historyStore;
        _peerStore = peerStore;
        _transferStore = transferStore;

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
            // --- 历史记录事件 ---
            case "item_meta_added":
            case "item_added":
            case "item_updated":
                var meta = envelope.Payload.Deserialize<ItemMetaPayload>();
                if (meta != null) _historyStore.Upsert(meta);
                break;

            // --- [新增] 设备/节点事件 ---
            case "peer_found":
            case "peer_changed":
            case "peer_online":
            case "peer_offline":
                var peer = envelope.Payload.Deserialize<PeerMetaPayload>();
                if (peer != null) _peerStore.Upsert(peer);
                break;

            // --- [新增] 传输进度事件 ---
            case "transfer_progress":
            case "transfer_update":
            case "transfer_started":
            case "transfer_completed":
            case "transfer_failed":
                var transfer = envelope.Payload.Deserialize<TransferUpdatePayload>();
                if (transfer != null) _transferStore.Update(transfer);
                break;

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
