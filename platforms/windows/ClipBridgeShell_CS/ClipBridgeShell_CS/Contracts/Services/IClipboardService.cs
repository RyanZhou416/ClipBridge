using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;

namespace ClipBridgeShell_CS.Contracts.Services;

public interface IClipboardService
{
    /// <summary>
    /// 剪贴板内容变化。事件携带变化发生时的快照：
    /// 由 <see cref="ClipboardService"/> 在“写回锁定内容”之前捕获，
    /// 因此订阅者拿到的是用户真正复制的那份内容，而不是回填后的锁定内容。
    /// 这也让订阅者不必再次读取系统剪贴板（省掉一次 GetContent + 全文哈希）。
    /// </summary>
    event EventHandler<ClipboardSnapshot> ContentChanged;
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
