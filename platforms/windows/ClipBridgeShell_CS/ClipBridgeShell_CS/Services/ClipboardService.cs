using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Contracts.Services;
using Windows.ApplicationModel.DataTransfer;

namespace ClipBridgeShell_CS.Services;

public sealed class ClipboardService : IClipboardService
{
    public Task<bool> SetTextAsync(string text)
    {
        var dp = new DataPackage();
        dp.SetText(text);
        Clipboard.SetContent(dp);
        Clipboard.Flush();
        return Task.FromResult(true);
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
}
