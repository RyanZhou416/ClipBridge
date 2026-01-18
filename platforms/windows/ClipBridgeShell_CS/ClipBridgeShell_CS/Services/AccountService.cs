using ClipBridgeShell_CS.Contracts.Services;
using Windows.Security.Credentials;

namespace ClipBridgeShell_CS.Services;

public class AccountService : IAccountService
{
    private const string VAULT_RESOURCE = "ClipBridge";
    private const string USERNAME_KEY = "Account_Username";

    private readonly ILocalSettingsService _localSettings;

    public event EventHandler<bool>? AccountStatusChanged;

    public AccountService(ILocalSettingsService localSettings)
    {
        _localSettings = localSettings;
    }

    public async Task SaveAccountAsync(string username, string password)
    {
        var vault = new PasswordVault();

        // 先删除旧的（如果存在）
        try
        {
            var oldCreds = vault.FindAllByResource(VAULT_RESOURCE);
            foreach (var oldCred in oldCreds)
            {
                vault.Remove(oldCred);
            }
        }
        catch { }

        // 保存新的
        var newCred = new PasswordCredential(VAULT_RESOURCE, username, password);
        vault.Add(newCred);

        // 保存账号名到LocalSettings（用于快速检查）
        await _localSettings.SaveSettingAsync(USERNAME_KEY, username);

        // 触发状态变化事件
        AccountStatusChanged?.Invoke(this, true);
    }

    public Task<(string username, string password)?> LoadAccountAsync()
    {
        var vault = new PasswordVault();
        try
        {
            var creds = vault.FindAllByResource(VAULT_RESOURCE);
            if (creds.Count > 0)
            {
                var cred = creds[0];
                cred.RetrievePassword(); // 解密密码
                return Task.FromResult<(string username, string password)?>((cred.UserName, cred.Password));
            }
        }
        catch { }
        return Task.FromResult<(string username, string password)?>(null);
    }

    public async Task<bool> HasAccountAsync()
    {
        var username = await _localSettings.ReadSettingAsync<string>(USERNAME_KEY);
        return !string.IsNullOrEmpty(username);
    }

    public async Task ClearAccountAsync()
    {
        var vault = new PasswordVault();
        try
        {
            var creds = vault.FindAllByResource(VAULT_RESOURCE);
            foreach (var cred in creds)
            {
                vault.Remove(cred);
            }
        }
        catch { }

        await _localSettings.SaveSettingAsync<string>(USERNAME_KEY, null);

        // 触发状态变化事件
        AccountStatusChanged?.Invoke(this, false);
    }
}
