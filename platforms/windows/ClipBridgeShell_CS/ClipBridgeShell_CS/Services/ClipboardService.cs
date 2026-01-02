using System.Security.Cryptography;
using System.Text;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using Windows.ApplicationModel.DataTransfer;
using System.Security.Cryptography;

namespace ClipBridgeShell_CS.Services;

public sealed class ClipboardService : IClipboardService
{
    public event EventHandler? ContentChanged;
    private readonly ICoreHostService _coreHostService;
    // 记录最后一次由本应用写入内容的指纹
    public string? LastWriteFingerprint { get; private set; }

    public ClipboardService(ICoreHostService coreHostService)
    {
        // 订阅系统剪贴板事件并转发
        Clipboard.ContentChanged += OnClipboardContentChanged;
        _coreHostService = coreHostService;
    }
    /// <summary>
    /// 系统剪贴板内容变化回调
    /// </summary>
    private async void OnClipboardContentChanged(object? sender, object e)
    {
        try
        {
            // 1. 获取最新快照
            var snapshot = await GetSnapshotAsync();
            if (snapshot == null)
                return;

            // 2. 防循环检查：如果指纹和最后一次写入的一致，说明是自己写的，忽略
            if (!string.IsNullOrEmpty(LastWriteFingerprint) && snapshot.Fingerprint == LastWriteFingerprint)
            {
                System.Diagnostics.Debug.WriteLine("[Watcher] Ignored self-copy.");
                return;
            }

            // 3. 调用 CoreHostService 写入数据库
            await _coreHostService.IngestLocalCopyAsync(snapshot);

            // 4. 触发内部事件（如果有其他 UI 监听）
            ContentChanged?.Invoke(this, EventArgs.Empty);
        } catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[Watcher] Process clipboard change failed: {ex.Message}");
        }
    }

    public async Task<bool> SetTextAsync(string text)
    {
        try
        {
            var dp = new DataPackage();
            dp.SetText(text);
            Clipboard.SetContent(dp);
            Clipboard.Flush();
            LastWriteFingerprint = ComputeHash(text);
            return await Task.FromResult(true);
        } catch (Exception ex)
        {
            // 记录日志或忽略（剪贴板占用是常见错误）
            System.Diagnostics.Debug.WriteLine($"Clipboard SetText failed: {ex.Message}");
            return false;
        }
    }

    public async Task<string?> GetTextAsync()
    {
        var data = Clipboard.GetContent();
        if (data.Contains(StandardDataFormats.Text))
        {
            return await data.GetTextAsync();
        }
        return null;
    }

    public async Task<ClipboardSnapshot?> GetSnapshotAsync()
    {
        try
        {
            // 获取当前剪贴板视图
            var data = Clipboard.GetContent();

            // 1. 处理文本 (v1 优先支持)
            if (data.Contains(StandardDataFormats.Text))
            {
                var text = await data.GetTextAsync();
                if (string.IsNullOrEmpty(text))
                    return null;

                var ts = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
                return new ClipboardSnapshot
                {
                    MimeType = "text/plain",
                    Data = text,
                    // 简单的预览：取前 50 个字符，移除换行
                    PreviewText = text.Length > 50
                        ? text.Substring(0, 50).Replace("\r", " ").Replace("\n", " ") + "..."
                        : text.Replace("\r", " ").Replace("\n", " "),
                    Timestamp = ts,
                    Fingerprint = ComputeHash(text)
                };
            }

            // TODO (M5.X): 处理 Bitmap 和 StorageItems (Files)

            return null;
        } catch (Exception ex)
        {
            // 剪贴板访问极易因其他程序占用而抛出异常，此时返回 null 即可，不要崩溃
            System.Diagnostics.Debug.WriteLine($"Clipboard GetSnapshot failed: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// 辅助：计算简单的 MD5 指纹
    /// </summary>
    /// <param name="input"></param>
    /// <returns></returns>
    private static string ComputeHash(string input)
    {
        if (string.IsNullOrEmpty(input))
            return string.Empty;
        var bytes = Encoding.UTF8.GetBytes(input);
        var hashBytes = MD5.HashData(bytes);
        return Convert.ToHexString(hashBytes);
    }
}
