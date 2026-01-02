using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;

namespace ClipBridgeShell_CS.Contracts.Services;

public interface IClipboardService
{
    // 暴露系统剪贴板变更事件
    event EventHandler ContentChanged;
    string? LastWriteFingerprint { get; }

    Task<bool> SetTextAsync(string text);
    Task<string?> GetTextAsync();
    // 获取当前剪贴板的标准化快照
    Task<ClipboardSnapshot?> GetSnapshotAsync();
    Task SetImageFromPathAsync(string path);
    Task SetFilesFromPathsAsync(IReadOnlyList<string> paths);
}
