using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Services;
using ClipBridgeShell_CS.Models;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;
using Microsoft.Extensions.Options;


namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class CoreHostSmokeTests
{
    public TestContext TestContext { get; set; } = null!;

    [TestMethod]
    public async Task Init_Shutdown_Loop_50()
    {
        var historyStore = new HistoryStore();
        var peerStore = new PeerStore();
        var transferStore = new TransferStore();

        var eventPump = new EventPumpService(historyStore, peerStore, transferStore);

        var fileService = new FileService();

        var testDataDir = Path.Combine(Path.GetTempPath(), "ClipBridgeShell_CS.Tests", "CoreHostSmokeTests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(testDataDir);

        var options = Options.Create(new LocalSettingsOptions
        {
            ApplicationDataFolder = testDataDir,
            LocalSettingsFile = "LocalSettings.json"
        });

        var localSettingsService = new LocalSettingsService(fileService, options);

        var coreHost = new CoreHostService(eventPump, localSettingsService);

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
