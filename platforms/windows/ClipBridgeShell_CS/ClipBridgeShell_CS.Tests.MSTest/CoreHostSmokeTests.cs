using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;


namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class CoreHostSmokeTests
{
    public TestContext TestContext { get; set; } = null!;

    [TestMethod]
    public async Task Init_Shutdown_Loop_50()
    {
        // 从 DI 里取你的 CoreHost（如果你希望完全走 App Host）
        // var coreHost = App.GetService<ICoreHostService>();

        // 更建议：这里 new 一个最小 CoreHostService（避免依赖 UI）
        var coreHost = new ClipBridgeShell_CS.Services.CoreHostService();

        for (var i = 1; i <= 50; i++)
        {
            TestContext.WriteLine($"[Loop {i}/50] init...");
            await coreHost.InitializeAsync();

            TestContext.WriteLine($"[Loop {i}/50] state={coreHost.State}");
            Assert.IsTrue(coreHost.State == CoreState.Ready || coreHost.State == CoreState.Degraded);

            TestContext.WriteLine($"[Loop {i}/50] shutdown...");
            await coreHost.ShutdownAsync();
        }

        TestContext.WriteLine("DONE Init_Shutdown_Loop_50");
    }
}
