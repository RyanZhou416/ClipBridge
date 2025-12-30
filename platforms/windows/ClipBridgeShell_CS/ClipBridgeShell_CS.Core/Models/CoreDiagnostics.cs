using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models;

public sealed class CoreDiagnostics
{
    public string? DllPath { get; set; }
    public string? DllLoadError { get; set; }          // DllNotFound / BadImageFormat / EntryPointNotFound / etc.
    public uint? FfiAbiMajor { get; set; }
    public uint? FfiAbiMinor { get; set; }

    public string? LastInitSummary { get; set; }       // “Init failed: ...”
    public string? LastInitEnvelopeJson { get; set; }  // 原样保留，便于发 issue

    public string? AppDataDir { get; set; }
    public string? CoreDataDir { get; set; }
    public string? CacheDir { get; set; }
    public string? LogDir { get; set; }

    public string ToClipboardText()
    {
        var sb = new StringBuilder();
        sb.AppendLine("=== ClipBridge Core Diagnostics ===");
        sb.AppendLine($"DllPath: {DllPath ?? "(null)"}");
        sb.AppendLine($"DllLoadError: {DllLoadError ?? "(null)"}");
        sb.AppendLine($"FfiAbi: {(FfiAbiMajor.HasValue ? $"{FfiAbiMajor}.{FfiAbiMinor}" : "(unknown)")}");
        sb.AppendLine($"LastInitSummary: {LastInitSummary ?? "(null)"}");
        sb.AppendLine($"AppDataDir: {AppDataDir ?? "(null)"}");
        sb.AppendLine($"CoreDataDir: {CoreDataDir ?? "(null)"}");
        sb.AppendLine($"CacheDir: {CacheDir ?? "(null)"}");
        sb.AppendLine($"LogDir: {LogDir ?? "(null)"}");
        sb.AppendLine("--- LastInitEnvelopeJson ---");
        sb.AppendLine(LastInitEnvelopeJson ?? "(null)");
        return sb.ToString();
    }
}
