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

    // 事件接收通知（供 ViewModel 订阅）
    public event Action<string>? EventReceived;

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
        // 触发 EventReceived 事件（供 ViewModel 订阅）
        EventReceived?.Invoke(json);

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
            // 说明：type 已统一 ToUpperInvariant()，因此下列每个值都同时接受大小写混写形式。
            // 目前 Core 实际发出的 type 字符串（混用大小写，外壳统一归一化）：
            //   ITEM_META_ADDED（api.rs:400 本地 ingest 顶层 meta / actor.rs:715 远端 payload.meta）
            //   peer_changed（api.rs:630） / TRANSFER_PROGRESS（actor.rs:1028）
            //   CONTENT_CACHED（actor.rs:1124、api.rs:716）
            //   TRANSFER_CANCELLED（actor.rs:1153） / TRANSFER_FAILED（actor.rs:1190）
            //   ITEM_DELETED（api.rs:793） / LOG_WRITTEN / LOGS_BATCH_WRITTEN
            // 兼容旧拼写：ITEM_ADDED / ITEM_UPDATED / PEER_FOUND / PEER_ONLINE / PEER_OFFLINE。

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
                    // Core 的 peer_changed 把字段直接放在顶层（cb_core/src/api.rs:629-634：
                    // type/device_id/share_to_peer/accept_from_peer），没有 payload；
                    // 其它 peer 事件历史上用 payload，两种形态都兼容。
                    JsonElement peerEl = default;
                    if (payload.ValueKind == JsonValueKind.Object)
                        peerEl = payload;
                    else if (root.TryGetProperty("device_id", out _))
                        peerEl = root;

                    if (peerEl.ValueKind != JsonValueKind.Object)
                        return;

                    var peer = peerEl.Deserialize<PeerMetaPayload>();
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

            // --- 传输进度（关键：否则 TransferStore 永远为空、仪表盘活动传输恒为 0）
            case "TRANSFER_PROGRESS":
                {
                    // Core 实际载荷（cb_core/src/session/actor.rs:1027-1035）只有：
                    //   { "transfer_id": string, "received": number, "total": number }
                    // 不含 state / item_id；外壳为 UI 契约补默认状态 "downloading"。
                    if (payload.ValueKind != JsonValueKind.Object)
                        return;

                    var progress = payload.Deserialize<TransferUpdatePayload>();
                    if (progress == null || string.IsNullOrWhiteSpace(progress.TransferId))
                        return;

                    if (string.IsNullOrWhiteSpace(progress.State))
                        progress.State = "downloading";

                    _transferStore.Upsert(progress);
                    break;
                }

            // --- 传输取消（入口点被取消时发出）
            case "TRANSFER_CANCELLED":
                {
                    // Core 载荷（cb_core/src/session/actor.rs:1152-1156）：{ "transfer_id": string }
                    if (payload.ValueKind != JsonValueKind.Object)
                        return;

                    var cancelled = payload.Deserialize<TransferUpdatePayload>();
                    if (cancelled == null || string.IsNullOrWhiteSpace(cancelled.TransferId))
                        return;

                    if (string.IsNullOrWhiteSpace(cancelled.State))
                        cancelled.State = "cancelled";

                    _transferStore.Upsert(cancelled);
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

                    // Core 的 TRANSFER_FAILED 使用 CoreErrorPayload（actor.rs:1179-1193）：
                    // code/message/scope/retryable/affects_session/detail{transfer_id}。
                    // 先落库为终态失败行，再通知 awaiter（两者都要）。
                    var failed = payloadObj.Deserialize<TransferUpdatePayload>() ?? new TransferUpdatePayload();
                    failed.TransferId = transferId!;
                    failed.State = "failed";
                    if (string.IsNullOrWhiteSpace(failed.Message))
                        failed.Message = reason;
                    _transferStore.Upsert(failed);

                    var ex = new Exception(string.IsNullOrWhiteSpace(reason)
                        ? "TRANSFER_FAILED"
                        : $"TRANSFER_FAILED: {reason}");

                    _awaiter.Fail(transferId!, ex);
                    break;
                }

            // --- 删除项（本地或全局删除后 Core 发出）
            case "ITEM_DELETED":
                {
                    var itemId = root.TryGetProperty("item_id", out var iidEl) ? iidEl.GetString() : null;
                    if (!string.IsNullOrEmpty(itemId))
                        _historyStore.RemoveByItemId(itemId);
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
