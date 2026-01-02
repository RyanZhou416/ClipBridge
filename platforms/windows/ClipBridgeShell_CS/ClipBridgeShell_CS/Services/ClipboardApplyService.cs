using System.Text;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models.Events;

namespace ClipBridgeShell_CS.Services;

public sealed class ClipboardApplyService
{
    private readonly CoreHostService _core;
    private readonly ContentFetchAwaiter _awaiter;
    private readonly IClipboardService _clipboard;

    public ClipboardApplyService(CoreHostService core, ContentFetchAwaiter awaiter, IClipboardService clipboard)
    {
        _core = core;
        _awaiter = awaiter;
        _clipboard = clipboard;
    }

    public async Task ApplyMetaToSystemClipboardAsync(ItemMetaPayload meta, CancellationToken ct = default)
    {
        System.Diagnostics.Debug.WriteLine($"[Apply] start item={meta.ItemId} kind={meta.Kind} mime={meta.Content?.Mime} bytes={meta.Content?.TotalBytes}");

        string transferId;
        try
        {
            transferId = await _core.EnsureContentCachedAsync(meta.ItemId, fileId: null);
            System.Diagnostics.Debug.WriteLine($"[Apply] ensure_content_cached -> transfer_id={transferId}");
        } catch (Exception ex)
        {
            // TODO: 用 InfoBar / ContentDialog 提示“源设备离线，无法取回内容”

            System.Diagnostics.Debug.WriteLine($"[Apply] ensure_content_cached FAILED: {ex}");
            return;
        }

        System.Diagnostics.Debug.WriteLine($"[Apply] waiting CONTENT_CACHED for transfer_id={transferId}");
        LocalContentRef local;
        try
        {
            local = await _awaiter.WaitAsync(transferId, ct);
            System.Diagnostics.Debug.WriteLine($"[Apply] CONTENT_CACHED received transfer_id={transferId} local_path={local.LocalPath ?? "<null>"} text_len={(local.TextUtf8?.Length ?? -1)} mime={local.Mime ?? "<null>"}");
        } catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[Apply] WaitAsync FAILED: {ex}");
            throw;
        }

        // 先只做 text，避免 kind 误判
        if (!string.Equals(meta.Kind, "text", StringComparison.OrdinalIgnoreCase))
        {
            System.Diagnostics.Debug.WriteLine($"[Apply] Not text yet: meta.kind={meta.Kind}. STOP.");
            return;
        }

        string text;
        if (!string.IsNullOrEmpty(local.TextUtf8))
        {
            text = local.TextUtf8!;
            System.Diagnostics.Debug.WriteLine($"[Apply] using text_utf8 length={text.Length}");
        }
        else if (!string.IsNullOrEmpty(local.LocalPath))
        {
            System.Diagnostics.Debug.WriteLine($"[Apply] reading text from file {local.LocalPath}");
            text = await File.ReadAllTextAsync(local.LocalPath!, System.Text.Encoding.UTF8, ct);
            System.Diagnostics.Debug.WriteLine($"[Apply] file read length={text.Length}");
        }
        else
        {
            System.Diagnostics.Debug.WriteLine("[Apply] ERROR: no text_utf8 and no local_path");
            throw new Exception("CONTENT_CACHED returned neither text_utf8 nor local_path");
        }

        System.Diagnostics.Debug.WriteLine("[Apply] writing to system clipboard...");
        await _clipboard.SetTextAsync(text);
        System.Diagnostics.Debug.WriteLine("[Apply] clipboard write done.");
    }
}
