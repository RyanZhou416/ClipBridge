using Windows.Security.Credentials;

namespace ClipBridgeShell_CS.Credentials;

/// <summary>
/// 默认（非便携）凭据存储：Windows <see cref="PasswordVault"/>。
/// 逻辑从原 <see cref="Services.AccountService"/> 原样搬过来，行为保持不变。
/// </summary>
public sealed class PasswordVaultCredentialStore : ICredentialStore
{
    private const string VaultResource = "ClipBridge";

    public string Kind => "password-vault";

    public Task<CredentialReadResult> ReadAsync()
    {
        // 注意：PasswordVault.FindAllByResource 在“没有凭据”时也会抛异常（HRESULT not found），
        // 旧实现正是靠 catch 把它当成“未配置”。这里必须保持同样的语义，
        // 否则默认模式的首次运行会被误判成“凭据不可用”而 fail-closed。
        IReadOnlyList<PasswordCredential> credentials;
        try
        {
            var vault = new PasswordVault();
            credentials = vault.FindAllByResource(VaultResource);
        }
        catch
        {
            return Task.FromResult(CredentialReadResult.NotFound);
        }

        if (credentials.Count == 0)
        {
            return Task.FromResult(CredentialReadResult.NotFound);
        }

        // 从这里开始：凭据已存在。即使 RetrievePassword 失败也必须算“已配置且不可用”，
        // 由 AccountService fail-closed（与旧实现 vaultCredentialFound 的语义一致）。
        try
        {
            var credential = credentials[0];
            credential.RetrievePassword(); // 解密密码

            var username = string.IsNullOrWhiteSpace(credential.UserName) ? null : credential.UserName;
            var password = string.IsNullOrWhiteSpace(credential.Password) ? null : credential.Password;
            if (password is null)
            {
                return Task.FromResult(CredentialReadResult.Unavailable(
                    username,
                    "PasswordVault credential is present but its password is empty."));
            }

            return Task.FromResult(CredentialReadResult.Available(username ?? string.Empty, password));
        }
        catch (Exception ex)
        {
            // 被清空/迁移/Windows Hello 失败等：不得泄露任何凭据内容。
            return Task.FromResult(CredentialReadResult.Unavailable(
                null,
                $"PasswordVault password is unreadable ({ex.GetType().Name})."));
        }
    }

    public Task SaveAsync(string username, string password)
    {
        var vault = new PasswordVault();

        // 先删除旧的（如果存在）。没有旧凭据时 FindAllByResource 会抛异常，与旧实现一致忽略。
        try
        {
            foreach (var oldCredential in vault.FindAllByResource(VaultResource))
            {
                vault.Remove(oldCredential);
            }
        }
        catch
        {
        }

        // Add 失败必须向上抛出：否则设置里记录了用户名，凭据却根本不存在。
        vault.Add(new PasswordCredential(VaultResource, username, password));
        return Task.CompletedTask;
    }

    public Task ClearAsync()
    {
        try
        {
            var vault = new PasswordVault();
            foreach (var credential in vault.FindAllByResource(VaultResource))
            {
                vault.Remove(credential);
            }
        }
        catch
        {
            // 没有凭据（或密码库不可用）时无需处理，与旧实现一致。
        }

        return Task.CompletedTask;
    }
}
