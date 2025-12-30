using System;
using System.Threading;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;

namespace ClipBridgeShell_CS.Core.Contracts.Services;

public interface ICoreHostService
{
    CoreState State { get; }
    string? LastError { get; }
    CoreDiagnostics Diagnostics { get; }

    event Action<CoreState>? StateChanged;

    Task InitializeAsync(CancellationToken ct = default);
    Task ShutdownAsync(CancellationToken ct = default);

    string GetDiagnosticsText();
}
