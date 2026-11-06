using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Models;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 审计 F11：账号已配置但密码不可读时必须 fail-closed，绝不能拿空密码初始化核心。
/// </summary>
[TestClass]
public class AccountFailClosedTests
{
    private static CoreHostService CreateCoreHost(IAccountService? accountService)
    {
        var eventPump = new EventPumpService(new HistoryStore(), new PeerStore(), new TransferStore(), new ContentFetchAwaiter());
        return new CoreHostService(eventPump, new InMemoryLocalSettingsService(), null, accountService);
    }

    [TestMethod]
    public async Task Initialize_AccountConfiguredButPasswordUnavailable_FailsClosedBeforeCoreInit()
    {
        var account = new StubAccountService(AccountLoadResult.Unavailable("alice"));
        var coreHost = CreateCoreHost(account);

        await coreHost.InitializeAsync();

        Assert.AreEqual(CoreState.Degraded, coreHost.State);
        StringAssert.Contains(coreHost.LastError, "无法读取账户凭据");
        StringAssert.Contains(coreHost.LastError, "重新登录");
        Assert.AreEqual("Core degraded: account credentials unavailable.", coreHost.Diagnostics.LastInitSummary);
        Assert.IsNull(coreHost.Diagnostics.DllPath, "fail-closed must happen before DLL discovery / FFI init");
    }

    [TestMethod]
    public async Task Initialize_NoAccountConfigured_KeepsFirstRunBehaviour()
    {
        var account = new StubAccountService(AccountLoadResult.NotConfigured);
        var coreHost = CreateCoreHost(account);

        await coreHost.InitializeAsync();

        Assert.AreEqual(CoreState.NotLoaded, coreHost.State);
        Assert.AreEqual("需要登录账号", coreHost.LastError);
        Assert.IsNull(coreHost.Diagnostics.DllPath, "first run must not touch the core");
    }

    [TestMethod]
    public async Task Initialize_MissingAccountService_FailsClosed()
    {
        var coreHost = CreateCoreHost(null);

        await coreHost.InitializeAsync();

        Assert.AreEqual(CoreState.Degraded, coreHost.State);
        StringAssert.Contains(coreHost.LastError, "无法读取账户凭据");
        Assert.IsNull(coreHost.Diagnostics.DllPath, "no account service means no proof of credentials");
    }

    [TestMethod]
    public async Task Initialize_EmptyPassword_FailsClosed()
    {
        // 伪造“密码标记为可用但实际为空”的结果：确保空密码永远不会走到 cb_init。
        var account = new StubAccountService(AccountLoadResult.Available("alice", string.Empty));
        var coreHost = CreateCoreHost(account);

        await coreHost.InitializeAsync();

        Assert.AreEqual(CoreState.Degraded, coreHost.State);
        StringAssert.Contains(coreHost.LastError, "无法读取账户凭据");
        Assert.IsNull(coreHost.Diagnostics.DllPath, "empty password must never be passed to the core");
    }
}
