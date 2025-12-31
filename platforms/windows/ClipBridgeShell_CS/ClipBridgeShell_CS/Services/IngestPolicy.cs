using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;

namespace ClipBridgeShell_CS.Services;

public enum IngestDecisionType
{
    Allow,
    Deny
}

public class IngestDecision
{
    public IngestDecisionType Type
    {
        get; set;
    }
    public string Reason { get; set; } = string.Empty;
}

public class IngestPolicy
{
    private readonly IClipboardService _clipboardService;
    private string? _lastIngestedFingerprint; // 用于连续去重
    private const int DedupWindowMs = 1000;   // 去重时间窗口 (v1 简单实现：只要指纹变了就算新，或者强制去重)

    public IngestPolicy(IClipboardService clipboardService)
    {
        _clipboardService = clipboardService;
    }

    public IngestDecision Decide(ClipboardSnapshot snapshot)
    {
        // 1. 空数据检查
        if (string.IsNullOrEmpty(snapshot.Data))
        {
            return new IngestDecision { Type = IngestDecisionType.Deny, Reason = "EmptyData" };
        }

        // 2. 回环防护：检查是否是我们自己刚才写入的
        // 如果当前快照指纹 == 最后一次写入的指纹，说明是 Self-Writeback
        if (!string.IsNullOrEmpty(snapshot.Fingerprint) &&
            snapshot.Fingerprint == _clipboardService.LastWriteFingerprint)
        {
            return new IngestDecision { Type = IngestDecisionType.Deny, Reason = "SelfWriteback" };
        }

        // 3. 连续去重：检查是否和上一次采集的一样
        if (snapshot.Fingerprint == _lastIngestedFingerprint)
        {
            return new IngestDecision { Type = IngestDecisionType.Deny, Reason = "Duplicate" };
        }

        // TODO (M5.X): 在这里检查 limits (大小限制)

        return new IngestDecision { Type = IngestDecisionType.Allow, Reason = "Pass" };
    }

    // 当快照成功 Ingest 后调用，更新去重状态
    public void OnIngestSuccess(ClipboardSnapshot snapshot)
    {
        _lastIngestedFingerprint = snapshot.Fingerprint;
    }
}
