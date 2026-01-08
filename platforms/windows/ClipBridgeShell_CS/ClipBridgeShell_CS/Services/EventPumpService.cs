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
    private readonly ContentFetchAwaiter _awaiter;

    // 无锁队列：生产者是 Core FFI 回调，消费者是 ProcessEventsAsync
    private readonly Channel<string> _channel;
    private readonly CancellationTokenSource _cts = new();

    // 日志写入事件通知
    public event EventHandler? LogWritten;

    public EventPumpService(HistoryStore historyStore, PeerStore peerStore, TransferStore transferStore, ContentFetchAwaiter awaiter)
    {
        _historyStore = historyStore;
        _peerStore = peerStore;
        _transferStore = transferStore;
        _awaiter = awaiter;
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
                        var shortJson = json.Length > 200 ? json[..200] + "..." : json;
                        System.Diagnostics.Debug.WriteLine($"[EventPump] recv {shortJson}");

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
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;

        if (!root.TryGetProperty("type", out var typeEl) || typeEl.ValueKind != JsonValueKind.String)
            return;

        var type = (typeEl.GetString() ?? string.Empty).Trim().ToUpperInvariant();

        // payload 是可选的
        JsonElement payload = default;
        if (root.TryGetProperty("payload", out var p) && p.ValueKind != JsonValueKind.Null && p.ValueKind != JsonValueKind.Undefined)
            payload = p;

        // meta 也是可选的（你 Core 的 ITEM_META_ADDED 就是顶层 meta）
        JsonElement metaTop = default;
        if (root.TryGetProperty("meta", out var m) && m.ValueKind == JsonValueKind.Object)
            metaTop = m;

        // 统一：取“本条事件代表的 ItemMeta 对象”，兼容：
        // 1) 顶层 meta
        // 2) payload.meta
        // 3) payload 本身就是 meta
        bool TryPickMeta(out JsonElement metaEl)
        {
            if (metaTop.ValueKind == JsonValueKind.Object)
            {
                metaEl = metaTop;
                return true;
            }

            if (payload.ValueKind == JsonValueKind.Object)
            {
                if (payload.TryGetProperty("meta", out var pm) && pm.ValueKind == JsonValueKind.Object)
                {
                    metaEl = pm;
                    return true;
                }

                // 某些实现可能直接把 meta 放在 payload 本体
                if (payload.TryGetProperty("item_id", out _) && payload.TryGetProperty("kind", out _))
                {
                    metaEl = payload;
                    return true;
                }
            }

            metaEl = default;
            return false;
        }

        switch (type)
        {
            // --- History meta
            case "ITEM_META_ADDED":
            case "ITEM_ADDED":
            case "ITEM_UPDATED":
                {
                    if (!TryPickMeta(out var metaEl))
                        return;

                    var meta = metaEl.Deserialize<ItemMetaPayload>();
                    if (meta != null)
                        _historyStore.Upsert(meta);
                    break;
                }

            // --- Peer events（一般仍然是 payload 形态）
            case "PEER_FOUND":
            case "PEER_CHANGED":
            case "PEER_ONLINE":
            case "PEER_OFFLINE":
                {
                    if (payload.ValueKind != JsonValueKind.Object)
                        return;

                    var peer = payload.Deserialize<PeerMetaPayload>();
                    if (peer != null)
                    {
                        // 如果 core 的 peer payload 没带 is_online，这里按事件类型补上
                        if (type == "PEER_ONLINE")
                            peer.IsOnline = true;
                        if (type == "PEER_OFFLINE")
                            peer.IsOnline = false;

                        _peerStore.Upsert(peer);
                    }
                    break;
                }

            // --- Lazy fetch success (关键)
            case "CONTENT_CACHED":
                {
                    System.Diagnostics.Debug.WriteLine("[EventPump] CONTENT_CACHED dispatching.");

                    if (payload.ValueKind != JsonValueKind.Object)
                        return;

                    var transferId = payload.TryGetProperty("transfer_id", out var tidEl) ? tidEl.GetString() : null;
                    if (string.IsNullOrWhiteSpace(transferId))
                        return;

                    string? itemId = payload.TryGetProperty("item_id", out var iidEl) ? iidEl.GetString() : null;

                    string? textUtf8 = null;
                    string? localPath = null;
                    string? mime = null;

                    if (payload.TryGetProperty("local_ref", out var lr) && lr.ValueKind == JsonValueKind.Object)
                    {
                        if (lr.TryGetProperty("text_utf8", out var t))
                            textUtf8 = t.GetString();
                        if (lr.TryGetProperty("local_path", out var lp))
                            localPath = lp.GetString();
                        if (lr.TryGetProperty("mime", out var mm))
                            mime = mm.GetString();
                    }

                    _awaiter.Resolve(transferId!, new LocalContentRef
                    {
                        TransferId = transferId!,
                        ItemId = itemId,
                        TextUtf8 = textUtf8,
                        LocalPath = localPath,
                        Mime = mime
                    });
                    break;
                }

            // --- Lazy fetch failed (关键)
            case "TRANSFER_FAILED":
                {
                    if (payload.ValueKind != JsonValueKind.Object)
                        return;

                    var payloadObj = payload;

                    // 尝试从 payload.detail.transfer_id 取
                    string? transferId = null;
                    string? reason = null;

                    if (payloadObj.TryGetProperty("detail", out var detail) && detail.ValueKind == JsonValueKind.Object)
                    {
                        if (detail.TryGetProperty("transfer_id", out var tidEl) && tidEl.ValueKind == JsonValueKind.String)
                            transferId = tidEl.GetString();

                        // 可选：把错误信息带上（字段名按你 core 实际 payload 调整）
                        if (detail.TryGetProperty("message", out var msgEl) && msgEl.ValueKind == JsonValueKind.String)
                            reason = msgEl.GetString();
                        else if (detail.TryGetProperty("error", out var errEl) && errEl.ValueKind == JsonValueKind.String)
                            reason = errEl.GetString();
                    }

                    // 某些实现可能直接是 payload.transfer_id
                    if (string.IsNullOrWhiteSpace(transferId) &&
                        payloadObj.TryGetProperty("transfer_id", out var tid2El) &&
                        tid2El.ValueKind == JsonValueKind.String)
                    {
                        transferId = tid2El.GetString();
                    }

                    if (string.IsNullOrWhiteSpace(transferId))
                        return;

                    var ex = new Exception(string.IsNullOrWhiteSpace(reason)
                        ? "TRANSFER_FAILED"
                        : $"TRANSFER_FAILED: {reason}");

                    _awaiter.Fail(transferId!, ex);
                    break;
                }

            // --- 日志写入事件
            case "LOG_WRITTEN":
            case "LOGS_BATCH_WRITTEN":
                {
                    // 通知订阅者日志已写入
                    LogWritten?.Invoke(this, EventArgs.Empty);
                    break;
                }

            default:
                // 未关心的事件类型直接忽略
                break;
        }
    }


    public void Shutdown()
    {
        _cts.Cancel();
    }
}
