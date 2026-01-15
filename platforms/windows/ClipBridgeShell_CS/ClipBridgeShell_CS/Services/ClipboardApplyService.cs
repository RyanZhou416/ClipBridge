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
        string transferId;
        try
        {
            transferId = await _core.EnsureContentCachedAsync(meta.ItemId, fileId: null);
        } catch (Exception ex)
        {
            // TODO: 用 InfoBar / ContentDialog 提示"源设备离线，无法取回内容"
            return;
        }

        LocalContentRef local;
        try
        {
            local = await _awaiter.WaitAsync(transferId, ct);
        } catch (Exception ex)
        {
            throw;
        }

        // 先只做 text，避免 kind 误判
        if (!string.Equals(meta.Kind, "text", StringComparison.OrdinalIgnoreCase))
        {
            return;
        }

        string text;
        if (!string.IsNullOrEmpty(local.TextUtf8))
        {
            text = local.TextUtf8!;
        }
        else if (!string.IsNullOrEmpty(local.LocalPath))
        {
            text = await File.ReadAllTextAsync(local.LocalPath!, System.Text.Encoding.UTF8, ct);
        }
        else
        {
            throw new Exception("CONTENT_CACHED returned neither text_utf8 nor local_path");
        }

        await _clipboard.SetTextAsync(text);
    }
}
