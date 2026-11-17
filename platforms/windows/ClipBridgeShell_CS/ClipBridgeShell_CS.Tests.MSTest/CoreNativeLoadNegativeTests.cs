using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Services;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 原生加载负路径：缺库、坏 PE、错误架构、缺少 ABI 导出、ABI 主版本不兼容都必须
/// 让正向 smoke 无法通过（Degraded + 明确诊断），而不是回退旧库或只打 warning。
/// </summary>
[TestClass]
public class CoreNativeLoadNegativeTests
{
    private const string DllName = "core_ffi_windows.dll";

    [TestMethod]
    public async Task Initialize_MissingDll_DegradesWithoutCreatingCoreData()
    {
        using var data = new TestDataDirectory(nameof(CoreNativeLoadNegativeTests));
        var missingPath = data.PathUnderRoot("missing", DllName);

        var coreHost = CreateHost(data, new FixedCoreLibraryLocator(missingPath));
        try
        {
            await coreHost.InitializeAsync();

            Assert.AreEqual(CoreState.Degraded, coreHost.State);
            Assert.AreEqual(missingPath, coreHost.Diagnostics.DllPath);
            StringAssert.Contains(coreHost.Diagnostics.DllLoadError, "not found");
            Assert.AreEqual("Core degraded: DLL missing.", coreHost.Diagnostics.LastInitSummary);
            AssertNoCoreWrites(data.Root);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }
    }

    [TestMethod]
    public async Task Initialize_CorruptPe_DegradesWithBadImageFormat()
    {
        using var data = new TestDataDirectory(nameof(CoreNativeLoadNegativeTests));
        var dllPath = data.PathUnderRoot("corrupt", DllName);
        Directory.CreateDirectory(Path.GetDirectoryName(dllPath)!);
        await File.WriteAllBytesAsync(dllPath, [0x4D, 0x5A, 0x00, 0x01, 0x02, 0x03]);

        var coreHost = CreateHost(data, new FixedCoreLibraryLocator(dllPath));
        try
        {
            await coreHost.InitializeAsync();

            Assert.AreEqual(CoreState.Degraded, coreHost.State);
            StringAssert.StartsWith(coreHost.Diagnostics.DllLoadError, "BadImageFormat:");
            StringAssert.Contains(coreHost.Diagnostics.LastInitSummary, "Arch mismatch");
            AssertNoCoreWrites(data.Root);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }
    }

