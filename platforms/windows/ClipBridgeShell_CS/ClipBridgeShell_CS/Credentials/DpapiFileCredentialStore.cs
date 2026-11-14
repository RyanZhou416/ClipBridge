using System.Diagnostics;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Credentials;

/// <summary>
/// 便携模式凭据存储：DPAPI（CurrentUser scope）加密后写入 <c>&lt;dataRoot&gt;\credentials.bin</c>。
/// </summary>
/// <remarks>
/// <para>文件格式（小端、无文本）：</para>
/// <code>
/// magic  : 43 42 43 52      ("CBCR")
/// version: 01
/// payload: CryptProtectData(UTF8 JSON {"username":..,"password":..}, entropy="ClipBridge.credentials.v1")
/// </code>
/// <para>安全约束：</para>
/// <list type="bullet">
/// <item>明文只存在于进程内存，落盘内容全部被 DPAPI 加密；文件里不会出现明文账号/密码。</item>
/// <item>写入走“临时文件 + File.Move(overwrite)”，避免半写文件；写失败向调用方抛出。</item>
/// <item>读取遇到损坏/截断/格式不符/解密失败时返回 <see cref="CredentialReadResult.Unavailable"/>，
/// 保留原文件（不静默删除），由上层提示重新登录。</item>
/// <item>文件权限沿用用户 profile 的默认 ACL（当前用户可读写），不额外放开。</item>
/// </list>
/// </remarks>
public sealed class DpapiFileCredentialStore : ICredentialStore
{
    /// <summary>便携数据根目录下的凭据文件名。</summary>
    public const string FileName = "credentials.bin";

    /// <summary>固定 entropy：把 DPAPI blob 与应用/用途绑死，拿到其它应用的保护数据也无法直接复用。</summary>
    private static readonly byte[] Entropy = Encoding.UTF8.GetBytes("ClipBridge.credentials.v1");

    private static readonly byte[] FileMagic = { (byte)'C', (byte)'B', (byte)'C', (byte)'R' };

    private const byte FormatVersion = 1;

    private const int HeaderLength = 5; // FileMagic(4) + FormatVersion(1)

    private readonly string _dataRoot;

    public DpapiFileCredentialStore(string dataRoot)
    {
        if (string.IsNullOrWhiteSpace(dataRoot))
        {
            throw new ArgumentException("Data root must not be empty.", nameof(dataRoot));
        }

        _dataRoot = dataRoot;
    }

    public string Kind => "dpapi-file";

    /// <summary>凭据文件完整路径（固定为 &lt;dataRoot&gt;\credentials.bin）。</summary>
    public string FilePath => Path.Combine(_dataRoot, FileName);

    public Task<CredentialReadResult> ReadAsync()
    {
        var path = FilePath;
        if (!File.Exists(path))
        {
            return Task.FromResult(CredentialReadResult.NotFound);
        }

        byte[] fileBytes;
        try
        {
            fileBytes = File.ReadAllBytes(path);
        }
        catch (Exception ex)
        {
            // 文件在但读不了（被占用/权限问题）：按“已配置但不可用”处理，绝不当作首次运行。
            return Task.FromResult(CredentialReadResult.Unavailable(
                null,
                $"Credential file is not readable ({ex.GetType().Name})."));
        }

        if (fileBytes.Length <= HeaderLength)
        {
            return Task.FromResult(Corrupt("Credential file is truncated."));
        }

        for (var i = 0; i < FileMagic.Length; i++)
        {
            if (fileBytes[i] != FileMagic[i])
            {
                return Task.FromResult(Corrupt("Credential file magic does not match."));
            }
        }

        if (fileBytes[FileMagic.Length] != FormatVersion)
        {
            return Task.FromResult(Corrupt(
                $"Unsupported credential file version {fileBytes[FileMagic.Length]}."));
        }

        var protectedPayload = new byte[fileBytes.Length - HeaderLength];
        Buffer.BlockCopy(fileBytes, HeaderLength, protectedPayload, 0, protectedPayload.Length);

        byte[]? plaintext = null;
        try
        {
            plaintext = DpapiProtector.Unprotect(protectedPayload, Entropy);
            var payload = JsonSerializer.Deserialize<CredentialPayload>(plaintext);
            if (payload is null
                || string.IsNullOrWhiteSpace(payload.Username)
                || string.IsNullOrWhiteSpace(payload.Password))
            {
                return Task.FromResult(CredentialReadResult.Unavailable(
                    string.IsNullOrWhiteSpace(payload?.Username) ? null : payload.Username,
                    "Credential payload is incomplete."));
            }

            return Task.FromResult(CredentialReadResult.Available(payload.Username, payload.Password));
        }
        catch (Exception ex)
        {
            // 损坏 / entropy 不匹配 / 换了 Windows 用户：保留文件，交给上层提示重新登录。
            return Task.FromResult(Corrupt($"Credential file cannot be decrypted ({ex.GetType().Name})."));
        }
        finally
        {
            if (plaintext is not null)
            {
                CryptographicOperations.ZeroMemory(plaintext);
            }

            CryptographicOperations.ZeroMemory(protectedPayload);
        }
    }

