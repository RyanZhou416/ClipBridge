using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>测试专用 DLL 定位器：让缺库/坏库/错误架构等负路径不依赖进程状态。</summary>
internal sealed class FixedCoreLibraryLocator : ICoreLibraryLocator
{
    private readonly string? _path;

    public FixedCoreLibraryLocator(string? path)
    {
        _path = path;
    }

    public string? FindCoreLibrary() => _path;
}

/// <summary>前台测试宿主：不注册 UI、不启动网络、不读写真实用户数据根。</summary>
internal static class CoreHostTestFactory
{
    public static CoreHostService Create(
        IDataRootProvider dataRootProvider,
        ILocalSettingsService localSettingsService,
        IAccountService? accountService = null,
        ICoreLibraryLocator? libraryLocator = null,
        uint expectedFfiAbiMajor = 1)
    {
        var eventPump = new EventPumpService(
            new HistoryStore(),
            new PeerStore(),
            new TransferStore(),
            new ContentFetchAwaiter());

        accountService ??= new StubAccountService(
            AccountLoadResult.Available("test-user", "test-password"));

        return new CoreHostService(
            eventPump,
            localSettingsService,
            null,
            accountService,
            dataRootProvider,
            libraryLocator ?? new DefaultCoreLibraryLocator(),
            expectedFfiAbiMajor);
    }

    /// <summary>
    /// 测试清理：无论断言是否失败都释放 core 句柄/后台线程；关闭异常只记录，不能覆盖
    /// 原始断言失败。
    /// </summary>
    public static async Task ShutdownQuietlyAsync(CoreHostService coreHost, Action<string>? log = null)
    {
        try
        {
            await coreHost.ShutdownAsync();
        }
        catch (Exception ex)
        {
            log?.Invoke($"ShutdownAsync threw during test cleanup: {ex}");
            System.Diagnostics.Debug.WriteLine($"[CoreHostTestFactory] ShutdownAsync threw: {ex}");
        }
    }
}
