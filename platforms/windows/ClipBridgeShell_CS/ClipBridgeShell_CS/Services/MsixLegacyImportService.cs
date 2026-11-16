//platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Services/MsixLegacyImportService.cs

using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

using ClipBridgeShell_CS.Helpers;

using Microsoft.Data.Sqlite;

using Windows.ApplicationModel;
using Windows.Storage;

namespace ClipBridgeShell_CS.Services;

/// <summary>MSIX 首启导入旧 MSI/EXE 数据的结果状态。</summary>
public enum MsixLegacyImportStatus
{
    /// <summary>当前进程不是打包应用（MSI/EXE/便携），不执行导入。</summary>
    NotPackaged = 0,

    /// <summary>满足跳过条件（已导入过、旧数据不存在或用户关闭了导入）。</summary>
    Skipped = 1,

    /// <summary>导入成功；sentinel 已写入。</summary>
    Imported = 2,

    /// <summary>导入失败；本次已经清理半成品，旧数据保持原样，下次启动会重试。</summary>
    Failed = 3,
}

/// <summary><see cref="MsixLegacyImportResult.SkipReason"/> 的具体原因。</summary>
public enum MsixLegacyImportSkipReason
{
    None = 0,
    NotPackaged = 1,
    OptOut = 2,
    TargetAlreadyHasCoreDb = 3,
    SourceCoreDbMissing = 4,
}

/// <summary>
/// MSIX 首启导入的结果。所有路径都只用于诊断/UI，不参与后续判定。
/// </summary>
public sealed class MsixLegacyImportResult
{
    public MsixLegacyImportStatus Status
    {
        get; init;
    }

    public MsixLegacyImportSkipReason SkipReason
    {
        get; init;
    }

    public string? SourceRoot
    {
        get; init;
    }

    public string? TargetRoot
    {
        get; init;
    }

    /// <summary>旧版本版本号（尽力而为；旧数据没有版本记录时为 null）。</summary>
    public string? SourceVersion
    {
        get; init;
    }

    public bool CopiedCoreDb
    {
        get; init;
    }

    public bool CopiedLogs
    {
        get; init;
    }

    public bool CopiedTls
    {
        get; init;
    }

    public bool CopiedSettings
    {
        get; init;
    }

    public int CopiedSettingKeys
    {
        get; init;
    }

    /// <summary>旧数据存在 Cache\ 但按默认策略没有导入。</summary>
    public bool CacheSkipped
    {
        get; init;
    }

    public string? SentinelPath
    {
        get; init;
    }

    public string? Error
    {
        get; init;
    }

    public bool IsImported => Status == MsixLegacyImportStatus.Imported;

    public override string ToString()
        => $"{Status}" +
           (SkipReason == MsixLegacyImportSkipReason.None ? string.Empty : $"({SkipReason})") +
           (string.IsNullOrEmpty(Error) ? string.Empty : $" error={Error}");
}

/// <summary>
/// 导入所需的设置存储。打包进程里写入 <see cref="ApplicationData.Current"/> 的
/// LocalSettings；测试注入内存实现，因此未打包的测试宿主永远不会碰到 WinRT API。
/// </summary>
public interface ILegacyImportSettingsStore
{
    /// <summary>读取一个键的原始值（与 LocalSettingsService 一致：JSON 字符串）。</summary>
    bool TryGetValue(string key, out string? jsonValue);

    void SetValue(string key, string jsonValue);

    void RemoveValue(string key);
}

/// <summary>
/// 生产实现：直接读写 <see cref="ApplicationData.Current"/> 的 LocalSettings。
/// 只允许在打包进程里构造（未打包时 WinRT 会抛异常，调用方已先返回 NotPackaged）。
/// </summary>
internal sealed class PackagedLocalSettingsStore : ILegacyImportSettingsStore
{
    public bool TryGetValue(string key, out string? jsonValue)
    {
        if (ApplicationData.Current.LocalSettings.Values.TryGetValue(key, out var raw))
        {
            jsonValue = raw as string ?? JsonSerializer.Serialize(raw);
            return true;
        }

        jsonValue = null;
        return false;
    }

    public void SetValue(string key, string jsonValue)
        => ApplicationData.Current.LocalSettings.Values[key] = jsonValue;

