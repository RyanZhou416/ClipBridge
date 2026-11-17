using Microsoft.Windows.ApplicationModel.DynamicDependency;

[assembly: WinUITestTarget(typeof(ClipBridgeShell_CS.App))]

namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class Initialize
{
    [AssemblyInitialize]
    public static void AssemblyInitialize(TestContext context)
    {
        // 测试宿主不得启动核心网络栈：disable_network=true 时 core 跳过 NetManager，
        // 不 bind 0.0.0.0、不起 mDNS，从而避免 Windows Defender 防火墙弹窗。
        // 仅测试工程设置；发布的应用保持默认 false。
        ClipBridgeShell_CS.Services.CoreHostService.DisableNetwork = true;

        // 诊断打点（startup.log / frame trace）会在 CoreConfigBuilder 默认根下写文件；
        // 测试进程显式关闭，避免任何路径经静态 helper 落到真实用户目录。
        Environment.SetEnvironmentVariable("CB_STARTUP_TRACE", null);
        Environment.SetEnvironmentVariable("CB_FRAME_TRACE", null);

        // TODO: Initialize the appropriate version of the Windows App SDK.
        // This is required when testing MSIX apps that are framework-dependent on the Windows App SDK.
        Bootstrap.TryInitialize(0x00010001, out var _);
    }

    [AssemblyCleanup]
    public static void AssemblyCleanup()
    {
        Bootstrap.Shutdown();
    }
}