    public Task SaveAsync(string username, string password)
    {
        // 明文 JSON 只在内存里短暂存在，随后立即清零；落盘前已整体被 DPAPI 加密。
        var payload = JsonSerializer.SerializeToUtf8Bytes(
            new CredentialPayload { Username = username, Password = password });

        byte[] protectedPayload;
        try
        {
            protectedPayload = DpapiProtector.Protect(payload, Entropy);
        }
        finally
        {
            CryptographicOperations.ZeroMemory(payload);
        }

        var fileBytes = new byte[HeaderLength + protectedPayload.Length];
        Buffer.BlockCopy(FileMagic, 0, fileBytes, 0, FileMagic.Length);
        fileBytes[FileMagic.Length] = FormatVersion;
        Buffer.BlockCopy(protectedPayload, 0, fileBytes, HeaderLength, protectedPayload.Length);

        try
        {
            WriteAtomic(fileBytes);
        }
        finally
        {
            CryptographicOperations.ZeroMemory(fileBytes);
            CryptographicOperations.ZeroMemory(protectedPayload);
        }

        return Task.CompletedTask;
    }

    public Task ClearAsync()
    {
        var path = FilePath;
        if (File.Exists(path))
        {
            // 删除失败向上抛，由 AccountService 记录；不能假装已退出登录。
            File.Delete(path);
        }

        return Task.CompletedTask;
    }

    private static CredentialReadResult Corrupt(string reason)
    {
        Debug.WriteLine($"[DpapiFileCredentialStore] {reason}");
        return CredentialReadResult.Unavailable(null, reason);
    }

    private void WriteAtomic(byte[] fileBytes)
    {
        Directory.CreateDirectory(_dataRoot);

        var targetPath = FilePath;
        var tempPath = $"{targetPath}.tmp-{Guid.NewGuid():N}";
        try
        {
            using (var stream = new FileStream(
                       tempPath,
                       FileMode.CreateNew,
                       FileAccess.Write,
                       FileShare.None,
                       bufferSize: 4096,
                       FileOptions.None))
            {
                stream.Write(fileBytes, 0, fileBytes.Length);
                stream.Flush(flushToDisk: true);
            }

            // 同卷 rename：读方要么看到旧文件，要么看到完整的新文件，不会看到半截。
            File.Move(tempPath, targetPath, overwrite: true);
        }
        finally
        {
            if (File.Exists(tempPath))
            {
                try
                {
                    File.Delete(tempPath);
                }
                catch
                {
                    // 临时文件清理是尽力而为；真正的写入失败已经通过异常向上抛出。
                }
            }
        }
    }

    /// <summary>加密前/解密后的明文载荷，字段名即序列化字段。</summary>
    private sealed class CredentialPayload
    {
        [JsonPropertyName("username")]
        public string? Username
        {
            get; set;
        }

        [JsonPropertyName("password")]
        public string? Password
        {
            get; set;
        }
    }
}
