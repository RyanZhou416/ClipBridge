using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;

namespace ClipBridgeShell_CS.Contracts.Services;

public interface IClipboardService
{
    event EventHandler ContentChanged;
    string? LastWriteFingerprint { get; }

    Task<bool> SetTextAsync(string text);
    Task<string?> GetTextAsync();
    Task<ClipboardSnapshot?> GetSnapshotAsync();
    Task SetImageFromPathAsync(string path);
    Task SetFilesFromPathsAsync(IReadOnlyList<string> paths);

    bool IsClipboardLocked { get; }
    string? LockedItemId { get; }
    void LockClipboard(string text, string itemId);
    void UnlockClipboard();
    event EventHandler<bool>? ClipboardLockChanged;
}
