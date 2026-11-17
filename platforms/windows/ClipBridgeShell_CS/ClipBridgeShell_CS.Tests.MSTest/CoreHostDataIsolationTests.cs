using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Services.Logging;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 核心生命周期相关的数据隔离：默认/便携/双实例都必须只使用注入的临时根，
/// 不读取真实用户数据根；清理只作用于本次测试拥有的目录。
/// </summary>
[TestClass]
public class CoreHostDataIsolationTests
{
    [TestMethod]
    public async Task Initialize_DefaultMode_KeepsCoreCacheLogTlsAndSettingsUnderTempRoot()
    {
        using var data = new TestDataDirectory(nameof(CoreHostDataIsolationTests));
        var sentinelBefore = await File.ReadAllTextAsync(data.SentinelPath);

        var settings = data.CreateSettingsService();
        var coreHost = CoreHostTestFactory.Create(
            new StubDataRootProvider(StubDataRootProvider.Default(data.Root)),
            settings);

        try
        {
            await coreHost.InitializeAsync();

            Assert.AreEqual(CoreState.Ready, coreHost.State);
            Assert.IsNull(coreHost.Diagnostics.DllLoadError);
            AssertOnlyUnderTempRoot(data, coreHost);

            // 真实 core 产物出现在临时根内；设置同样如此。
            Assert.IsTrue(File.Exists(Path.Combine(data.Root, "core.db")));
            Assert.IsTrue(File.Exists(Path.Combine(data.Root, "logs.db")));
            Assert.IsTrue(File.Exists(Path.Combine(data.Root, "LocalSettings.json")));
            // 测试宿主关闭网络栈时 TLS 按需生成；此处只验证 core 的 data_dir 已是临时根，
            // core 的 tls/ 永远从 data_dir 派生（见 cert.rs），不会落到真实 LocalAppData。
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }

        // 测试边界：兄弟 sentinel 完全没被触碰。
        Assert.AreEqual(sentinelBefore, await File.ReadAllTextAsync(data.SentinelPath));
    }

    [TestMethod]
    public async Task Initialize_PortableMode_UsesSettingsAndDataInsidePortableRoot()
    {
        using var data = new TestDataDirectory(nameof(CoreHostDataIsolationTests));
        var portableRoot = StubDataRootProvider.Portable(data.Root);
        var settings = data.CreateSettingsService(new StubDataRootProvider(portableRoot));
        var coreHost = CoreHostTestFactory.Create(
            new StubDataRootProvider(portableRoot),
            settings);

        try
        {
            await coreHost.InitializeAsync();

            Assert.AreEqual(CoreState.Ready, coreHost.State);
            AssertOnlyUnderTempRoot(data, coreHost);
            Assert.IsTrue(File.Exists(Path.Combine(data.Root, "settings.json")), "portable settings must stay in the portable root");
            Assert.IsFalse(
                File.Exists(Path.Combine(data.Root, "LocalSettings.json")),
                "portable mode must not mix in the default settings file");
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }
    }

    [TestMethod]
    public async Task TwoHostsWithDifferentRoots_DoNotShareDataDirectories()
    {
        using var first = new TestDataDirectory($"{nameof(CoreHostDataIsolationTests)}.first");
        using var second = new TestDataDirectory($"{nameof(CoreHostDataIsolationTests)}.second");

        var firstHost = CoreHostTestFactory.Create(
            new StubDataRootProvider(StubDataRootProvider.Default(first.Root)),
            first.CreateSettingsService());
        var secondHost = CoreHostTestFactory.Create(
            new StubDataRootProvider(StubDataRootProvider.Default(second.Root)),
            second.CreateSettingsService());

        try
        {
            await firstHost.InitializeAsync();
            Assert.AreEqual(CoreState.Ready, firstHost.State);
            await CoreHostTestFactory.ShutdownQuietlyAsync(firstHost);

            await secondHost.InitializeAsync();
            Assert.AreEqual(CoreState.Ready, secondHost.State);
            AssertOnlyUnderTempRoot(second, secondHost);

            Assert.IsTrue(File.Exists(Path.Combine(first.Root, "core.db")));
            Assert.IsTrue(File.Exists(Path.Combine(second.Root, "core.db")));
            Assert.AreNotEqual(firstHost.Diagnostics.CoreDataDir, secondHost.Diagnostics.CoreDataDir);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(firstHost);
            await CoreHostTestFactory.ShutdownQuietlyAsync(secondHost);
        }
    }

