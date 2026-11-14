namespace ClipBridgeShell_CS.Credentials;

/// <summary>
/// 凭据存储的一次读取结果。
/// </summary>
/// <remarks>
/// 必须区分三种情况，调用方（<see cref="Services.AccountService"/>）据此保持 fail-closed：
/// <list type="number">
/// <item><see cref="NotFound"/>：存储里完全没有凭据痕迹，可以当首次运行；</item>
/// <item><see cref="Unavailable"/>：有痕迹但读不出来（损坏/解密失败/密码为空），
/// 调用方必须按“已配置但不可用”处理并提示重新登录；</item>
/// <item>可用：用户名/密码均非空。</item>
/// </list>
/// <see cref="FaultReason"/> 只用于诊断日志，绝不能包含密码或其它凭据明文。
/// </remarks>
public sealed class CredentialReadResult
{
    private CredentialReadResult(bool found, bool passwordAvailable, string? username, string? password, string? faultReason)
    {
        Found = found;
        PasswordAvailable = passwordAvailable;
        Username = username;
        Password = password;
        FaultReason = faultReason;
    }

    /// <summary>
    /// 存储中是否存在凭据痕迹。损坏/无法解密的文件也算存在：
    /// 否则用户会先被当成“首次运行”，然后以新账号重新初始化核心。
    /// </summary>
    public bool Found
    {
        get;
    }

    /// <summary>密码是否成功读取且非空。</summary>
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

    /// <summary>读取/解密失败原因（不含明文），成功或不存在时为 null。</summary>
    public string? FaultReason
    {
        get;
    }

    public static CredentialReadResult NotFound { get; } = new(false, false, null, null, null);

    public static CredentialReadResult Available(string username, string password)
        => new(true, true, username, password, null);

    public static CredentialReadResult Unavailable(string? username, string? faultReason = null)
        => new(true, false, username, null, faultReason);
}

/// <summary>
/// 账号凭据存储。默认实现是 Windows <c>PasswordVault</c>；便携模式用 DPAPI 加密文件，
/// 保证 <c>credentials.bin</c> 落在便携数据根目录内。
/// </summary>
public interface ICredentialStore
{
    /// <summary>存储种类标识（仅用于诊断，不包含任何凭据内容）。</summary>
    string Kind
    {
        get;
    }

    /// <summary>
    /// 读取凭据。读取/解密失败必须返回 <see cref="CredentialReadResult.Unavailable"/> 而不是抛异常，
    /// 更不能删除损坏的文件。
    /// </summary>
    Task<CredentialReadResult> ReadAsync();

    /// <summary>保存/覆盖凭据。写入失败必须向调用方抛出异常，不能静默失败。</summary>
    Task SaveAsync(string username, string password);

    /// <summary>清除凭据。删除失败是否忽略由调用方决定。</summary>
    Task ClearAsync();
}
