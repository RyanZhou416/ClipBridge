using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Stores;


namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class CoreHostSmokeTests
{
    public TestContext TestContext { get; set; } = null!;

    [TestMethod]
    public async Task Init_Shutdown_Loop_50()
    {
        var historyStore = new HistoryStore();
        var eventPump = new ClipBridgeShell_CS.Services.EventPumpService(historyStore);
        var coreHost = new ClipBridgeShell_CS.Services.CoreHostService(eventPump);

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
