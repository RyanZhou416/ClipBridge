using System.Text;
using System.Text.Json;

using ClipBridgeShell_CS.Credentials;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 便携模式凭据存储：DPAPI（CurrentUser scope）+ 固定 entropy 加密文件。
/// 覆盖往返、损坏/篡改/错误 entropy 的 fail-closed 读取、写失败向上报错、以及“文件里不含明文”。
/// </summary>
[TestClass]
public class CredentialStoreTests
{
    private const string Username = "portable-user@example.test";
    private const string Password = "S3cret-Portable-Passw0rd!";
    private const string EntropyString = "ClipBridge.credentials.v1";

    private string _dataRoot = null!;

    [TestInitialize]
    public void Setup()
    {
        _dataRoot = Path.Combine(
            Path.GetTempPath(),
            "ClipBridgeShell_CS.Tests",
            "CredentialStoreTests",
            Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_dataRoot);
    }

    [TestCleanup]
    public void Cleanup()
    {
        try
        {
            if (Directory.Exists(_dataRoot))
            {
                Directory.Delete(_dataRoot, recursive: true);
            }
        }
        catch
        {
            // 测试清理尽力而为。
        }
    }

    [TestMethod]
    public async Task SaveThenRead_RoundTripsThroughDpapiFile()
    {
        var store = new DpapiFileCredentialStore(_dataRoot);
        Assert.AreEqual(Path.Combine(_dataRoot, "credentials.bin"), store.FilePath);

        await store.SaveAsync(Username, Password);

        Assert.IsTrue(File.Exists(store.FilePath), "credentials.bin must be written under the portable data root");
        Assert.AreEqual(1, Directory.GetFiles(_dataRoot).Length, "atomic write must not leave temporary files");

        var bytes = await File.ReadAllBytesAsync(store.FilePath);
        Assert.IsTrue(bytes.Length > 5);
        CollectionAssert.AreEqual(
            new[] { (byte)'C', (byte)'B', (byte)'C', (byte)'R' },
            bytes.Take(4).ToArray());
        Assert.AreEqual((byte)1, bytes[4], "format version");

        AssertDoesNotContainPlaintext(bytes, Username);
        AssertDoesNotContainPlaintext(bytes, Password);

        var result = await store.ReadAsync();
        Assert.IsTrue(result.Found);
        Assert.IsTrue(result.PasswordAvailable);
        Assert.AreEqual(Username, result.Username);
        Assert.AreEqual(Password, result.Password);
        Assert.IsNull(result.FaultReason);
    }

    [TestMethod]
    public async Task SaveTwice_ReplacesCredential()
    {
        var store = new DpapiFileCredentialStore(_dataRoot);
        await store.SaveAsync(Username, Password);
        await store.SaveAsync("another-user", "Another-Passw0rd!");

        var result = await store.ReadAsync();
        Assert.IsTrue(result.PasswordAvailable);
        Assert.AreEqual("another-user", result.Username);
        Assert.AreEqual("Another-Passw0rd!", result.Password);
        Assert.AreEqual(1, Directory.GetFiles(_dataRoot).Length);
    }

    [TestMethod]
    public async Task Read_MissingFile_ReturnsNotFound()
    {
        var store = new DpapiFileCredentialStore(_dataRoot);
        var result = await store.ReadAsync();

        Assert.IsFalse(result.Found);
        Assert.IsFalse(result.PasswordAvailable);
        Assert.IsNull(result.Username);
        Assert.IsNull(result.FaultReason);
    }

    [TestMethod]
    public async Task Read_TruncatedFile_FailsClosedAndKeepsFile()
    {
        var store = new DpapiFileCredentialStore(_dataRoot);
        await File.WriteAllBytesAsync(store.FilePath, new byte[] { (byte)'C', (byte)'B', (byte)'C' });

        var result = await store.ReadAsync();

        Assert.IsTrue(result.Found, "a corrupt credential file still counts as configured");
        Assert.IsFalse(result.PasswordAvailable);
        Assert.IsNotNull(result.FaultReason);
        Assert.IsTrue(File.Exists(store.FilePath), "corrupt file must be preserved for diagnosis, not deleted");
    }

    [TestMethod]
    public async Task Read_BadMagic_FailsClosedAndKeepsFile()
    {
        var store = new DpapiFileCredentialStore(_dataRoot);
        await File.WriteAllBytesAsync(store.FilePath, new byte[] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 });

        var result = await store.ReadAsync();

