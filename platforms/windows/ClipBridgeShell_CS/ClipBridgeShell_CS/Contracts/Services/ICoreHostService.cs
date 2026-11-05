using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Models;

namespace ClipBridgeShell_CS.Contracts.Services;

/// <summary>
/// Core Bridge：UI 层（ViewModels / Views）访问核心的唯一受支持入口。
/// </summary>
/// <remarks>
/// 该接口封装了所有 <c>CoreInterop</c> P/Invoke 调用、句柄生命周期保护与 JSON envelope 解析，
/// UI 层不得直接引用 <c>Interop.CoreInterop</c>。新增核心能力应在这里先暴露强类型方法，
/// 再由调用方使用，从而保证分层（Integration / Core Bridge / Projection+UX）不被绕过。
/// </remarks>
public interface ICoreHostService
{
    CoreState State { get; }
    string? LastError { get; }
    CoreDiagnostics Diagnostics { get; }

    event Action<CoreState>? StateChanged;

    Task InitializeAsync(CancellationToken ct = default);
    Task ShutdownAsync(CancellationToken ct = default);
    Task IngestLocalCopy(string snapshotJson);
    string GetDiagnosticsText();
    Task<HistoryPage> ListHistoryAsync(HistoryQuery query);

    /// <summary>
    /// 获取核心句柄（遗留 API，不安全）。
    /// </summary>
    /// <remarks>
    /// 返回值在锁释放后可能立即失效：并发的 <see cref="ShutdownAsync"/> 会清空句柄并释放核心实例，
    /// 调用方“判空后再调用 FFI”存在 TOCTOU 窗口。新代码请使用
    /// <see cref="TryUseHandle"/> 或本接口的强类型方法。
    /// </remarks>
    [Obsolete("Use TryUseHandle or a typed ICoreHostService method")]
    IntPtr GetHandle();

    /// <summary>
    /// 在句柄锁内执行 <paramref name="action"/>，保证调用期间不会发生核心释放。
    /// </summary>
    /// <param name="action">
    /// 接收当前核心句柄的同步委托（不得在内部 await），返回 true 表示调用成功。
    /// </param>
    /// <returns>句柄可用时返回 <paramref name="action"/> 的结果；否则返回 false。</returns>
    bool TryUseHandle(Func<IntPtr, bool> action);

    // 设备管理方法
    List<PeerMetaPayload> ListPeers();
    void SetPeerPolicy(string peerId, bool? shareTo, bool? acceptFrom);
    void ClearPeerFingerprint(string peerId);
    void ClearLocalCert();

    Task DeleteItemLocalAsync(string itemId);
    Task DeleteItemGlobalAsync(string itemId);

    // -------------------- 日志 (Logs) --------------------

    /// <summary>写入一条核心日志，返回核心分配的日志 ID；核心未就绪时返回 0。</summary>
    Task<long> WriteLogAsync(int level, string component, string category, string messageEn,
        string? messageZhCn = null, string? exception = null, string? propsJson = null, long tsUtc = 0);

    /// <summary>查询最新日志；核心未就绪时返回空列表。</summary>
    Task<List<LogRow>> QueryLogsLatestAsync(int levelMin, string? like, int limit, string? lang = null);

    /// <summary>查询 ID 之后的新增日志；核心未就绪时返回空列表。</summary>
    Task<List<LogRow>> QueryLogsAfterIdAsync(long afterId, int levelMin, string? like, int limit, string? lang = null);

    /// <summary>查询 ID 之前的更早日志；核心未就绪时返回空列表。</summary>
    Task<List<LogRow>> QueryLogsBeforeIdAsync(long beforeId, int levelMin, string? like, int limit, string? lang = null);

    /// <summary>按时间范围分页查询日志；核心未就绪时返回空列表。</summary>
    Task<List<LogRow>> QueryLogsRangeAsync(long startMs, long endMs, int levelMin, string? like, int limit, int offset, string? lang = null);

    /// <summary>删除早于 <paramref name="cutoffMs"/> 的日志，返回删除条数；核心未就绪时返回 0。</summary>
    Task<long> DeleteLogsBeforeAsync(long cutoffMs);

    /// <summary>按 ID 列表删除日志，返回删除条数；核心未就绪时返回 0。</summary>
    Task<long> DeleteLogsByIdsAsync(long[] ids);

    /// <summary>查询日志统计；核心未就绪时返回空统计。</summary>
    Task<LogStats> GetLogsStatsAsync();

    /// <summary>查询日志来源统计；核心未就绪时返回空统计。</summary>
    Task<SourceStats> GetLogsSourceStatsAsync();

    /// <summary>清空日志数据库；核心未就绪或句柄不可用时返回 false。</summary>
    Task<bool> ClearLogsDbAsync();

    // -------------------- 统计 (Stats) --------------------

    /// <summary>查询缓存统计；核心未就绪时返回空结果。</summary>
    Task<CacheStatsResult> QueryCacheStatsAsync(long startTsMs = 0, long endTsMs = 0, int bucketSec = 10);

    /// <summary>查询网络统计；核心未就绪时返回空结果。</summary>
    Task<NetworkStatsResult> QueryNetStatsAsync(long startTsMs = 0, long endTsMs = 0, int bucketSec = 10);

    /// <summary>查询活动统计；核心未就绪时返回空结果。</summary>
    Task<ActivityStatsResult> QueryActivityStatsAsync(long startTsMs = 0, long endTsMs = 0, int bucketSec = 60);

    /// <summary>清空核心数据库；核心未就绪或句柄不可用时返回 false。</summary>
    Task<bool> ClearCoreDbAsync();

    /// <summary>清空缓存；核心未就绪或句柄不可用时返回 false。</summary>
    Task<bool> ClearCacheAsync();

    /// <summary>清空统计数据库；核心未就绪或句柄不可用时返回 false。</summary>
    Task<bool> ClearStatsDbAsync();
}