    [TestMethod]
    public async Task AccountChange_ClearsTlsOnlyUnderInjectedRoot()
    {
        using var data = new TestDataDirectory(nameof(CoreHostDataIsolationTests));
        var account = new StubAccountService(AccountLoadResult.Available("alice", "alice-password"));
        var coreHost = CoreHostTestFactory.Create(
            new StubDataRootProvider(StubDataRootProvider.Default(data.Root)),
            data.CreateSettingsService(),
            account);

        try
        {
            var tlsDir = data.PathUnderRoot("tls");
            Directory.CreateDirectory(tlsDir);
            var tlsMarker = Path.Combine(tlsDir, "cert.der");
            await File.WriteAllBytesAsync(tlsMarker, [0x30, 0x82]);

            await coreHost.InitializeAsync();
            Assert.AreEqual(CoreState.Ready, coreHost.State);

            // 账号切换会调用 ClearCertificateFilesAsync；它必须只删除注入根下的 tls。
            account.State = AccountLoadResult.Available("bob", "bob-password");
            await coreHost.InitializeWithAccountAsync();

            Assert.AreEqual(CoreState.Ready, coreHost.State);
            Assert.IsFalse(File.Exists(tlsMarker), "account change must clear tls under the injected root");
            AssertOnlyUnderTempRoot(data, coreHost);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }
    }

    [TestMethod]
    public void StashLogs_WriteAndClearOnlyTheInjectedRoot()
    {
        using var first = new TestDataDirectory("StashLogIsolation.first");
        using var second = new TestDataDirectory("StashLogIsolation.second");
        var firstLogs = new StashLogManager(
            new StubDataRootProvider(StubDataRootProvider.Default(first.Root)));
        var secondLogs = new StashLogManager(
            new StubDataRootProvider(StubDataRootProvider.Default(second.Root)));

        firstLogs.WriteLog(new LogEntry { Id = 1, TsUtc = 1, Message = "first synthetic log" });
        secondLogs.WriteLog(new LogEntry { Id = 2, TsUtc = 2, Message = "second synthetic log" });
        var firstPath = first.PathUnderRoot("logs", "stash.log");
        var secondPath = second.PathUnderRoot("logs", "stash.log");
        Assert.IsTrue(File.Exists(firstPath), "stash log must use the first injected root");
        Assert.IsTrue(File.Exists(secondPath), "stash log must use the second injected root");
        using var firstJson = System.Text.Json.JsonDocument.Parse(File.ReadAllText(firstPath));
        Assert.AreEqual("first synthetic log", firstJson.RootElement.GetProperty("Message").GetString());
        var secondBefore = File.ReadAllBytes(secondPath);

        firstLogs.Clear();

        Assert.IsFalse(File.Exists(firstPath));
        Assert.IsFalse(firstLogs.HasStashedLogs());
        CollectionAssert.AreEqual(secondBefore, File.ReadAllBytes(secondPath));
        Assert.IsTrue(secondLogs.HasStashedLogs());
        Assert.IsTrue(File.Exists(first.SentinelPath));
        Assert.IsTrue(File.Exists(second.SentinelPath));
    }

    private static void AssertOnlyUnderTempRoot(TestDataDirectory data, CoreHostService coreHost)
    {
        data.AssertContained(coreHost.Diagnostics.AppDataDir, "AppDataDir");
        data.AssertContained(coreHost.Diagnostics.CoreDataDir, "CoreDataDir");
        data.AssertContained(coreHost.Diagnostics.CacheDir, "CacheDir");
        data.AssertContained(coreHost.Diagnostics.LogDir, "LogDir");
    }
}