    [TestMethod]
    public async Task Initialize_WrongArchitectureDll_DegradesWithBadImageFormat()
    {
        using var data = new TestDataDirectory(nameof(CoreNativeLoadNegativeTests));
        var dllPath = data.PathUnderRoot("wrong-arch", DllName);
        Directory.CreateDirectory(Path.GetDirectoryName(dllPath)!);

        var wow64Kernel32 = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.Windows),
            "SysWOW64",
            "kernel32.dll");
        if (Environment.Is64BitProcess && File.Exists(wow64Kernel32))
        {
            File.Copy(wow64Kernel32, dllPath);
        }
        else
        {
            // 退路：声明 i386 的最小 PE 头；x64 加载器同样以 bad image 拒绝。
            await File.WriteAllBytesAsync(dllPath, CreateMinimalX86Pe());
        }

        var coreHost = CreateHost(data, new FixedCoreLibraryLocator(dllPath));
        try
        {
            await coreHost.InitializeAsync();

            Assert.AreEqual(CoreState.Degraded, coreHost.State);
            StringAssert.StartsWith(coreHost.Diagnostics.DllLoadError, "BadImageFormat:");
            AssertNoCoreWrites(data.Root);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }
    }

    [TestMethod]
    public async Task Initialize_LibraryWithoutClipBridgeAbi_DegradesWithEntryPointNotFound()
    {
        using var data = new TestDataDirectory(nameof(CoreNativeLoadNegativeTests));
        var dllPath = data.PathUnderRoot("incompatible", DllName);
        Directory.CreateDirectory(Path.GetDirectoryName(dllPath)!);

        var compatiblePe = new[]
        {
            Path.Combine(Environment.SystemDirectory, "version.dll"),
            Path.Combine(Environment.SystemDirectory, "bcrypt.dll"),
            Path.Combine(Environment.SystemDirectory, "kernel32.dll"),
        }.First(File.Exists);
        File.Copy(compatiblePe, dllPath);

        var coreHost = CreateHost(data, new FixedCoreLibraryLocator(dllPath));
        try
        {
            await coreHost.InitializeAsync();

            Assert.AreEqual(CoreState.Degraded, coreHost.State);
            StringAssert.StartsWith(coreHost.Diagnostics.DllLoadError, "EntryPointNotFound:");
            StringAssert.Contains(coreHost.Diagnostics.LastInitSummary, "incompatible core library");
            AssertNoCoreWrites(data.Root);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }
    }

    [TestMethod]
    public async Task Initialize_AbiMajorMismatch_DegradesBeforeCoreInit()
    {
        var realDll = new DefaultCoreLibraryLocator().FindCoreLibrary();
        Assert.IsNotNull(realDll, "positive native build must provide core_ffi_windows.dll");

        using var data = new TestDataDirectory(nameof(CoreNativeLoadNegativeTests));
        var coreHost = CoreHostTestFactory.Create(
            new StubDataRootProvider(StubDataRootProvider.Default(data.Root)),
            data.CreateSettingsService(),
            libraryLocator: new FixedCoreLibraryLocator(realDll),
            expectedFfiAbiMajor: 2);
        try
        {
            await coreHost.InitializeAsync();

            Assert.AreEqual(CoreState.Degraded, coreHost.State);
            StringAssert.Contains(coreHost.LastError, "ABI 不兼容");
            StringAssert.Contains(coreHost.Diagnostics.LastInitSummary, "ABI mismatch");
            Assert.AreEqual(1u, coreHost.Diagnostics.FfiAbiMajor, "the real DLL must report its actual major version");
            AssertNoCoreWrites(data.Root);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(coreHost);
        }
    }

    [TestMethod]
    public async Task Initialize_ConflictingResolverPath_DegradesInsteadOfUsingAnotherLibrary()
    {
        var realDll = new DefaultCoreLibraryLocator().FindCoreLibrary();
        Assert.IsNotNull(realDll, "positive native build must provide core_ffi_windows.dll");

        using var firstData = new TestDataDirectory($"{nameof(CoreNativeLoadNegativeTests)}.first");
        using var secondData = new TestDataDirectory($"{nameof(CoreNativeLoadNegativeTests)}.second");

        // 先让进程 resolver 绑定到本次构建的 DLL（正常生产路径）。
        var firstHost = CoreHostTestFactory.Create(
            new StubDataRootProvider(StubDataRootProvider.Default(firstData.Root)),
            firstData.CreateSettingsService());
        try
        {
            await firstHost.InitializeAsync();
            Assert.AreEqual(CoreState.Ready, firstHost.State);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(firstHost);
        }

        // 第二个 host 指向同一 DLL 的副本（不同路径）：ABI 探测会成功，但 resolver
        // 不能静默换库；必须 Degraded，且不得初始化 core。
        var copyPath = secondData.PathUnderRoot("copy", DllName);
        Directory.CreateDirectory(Path.GetDirectoryName(copyPath)!);
        File.Copy(realDll, copyPath);

        var secondHost = CreateHost(secondData, new FixedCoreLibraryLocator(copyPath));
        try
        {
            await secondHost.InitializeAsync();

            Assert.AreEqual(CoreState.Degraded, secondHost.State);
            StringAssert.StartsWith(secondHost.Diagnostics.DllLoadError, "ResolverConflict:");
            AssertNoCoreWrites(secondData.Root);
        }
        finally
        {
            await CoreHostTestFactory.ShutdownQuietlyAsync(secondHost);
        }
    }

    private static CoreHostService CreateHost(TestDataDirectory data, ICoreLibraryLocator locator)
        => CoreHostTestFactory.Create(
            new StubDataRootProvider(StubDataRootProvider.Default(data.Root)),
            data.CreateSettingsService(),
            libraryLocator: locator);

    private static void AssertNoCoreWrites(string root)
    {
        Assert.IsFalse(File.Exists(Path.Combine(root, "core.db")), "failed native load must not create core.db");
        Assert.IsFalse(File.Exists(Path.Combine(root, "logs.db")), "failed native load must not create logs.db");
        Assert.IsFalse(Directory.Exists(Path.Combine(root, "tls")), "failed native load must not create tls material");
    }

    private static byte[] CreateMinimalX86Pe()
    {
        var image = new byte[0x200];
        image[0] = (byte)'M';
        image[1] = (byte)'Z';
        BitConverter.GetBytes(0x80).CopyTo(image, 0x3C);
        image[0x80] = (byte)'P';
        image[0x81] = (byte)'E';
        BitConverter.GetBytes((ushort)0x014C).CopyTo(image, 0x84); // IMAGE_FILE_MACHINE_I386
        return image;
    }
}
