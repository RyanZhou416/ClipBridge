using ClipBridgeShell_CS.Core.Services;
using ClipBridgeShell_CS.Credentials;
using ClipBridgeShell_CS.Models;
using ClipBridgeShell_CS.Services;

using Microsoft.Extensions.Options;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 便携模式的用户信息落盘位置：设置 → &lt;dataRoot&gt;\settings.json；
/// 凭据存储选择 → 仅 Portable 用 DPAPI 文件，默认/回退仍用 PasswordVault。
/// </summary>
[TestClass]
public class PortableUserDataTests
{
    private string _dataRoot = null!;
    private string _legacyFolder = null!;

    [TestInitialize]
    public void Setup()
    {
        var baseDir = Path.Combine(
            Path.GetTempPath(),
            "ClipBridgeShell_CS.Tests",
            "PortableUserDataTests",
            Guid.NewGuid().ToString("N"));
        _dataRoot = Path.Combine(baseDir, "portable-data");
        _legacyFolder = Path.Combine(baseDir, "legacy-appdata");
        Directory.CreateDirectory(_dataRoot);
    }

    [TestCleanup]
    public void Cleanup()
    {
        try
        {
            var parent = Path.GetDirectoryName(_dataRoot);
            if (parent != null && Directory.Exists(parent))
            {
                Directory.Delete(parent, recursive: true);
            }
        }
        catch
        {
            // 测试清理尽力而为。
        }
    }

    [TestMethod]
    public async Task PortableMode_SettingsGoToDataRootSettingsJson()
    {
        var service = CreateSettingsService(new StubDataRootProvider(StubDataRootProvider.Portable(_dataRoot)));

        await service.SaveSettingAsync("Account_Username", "portable-user");

        var portableSettings = Path.Combine(_dataRoot, "settings.json");
        Assert.IsTrue(File.Exists(portableSettings), "portable settings must be <dataRoot>\\settings.json");
        Assert.IsFalse(
            File.Exists(Path.Combine(_legacyFolder, "LocalSettings.json")),
            "portable mode must not write the legacy LocalAppData settings file");

        // 同一服务读回，确认 roundtrip。
        Assert.AreEqual("portable-user", await service.ReadSettingAsync<string>("Account_Username"));

        // 文件格式与旧 LocalSettings.json 一致（JSON 字典），便于迁移排查。
        var json = await File.ReadAllTextAsync(portableSettings);
        StringAssert.Contains(json, "Account_Username");
    }

    [TestMethod]
    public async Task DefaultMode_KeepsLegacySettingsLocation()
    {
        var service = CreateSettingsService(new StubDataRootProvider(StubDataRootProvider.Default(Path.Combine(_legacyFolder, "ClipBridge"))));

        await service.SaveSettingAsync("Account_Username", "legacy-user");

        var legacySettings = Path.Combine(_legacyFolder, "LocalSettings.json");
        Assert.IsTrue(File.Exists(legacySettings), "default mode must keep the legacy LocalSettings.json path");
        Assert.IsFalse(File.Exists(Path.Combine(_dataRoot, "settings.json")));
        Assert.AreEqual("legacy-user", await service.ReadSettingAsync<string>("Account_Username"));
    }

    [TestMethod]
    public void CredentialStoreSelection_OnlyPortableUsesDpapiFile()
    {
        var portable = CredentialStoreSelector.Create(StubDataRootProvider.Portable(_dataRoot));
        Assert.IsInstanceOfType(portable, typeof(DpapiFileCredentialStore));
        Assert.AreEqual(Path.Combine(_dataRoot, "credentials.bin"), ((DpapiFileCredentialStore)portable).FilePath);

        var defaultMode = CredentialStoreSelector.Create(StubDataRootProvider.Default(_legacyFolder));
        Assert.IsInstanceOfType(defaultMode, typeof(PasswordVaultCredentialStore));

        // marker 存在但便携目录不可写 → 解析器整体回退为 LocalAppData：凭据也必须留在 PasswordVault，
        // 不能写到一个用户以为是便携目录、实际不可用的位置。
        var fallback = CredentialStoreSelector.Create(
            StubDataRootProvider.Default(_legacyFolder, "portable data dir is not writable"));
        Assert.IsInstanceOfType(fallback, typeof(PasswordVaultCredentialStore));
    }

    [TestMethod]
    public async Task PortableSettings_ReadBeforeWrite_ReturnsDefaultWithoutCreatingLegacyFile()
    {
        var service = CreateSettingsService(new StubDataRootProvider(StubDataRootProvider.Portable(_dataRoot)));

        var value = await service.ReadSettingAsync<string>("DoesNotExist");

        Assert.IsNull(value);
        Assert.IsFalse(Directory.Exists(_legacyFolder), "reading settings must not create the legacy folder");
    }

    private LocalSettingsService CreateSettingsService(IDataRootProvider dataRootProvider)
    {
        var options = Options.Create(new LocalSettingsOptions
        {
            ApplicationDataFolder = _legacyFolder,
            LocalSettingsFile = "LocalSettings.json",
        });

        return new LocalSettingsService(new FileService(), options, dataRootProvider);
    }
}
