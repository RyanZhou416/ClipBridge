namespace ClipBridgeShell_CS.Contracts.Services;

/// <summary>
/// 账号凭据读取结果。
/// </summary>
/// <remarks>
/// 必须把“未配置账号”（首次运行）与“已配置但密码读不出来”（PasswordVault 被清空/
/// 迁移失败/Windows Hello 失败）区分开：前者是正常的首次运行，后者必须 fail-closed，
/// 绝不能拿空密码去初始化核心（否则 core 会注册一个空密码即可认证的 OPAQUE 账号）。
/// </remarks>
public sealed class AccountLoadResult
{
    private AccountLoadResult(bool configured, bool passwordAvailable, string? username, string? password)
    {
        Configured = configured;
        PasswordAvailable = passwordAvailable;
        Username = username;
        Password = password;
    }

    /// <summary>
    /// 账号是否已配置：本地设置里记录了用户名，或 PasswordVault 中存在凭据。
    /// </summary>
    public bool Configured
    {
        get;
    }

    /// <summary>
    /// 密码是否成功读取且非空。只有此值为 true 时才允许把凭据交给核心。
    /// </summary>
    public bool PasswordAvailable
    {
        get;
    }

    public string? Username
    {
        get;
    }

    public string? Password
    {
        get;
    }

    /// <summary>完全未配置账号（首次运行）：本地设置与 PasswordVault 都没有账号痕迹。</summary>
    public static AccountLoadResult NotConfigured { get; } = new(false, false, null, null);

    /// <summary>账号已配置，但密码不可读（缺失、读取失败或为空）。</summary>
    public static AccountLoadResult Unavailable(string? username) => new(true, false, username, null);

    /// <summary>账号已配置且凭据可读。</summary>
    public static AccountLoadResult Available(string username, string password) => new(true, true, username, password);
}

public interface IAccountService
{
    /// <summary>
    /// 保存账号和密码
    /// </summary>
    Task SaveAccountAsync(string username, string password);

    /// <summary>
    /// 从PasswordVault读取账号和密码
    /// </summary>
    Task<(string username, string password)?> LoadAccountAsync();

    /// <summary>
    /// 读取账号状态，区分“未配置账号”与“已配置但凭据不可读”。
    /// 核心初始化必须使用本方法；凭据不可读时不得回退到空密码。
    /// </summary>
    Task<AccountLoadResult> LoadAccountStateAsync();

    /// <summary>
    /// 检查是否存在可用的保存账号（用户名与可读密码都存在）
    /// </summary>
    Task<bool> HasAccountAsync();

    /// <summary>
    /// 清除保存的账号信息
    /// </summary>
    Task ClearAccountAsync();

    /// <summary>
    /// 账号状态变化事件
    /// </summary>
    event EventHandler<bool> AccountStatusChanged;
}