    public void RemoveValue(string key)
        => ApplicationData.Current.LocalSettings.Values.Remove(key);
}

/// <summary>
/// 导入过程的可注入参数。生产调用不传任何参数；测试通过它模拟打包环境与临时目录。
/// </summary>
public sealed class MsixLegacyImportOptions
{
    /// <summary>null = 用 <see cref="RuntimeHelper.IsMSIX"/> 自动判断。</summary>
    public bool? IsPackaged
    {
        get; set;
    }

    /// <summary>旧数据根；默认 &lt;真实 %LOCALAPPDATA%&gt;\ClipBridge。</summary>
    public string? SourceDataRoot
    {
        get; set;
    }

    /// <summary>旧设置文件；默认 &lt;真实 %LOCALAPPDATA%&gt;\ClipBridgeShell_CS\ApplicationData\LocalSettings.json。</summary>
    public string? SourceSettingsFile
    {
        get; set;
    }

    /// <summary>目标根；默认 <see cref="ApplicationData.Current"/> 的 LocalFolder（LocalState）。</summary>
    public string? TargetRoot
    {
        get; set;
    }

    public ILegacyImportSettingsStore? SettingsStore
    {
        get; set;
    }

    public DateTimeOffset? Now
    {
        get; set;
    }

    /// <summary>当前应用版本；null = 打包版本 / 程序集版本。</summary>
    public string? AppVersion
    {
        get; set;
    }
}

/// <summary>
/// MSIX（LocalState）首次启动时，从真实 <c>%LOCALAPPDATA%</c> 下的旧 MSI/EXE 数据导入：
/// <list type="number">
/// <item>只在打包进程执行；</item>
/// <item>LocalState 已有 <c>core.db</c>、旧 data 根没有 <c>core.db</c>、或用户设置
/// <c>LegacyImportOptOut=true</c> 时跳过；</item>
/// <item>SQLite 用只读连接 + <c>VACUUM INTO</c> 生成一致性快照（不是直接拷活库）；</item>
/// <item>先写 <c>&lt;LocalState&gt;\.import-tmp\</c>，全部成功后才逐项 move 到最终位置；
/// 任何一步失败都会清理半成品，旧数据只读、不修改；</item>
/// <item>设置按 <see cref="ILegacyImportSettingsStore"/> 写入 LocalSettings（值为 JSON 字符串），
/// <c>Core_DeviceId</c> 随全量键一起保留；</item>
/// <item>成功后写 sentinel <c>&lt;LocalState&gt;\.legacy-import.json</c>；</item>
/// <item>CAS <c>Cache\</c> 默认不导入，只在结果里标记 <see cref="MsixLegacyImportResult.CacheSkipped"/>。</item>
/// </list>
/// 目标存在性检查与 move 都在同一方法内完成；本类不抛异常，失败一律返回
/// <see cref="MsixLegacyImportStatus.Failed"/> 并保留 <c>legacy-import.log</c> 现场。
/// </summary>
public static class MsixLegacyImportService
{
    /// <summary>“不再导入”开关的键（写在 MSIX LocalSettings，与 LocalSettingsService 的 JSON 字符串一致）。</summary>
    public const string LegacyImportOptOutKey = "LegacyImportOptOut";

    /// <summary>成功导入的 sentinel 文件名（位于 LocalState 根）。</summary>
    public const string SentinelFileName = ".legacy-import.json";

    /// <summary>导入现场诊断日志（位于 LocalState 根，失败也保留）。</summary>
    public const string ImportLogFileName = "legacy-import.log";

    /// <summary>暂存目录（位于 LocalState 根）。成功/失败都会删除。</summary>
    public const string TempDirectoryName = ".import-tmp";

    private const string LegacyDataDirectoryName = "ClipBridge";
    private const string LegacySettingsRelativePath = @"ClipBridgeShell_CS\ApplicationData\LocalSettings.json";
    private const string CoreDbFileName = "core.db";
    private const string LogsDbFileName = "logs.db";
    private const string CacheDirectoryName = "Cache";
    private const string TlsDirectoryName = "tls";

    /// <summary>与旧 core 对应；只复制 ADR 明确列出的三个文件。</summary>
    private static readonly string[] TlsFileNames = ["cert.der", "kek", "key.encrypted"];