        Assert.IsTrue(result.Found);
        Assert.IsFalse(result.PasswordAvailable);
        Assert.IsNotNull(result.FaultReason);
        Assert.IsTrue(File.Exists(store.FilePath));
    }

    [TestMethod]
    public async Task Read_TamperedBlob_FailsClosedAndKeepsFile()
    {
        var store = new DpapiFileCredentialStore(_dataRoot);
        await store.SaveAsync(Username, Password);

        var bytes = await File.ReadAllBytesAsync(store.FilePath);
        bytes[^1] ^= 0xFF; // 破坏 DPAPI blob 尾部（MAC 校验失败）
        await File.WriteAllBytesAsync(store.FilePath, bytes);

        var result = await store.ReadAsync();

        Assert.IsTrue(result.Found);
        Assert.IsFalse(result.PasswordAvailable);
        Assert.IsNotNull(result.FaultReason);
        Assert.IsTrue(File.Exists(store.FilePath));

        // 明文不应因为“损坏”而泄漏：文件仍是二进制密文。
        AssertDoesNotContainPlaintext(bytes, Username);
        AssertDoesNotContainPlaintext(bytes, Password);
    }

    [TestMethod]
    public async Task Read_BlobProtectedWithWrongEntropy_FailsClosed()
    {
        // 模拟“别人用不同 entropy 保护的数据”/entropy 版本升级后的旧文件：
        // DPAPI 解密必须失败，而不是读到错误内容。
        var payload = JsonSerializer.SerializeToUtf8Bytes(new { username = Username, password = Password });
        var wrongEntropy = Encoding.UTF8.GetBytes(EntropyString + "-wrong");
        var protectedPayload = DpapiProtector.Protect(payload, wrongEntropy);

        var store = new DpapiFileCredentialStore(_dataRoot);
        var fileBytes = new byte[5 + protectedPayload.Length];
        fileBytes[0] = (byte)'C';
        fileBytes[1] = (byte)'B';
        fileBytes[2] = (byte)'C';
        fileBytes[3] = (byte)'R';
        fileBytes[4] = 1;
        Buffer.BlockCopy(protectedPayload, 0, fileBytes, 5, protectedPayload.Length);
        await File.WriteAllBytesAsync(store.FilePath, fileBytes);

        var result = await store.ReadAsync();

        Assert.IsTrue(result.Found);
        Assert.IsFalse(result.PasswordAvailable);
        Assert.IsNotNull(result.FaultReason);
        Assert.IsTrue(File.Exists(store.FilePath));
    }

    [TestMethod]
    public async Task Read_PayloadWithoutPassword_FailsClosed()
    {
        var payload = JsonSerializer.SerializeToUtf8Bytes(new { username = Username, password = "" });
        var protectedPayload = DpapiProtector.Protect(payload, Encoding.UTF8.GetBytes(EntropyString));

        var store = new DpapiFileCredentialStore(_dataRoot);
        var fileBytes = new byte[5 + protectedPayload.Length];
        fileBytes[0] = (byte)'C';
        fileBytes[1] = (byte)'B';
        fileBytes[2] = (byte)'C';
        fileBytes[3] = (byte)'R';
        fileBytes[4] = 1;
        Buffer.BlockCopy(protectedPayload, 0, fileBytes, 5, protectedPayload.Length);
        await File.WriteAllBytesAsync(store.FilePath, fileBytes);

        var result = await store.ReadAsync();

        Assert.IsTrue(result.Found);
        Assert.IsFalse(result.PasswordAvailable);
        Assert.IsNotNull(result.FaultReason);
    }

    [TestMethod]
    public async Task Clear_RemovesFileAndReadReturnsNotFound()
    {
        var store = new DpapiFileCredentialStore(_dataRoot);
        await store.SaveAsync(Username, Password);

        await store.ClearAsync();

        Assert.IsFalse(File.Exists(store.FilePath));
        var result = await store.ReadAsync();
        Assert.IsFalse(result.Found);
        Assert.IsFalse(result.PasswordAvailable);
    }

    [TestMethod]
    public async Task Save_WhenDataRootIsNotWritable_Throws()
    {
        // 让 dataRoot 的父路径是一个普通文件：创建目录/写文件必然失败，
        // 验证写失败不会被静默吞掉（否则会出现“设置里记了用户名、凭据却没存上”）。
        var blocker = Path.Combine(_dataRoot, "blocker");
        await File.WriteAllTextAsync(blocker, "not a directory");
        var store = new DpapiFileCredentialStore(Path.Combine(blocker, "data"));

        var threw = false;
        try
        {
            await store.SaveAsync(Username, Password);
        }
        catch (IOException)
        {
            threw = true;
        }
        catch (UnauthorizedAccessException)
        {
            threw = true;
        }

        Assert.IsTrue(threw, "credential write failure must surface to the caller");
    }

    [TestMethod]
    public void DpapiProtector_ProtectUnprotect_RoundTrips()
    {
        var plaintext = Encoding.UTF8.GetBytes("{\"username\":\"u\",\"password\":\"p\"}");
        var entropy = Encoding.UTF8.GetBytes(EntropyString);

        var ciphertext = DpapiProtector.Protect(plaintext, entropy);
        CollectionAssert.AreNotEqual(plaintext, ciphertext);

        var decrypted = DpapiProtector.Unprotect(ciphertext, entropy);
        CollectionAssert.AreEqual(plaintext, decrypted);
    }

    private static void AssertDoesNotContainPlaintext(byte[] haystack, string plaintext)
    {
        foreach (var encoding in new[] { Encoding.UTF8, Encoding.Unicode, Encoding.BigEndianUnicode })
        {
            var needle = encoding.GetBytes(plaintext);
            Assert.IsFalse(
                Contains(haystack, needle),
                $"credentials.bin must not contain plaintext ({encoding.WebName}).");
        }

        // 常见的 Base64 包装也不允许出现。
        var base64 = Encoding.ASCII.GetBytes(Convert.ToBase64String(Encoding.UTF8.GetBytes(plaintext)));
        Assert.IsFalse(Contains(haystack, base64), "credentials.bin must not contain base64 plaintext.");
    }

    private static bool Contains(byte[] haystack, byte[] needle)
    {
        if (needle.Length == 0 || haystack.Length < needle.Length)
        {
            return false;
        }

        for (var i = 0; i <= haystack.Length - needle.Length; i++)
        {
            var match = true;
            for (var j = 0; j < needle.Length; j++)
            {
                if (haystack[i + j] != needle[j])
                {
                    match = false;
                    break;
                }
            }

            if (match)
            {
                return true;
            }
        }

        return false;
    }
}
