namespace ClipBridgeShell_CS.Contracts.Services;

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
    /// 检查是否有保存的账号
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