    /// <summary>旧设置里可能记录旧版本号的键（按优先级）。</summary>
    private static readonly string[] LegacyVersionKeys = ["App_Version", "AppVersion", "Version"];

    /// <summary>FOLDERID_LocalAppData。</summary>
    private static readonly Guid LocalAppDataFolderId = new("F1B32785-6FBA-4FCF-9D55-7B8E7F157091");

    /// <summary>
    /// 执行一次导入。绝不抛异常：任何失败都返回 <see cref="MsixLegacyImportStatus.Failed"/>。
    /// </summary>
    public static MsixLegacyImportResult Run(MsixLegacyImportOptions? options = null)
    {
        options ??= new MsixLegacyImportOptions();
        var isPackaged = options.IsPackaged ?? RuntimeHelper.IsMSIX;
        if (!isPackaged)
        {
            return new MsixLegacyImportResult
            {
                Status = MsixLegacyImportStatus.NotPackaged,
                SkipReason = MsixLegacyImportSkipReason.NotPackaged,
            };
        }

        var now = options.Now ?? DateTimeOffset.Now;
        var appVersion = options.AppVersion ?? TryGetPackageVersion() ?? TryGetAssemblyVersion();

        string? targetRoot = null;
        string? sourceRoot = null;
        string? sourceResolution = null;
        string? legacySettingsFile = null;
        string? tempDirectory = null;
        var rollback = new List<Action>();
        var sourceVersion = (string?)null;
        var copiedLogs = false;
        var copiedTls = false;
        var copiedSettings = false;
        var copiedSettingKeys = 0;
        var cacheSkipped = false;
        long sourceSchemaVersion = 0;

        try
        {
            targetRoot = options.TargetRoot ?? ApplicationData.Current.LocalFolder.Path;
            Directory.CreateDirectory(targetRoot);

            (sourceRoot, legacySettingsFile, sourceResolution) = ResolveSource(options);
            AppendImportLog(
                targetRoot,
                $"start packaged=True target={targetRoot} source={sourceRoot} sourceResolution={sourceResolution} " +
                $"legacySettings={legacySettingsFile} appVersion={appVersion ?? "unknown"}");

            var settingsStore = options.SettingsStore ?? new PackagedLocalSettingsStore();

            // ① 用户已经选择“不再导入”（当前 MSIX 设置，优先级最高）。
            if (settingsStore.TryGetValue(LegacyImportOptOutKey, out var currentOptOut)
                && IsTruthyJson(currentOptOut))
            {
                AppendImportLog(targetRoot, "skip: LegacyImportOptOut=true (current LocalSettings)");
                return Skipped(
                    MsixLegacyImportSkipReason.OptOut, sourceRoot, targetRoot, sourceVersion, cacheSkipped);
            }

            // ② 读旧设置（即使稍后跳过数据导入，也要先尊重旧数据里的 opt-out）。
            var legacySettings = ReadLegacySettings(legacySettingsFile);
            if (legacySettings is not null
                && legacySettings.TryGetValue(LegacyImportOptOutKey, out var legacyOptOut)
                && IsTruthyJson(legacyOptOut))
            {
                AppendImportLog(targetRoot, "skip: LegacyImportOptOut=true (legacy LocalSettings.json)");
                return Skipped(
                    MsixLegacyImportSkipReason.OptOut, sourceRoot, targetRoot, sourceVersion, cacheSkipped);
            }

            sourceVersion = TryReadLegacyVersion(legacySettings, sourceRoot);

            // ③ 目标已经有 core.db：已经导入过（或 core 已运行），绝不覆盖。
            var targetCoreDb = Path.Combine(targetRoot, CoreDbFileName);
            if (File.Exists(targetCoreDb))
            {
                AppendImportLog(targetRoot, "skip: target core.db already exists");
                return Skipped(
                    MsixLegacyImportSkipReason.TargetAlreadyHasCoreDb,
                    sourceRoot, targetRoot, sourceVersion, cacheSkipped);
            }

            // ④ 旧数据不存在 core.db：没有什么可导入（tls/设置单独存在也不导入，避免
            //    产生“有设置/证书但没有历史”的半接管状态）。
            var sourceCoreDb = Path.Combine(sourceRoot!, CoreDbFileName);
            if (!File.Exists(sourceCoreDb))
            {
                AppendImportLog(targetRoot, $"skip: source core.db missing ({sourceCoreDb})");
                return Skipped(
                    MsixLegacyImportSkipReason.SourceCoreDbMissing,
                    sourceRoot, targetRoot, sourceVersion, cacheSkipped);
            }

            cacheSkipped = HasCacheContent(Path.Combine(sourceRoot!, CacheDirectoryName));
            var sourceCacheDir = Path.Combine(sourceRoot!, CacheDirectoryName);
            AppendImportLog(
                targetRoot,
                $"source core.db found size={SafeFileLength(sourceCoreDb)} cacheSkipped={cacheSkipped} " +
                $"cacheDir={sourceCacheDir} cacheExists={Directory.Exists(sourceCacheDir)}");

            // ⑤ 全部内容先写进 LocalState 下的暂存目录。
            tempDirectory = Path.Combine(targetRoot, TempDirectoryName);
            TryDeleteDirectory(tempDirectory);
            Directory.CreateDirectory(tempDirectory);

            var tempCoreDb = Path.Combine(tempDirectory, CoreDbFileName);
            sourceSchemaVersion = SnapshotSqlite(sourceCoreDb, tempCoreDb);

            var sourceLogsDb = Path.Combine(sourceRoot!, LogsDbFileName);
            var tempLogsDb = Path.Combine(tempDirectory, LogsDbFileName);
            if (File.Exists(sourceLogsDb))
            {
                SnapshotSqlite(sourceLogsDb, tempLogsDb);
                copiedLogs = true;
            }

            var tempTls = Path.Combine(tempDirectory, TlsDirectoryName);
            var stagedTlsFiles = CopyLegacyTls(Path.Combine(sourceRoot!, TlsDirectoryName), tempTls);

            // ⑥ 发布：目标 core.db 是“已导入”的唯一判据，必须最后再确认一次。
            if (File.Exists(targetCoreDb))
            {
                AppendImportLog(targetRoot, "skip: target core.db appeared while staging; keeping existing");
                return Skipped(
                    MsixLegacyImportSkipReason.TargetAlreadyHasCoreDb,
                    sourceRoot, targetRoot, sourceVersion, cacheSkipped, copiedLogs, copiedTls, copiedSettings,
                    copiedSettingKeys);
            }

            // 先发布数据文件，再写设置，最后写 sentinel（任何一步失败都会回滚已发布内容）。
            File.Move(tempCoreDb, targetCoreDb);
            rollback.Add(() => TryDeleteFile(targetCoreDb));

            if (copiedLogs)
            {
                var targetLogsDb = Path.Combine(targetRoot, LogsDbFileName);
                File.Move(tempLogsDb, targetLogsDb, overwrite: true);
                rollback.Add(() => TryDeleteFile(targetLogsDb));
            }

            if (stagedTlsFiles.Count > 0)
            {
                var targetTls = Path.Combine(targetRoot, TlsDirectoryName);
                if (Directory.Exists(targetTls) && Directory.EnumerateFileSystemEntries(targetTls).Any())
                {
                    // 正常首启不会出现；出现时保留现有证书目录，绝不覆盖用户的密钥材料。
                    AppendImportLog(
                        targetRoot,
                        $"tls: target tls already has entries; keeping existing (staged={string.Join(",", stagedTlsFiles)})");
                }
                else
                {
                    if (Directory.Exists(targetTls))
                    {
                        Directory.Delete(targetTls, recursive: false);
                    }

                    Directory.Move(tempTls, targetTls);
                    rollback.Add(() => TryDeleteDirectory(targetTls));
                    copiedTls = true;
                }
            }

            if (legacySettings is { Count: > 0 })
            {
                copiedSettingKeys = ApplySettings(settingsStore, legacySettings, rollback);
                copiedSettings = copiedSettingKeys > 0;
            }

            var importedSettingKeys = legacySettings is { Count: > 0 }
                ? string.Join(",", legacySettings.Keys.OrderBy(key => key, StringComparer.Ordinal))
                : "-";

            var sentinelPath = Path.Combine(targetRoot, SentinelFileName);
            var previousSentinel = CaptureFileOrNull(sentinelPath);
            if (previousSentinel is not null)
            {
                rollback.Add(() => TryRestoreFile(sentinelPath, previousSentinel));
            }

            var sentinelTemp = Path.Combine(tempDirectory, "sentinel.json");
            var sentinelJson = JsonSerializer.Serialize(
                new
                {
                    schemaVersion = 1,
                    result = "imported",
                    source = sourceRoot,
                    target = targetRoot,
                    sourceVersion,
                    sourceSchemaVersion,
                    importedByVersion = appVersion,
                    importedAtUtc = now.ToUniversalTime().ToString("o"),
                    includesCache = false,
                    cacheSkipped,
                    copied = new
                    {
                        coreDb = true,
                        logs = copiedLogs,
                        tls = copiedTls,
                        settings = copiedSettings,
                    },
                    settingsKeys = copiedSettingKeys,
                    tlsFiles = stagedTlsFiles,
                },
                new JsonSerializerOptions { WriteIndented = true });
            File.WriteAllText(sentinelTemp, sentinelJson, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
            File.Move(sentinelTemp, sentinelPath, overwrite: true);

            AppendImportLog(
                targetRoot,
                $"imported coreDb={SafeFileLength(targetCoreDb)} logs={copiedLogs} tls={copiedTls}({stagedTlsFiles.Count}) " +
                $"settings={copiedSettings}({copiedSettingKeys} keys: {importedSettingKeys}) cacheSkipped={cacheSkipped} " +
                $"sentinel={sentinelPath}");

            return new MsixLegacyImportResult
            {
                Status = MsixLegacyImportStatus.Imported,
                SkipReason = MsixLegacyImportSkipReason.None,
                SourceRoot = sourceRoot,
                TargetRoot = targetRoot,
                SourceVersion = sourceVersion,
                CopiedCoreDb = true,
                CopiedLogs = copiedLogs,
                CopiedTls = copiedTls,
                CopiedSettings = copiedSettings,
                CopiedSettingKeys = copiedSettingKeys,
                CacheSkipped = cacheSkipped,
                SentinelPath = sentinelPath,
            };
        }
        catch (Exception ex)
        {
            // 回滚已发布的内容（逆序），确保 LocalState 里不留“半个导入”。
            for (var i = rollback.Count - 1; i >= 0; i--)
            {
                try
                {
                    rollback[i]();
                }
                catch (Exception rollbackError)
                {
                    Debug.WriteLine($"[MsixLegacyImport] rollback step failed: {rollbackError}");
                }
            }

            if (targetRoot is not null)
            {
                AppendImportLog(targetRoot, $"failed: {ex.GetType().Name}: {ex}");
            }

            Debug.WriteLine($"[MsixLegacyImport] import failed: {ex}");
            return new MsixLegacyImportResult
            {
                Status = MsixLegacyImportStatus.Failed,
                SkipReason = MsixLegacyImportSkipReason.None,
                SourceRoot = sourceRoot,
                TargetRoot = targetRoot,
                SourceVersion = sourceVersion,
                CopiedCoreDb = false,
                CopiedLogs = false,
                CopiedTls = false,
                CopiedSettings = false,
                CopiedSettingKeys = 0,
                CacheSkipped = cacheSkipped,
                Error = $"{ex.GetType().Name}: {ex.Message}",
            };
        }
        finally
        {
            // 暂存目录无论成功失败都不能留下（回滚只处理已发布项）。
            if (tempDirectory is not null)
            {
                TryDeleteDirectory(tempDirectory);
            }
        }
    }

    /// <summary>
    /// 解析旧数据根与旧设置文件，并记录解析依据（打包进程里 %LOCALAPPDATA% 可能被
    /// AppData 虚拟化，这里优先用 <c>SHGetKnownFolderPath</c>，再和
    /// <see cref="Environment.GetFolderPath(Environment.SpecialFolder)"/> 的结果互相校验）。
    /// </summary>
    private static (string SourceRoot, string SettingsFile, string Resolution) ResolveSource(
        MsixLegacyImportOptions options)
    {
        if (!string.IsNullOrWhiteSpace(options.SourceDataRoot))
        {
            var settingsFile = options.SourceSettingsFile
                ?? Path.Combine(
                    Path.GetDirectoryName(options.SourceDataRoot) ?? options.SourceDataRoot,
                    @"ClipBridgeShell_CS\ApplicationData\LocalSettings.json");
            return (
                options.SourceDataRoot,
                settingsFile,
                "explicit override (test)");
        }

        var environmentPath = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        var knownFolderPath = TryGetKnownFolderPath(LocalAppDataFolderId);

        var candidates = new List<(string Path, string Via)>();
        if (!string.IsNullOrWhiteSpace(knownFolderPath))
        {
            candidates.Add((knownFolderPath!, "SHGetKnownFolderPath"));
        }

        if (!string.IsNullOrWhiteSpace(environmentPath)
            && !string.Equals(environmentPath, knownFolderPath, StringComparison.OrdinalIgnoreCase))
        {
            candidates.Add((environmentPath, "Environment.GetFolderPath"));
        }

        var detail = string.Join(
            " | ",
            candidates.Select(c => $"{c.Via}={c.Path} core.db={File.Exists(Path.Combine(c.Path, LegacyDataDirectoryName, CoreDbFileName))}"));

        // 优先选“真实能看到旧 core.db”的那份路径：虚拟化路径通常只有本包写过的文件。
        foreach (var candidate in candidates)
        {
            if (File.Exists(Path.Combine(candidate.Path, LegacyDataDirectoryName, CoreDbFileName)))
            {
                return (
                    Path.Combine(candidate.Path, LegacyDataDirectoryName),
                    Path.Combine(candidate.Path, LegacySettingsRelativePath),
                    $"{detail} -> chosen {candidate.Via}");
            }
        }

        var chosen = candidates.FirstOrDefault();
        if (string.IsNullOrWhiteSpace(chosen.Path))
        {
            throw new InvalidOperationException(
                "cannot resolve LocalApplicationData (both SHGetKnownFolderPath and Environment.GetFolderPath failed)");
        }

        return (
            Path.Combine(chosen.Path, LegacyDataDirectoryName),
            Path.Combine(chosen.Path, LegacySettingsRelativePath),
            $"{detail} -> chosen {chosen.Via} (legacy core.db not found)");
    }

    /// <summary>
    /// 用只读连接 + <c>VACUUM INTO</c> 生成一致性快照；返回源库 <c>PRAGMA user_version</c>
    /// （仅写进 sentinel，便于诊断）。<c>VACUUM INTO</c> 是读事务，会包含已提交的 WAL 内容，
    /// 不依赖调用方先 checkpoint，也绝不修改源库。
    /// </summary>
    private static long SnapshotSqlite(string sourcePath, string destinationPath)
    {
        var connectionString = new SqliteConnectionStringBuilder
        {
            DataSource = sourcePath,
            Mode = SqliteOpenMode.ReadOnly,
            Cache = SqliteCacheMode.Private,
            Pooling = false,
            DefaultTimeout = 10,
        }.ToString();

        using var connection = new SqliteConnection(connectionString);
        connection.Open();

        long userVersion;
        using (var versionCommand = connection.CreateCommand())
        {
            versionCommand.CommandText = "PRAGMA user_version";
            userVersion = Convert.ToInt64(versionCommand.ExecuteScalar() ?? 0L);
        }

        using var vacuum = connection.CreateCommand();
        vacuum.CommandText = "VACUUM INTO $destination";
        vacuum.Parameters.AddWithValue("$destination", destinationPath);
        vacuum.ExecuteNonQuery();

        if (!File.Exists(destinationPath))
        {
            throw new InvalidOperationException($"VACUUM INTO produced no snapshot: {destinationPath}");
        }

        return userVersion;
    }

    /// <summary>
    /// 读取旧 LocalSettings.json（JSON 对象）。返回 null 表示文件不存在；
    /// 空文件视为空对象；结构/JSON 非法时抛异常（整次导入失败，避免丢 <c>Core_DeviceId</c>）。
    /// 每个值都转换成与 LocalSettingsService 相同的“JSON 字符串”语义。
    /// </summary>
    private static Dictionary<string, string>? ReadLegacySettings(string? path)
    {
        if (string.IsNullOrWhiteSpace(path) || !File.Exists(path))
        {
            return null;
        }

        var text = File.ReadAllText(path);
        if (string.IsNullOrWhiteSpace(text))
        {
            return new Dictionary<string, string>(StringComparer.Ordinal);
        }

        using var document = JsonDocument.Parse(text);
        if (document.RootElement.ValueKind != JsonValueKind.Object)
        {
            throw new InvalidDataException($"legacy LocalSettings.json is not a JSON object: {path}");
        }

        var values = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var property in document.RootElement.EnumerateObject())
        {
            var raw = property.Value.ValueKind == JsonValueKind.String
                ? property.Value.GetString() ?? string.Empty
                : property.Value.GetRawText();
            values[property.Name] = IsValidJson(raw) ? raw : JsonSerializer.Serialize(raw);
        }

        return values;
    }

