using Microsoft.VisualStudio.TestTools.UnitTesting;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Contracts.Services;
using System;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Tests.MSTest.Services;

[TestClass]
public class IngestPolicyTests
{
    // 1. 定义一个简单的伪造服务，用于控制测试环境
    private class FakeClipboardService : IClipboardService
    {
        public event EventHandler ContentChanged;
        public string? LastWriteFingerprint
        {
            get; set;
        }

        public Task<ClipboardSnapshot?> GetSnapshotAsync() => Task.FromResult<ClipboardSnapshot?>(null);
        public Task<bool> SetTextAsync(string text) => Task.FromResult(true);
        public Task<string?> GetTextAsync() => Task.FromResult<string?>(null);
        public Task SetImageFromPathAsync(string path) => Task.CompletedTask;
        public Task SetFilesFromPathsAsync(IReadOnlyList<string> paths) => Task.CompletedTask;
    }

    private FakeClipboardService _fakeService = null!;
    private IngestPolicy _policy = null!;

    [TestInitialize]
    public void Setup()
    {
        _fakeService = new FakeClipboardService();
        _policy = new IngestPolicy(_fakeService);
    }

    [TestMethod]
    public void Decide_ShouldDeny_WhenDataIsEmpty()
    {
        // Arrange
        var snapshot = new ClipboardSnapshot { Data = "", Fingerprint = "abc" };

        // Act
        var result = _policy.Decide(snapshot);

        // Assert
        Assert.AreEqual(IngestDecisionType.Deny, result.Type);
        Assert.AreEqual("EmptyData", result.Reason);
    }

    [TestMethod]
    public void Decide_ShouldAllow_WhenNewContent()
    {
        // Arrange
        var snapshot = new ClipboardSnapshot { Data = "Hello", Fingerprint = "hash_hello" };

        // Act
        var result = _policy.Decide(snapshot);

        // Assert
        Assert.AreEqual(IngestDecisionType.Allow, result.Type);
    }

    [TestMethod]
    public void Decide_ShouldDeny_WhenSelfWriteback()
    {
        // Arrange
        // 模拟：这个指纹是 Shell 刚刚写入剪贴板的
        _fakeService.LastWriteFingerprint = "hash_self_write";

        var snapshot = new ClipboardSnapshot
        {
            Data = "FromShell",
            Fingerprint = "hash_self_write" // 指纹匹配
        };

        // Act
        var result = _policy.Decide(snapshot);

        // Assert
        Assert.AreEqual(IngestDecisionType.Deny, result.Type);
        Assert.AreEqual("SelfWriteback", result.Reason);
    }

    [TestMethod]
    public void Decide_ShouldAllow_WhenFingerprintDifferentFromLastWrite()
    {
        // Arrange
        _fakeService.LastWriteFingerprint = "hash_old_write";

        var snapshot = new ClipboardSnapshot
        {
            Data = "FromUser",
            Fingerprint = "hash_user_copy" // 指纹不匹配
        };

        // Act
        var result = _policy.Decide(snapshot);

        // Assert
        Assert.AreEqual(IngestDecisionType.Allow, result.Type);
    }

    [TestMethod]
    public void Decide_ShouldDeny_WhenDuplicate_Consecutive()
    {
        // Arrange
        var snapshot1 = new ClipboardSnapshot { Data = "Same", Fingerprint = "hash_same" };
        var snapshot2 = new ClipboardSnapshot { Data = "Same", Fingerprint = "hash_same" };

        // Act 1: 第一次采集
        var result1 = _policy.Decide(snapshot1);
        // 重要：模拟 Watcher 调用成功回调，更新内部状态
        if (result1.Type == IngestDecisionType.Allow)
        {
            _policy.OnIngestSuccess(snapshot1);
        }

        // Act 2: 第二次采集相同内容
        var result2 = _policy.Decide(snapshot2);

        // Assert
        Assert.AreEqual(IngestDecisionType.Allow, result1.Type, "First time should allow");
        Assert.AreEqual(IngestDecisionType.Deny, result2.Type, "Second time should deny as duplicate");
        Assert.AreEqual("Duplicate", result2.Reason);
    }

    [TestMethod]
    public void Decide_ShouldAllow_WhenNewContent_AfterDuplicate()
    {
        // Arrange
        // 关键修正：必须设置 Data，否则会被策略的第一步 "EmptyData" 拦截并拒绝
        var s1 = new ClipboardSnapshot { Data = "Content A", Fingerprint = "A" };
        var s2 = new ClipboardSnapshot { Data = "Content B", Fingerprint = "B" };

        // Act
        _policy.Decide(s1);          // 决策 A (虽然这里返回值没用，但为了模拟真实流程)
        _policy.OnIngestSuccess(s1); // 记录 A 已经入库

        var result = _policy.Decide(s2); // 来了 B

        // Assert
        Assert.AreEqual(IngestDecisionType.Allow, result.Type);
    }
}
