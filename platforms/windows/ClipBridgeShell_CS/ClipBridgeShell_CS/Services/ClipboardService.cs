using System.Security.Cryptography;
using System.Text;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Streams;

namespace ClipBridgeShell_CS.Services;

public sealed class ClipboardService : IClipboardService
{
    public event EventHandler? ContentChanged;
    public event EventHandler<bool>? ClipboardLockChanged;
    private readonly ICoreHostService _coreHostService;
    public string? LastWriteFingerprint { get; private set; }

    private string? _lockedText;
    private bool _isRestoringLock;
    public bool IsClipboardLocked => _lockedText != null;
    public string? LockedItemId { get; private set; }

    public ClipboardService(ICoreHostService coreHostService)
    {
        Clipboard.ContentChanged += OnClipboardContentChanged;
        _coreHostService = coreHostService;
    }

    public void LockClipboard(string text, string itemId)
    {
        _lockedText = text;
        LockedItemId = itemId;
        ClipboardLockChanged?.Invoke(this, true);
    }

    public void UnlockClipboard()
    {
        _lockedText = null;
        LockedItemId = null;
        ClipboardLockChanged?.Invoke(this, false);
    }

    private async void OnClipboardContentChanged(object? sender, object e)
    {
        try
        {
            if (_isRestoringLock) return;

            var snapshot = await GetSnapshotAsync();
            if (snapshot == null) return;

            if (!string.IsNullOrEmpty(LastWriteFingerprint) && snapshot.Fingerprint == LastWriteFingerprint)
            {
                System.Diagnostics.Debug.WriteLine("[Watcher] Ignored self-copy.");
                return;
            }

            // Ingest to core regardless of lock state (record & share new content)
            await _coreHostService.IngestLocalCopyAsync(snapshot);
            ContentChanged?.Invoke(this, EventArgs.Empty);

            // If locked, restore locked content after ingestion
            if (_lockedText != null)
            {
                var lockedFingerprint = ComputeHash(_lockedText);
                if (snapshot.Fingerprint != lockedFingerprint)
                {
                    _isRestoringLock = true;
                    try
                    {
                        await SetTextAsync(_lockedText);
                    }
                    finally
                    {
                        _isRestoringLock = false;
                    }
                }
            }
        } catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[Watcher] Process clipboard change failed: {ex.Message}");
        }
    }

    public async Task<bool> SetTextAsync(string text)
    {
        LastWriteFingerprint = ComputeHash(text);
        // 最多重试 5 次
        const int MaxRetries = 10;
        // 每次等待 100ms
        const int DelayMs = 100;

        for (int i = 0; i < MaxRetries; i++)
        {
            try
            {
                var dp = new DataPackage();
                dp.SetText(text);

                // 建议：显式指定要在 UI 线程操作（虽然 DataPackage 不强制，但 Flush 涉及系统状态）
                // 如果你已经在 UI 线程，这行不是必须的，但加上更保险
                Clipboard.SetContent(dp);
                Clipboard.Flush();
                return true; // 成功则退出
            } catch (System.Runtime.InteropServices.COMException ex)
            {
                // 0x800401D0 (CLIPBRD_E_CANT_OPEN) 是剪贴板被占用
                // 检查是否是最后一次重试
                if (i == MaxRetries - 1 || (uint)ex.HResult != 0x800401D0)
                {
                    System.Diagnostics.Debug.WriteLine($"[Clipboard] Fatal Error after retries: {ex.Message}");
                    // 这里可以选择 throw，也可以 return false，取决于你是否想让上层知道
                    throw;
                }

                // 剪贴板忙，稍等后重试
                if (i == MaxRetries)
                {
                    System.Diagnostics.Debug.WriteLine($"[Clipboard] Locked (0x800401D0), retried to max:{MaxRetries}...");
                }
                await Task.Delay(DelayMs);
            } catch (Exception ex)
            {
                // 其他异常直接抛出
                System.Diagnostics.Debug.WriteLine($"[Clipboard] EXCEPTION: {ex}");
                throw;
            }
        }
        return false;
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

    public async Task SetImageFromPathAsync(string path)
    {
        var file = await StorageFile.GetFileFromPathAsync(path);
        var dp = new DataPackage();
        dp.SetBitmap(RandomAccessStreamReference.CreateFromFile(file));
        Clipboard.SetContent(dp);
        Clipboard.Flush();
    }

    public async Task SetFilesFromPathsAsync(IReadOnlyList<string> paths)
    {
        var items = new List<IStorageItem>(paths.Count);
        foreach (var p in paths)
            items.Add(await StorageFile.GetFileFromPathAsync(p));

        var dp = new DataPackage();
        dp.SetStorageItems(items);
        Clipboard.SetContent(dp);
        Clipboard.Flush();
    }
}
