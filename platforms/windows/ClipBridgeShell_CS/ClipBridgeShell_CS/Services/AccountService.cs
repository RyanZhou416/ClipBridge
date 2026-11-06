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

    public async Task<(string username, string password)?> LoadAccountAsync()
    {
        var state = await LoadAccountStateAsync();
        return state.Configured
            && state.PasswordAvailable
            && !string.IsNullOrWhiteSpace(state.Username)
            && !string.IsNullOrWhiteSpace(state.Password)
            ? (state.Username, state.Password)
            : null;
    }

    /// <summary>
    /// 读取账号状态（审计 F11）：本地设置里有账号名、但 PasswordVault 读不出密码时，
    /// 返回 <see cref="AccountLoadResult.Unavailable"/>，由调用方 fail-closed；
    /// 只有用户名和密码都不存在时才返回 <see cref="AccountLoadResult.NotConfigured"/>（首次运行）。
    /// </summary>
    public async Task<AccountLoadResult> LoadAccountStateAsync()
    {
        string? settingsUsername;
        try
        {
            settingsUsername = await _localSettings.ReadSettingAsync<string>(USERNAME_KEY);
        }
        catch (Exception)
        {
            // 本地设置都读不出来时无法证明是首次运行，按“已配置但不可用”处理（fail-closed）。
            return AccountLoadResult.Unavailable(null);
        }

        var configuredUsername = string.IsNullOrWhiteSpace(settingsUsername) ? null : settingsUsername;

        var vaultCredentialFound = false;
        string? vaultUsername = null;
        string? vaultPassword = null;
        try
        {
            var vault = new PasswordVault();
            var creds = vault.FindAllByResource(VAULT_RESOURCE);
            if (creds.Count > 0)
            {
                // 凭据存在但 RetrievePassword 失败（被清空/迁移/Windows Hello 失败）也必须算“已配置”。
                vaultCredentialFound = true;
                var cred = creds[0];
                cred.RetrievePassword(); // 解密密码
                vaultUsername = string.IsNullOrWhiteSpace(cred.UserName) ? null : cred.UserName;
                vaultPassword = string.IsNullOrWhiteSpace(cred.Password) ? null : cred.Password;
            }
        }
        catch
        {
            // 凭据缺失/解密失败：保持 vaultPassword == null，由下面的分支 fail-closed。
        }

        if (!string.IsNullOrWhiteSpace(vaultPassword))
        {
            // 成功路径沿用旧行为：优先使用 PasswordVault 里的用户名（即登录时输入的值）。
            var username = vaultUsername ?? configuredUsername;
            if (!string.IsNullOrWhiteSpace(username))
            {
                return AccountLoadResult.Available(username, vaultPassword);
            }
        }

        // 本地设置或 PasswordVault 任一处仍有账号痕迹：不能当作“未配置”，必须 fail-closed。
        if (configuredUsername != null || vaultCredentialFound)
        {
            return AccountLoadResult.Unavailable(configuredUsername ?? vaultUsername);
        }

        return AccountLoadResult.NotConfigured;
    }

    public async Task<bool> HasAccountAsync()
    {
        // 必须同时具备用户名与“可读且非空”的密码；账号名存在但密码读不出来不算有账号，
        // 这样 UI 会走重新登录流程，而不是拿空密码去初始化核心。
        var state = await LoadAccountStateAsync();
        return state.Configured
            && state.PasswordAvailable
            && !string.IsNullOrWhiteSpace(state.Username);
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

        await _localSettings.SaveSettingAsync<string>(USERNAME_KEY, null!);

        // 触发状态变化事件
        AccountStatusChanged?.Invoke(this, false);
    }
}
