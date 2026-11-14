using ClipBridgeShell_CS.Core.Services;

namespace ClipBridgeShell_CS.Credentials;

/// <summary>
/// 凭据存储选择器：整个应用只在这里决定“用 PasswordVault 还是 DPAPI 文件”。
/// </summary>
/// <remarks>
/// 只有 <see cref="CoreConfigBuilder.ResolveDataRootInfo"/> 明确返回
/// <see cref="DataRootMode.Portable"/> 时才用文件存储；以下两种情况都保持 PasswordVault：
/// <list type="number">
/// <item>没有 <c>ClipBridge.portable</c> marker（默认安装/老用户）；</item>
/// <item>marker 存在但便携目录不可写 → 解析器整体回退到 LocalAppData（模式不是 Portable）。</item>
/// </list>
/// 便携目录不可写时绝不把凭据写到一半：回退由解析器整体决定，这里只跟随最终 Mode。
/// </remarks>
public static class CredentialStoreSelector
{
    public static ICredentialStore Create(DataRootResolution dataRoot)
    {
        ArgumentNullException.ThrowIfNull(dataRoot);

        return dataRoot.Mode == DataRootMode.Portable
            ? new DpapiFileCredentialStore(dataRoot.Root)
            : new PasswordVaultCredentialStore();
    }
}
