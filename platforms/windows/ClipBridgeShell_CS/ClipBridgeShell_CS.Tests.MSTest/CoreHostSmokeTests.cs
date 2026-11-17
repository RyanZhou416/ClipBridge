using System.Security.Cryptography;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 正向原生 smoke：必须使用本次构建的真实 DLL、达到 Ready、ABI 兼容、无 DllLoadError，
/// 并完成初始化/关闭。所有数据（core/cache/log/tls/settings）都落在独占临时根。
/// </summary>
[TestClass]
public class CoreHostSmokeTests
{
    private const int LoopCount = 50;
    private const string ExpectedDllPathEnv = "CLIPBRIDGE_CORE_FFI_DLL";
    private const string ExpectedDllHashEnv = "CLIPBRIDGE_CORE_FFI_SHA256";

    public TestContext TestContext { get; set; } = null!;

    [TestMethod]
    public async Task Init_Shutdown_Loop_50()
    {
        using var data = new TestDataDirectory(nameof(CoreHostSmokeTests));
        var localSettingsService = data.CreateSettingsService();

        // 审计 F11 之后，缺少可用账号凭据时 CoreHostService 会 fail-closed；
        // 冒烟测试需要真实初始化核心，因此注入一组固定的合成凭据（绝不读取 PasswordVault）。
        var accountService = new StubAccountService(
            AccountLoadResult.Available("smoke-test-user", "smoke-test-password"));

        var coreHost = new CoreHostService(
            new EventPumpService(new HistoryStore(), new PeerStore(), new TransferStore(), new ContentFetchAwaiter()),
            localSettingsService,
            null,
            accountService,
            new StubDataRootProvider(StubDataRootProvider.Default(data.Root)));

        try
        {
            for (var i = 1; i <= LoopCount; i++)
            {
                try
                {
                    TestContext.WriteLine($"[Loop {i}/{LoopCount}] init...");
                    await coreHost.InitializeAsync();

                    TestContext.WriteLine($"[Loop {i}/{LoopCount}] state={coreHost.State} abi={coreHost.Diagnostics.FfiAbiMajor}.{coreHost.Diagnostics.FfiAbiMinor}");
                    Assert.AreEqual(CoreState.Ready, coreHost.State, "positive smoke must reach Ready, never Degraded");
                    Assert.IsNull(coreHost.Diagnostics.DllLoadError, "positive smoke must not report a DLL load error");
                    Assert.AreEqual(1u, coreHost.Diagnostics.FfiAbiMajor, "FFI ABI major must match the consumer");
                    Assert.IsNotNull(coreHost.Diagnostics.FfiAbiMinor, "FFI ABI minor must be reported by the real core");
                    Assert.AreEqual("Init OK", coreHost.Diagnostics.LastInitSummary);

                    // 数据隔离：core/cache/log 路径全部来自注入的临时根。
                    data.AssertContained(coreHost.Diagnostics.AppDataDir, "AppDataDir");
                    data.AssertContained(coreHost.Diagnostics.CoreDataDir, "CoreDataDir");
                    data.AssertContained(coreHost.Diagnostics.CacheDir, "CacheDir");
                    data.AssertContained(coreHost.Diagnostics.LogDir, "LogDir");

                    if (i == 1)
                    {
                        AssertLoadedDllIsFreshBuild(coreHost);
                        Assert.IsTrue(File.Exists(Path.Combine(data.Root, "core.db")), "core.db must be created under the temp root");
                        Assert.IsTrue(File.Exists(Path.Combine(data.Root, "logs.db")), "logs.db must be created under the temp root");
                        Assert.IsTrue(Directory.Exists(Path.Combine(data.Root, "Cache")), "cache must be created under the temp root");
                        // 测试宿主 disable_network=true，core 不启动 Transport，TLS 材料可能尚未按需生成；
                        // 一旦生成，cert.rs 固定在 data_dir/tls 下，而 data_dir 已断言等于临时根。
                        Assert.IsTrue(
                            File.Exists(Path.Combine(data.Root, "LocalSettings.json")),
                            "settings must be written under the temp root, not the real user profile");
                    }
                }
                finally
                {
                    await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost, TestContext.WriteLine);
                }

                Assert.AreEqual(CoreState.NotLoaded, coreHost.State);
            }

            TestContext.WriteLine("DONE Init_Shutdown_Loop_50");
        }
        finally
        {
            // 防御性兜底：即使上面的循环在 init 与 shutdown 之间抛出，也不留核心线程/句柄。
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost, TestContext.WriteLine);
        }
    }

    /// <summary>
    /// 证明实际加载的是本次构建复制到测试输出目录的 DLL，而不是从上级目录捡到的旧 DLL。
    /// windows.ps1 会在运行测试前注入本次 cargo 产物的绝对路径与 SHA256；开发者直接
    /// dotnet test 时至少要求“加载文件 == 测试输出目录中的 DLL”。
    /// </summary>
    private static void AssertLoadedDllIsFreshBuild(CoreHostService coreHost)
    {
        var actualPath = coreHost.Diagnostics.DllPath;
        Assert.IsFalse(string.IsNullOrWhiteSpace(actualPath), "positive smoke must report the loaded DLL path");
        Assert.IsTrue(File.Exists(actualPath), $"loaded DLL must exist: {actualPath}");

        var outputCopy = Path.Combine(AppContext.BaseDirectory, "core_ffi_windows.dll");
        var expectedPath = Environment.GetEnvironmentVariable(ExpectedDllPathEnv);
        var expectedHash = Environment.GetEnvironmentVariable(ExpectedDllHashEnv);
        if (string.IsNullOrWhiteSpace(expectedHash))
        {
            Assert.IsTrue(File.Exists(outputCopy), $"test output must contain core_ffi_windows.dll: {outputCopy}");
            expectedHash = Sha256File(outputCopy);
        }

        Assert.IsTrue(
            string.Equals(expectedHash.Trim(), Sha256File(actualPath), StringComparison.OrdinalIgnoreCase),
            $"loaded DLL hash must equal the freshly built core_ffi_windows.dll. expected={expectedHash} actual={Sha256File(actualPath)} path={actualPath}");

        // CI 期望路径是 target/<triple>/release 下的原始产物；dotnet 会把它复制到测试输出目录，
        // 因此只要内容 hash 一致，接受“原始产物”或“输出目录副本”这两种路径。
        if (!string.IsNullOrWhiteSpace(expectedPath))
        {
            Assert.IsTrue(
                PathsEqual(actualPath, expectedPath) || PathsEqual(actualPath, outputCopy),
                $"loaded DLL path must be the CI-built artifact or its test output copy. expected={expectedPath} actual={actualPath}");
        }
    }

    private static bool PathsEqual(string left, string right)
        => string.Equals(
            Path.GetFullPath(left).TrimEnd(Path.DirectorySeparatorChar),
            Path.GetFullPath(right).TrimEnd(Path.DirectorySeparatorChar),
            StringComparison.OrdinalIgnoreCase);

    private static string Sha256File(string path)
    {
        using var stream = File.OpenRead(path);
        return Convert.ToHexString(SHA256.HashData(stream));
    }
}
