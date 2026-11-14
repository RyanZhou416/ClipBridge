using System.Diagnostics;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Credentials;

namespace ClipBridgeShell_CS.Services;

/// <summary>
/// 账号服务：用户名快速检查走 <see cref="ILocalSettingsService"/>，密码走
/// <see cref="ICredentialStore"/>（默认 PasswordVault；便携模式为 DPAPI 文件）。
/// </summary>
public class AccountService : IAccountService
{
    private const string USERNAME_KEY = "Account_Username";

    private readonly ILocalSettingsService _localSettings;
    private readonly ICredentialStore _credentialStore;

    public event EventHandler<bool>? AccountStatusChanged;

    public AccountService(ILocalSettingsService localSettings, ICredentialStore credentialStore)
    {
        _localSettings = localSettings;
        _credentialStore = credentialStore;
    }

    public async Task SaveAccountAsync(string username, string password)
    {
        // 先写凭据：写失败（例如便携目录突然不可写）必须向上抛，不能先留下“已配置用户名”。
        await _credentialStore.SaveAsync(username, password);

        // 保存账号名到 LocalSettings（用于快速检查；便携模式下该文件在 <dataRoot>\settings.json）。
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
    /// 读取账号状态（审计 F11）：本地设置里有账号名、但凭据存储读不出密码时，
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

        CredentialReadResult stored;
        try
        {
            stored = await _credentialStore.ReadAsync();
        }
        catch (Exception ex)
        {
            // 存储自身异常也必须 fail-closed（不能证明没有账号）。
            Debug.WriteLine($"[AccountService] Credential store '{_credentialStore.Kind}' read failed: {ex.GetType().Name}");
            stored = CredentialReadResult.Unavailable(null, "Credential store read failed.");
        }

        if (stored.PasswordAvailable && !string.IsNullOrWhiteSpace(stored.Password))
        {
            // 成功路径沿用旧行为：优先使用凭据存储里的用户名（即登录时输入的值）。
            var username = string.IsNullOrWhiteSpace(stored.Username) ? configuredUsername : stored.Username;
            if (!string.IsNullOrWhiteSpace(username))
            {
                return AccountLoadResult.Available(username, stored.Password);
            }
        }

        // 本地设置或凭据存储任一处仍有账号痕迹：不能当作“未配置”，必须 fail-closed。
        // 损坏的 credentials.bin 也属于“有痕迹但不可用”，绝不静默当成首次运行。
        if (configuredUsername != null || stored.Found)
        {
            return AccountLoadResult.Unavailable(configuredUsername ?? stored.Username);
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
        try
        {
            await _credentialStore.ClearAsync();
        }
        catch (Exception ex)
        {
            // 与旧实现一致：清理失败不阻止退出登录；凭据若残留，下次读取会 fail-closed 提示重新登录。
            Debug.WriteLine($"[AccountService] Credential store '{_credentialStore.Kind}' clear failed: {ex.GetType().Name}");
        }

        await _localSettings.SaveSettingAsync<string>(USERNAME_KEY, null!);

        // 触发状态变化事件
        AccountStatusChanged?.Invoke(this, false);
    }
}
