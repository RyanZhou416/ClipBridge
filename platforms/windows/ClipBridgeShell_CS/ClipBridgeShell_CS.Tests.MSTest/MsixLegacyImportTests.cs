using System.Text;
using System.Text.Json;

using ClipBridgeShell_CS.Services;

using Microsoft.Data.Sqlite;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// MSIX 首启导入旧 MSI/EXE 数据：状态机、原子性、一致性快照、设置/sentinel、幂等与 opt-out。
/// 测试全部使用临时目录和注入的打包判定，不依赖本机是否真的安装了 MSIX。
/// </summary>
[TestClass]
public class MsixLegacyImportTests
{
    private const string DeviceId = "11111111-2222-3333-4444-555555555555";

    private string _baseDir = null!;
    private string _sourceRoot = null!;
    private string _sourceSettingsFile = null!;
    private string _targetRoot = null!;
    private readonly List<IDisposable> _disposables = [];

    [TestInitialize]
    public void Setup()
    {
        _baseDir = Path.Combine(
            Path.GetTempPath(),
            "ClipBridgeShell_CS.Tests",
            "MsixLegacyImportTests",
            Guid.NewGuid().ToString("N"));
        _sourceRoot = Path.Combine(_baseDir, "legacy", "ClipBridge");
        _sourceSettingsFile = Path.Combine(
            _baseDir, "legacy", "ClipBridgeShell_CS", "ApplicationData", "LocalSettings.json");
        _targetRoot = Path.Combine(_baseDir, "target", "LocalState");
        Directory.CreateDirectory(_sourceRoot);
        Directory.CreateDirectory(_targetRoot);
    }

    [TestCleanup]
    public void Cleanup()
    {
        foreach (var disposable in _disposables)
        {
            try
            {
                disposable.Dispose();
            }
            catch
            {
            }
        }

        _disposables.Clear();

        try
        {
            if (Directory.Exists(_baseDir))
            {
                Directory.Delete(_baseDir, recursive: true);
            }
        }
        catch
        {
            // 测试清理尽力而为。
        }
    }

