using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models;

public sealed class CoreInitResult
{
    public bool Ok { get; init; }
    public string? ErrorCode { get; init; }     // 统一映射后的“逻辑码”（Shell 用）
    public string? Message { get; init; }
    public string? EnvelopeJson { get; init; }
}