    /// <summary>
    /// 把旧设置写入目标 LocalSettings；返回写入的键数。每个键的旧值都登记到 rollback，
    /// 失败时恢复原状（包括“原先不存在”的键）。
    /// </summary>
    private static int ApplySettings(
        ILegacyImportSettingsStore store,
        IReadOnlyDictionary<string, string> settings,
        List<Action> rollback)
    {
        var applied = new List<(string Key, bool Existed, string? PreviousValue)>();
        foreach (var (key, value) in settings)
        {
            var existed = store.TryGetValue(key, out var previousValue);
            store.SetValue(key, value);
            applied.Add((key, existed, previousValue));
        }

        rollback.Add(() =>
        {
            for (var i = applied.Count - 1; i >= 0; i--)
            {
                var item = applied[i];
                if (item.Existed)
                {
                    store.SetValue(item.Key, item.PreviousValue ?? string.Empty);
                }
                else
                {
                    store.RemoveValue(item.Key);
                }
            }
        });

        return applied.Count;
    }

    /// <summary>只复制 tls 目录里 ADR 明确列出的三个文件；返回实际暂存的文件名。</summary>
    private static List<string> CopyLegacyTls(string sourceTlsDirectory, string destinationTlsDirectory)
    {
        var copied = new List<string>();
        if (!Directory.Exists(sourceTlsDirectory))
        {
            return copied;
        }

        Directory.CreateDirectory(destinationTlsDirectory);
        foreach (var fileName in TlsFileNames)
        {
            var source = Path.Combine(sourceTlsDirectory, fileName);
            if (!File.Exists(source))
            {
                continue;
            }

            File.Copy(source, Path.Combine(destinationTlsDirectory, fileName), overwrite: true);
            copied.Add(fileName);
        }

        return copied;
    }

