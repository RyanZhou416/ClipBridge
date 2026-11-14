using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Credentials;
using ClipBridgeShell_CS.Services;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// AccountService 通过 <see cref="ICredentialStore"/> 读写密码后，必须保持原有 fail-closed 语义：
/// 凭据存储里有痕迹但读不出来 → Unavailable（而不是首次运行 NotConfigured）。
/// </summary>
[TestClass]
public class AccountServiceCredentialStoreTests
{
    private const string UsernameKey = "Account_Username";

    private static (AccountService Service, InMemoryLocalSettingsService Settings, StubCredentialStore Store) Create()
    {
        var settings = new InMemoryLocalSettingsService();
        var store = new StubCredentialStore();
        return (new AccountService(settings, store), settings, store);
    }

    [TestMethod]
    public async Task Save_WritesCredentialStoreAndUsernameSetting()
    {
        var (service, settings, store) = Create();

        await service.SaveAccountAsync("alice", "P@ssw0rd-Alice-123");

        Assert.AreEqual(("alice", "P@ssw0rd-Alice-123"), store.SavedCredential);
        Assert.AreEqual("alice", await settings.ReadSettingAsync<string>(UsernameKey));

        var state = await service.LoadAccountStateAsync();
        Assert.IsTrue(state.Configured);
        Assert.IsTrue(state.PasswordAvailable);
        Assert.AreEqual("alice", state.Username);
        Assert.AreEqual("P@ssw0rd-Alice-123", state.Password);
    }

    [TestMethod]
    public async Task Save_FailsLoudly_WhenCredentialStoreWriteFails_AndDoesNotRecordUsername()
    {
        var (service, settings, store) = Create();
        store.ThrowOnSave = true;

        await Assert.ThrowsExactlyAsync<IOException>(
            () => service.SaveAccountAsync("alice", "P@ssw0rd-Alice-123"));

        Assert.IsNull(await settings.ReadSettingAsync<string>(UsernameKey));
    }

    [TestMethod]
    public async Task Load_StoreUnavailable_WithUsernameInSettings_FailsClosed()
    {
        var (service, settings, store) = Create();
        await settings.SaveSettingAsync(UsernameKey, "alice");
        store.Result = CredentialReadResult.Unavailable("alice", "corrupt file");

        var state = await service.LoadAccountStateAsync();

        Assert.IsTrue(state.Configured);
        Assert.IsFalse(state.PasswordAvailable);
        Assert.AreEqual(AccountLoadResult.Unavailable("alice").Username, state.Username);
        Assert.IsFalse(await service.HasAccountAsync());
        Assert.IsNull(await service.LoadAccountAsync());
    }

    [TestMethod]
    public async Task Load_StoreUnavailable_WithoutUsernameInSettings_StillFailsClosed()
    {
        // 关键场景：便携模式下 settings.json 丢了，但 credentials.bin 存在且损坏/解不开。
        // 不能因为“设置里没有用户名”就当首次运行。
        var (service, _, store) = Create();
        store.Result = CredentialReadResult.Unavailable(null, "corrupt file");

        var state = await service.LoadAccountStateAsync();

        Assert.IsTrue(state.Configured);
        Assert.IsFalse(state.PasswordAvailable);
        Assert.IsFalse(await service.HasAccountAsync());
    }

    [TestMethod]
    public async Task Load_NothingConfigured_ReturnsNotConfigured()
    {
        var (service, _, _) = Create();

        var state = await service.LoadAccountStateAsync();

        Assert.AreSame(AccountLoadResult.NotConfigured, state);
        Assert.IsFalse(await service.HasAccountAsync());
    }

    [TestMethod]
    public async Task Load_StoreThrows_TreatedAsFailClosed()
    {
        // 凭据存储自身抛异常时也不能回退成“未配置”。
        var serviceWithThrowingStore = new AccountService(
            new InMemoryLocalSettingsService(),
            new ThrowingCredentialStore());

        var state = await serviceWithThrowingStore.LoadAccountStateAsync();

        Assert.IsTrue(state.Configured);
        Assert.IsFalse(state.PasswordAvailable);
        Assert.IsFalse(await serviceWithThrowingStore.HasAccountAsync());
    }

    [TestMethod]
    public async Task Clear_ClearsStoreAndUsernameSetting()
    {
        var (service, settings, store) = Create();
        await service.SaveAccountAsync("alice", "P@ssw0rd-Alice-123");

        await service.ClearAccountAsync();

        Assert.AreEqual(1, store.ClearCount);
        Assert.IsNull(await settings.ReadSettingAsync<string>(UsernameKey));
        Assert.IsFalse((await service.LoadAccountStateAsync()).Configured);
    }

    [TestMethod]
    public async Task Clear_WhenStoreDeleteFails_StillClearsUsernameAndDoesNotThrow()
    {
        var (service, settings, store) = Create();
        await service.SaveAccountAsync("alice", "P@ssw0rd-Alice-123");
        store.ThrowOnClear = true;

        await service.ClearAccountAsync();

        Assert.IsNull(await settings.ReadSettingAsync<string>(UsernameKey));
    }

    private sealed class ThrowingCredentialStore : ICredentialStore
    {
        public string Kind => "throwing";

        public Task<CredentialReadResult> ReadAsync()
            => throw new InvalidOperationException("simulated store failure");

        public Task SaveAsync(string username, string password) => throw new InvalidOperationException();

        public Task ClearAsync() => throw new InvalidOperationException();
    }
}