    [TestMethod]
    public void Run_WhenNotPackaged_ReturnsNotPackagedAndDoesNotTouchTarget()
    {
        CreateFullLegacySource();

        // 不传 IsPackaged：走 RuntimeHelper.IsMSIX（测试宿主不是打包应用）。
        var result = MsixLegacyImportService.Run(new MsixLegacyImportOptions
        {
            SourceDataRoot = _sourceRoot,
            SourceSettingsFile = _sourceSettingsFile,
            TargetRoot = _targetRoot,
        });

        Assert.AreEqual(MsixLegacyImportStatus.NotPackaged, result.Status);
        Assert.AreEqual(MsixLegacyImportSkipReason.NotPackaged, result.SkipReason);
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, "core.db")));
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.SentinelFileName)));
        Assert.IsFalse(Directory.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.TempDirectoryName)));
    }

    [TestMethod]
    public void Run_WhenTargetAlreadyHasCoreDb_SkipsWithoutOverwriting()
    {
        CreateFullLegacySource();
        var targetCoreDb = Path.Combine(_targetRoot, "core.db");
        var existingBytes = Encoding.UTF8.GetBytes("existing core db");
        File.WriteAllBytes(targetCoreDb, existingBytes);

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Skipped, result.Status);
        Assert.AreEqual(MsixLegacyImportSkipReason.TargetAlreadyHasCoreDb, result.SkipReason);
        CollectionAssert.AreEqual(existingBytes, File.ReadAllBytes(targetCoreDb));
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, "logs.db")));
        Assert.IsFalse(Directory.Exists(Path.Combine(_targetRoot, "tls")));
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.SentinelFileName)));
        Assert.AreEqual(0, store.Values.Count, "skipped import must not write settings");
    }

    [TestMethod]
    public void Run_WhenSourceCoreDbMissing_Skips()
    {
        CreateSourceSettings();
        File.WriteAllText(Path.Combine(_sourceRoot, "logs.db"), "not a database");

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Skipped, result.Status);
        Assert.AreEqual(MsixLegacyImportSkipReason.SourceCoreDbMissing, result.SkipReason);
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, "core.db")));
        Assert.AreEqual(0, store.Values.Count);
    }

    [TestMethod]
    public void Run_NormalImport_CopiesCoreLogsTlsSettingsAndWritesSentinel()
    {
        CreateFullLegacySource();
        var sourceCoreBytes = File.ReadAllBytes(Path.Combine(_sourceRoot, "core.db"));
        var sourceTlsFiles = Directory.GetFiles(Path.Combine(_sourceRoot, "tls"))
            .ToDictionary(path => Path.GetFileName(path), File.ReadAllBytes);

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Imported, result.Status, result.ToString());
        Assert.IsTrue(result.CopiedCoreDb);
        Assert.IsTrue(result.CopiedLogs);
        Assert.IsTrue(result.CopiedTls);
        Assert.IsTrue(result.CopiedSettings);
        Assert.AreEqual(3, result.CopiedSettingKeys);
        Assert.IsTrue(result.CacheSkipped);

        // core.db 是 VACUUM INTO 快照：结构与数据都在。
        Assert.AreEqual(3, CountRows(Path.Combine(_targetRoot, "core.db"), "items"));
        Assert.AreEqual(2, CountRows(Path.Combine(_targetRoot, "logs.db"), "logs"));

        // tls 三个文件按字节复制。
        foreach (var (name, bytes) in sourceTlsFiles)
        {
            var targetFile = Path.Combine(_targetRoot, "tls", name);
            Assert.IsTrue(File.Exists(targetFile), $"missing tls file {name}");
            CollectionAssert.AreEqual(bytes, File.ReadAllBytes(targetFile));
        }

        // 设置写入 LocalSettings（JSON 字符串语义），Core_DeviceId 必须保留。
        Assert.AreEqual("\"zh-CN\"", store.Values["PreferredLanguage"]);
        Assert.AreEqual("\"false\"", store.Values["IsFirstRun"]);
        Assert.AreEqual($"\"{DeviceId}\"", store.Values["Core_DeviceId"]);
        Assert.IsFalse(store.Values.ContainsKey(MsixLegacyImportService.LegacyImportOptOutKey));

        // sentinel：只记录导入成功，含来源/目标/时间/版本。
        var sentinelPath = Path.Combine(_targetRoot, MsixLegacyImportService.SentinelFileName);
        Assert.IsTrue(File.Exists(sentinelPath));
        using (var sentinel = JsonDocument.Parse(File.ReadAllText(sentinelPath)))
        {
            var root = sentinel.RootElement;
            Assert.AreEqual("imported", root.GetProperty("result").GetString());
            Assert.AreEqual(_sourceRoot, root.GetProperty("source").GetString());
            Assert.AreEqual(_targetRoot, root.GetProperty("target").GetString());
            Assert.AreEqual("0.9.9", root.GetProperty("sourceVersion").GetString());
            Assert.AreEqual("1.0.0.0", root.GetProperty("importedByVersion").GetString());
            Assert.IsFalse(string.IsNullOrWhiteSpace(root.GetProperty("importedAtUtc").GetString()));
            Assert.IsFalse(root.GetProperty("includesCache").GetBoolean());
        }

        // CAS 默认跳过，且不落半个临时目录。
        Assert.IsFalse(Directory.Exists(Path.Combine(_targetRoot, "Cache")));
        Assert.IsFalse(Directory.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.TempDirectoryName)));

        // 旧数据只读：core.db 内容不变。
        CollectionAssert.AreEqual(sourceCoreBytes, File.ReadAllBytes(Path.Combine(_sourceRoot, "core.db")));
    }

    [TestMethod]
    public void Run_WalSourceWithoutCheckpoint_SnapshotContainsCommittedRows()
    {
        CreateCoreDatabase(Path.Combine(_sourceRoot, "core.db"), 4, wal: true, keepConnectionOpen: true);
        CreateSourceSettings();
        File.WriteAllText(Path.Combine(_sourceRoot, "version.txt"), "0.9.9");

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Imported, result.Status, result.ToString());
        Assert.AreEqual(4, CountRows(Path.Combine(_targetRoot, "core.db"), "items"));
    }

    [TestMethod]
    public void Run_CorruptedSourceCoreDb_FailsWithoutHalfProducts()
    {
        var coreDb = Path.Combine(_sourceRoot, "core.db");
        var garbage = Encoding.ASCII.GetBytes(new string('x', 4096));
        File.WriteAllBytes(coreDb, garbage);
        File.WriteAllText(Path.Combine(_sourceRoot, "logs.db"), "not a database");
        CreateSourceTls();
        CreateSourceSettings();

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Failed, result.Status);
        Assert.IsFalse(string.IsNullOrWhiteSpace(result.Error));
        AssertNoHalfProducts(store);
        CollectionAssert.AreEqual(garbage, File.ReadAllBytes(coreDb), "source db must not be modified");
        Assert.IsTrue(
            File.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.ImportLogFileName)),
            "failure diagnostics must be kept");
    }

    [TestMethod]
    public void Run_SourceCoreDbLockedExclusively_FailsWithoutHalfProducts()
    {
        CreateFullLegacySource();
        var coreDb = Path.Combine(_sourceRoot, "core.db");
        var lockStream = new FileStream(coreDb, FileMode.Open, FileAccess.ReadWrite, FileShare.None);
        _disposables.Add(lockStream);

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Failed, result.Status);
        AssertNoHalfProducts(store);
    }

    [TestMethod]
    public void Run_LegacyOptOut_SkipsAndDoesNotImport()
    {
        CreateFullLegacySource(new Dictionary<string, string>
        {
            [MsixLegacyImportService.LegacyImportOptOutKey] = "true",
        });

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Skipped, result.Status);
        Assert.AreEqual(MsixLegacyImportSkipReason.OptOut, result.SkipReason);
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, "core.db")));
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.SentinelFileName)));
        Assert.AreEqual(0, store.Values.Count);
    }

    [TestMethod]
    public void Run_CurrentLocalSettingsOptOut_SkipsAndDoesNotImport()
    {
        CreateFullLegacySource();

        var store = new InMemoryLegacyImportSettingsStore();
        store.Values[MsixLegacyImportService.LegacyImportOptOutKey] = "true";
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Skipped, result.Status);
        Assert.AreEqual(MsixLegacyImportSkipReason.OptOut, result.SkipReason);
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, "core.db")));
        Assert.IsTrue(store.Values.ContainsKey(MsixLegacyImportService.LegacyImportOptOutKey));
    }

    [TestMethod]
    public void Run_SecondCall_IsIdempotent()
    {
        CreateFullLegacySource();

        var store = new InMemoryLegacyImportSettingsStore();
        var first = RunImport(store);
        Assert.AreEqual(MsixLegacyImportStatus.Imported, first.Status, first.ToString());

        var coreDb = Path.Combine(_targetRoot, "core.db");
        var sentinel = Path.Combine(_targetRoot, MsixLegacyImportService.SentinelFileName);
        var coreBytesAfterFirst = File.ReadAllBytes(coreDb);
        var sentinelBytesAfterFirst = File.ReadAllBytes(sentinel);
        var settingsAfterFirst = new Dictionary<string, string>(store.Values, StringComparer.Ordinal);

        var second = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Skipped, second.Status);
        Assert.AreEqual(MsixLegacyImportSkipReason.TargetAlreadyHasCoreDb, second.SkipReason);
        CollectionAssert.AreEqual(coreBytesAfterFirst, File.ReadAllBytes(coreDb));
        CollectionAssert.AreEqual(sentinelBytesAfterFirst, File.ReadAllBytes(sentinel));
        CollectionAssert.AreEquivalent(
            settingsAfterFirst.ToArray(),
            store.Values.ToArray());
        Assert.IsFalse(Directory.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.TempDirectoryName)));
    }

    [TestMethod]
    public void Run_MalformedLegacySettings_FailsWithoutHalfProducts()
    {
        CreateFullLegacySource();
        File.WriteAllText(_sourceSettingsFile, "{ this is not json");

        var store = new InMemoryLegacyImportSettingsStore();
        var result = RunImport(store);

        Assert.AreEqual(MsixLegacyImportStatus.Failed, result.Status);
        AssertNoHalfProducts(store);
    }

    private MsixLegacyImportResult RunImport(ILegacyImportSettingsStore store)
        => MsixLegacyImportService.Run(new MsixLegacyImportOptions
        {
            IsPackaged = true,
            SourceDataRoot = _sourceRoot,
            SourceSettingsFile = _sourceSettingsFile,
            TargetRoot = _targetRoot,
            SettingsStore = store,
            Now = new DateTimeOffset(2026, 11, 16, 12, 0, 0, TimeSpan.Zero),
            AppVersion = "1.0.0.0",
        });

    private void AssertNoHalfProducts(InMemoryLegacyImportSettingsStore store)
    {
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, "core.db")), "core.db must be rolled back");
        Assert.IsFalse(File.Exists(Path.Combine(_targetRoot, "logs.db")), "logs.db must be rolled back");
        Assert.IsFalse(Directory.Exists(Path.Combine(_targetRoot, "tls")), "tls must be rolled back");
        Assert.IsFalse(
            File.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.SentinelFileName)),
            "sentinel must only be written on success");
        Assert.IsFalse(
            Directory.Exists(Path.Combine(_targetRoot, MsixLegacyImportService.TempDirectoryName)),
            "temporary staging directory must be removed");
        Assert.AreEqual(0, store.Values.Count, "settings must be rolled back on failure");
    }

    private void CreateFullLegacySource(IReadOnlyDictionary<string, string>? additionalSettings = null)
    {
        CreateCoreDatabase(Path.Combine(_sourceRoot, "core.db"), 3, wal: false, keepConnectionOpen: false);
        CreateLogsDatabase(Path.Combine(_sourceRoot, "logs.db"), 2);
        CreateSourceTls();
        CreateSourceSettings(additionalSettings);

        // CAS 历史缓存：默认不导入，但会出现在结果标记里。
        var cacheBlobs = Path.Combine(_sourceRoot, "Cache", "blobs", "sha256");
        Directory.CreateDirectory(cacheBlobs);
        File.WriteAllBytes(Path.Combine(cacheBlobs, "abc123"), [1, 2, 3, 4]);

        File.WriteAllText(Path.Combine(_sourceRoot, "version.txt"), "0.9.9");
    }

    private void CreateSourceTls()
    {
        var tls = Path.Combine(_sourceRoot, "tls");
        Directory.CreateDirectory(tls);
        File.WriteAllBytes(Path.Combine(tls, "cert.der"), [0x30, 0x82, 0x01, 0x02]);
        File.WriteAllBytes(Path.Combine(tls, "kek"), Enumerable.Range(0, 32).Select(i => (byte)i).ToArray());
        File.WriteAllBytes(Path.Combine(tls, "key.encrypted"), [9, 8, 7, 6, 5]);
    }

    private void CreateSourceSettings(IReadOnlyDictionary<string, string>? additionalSettings = null)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(_sourceSettingsFile)!);
        var settings = new Dictionary<string, string>(StringComparer.Ordinal)
        {
            ["PreferredLanguage"] = "\"zh-CN\"",
            ["IsFirstRun"] = "\"false\"",
            ["Core_DeviceId"] = $"\"{DeviceId}\"",
        };

        if (additionalSettings is not null)
        {
            foreach (var (key, value) in additionalSettings)
            {
                settings[key] = value;
            }
        }

        File.WriteAllText(
            _sourceSettingsFile,
            JsonSerializer.Serialize(settings),
            new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
    }

    private void CreateCoreDatabase(string path, int rows, bool wal, bool keepConnectionOpen)
    {
        var connection = new SqliteConnection(new SqliteConnectionStringBuilder
        {
            DataSource = path,
            Pooling = false,
        }.ToString());
        connection.Open();

        using (var command = connection.CreateCommand())
        {
            if (wal)
            {
                command.CommandText = "PRAGMA journal_mode=WAL;";
                command.ExecuteNonQuery();
            }

            command.CommandText = "CREATE TABLE items(id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL);";
            command.ExecuteNonQuery();

            for (var i = 1; i <= rows; i++)
            {
                command.CommandText = $"INSERT INTO items(content) VALUES ('row-{i}');";
                command.ExecuteNonQuery();
            }
        }

        // WAL 测试需要连接保持打开，确保 -wal 里还有未 checkpoint 的已提交数据；
        // 连接由 TestCleanup 统一释放，之后才删临时目录。
        if (keepConnectionOpen)
        {
            _disposables.Add(connection);
            return;
        }

        connection.Dispose();
    }

    private static void CreateLogsDatabase(string path, int rows)
    {
        using var connection = new SqliteConnection(new SqliteConnectionStringBuilder
        {
            DataSource = path,
            Pooling = false,
        }.ToString());
        connection.Open();
        using var command = connection.CreateCommand();
        command.CommandText = "CREATE TABLE logs(id INTEGER PRIMARY KEY AUTOINCREMENT, message TEXT NOT NULL);";
        command.ExecuteNonQuery();
        for (var i = 1; i <= rows; i++)
        {
            command.CommandText = $"INSERT INTO logs(message) VALUES ('log-{i}');";
            command.ExecuteNonQuery();
        }
    }

    private static long CountRows(string path, string table)
    {
        using var connection = new SqliteConnection(new SqliteConnectionStringBuilder
        {
            DataSource = path,
            Mode = SqliteOpenMode.ReadOnly,
            Pooling = false,
        }.ToString());
        connection.Open();
        using var command = connection.CreateCommand();
        command.CommandText = $"SELECT COUNT(*) FROM {table}";
        return Convert.ToInt64(command.ExecuteScalar());
    }

    /// <summary>测试用 LocalSettings 替身；值语义与 LocalSettingsService 的 MSIX 分支一致（JSON 字符串）。</summary>
    private sealed class InMemoryLegacyImportSettingsStore : ILegacyImportSettingsStore
    {
        public Dictionary<string, string> Values { get; } = new(StringComparer.Ordinal);

        public bool TryGetValue(string key, out string? jsonValue)
            => Values.TryGetValue(key, out jsonValue);

        public void SetValue(string key, string jsonValue)
            => Values[key] = jsonValue;

        public void RemoveValue(string key)
            => Values.Remove(key);
    }
}