    /// <summary>旧版本号：优先旧设置里的显式键，其次旧数据根下的 version.txt。</summary>
    private static string? TryReadLegacyVersion(
        IReadOnlyDictionary<string, string>? legacySettings,
        string sourceRoot)
    {
        if (legacySettings is not null)
        {
            foreach (var key in LegacyVersionKeys)
            {
                if (!legacySettings.TryGetValue(key, out var json) || string.IsNullOrWhiteSpace(json))
                {
                    continue;
                }

                var value = JsonStringValue(json);
                if (!string.IsNullOrWhiteSpace(value))
                {
                    return value;
                }
            }
        }

        try
        {
            var versionFile = Path.Combine(sourceRoot, "version.txt");
            if (File.Exists(versionFile))
            {
                var text = File.ReadAllText(versionFile).Trim();
                if (text.Length > 0)
                {
                    return text;
                }
            }
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] read version.txt failed: {ex.Message}");
        }

        return null;
    }

    /// <summary>Cache\ 存在任意条目即视为“有历史 CAS 内容但被跳过”。</summary>
    private static bool HasCacheContent(string cacheDirectory)
    {
        try
        {
            return Directory.Exists(cacheDirectory)
                && Directory.EnumerateFileSystemEntries(cacheDirectory).Any();
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] inspect cache failed: {ex.Message}");
            return Directory.Exists(cacheDirectory);
        }
    }

    /// <summary>JSON 布尔判定：兼容 true、"true"、1，以及双层 JSON 字符串。</summary>
    private static bool IsTruthyJson(string? json)
    {
        if (string.IsNullOrWhiteSpace(json))
        {
            return false;
        }

        try
        {
            using var document = JsonDocument.Parse(json);
            return document.RootElement.ValueKind switch
            {
                JsonValueKind.True => true,
                JsonValueKind.False => false,
                JsonValueKind.Number => document.RootElement.TryGetInt64(out var number) && number != 0,
                JsonValueKind.String => bool.TryParse(document.RootElement.GetString(), out var parsed) && parsed,
                _ => false,
            };
        }
        catch
        {
            return bool.TryParse(json.Trim(), out var parsed) && parsed;
        }
    }

    private static string? JsonStringValue(string json)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            return document.RootElement.ValueKind switch
            {
                JsonValueKind.String => document.RootElement.GetString(),
                JsonValueKind.Null => null,
                _ => document.RootElement.GetRawText(),
            };
        }
        catch
        {
            return json.Trim();
        }
    }

    private static bool IsValidJson(string text)
    {
        if (string.IsNullOrWhiteSpace(text))
        {
            return false;
        }

        try
        {
            using var _ = JsonDocument.Parse(text);
            return true;
        }
        catch
        {
            return false;
        }
    }

    private static MsixLegacyImportResult Skipped(
        MsixLegacyImportSkipReason reason,
        string? sourceRoot,
        string targetRoot,
        string? sourceVersion,
        bool cacheSkipped,
        bool copiedLogs = false,
        bool copiedTls = false,
        bool copiedSettings = false,
        int copiedSettingKeys = 0)
        => new()
        {
            Status = MsixLegacyImportStatus.Skipped,
            SkipReason = reason,
            SourceRoot = sourceRoot,
            TargetRoot = targetRoot,
            SourceVersion = sourceVersion,
            CopiedCoreDb = false,
            CopiedLogs = copiedLogs,
            CopiedTls = copiedTls,
            CopiedSettings = copiedSettings,
            CopiedSettingKeys = copiedSettingKeys,
            CacheSkipped = cacheSkipped,
        };

    private static string? TryGetPackageVersion()
    {
        try
        {
            var version = Package.Current.Id.Version;
            return $"{version.Major}.{version.Minor}.{version.Build}.{version.Revision}";
        }
        catch
        {
            // 未打包（测试宿主）时 Package.Current 会抛异常。
            return null;
        }
    }

    private static string? TryGetAssemblyVersion()
    {
        try
        {
            return typeof(MsixLegacyImportService).Assembly.GetName().Version?.ToString();
        }
        catch
        {
            return null;
        }
    }

    private static string? TryGetKnownFolderPath(Guid folderId)
    {
        var pathPointer = IntPtr.Zero;
        try
        {
            var id = folderId;
            if (SHGetKnownFolderPath(ref id, 0, IntPtr.Zero, out pathPointer) != 0 || pathPointer == IntPtr.Zero)
            {
                return null;
            }

            var path = Marshal.PtrToStringUni(pathPointer);
            return string.IsNullOrWhiteSpace(path) ? null : path;
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] SHGetKnownFolderPath failed: {ex.Message}");
            return null;
        }
        finally
        {
            if (pathPointer != IntPtr.Zero)
            {
                Marshal.FreeCoTaskMem(pathPointer);
            }
        }
    }

    private static void AppendImportLog(string? targetRoot, string message)
    {
        if (string.IsNullOrWhiteSpace(targetRoot))
        {
            return;
        }

        try
        {
            Directory.CreateDirectory(targetRoot);
            var line = $"{DateTimeOffset.Now:o}\t{message}{Environment.NewLine}";
            File.AppendAllText(
                Path.Combine(targetRoot, ImportLogFileName),
                line,
                new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] append log failed: {ex.Message}");
        }
    }

    private static long SafeFileLength(string path)
    {
        try
        {
            return new FileInfo(path).Length;
        }
        catch
        {
            return -1;
        }
    }

    private static byte[]? CaptureFileOrNull(string path)
    {
        try
        {
            return File.Exists(path) ? File.ReadAllBytes(path) : null;
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] capture file failed ({path}): {ex.Message}");
            return null;
        }
    }

    private static void TryRestoreFile(string path, byte[] content)
    {
        try
        {
            File.WriteAllBytes(path, content);
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] restore file failed ({path}): {ex.Message}");
        }
    }

    private static void TryDeleteFile(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                File.Delete(path);
            }
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] delete file failed ({path}): {ex.Message}");
        }
    }

    private static void TryDeleteDirectory(string path)
    {
        try
        {
            if (Directory.Exists(path))
            {
                Directory.Delete(path, recursive: true);
            }
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[MsixLegacyImport] delete directory failed ({path}): {ex.Message}");
        }
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int SHGetKnownFolderPath(
        ref Guid rfid,
        uint dwFlags,
        IntPtr hToken,
        out IntPtr ppszPath);
}
