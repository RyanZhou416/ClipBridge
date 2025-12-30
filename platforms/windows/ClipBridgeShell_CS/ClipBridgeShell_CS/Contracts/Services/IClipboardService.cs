using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Contracts.Services;

public interface IClipboardService
{
    Task<bool> SetTextAsync(string text);
    Task<string?> GetTextAsync();
}
