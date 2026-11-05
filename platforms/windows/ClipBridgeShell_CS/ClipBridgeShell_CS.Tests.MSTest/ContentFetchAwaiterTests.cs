using ClipBridgeShell_CS.Services;

namespace ClipBridgeShell_CS.Tests.MSTest;

[TestClass]
public class ContentFetchAwaiterTests
{
    [TestMethod]
    public async Task Fail_BeforeWait_ShouldSurfaceImmediatelyToNextWait()
    {
        // Arrange：失败事件先于 WaitAsync 到达（EnsureContentCachedAsync 返回与注册等待之间）
        var awaiter = new ContentFetchAwaiter();
        awaiter.Fail("tr-fail-early", new InvalidOperationException("boom"));

        // Act + Assert：下一次等待必须立即拿到同一个失败，而不是永远挂起
        var ex = await Assert.ThrowsExactlyAsync<InvalidOperationException>(
            async () => await awaiter.WaitAsync(
                "tr-fail-early", CancellationToken.None, TimeSpan.FromMilliseconds(50)));

        Assert.AreEqual("boom", ex.Message);
    }

    [TestMethod]
    public async Task WaitAsync_ShouldTimeout_WhenNoTerminalEvent()
    {
        // Arrange：没有任何 CONTENT_CACHED / TRANSFER_FAILED
        var awaiter = new ContentFetchAwaiter();

        // Act + Assert：必须超时收尾，否则 AsyncRelayCommand 会永久失活
        var ex = await Assert.ThrowsExactlyAsync<TimeoutException>(
            async () => await awaiter.WaitAsync(
                "tr-timeout", CancellationToken.None, TimeSpan.FromMilliseconds(50)));

        StringAssert.Contains(ex.Message, "timed out");
    }

    [TestMethod]
    public async Task Resolve_BeforeWait_ShouldSurfacePendingContent()
    {
        var awaiter = new ContentFetchAwaiter();
        awaiter.Resolve("tr-pending", new LocalContentRef
        {
            TransferId = "tr-pending",
            ItemId = "item-pending",
            TextUtf8 = "hello",
        });

        var local = await awaiter.WaitAsync("tr-pending", CancellationToken.None, TimeSpan.FromMilliseconds(50));

        Assert.AreEqual("tr-pending", local.TransferId);
        Assert.AreEqual("hello", local.TextUtf8);
    }
}
