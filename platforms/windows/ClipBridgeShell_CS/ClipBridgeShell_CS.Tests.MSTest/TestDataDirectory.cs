using ClipBridgeShell_CS.Core.Services;
using ClipBridgeShell_CS.Models;
using ClipBridgeShell_CS.Services;
using Microsoft.Extensions.Options;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 每个测试独占的临时数据根。所有路径固定在
/// <c>%TEMP%\ClipBridgeShell_CS.Tests\&lt;suite&gt;\&lt;guid&gt;\root</c> 下；
/// 同级的 sentinel 文件用来证明清理只删除本测试自己的目录，不越过测试边界。
/// </summary>
internal sealed class TestDataDirectory : IDisposable
{
    private const string SentinelName = "outside-sentinel.txt";
    private static readonly string TestBaseRoot = Path.GetFullPath(
        Path.Combine(Path.GetTempPath(), "ClipBridgeShell_CS.Tests"));

    private readonly string _baseDir;
    private bool _disposed;

    public TestDataDirectory(string suiteName)
    {
        _baseDir = Path.Combine(TestBaseRoot, suiteName, Guid.NewGuid().ToString("N"));
        Root = Path.Combine(_baseDir, "root");
        Directory.CreateDirectory(Root);

        SentinelPath = Path.Combine(_baseDir, SentinelName);
        File.WriteAllText(SentinelPath, "sentinel must survive test cleanup");
    }

    public string Root
    {
        get;
    }

    public string SentinelPath
    {
        get;
    }

    /// <summary>组合 Root 下的相对路径，并拒绝任何 <c>..</c>/绝对路径逃逸。</summary>
    public string PathUnderRoot(params string[] relativeParts)
    {
        var parts = new string[relativeParts.Length + 1];
        parts[0] = Root;
        relativeParts.CopyTo(parts, 1);

        var combined = Path.GetFullPath(Path.Combine(parts));
        AssertContained(combined);
        return combined;
    }

    /// <summary>断言路径与 Root 同根（不跟随符号链接；测试内不使用链接）。</summary>
    public void AssertContained(string? path, string? because = null)
    {
        Assert.IsFalse(string.IsNullOrWhiteSpace(path), because ?? "path must not be empty");
        Assert.IsTrue(
            IsContained(path!),
            $"{because ?? "path"} must stay under the test data root '{Root}', but was '{path}'.");
    }

    public bool IsContained(string path)
    {
        var fullPath = Path.GetFullPath(path);
        var fullRoot = Path.GetFullPath(Root).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        return fullPath.Equals(fullRoot, StringComparison.OrdinalIgnoreCase)
            || fullPath.StartsWith(fullRoot + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>创建与生产默认模式等价、但完全落在临时目录的设置服务。</summary>
    public LocalSettingsService CreateSettingsService(IDataRootProvider? dataRootProvider = null)
    {
        var options = Options.Create(new LocalSettingsOptions
        {
            ApplicationDataFolder = Root,
            LocalSettingsFile = "LocalSettings.json",
        });

        return new LocalSettingsService(
            new FileService(),
            options,
            dataRootProvider ?? new StubDataRootProvider(StubDataRootProvider.Default(Root)));
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;

        // 清理边界：sentinel 必须还在，且只删除本测试的 base 目录。
        if (!File.Exists(SentinelPath))
        {
            throw new InvalidOperationException(
                $"Test cleanup boundary violated: sentinel '{SentinelPath}' was removed.");
        }

        var fullBase = Path.GetFullPath(_baseDir);
        var expectedParent = TestBaseRoot.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        if (!fullBase.StartsWith(expectedParent + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException(
                $"Refusing to delete '{fullBase}': it is outside the test temp root '{expectedParent}'.");
        }

        File.Delete(SentinelPath);

        // 核心关闭后 SQLite/WAL 后台句柄可能还要几毫秒才释放；清理只作用于本测试
        // 独占目录，短暂重试后仍失败就保留目录（下次可用同一临时根排障），绝不越界删除。
        for (var attempt = 0; attempt < 20 && Directory.Exists(fullBase); attempt++)
        {
            try
            {
                Directory.Delete(fullBase, recursive: true);
            }
            catch (IOException ex)
            {
                System.Diagnostics.Debug.WriteLine($"[TestDataDirectory] cleanup attempt {attempt}: {ex.Message}");
                Thread.Sleep(50);
            }
            catch (UnauthorizedAccessException ex)
            {
                System.Diagnostics.Debug.WriteLine($"[TestDataDirectory] cleanup attempt {attempt}: {ex.Message}");
                Thread.Sleep(50);
            }
        }

        if (Directory.Exists(fullBase))
        {
            System.Diagnostics.Debug.WriteLine($"[TestDataDirectory] left test directory behind (still locked): {fullBase}");
        }
    }
}
