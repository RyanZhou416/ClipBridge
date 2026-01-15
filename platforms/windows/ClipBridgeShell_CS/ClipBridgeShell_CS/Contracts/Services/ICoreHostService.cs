using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Models.Events;

namespace ClipBridgeShell_CS.Contracts.Services;

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
    Task IngestLocalCopyAsync(ClipBridgeShell_CS.Core.Models.ClipboardSnapshot snapshot);
    IntPtr GetHandle(); // 获取核心句柄（用于日志写入等）

    // 设备管理方法
    List<PeerMetaPayload> ListPeers();
    void SetPeerPolicy(string peerId, bool? shareTo, bool? acceptFrom);
    void ClearPeerFingerprint(string peerId);
    void ClearLocalCert();
}
