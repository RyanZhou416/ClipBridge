using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Models;
using ClipBridgeShell_CS.Stores;
using Microsoft.Extensions.Logging;
using Windows.Storage;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Services;

public sealed class CoreHostService : ICoreHostService
{
    // 壳侧期望的 FFI ABI 主版本：必须与 Rust 导出 cb_get_ffi_version 返回的 major 一致。
    // 依据：crates/cb_core_ffi/src/exports.rs 中 cb_get_ffi_version 返回 1，macOS 壳同样期望 1。
    private const uint ExpectedFfiAbiMajor = 1;

    /// <summary>
    /// 账号已配置但凭据不可读时展示给用户的降级提示（审计 F11）。
    /// </summary>
    private const string CredentialsUnavailableMessage = "无法读取账户凭据，请重新登录/重新设置密码";

    private readonly CoreConfigBuilder _cfgBuilder = new();
    private CoreInterop.CbOnEventFn? _onEventThunk;
    private GCHandle _selfHandle;
    private IntPtr _coreHandle = IntPtr.Zero;
    // 保护 _coreHandle 的“取句柄 → 调用 FFI”与 ShutdownAsync 的“清空 → cb_shutdown”互斥。
    private readonly object _handleLock = new();
    private readonly EventPumpService _eventPump;
    private readonly ILocalSettingsService _localSettingsService;
    private readonly IAccountService? _accountService;
    private readonly ILogger<CoreHostService>? _logger;
    // 定义 JSON 序列化选项
    private readonly JsonSerializerOptions _jsonOpts;
    // 解析 Core 返回的 envelope 时复用同一组选项（与旧 CoreInterop 内部行为保持一致）
    private static readonly JsonSerializerOptions _parseOpts = new()
    {
        PropertyNameCaseInsensitive = true,
        NumberHandling = JsonNumberHandling.AllowReadingFromString
    };
    // 保存当前账号 UID，用于检测账号切换
    private string? _currentAccountUid;

    /// <summary>
    /// 仅供测试宿主使用：置 true 后传给 core 的 config 会带 disable_network=true，
    /// core 不启动 NetManager（不 bind 端口、不起 mDNS），避免 Windows 防火墙弹窗。
    /// 生产路径必须保持 false。
    /// </summary>
    public static bool DisableNetwork
    {
        get; set;
    }

    public CoreState State { get; private set; } = CoreState.NotLoaded;
    public CoreDiagnostics Diagnostics { get; } = new();
    public event Action<CoreState>? StateChanged;
    public string? LastError
    {
        get; private set;
    }

    public CoreHostService(EventPumpService eventPump, ILocalSettingsService localSettingsService, ILoggerFactory? loggerFactory = null, IAccountService? accountService = null)
    {
        _eventPump = eventPump;
        _localSettingsService = localSettingsService;
        _accountService = accountService;
        _logger = loggerFactory?.CreateLogger<CoreHostService>();
        _jsonOpts = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
            WriteIndented = false,
            DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull
        };
    }

    public async Task InitializeAsync(CancellationToken ct = default)
    {
        if (State is CoreState.Loading or CoreState.Ready) return;

        _logger?.LogInformation("CoreHostService initializing...");
        LastError = null;
        SetState(CoreState.Loading);

        // 账号凭据必须在初始化核心之前可用：core 用 account_password 注册 OPAQUE 账号，
        // 空密码 = 任何 LAN peer 都能通过握手并被自动 TOFU 固定（审计 F11，fail-closed）。
        string accountUid;
        string accountPassword;

        if (_accountService == null)
        {
            // 生产 DI 始终注入 IAccountService；缺失时无法证明凭据可用，绝不能回退到空密码。
            FailClosedForCredentials("IAccountService is not available.");
            return;
        }

        var accountState = await _accountService.LoadAccountStateAsync();

        if (!accountState.Configured)
        {
            // 用户名和密码都不存在 = 合法的首次运行：保持原有“等待登录”行为。
            _logger?.LogInformation("No account configured, waiting for account login");
            LastError = "需要登录账号";
            SetState(CoreState.NotLoaded);
            return;
        }

        if (!accountState.PasswordAvailable
            || string.IsNullOrWhiteSpace(accountState.Username)
            || string.IsNullOrWhiteSpace(accountState.Password))
        {
            // 账号存在但密码不可读（PasswordVault 被清空/迁移失败/Windows Hello 失败）。
            FailClosedForCredentials("Account is configured but its password is unavailable.");
            return;
        }

        accountUid = accountState.Username;
        accountPassword = accountState.Password;

        var deviceId = await GetOrCreateDeviceIdAsync();

        // 1. 路径构建（唯一解析入口：便携 / LocalAppData 由 CoreConfigBuilder 决定）
        var dataRoot = CoreConfigBuilder.ResolveDataRootInfo("ClipBridge");
        var paths = _cfgBuilder.BuildPaths("ClipBridge");
        Diagnostics.AppDataDir = paths.AppDataDir;
        Diagnostics.CoreDataDir = paths.CoreDataDir;
        Diagnostics.CacheDir = paths.CacheDir;
        Diagnostics.LogDir = paths.LogDir;

        // 便携模式被请求但 <exeDir>\data 不可写时，解析器已经整体回退到 %LOCALAPPDATA%
        // （不会一半写便携目录、一半写 LocalAppData）。这里必须留下日志；App 启动时还会
        // 弹一次明确的用户提示（见 App.NotifyPortableDataRootFallback）。
        if (dataRoot.PortableFallbackReason is { Length: > 0 } fallbackReason)
        {
            _logger?.LogWarning(
                "Portable data root is not usable; falling back to {Fallback}. Reason: {Reason}",
                dataRoot.Root,
                fallbackReason);
        }
        else if (dataRoot.IsPortable)
        {
            _logger?.LogInformation("Portable data root in use: {Root}", dataRoot.Root);
        }

        string localFolder;
        string cacheFolder;

        if (RuntimeHelper.IsMSIX)
        {
            localFolder = ApplicationData.Current.LocalFolder.Path;
            cacheFolder = ApplicationData.Current.LocalCacheFolder.Path;
        }
        else
        {
            // 不再在这里重复拼路径：统一用 BuildPaths 的结果，避免出现“诊断显示的路径”
            // 与“真正传给 core 的路径”不一致（历史 bug 的来源之一）。
            localFolder = paths.DataDir;
            cacheFolder = paths.CacheDir;
            Directory.CreateDirectory(localFolder);
            Directory.CreateDirectory(cacheFolder);

            // 一次性清理旧布局留下的空目录（core/、logs/、shell/）。
            TryRemoveEmptyLegacyDirs(paths.AppDataDir);
        }

        // 更新当前账号 UID（用于后续检测账号切换）
        _currentAccountUid = accountUid;

        // 防御性兜底：上面的分支已保证密码非空；这里再守一次 FFI 边界的不变量，
        // 任何未来的改动都不得让空密码进入 core。
        if (string.IsNullOrWhiteSpace(accountPassword))
        {
            FailClosedForCredentials("Account password became empty before core init.");
            return;
        }

        var config = new
        {
            device_id = deviceId, // 手动写成 snake_case 属性名，或者依赖 Policy
            device_name = System.Environment.MachineName,
            account_uid = accountUid,
            account_password = accountPassword,
            data_dir = localFolder,
            cache_dir = cacheFolder,

            // 测试宿主可关闭核心网络栈，避免 bind/mDNS 触发 Windows 防火墙弹窗。
            // 生产路径 DisableNetwork 恒为 false。
            disable_network = DisableNetwork,

            // 嵌套的 app_config
            app_config = new
            {
                global_policy = "AllowAll",
                size_limits = new
                {
                    soft_text_bytes = 1024 * 1024
                }
            }
        };

        var configJson = JsonSerializer.Serialize(config, _jsonOpts);

        // 2. 查找 DLL
        var dllFullPath = FindCoreDll("core_ffi_windows.dll");
        Diagnostics.DllPath = dllFullPath;

        if (string.IsNullOrWhiteSpace(dllFullPath) || !File.Exists(dllFullPath))
        {
            _logger?.LogWarning("Core DLL not found");
            Diagnostics.DllLoadError = "DLL not found in app output directories.";
            Diagnostics.LastInitSummary = "Core degraded: DLL missing.";
            LastError = Diagnostics.DllLoadError;
            SetState(CoreState.Degraded);
            return;
        }
        _logger?.LogInformation("Core DLL loaded from: {Path}", dllFullPath);

        try
        {
            // 3. 配置 Resolver（CoreInterop 内部线程安全且幂等）
            if (!CoreInterop.ConfigureDllPath(dllFullPath))
            {
                _logger?.LogWarning("CoreInterop already configured with a different DLL path; keeping the first configuration");
            }

            // 4. 检查 ABI
            CoreInterop.cb_get_ffi_version(out var major, out var minor);
            Diagnostics.FfiAbiMajor = major;
            Diagnostics.FfiAbiMinor = minor;

            // ABI 主版本不匹配是硬失败：继续初始化会在不兼容的 ABI 上调用 FFI，只会静默错乱。
            // 与 init 失败保持一致的对外表现：State=Degraded、LastError、LastInitSummary 都会被设置。
            if (major != ExpectedFfiAbiMajor)
            {
                _logger?.LogError(
                    "FFI ABI mismatch: core reports {CoreMajor}.{CoreMinor}, shell requires major {ExpectedMajor}",
                    major, minor, ExpectedFfiAbiMajor);
                LastError = $"FFI ABI 不兼容：core 返回 {major}.{minor}，壳侧要求 major {ExpectedFfiAbiMajor}。";
                Diagnostics.LastInitSummary = $"Core degraded: FFI ABI mismatch (core {major}.{minor}, shell {ExpectedFfiAbiMajor}.x).";
                SetState(CoreState.Degraded);
                return;
            }

            // 5. 准备回调
            _onEventThunk = (eventJsonPtr, userData) =>
            {
                var json = Marshal.PtrToStringUTF8(eventJsonPtr) ?? "{}";
                _eventPump.Enqueue(json);
            };
            if (!_selfHandle.IsAllocated) _selfHandle = GCHandle.Alloc(this);
            var userData = GCHandle.ToIntPtr(_selfHandle);

            // 6. Init
            // 记录初始化开始时间，用于写入"核心开"日志（使其时间戳早于核心初始化日志）
            // 注意：核心初始化日志是在 cb_init 调用时立即写入的，所以我们需要使用一个更早的时间戳
            var initStartTime = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() - 50; // 减去50ms，确保早于核心初始化日志

            // [FIX] 必须使用 Task.Run 避免阻塞 UI 线程，虽然 init 应该很快
            await Task.Run(() =>
            {
                var envelopePtr = CoreInterop.cb_init(configJson, _onEventThunk, userData);
                var envelopeJson = CoreInterop.PtrToStringAndFree(envelopePtr);
                Diagnostics.LastInitEnvelopeJson = envelopeJson;

                // [FIX 7] 解析结果并提取 Handle
                // 假设成功时 JSON 为: {"ok": true, "data": {"handle": 12345678}} 或 {"ok": true, "data": 12345678}
                if (!TryParseEnvelope<JsonElement>(envelopeJson, out var dataEl, out var errCode, out var errMsg))
                {
                    throw new Exception($"Core error: {errCode} - {errMsg}");
                }

                var handle = ExtractHandle(dataEl, out var handleError);
                if (handleError != null)
                {
                    throw new Exception(handleError);
                }

                if (handle == IntPtr.Zero)
                {
                    throw new Exception("Init OK but returned handle is NULL/Zero.");
                }

                // 句柄的写入也必须与 ShutdownAsync 的读取/清空互斥。
                lock (_handleLock)
                {
                    _coreHandle = handle;
                }
            });

            _logger?.LogInformation("Core initialized successfully, handle: {Handle}", _coreHandle);
            Diagnostics.LastInitSummary = "Init OK";
            SetState(CoreState.Ready);
            StartupTrace.Mark("core-ready");

            // 写入"核心开"日志到核心（使用较早的时间戳，使其出现在核心初始化日志之前）
            try
            {
                var currentLang = GetCurrentLanguage();
                var messageEn = "-------------Core Started-----------";
                var messageZhCn = "-------------核心开-----------";
                WriteLogCore(_coreHandle, 2, "System", "Separator", messageEn, messageZhCn, null, null, initStartTime);
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[CoreHostService] Failed to write 'Core Started' log: {ex}");
            }
        }
        catch (DllNotFoundException ex)
        {
            _logger?.LogError(ex, "Core initialization failed");
            Diagnostics.DllLoadError = $"DllNotFound: {ex.Message}";
            Diagnostics.LastInitSummary = "Core degraded: DLL load failed.";
            LastError = ex.Message;
            SetState(CoreState.Degraded);
        }
        catch (BadImageFormatException ex)
        {
            _logger?.LogError(ex, "Core initialization failed");
            Diagnostics.DllLoadError = $"BadImageFormat: {ex.Message}";
            Diagnostics.LastInitSummary = "Core degraded: Arch mismatch (x64/x86).";
            LastError = ex.Message;
            SetState(CoreState.Degraded);
        }
        catch (Exception ex)
        {
            Diagnostics.LastInitSummary = $"Init failed: {ex.GetType().Name}: {ex.Message}";
            LastError = ex.Message;
            SetState(CoreState.Degraded);
        }
    }

    /// <summary>
    /// 在账号登录后初始化核心
    /// </summary>
    public async Task InitializeWithAccountAsync(CancellationToken ct = default)
    {
        // 获取新的账号信息
        string? newAccountUid = null;
        if (_accountService != null)
        {
            var accountState = await _accountService.LoadAccountStateAsync();
            if (accountState.Configured && accountState.PasswordAvailable)
            {
                newAccountUid = accountState.Username;
            }
        }

        // 检测账号是否改变
        var accountChanged = _currentAccountUid != null && newAccountUid != null && _currentAccountUid != newAccountUid;

        // 如果已经初始化，先关闭
        if (State == CoreState.Ready || State == CoreState.Loading)
        {
            await ShutdownAsync(ct);
        }

        // 如果账号改变了，清理证书文件（在初始化前）
        if (accountChanged)
        {
            _logger?.LogInformation("Account changed from {OldUid} to {NewUid}, clearing certificate", _currentAccountUid, newAccountUid);
            await ClearCertificateFilesAsync();
        }

        // 重新初始化
        await InitializeAsync(ct);

        // 更新当前账号 UID
        _currentAccountUid = newAccountUid;
    }

    /// <summary>
    /// 直接清理证书文件（不依赖核心实例）
    /// </summary>
    private async Task ClearCertificateFilesAsync()
    {
        try
        {
            string localFolder;
            if (RuntimeHelper.IsMSIX)
            {
                localFolder = ApplicationData.Current.LocalFolder.Path;
            }
            else
            {
                // 与 CoreConfigBuilder.BuildPaths 用同一个解析器：便携模式下证书也在 <exeDir>\data。
                localFolder = CoreConfigBuilder.ResolveDataRoot("ClipBridge");
            }

            var tlsDir = System.IO.Path.Combine(localFolder, "tls");

            if (System.IO.Directory.Exists(tlsDir))
            {
                await Task.Run(() =>
                {
                    try
                    {
                        System.IO.Directory.Delete(tlsDir, true);
                        _logger?.LogInformation("Certificate directory cleared: {Path}", tlsDir);
                    }
                    catch (Exception ex)
                    {
                        _logger?.LogWarning(ex, "Failed to clear certificate directory: {Path}", tlsDir);
                    }
                });
            }
        }
        catch (Exception ex)
        {
            _logger?.LogWarning(ex, "Failed to clear certificate files");
        }
    }

    public async Task ShutdownAsync(CancellationToken ct = default)
    {
        if (State == CoreState.NotLoaded)
        {
            return;
        }

        SetState(CoreState.ShuttingDown);

        // 清空句柄与 cb_shutdown 必须在 _handleLock 内完成：
        // 并发的 GetHandle/TryUseHandle 要么在锁前拿到有效句柄并完成调用，
        // 要么在锁后看到 IntPtr.Zero，不会在 Rust 侧释放实例之后继续使用指针。
        await Task.Run(() =>
        {
            lock (_handleLock)
            {
                var handle = _coreHandle;
                _coreHandle = IntPtr.Zero;

                if (handle == IntPtr.Zero)
                {
                    return;
                }

                // 写入"核心关"日志到核心（在关闭前）
                try
                {
                    var currentLang = GetCurrentLanguage();
                    var messageEn = "-------------Core Stopped-----------";
                    var messageZhCn = "-------------核心关-----------";
                    WriteLogCore(handle, 2, "System", "Separator", messageEn, messageZhCn, null, null, 0);
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"[CoreHostService] Failed to write 'Core Stopped' log: {ex}");
                }

                try
                {
                    // cb_shutdown 返回 *const c_char envelope：必须读回并释放，否则每次关停泄漏一个 Rust 分配。
                    var shutdownPtr = CoreInterop.cb_shutdown(handle);
                    var shutdownJson = CoreInterop.PtrToStringAndFree(shutdownPtr);

                    if (TryParseEnvelope<JsonElement>(shutdownJson, out _, out var errCode, out var errMsg))
                    {
                        _logger?.LogInformation("Core shutdown completed.");
                    }
                    else
                    {
                        _logger?.LogWarning("Core shutdown returned an error envelope: {Code} - {Message}", errCode, errMsg);
                    }
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"[CoreHostService] ShutdownAsync: Shutdown error: {ex}");
                }
            }
        });

        if (_selfHandle.IsAllocated)
        {
            _selfHandle.Free();
        }

        SetState(CoreState.NotLoaded);
    }

    /// <summary>
    /// 获取当前核心句柄。
    /// </summary>
    /// <remarks>
    /// 警告：该返回值在锁释放后可能立即失效——并发的 <see cref="ShutdownAsync"/> 会在锁内把句柄置零
    /// 并调用 Rust 的 cb_shutdown。调用方“判空后再调用 FFI”存在 TOCTOU 窗口，可能造成 use-after-free
    /// 或 double-free。新代码请使用 <see cref="TryUseHandle"/> 把调用包在锁内；本方法仅为兼容既有
    /// 调用点而保留，迁移完成后应移除。
    /// </remarks>
    [Obsolete("Use TryUseHandle or a typed ICoreHostService method")]
    public IntPtr GetHandle()
    {
        lock (_handleLock)
        {
            return State == CoreState.Ready && _coreHandle != IntPtr.Zero ? _coreHandle : IntPtr.Zero;
        }
    }

    /// <summary>
    /// 在句柄锁内执行 <paramref name="action"/>，保证执行期间不会发生 <see cref="ShutdownAsync"/> 的释放逻辑。
    /// 这是替代“<see cref="GetHandle"/> 判空后直接调用 FFI”的线程安全用法。
    /// </summary>
    /// <param name="action">接收当前核心句柄的委托；返回 true 表示调用成功。</param>
    /// <returns>句柄可用时返回 <paramref name="action"/> 的结果；句柄不可用时返回 false。</returns>
    public bool TryUseHandle(Func<IntPtr, bool> action)
    {
        if (action is null)
        {
            throw new ArgumentNullException(nameof(action));
        }

        lock (_handleLock)
        {
            var handle = _coreHandle;
            if (State != CoreState.Ready || handle == IntPtr.Zero)
            {
                return false;
            }

            // 帧时序诊断（默认关闭）：所有 cb_* 调用都经过这里，因此在这里统一计时即可覆盖
            // “UI 线程上同步调用 FFI”的情况（超过 5ms 才落盘，标签取调用方方法名）。
            // 未设置 CB_FRAME_TRACE 时 FrameTrace.Enabled 为 false，下面多出的只有一次字段读取。
            if (!FrameTrace.Enabled)
            {
                return action(handle);
            }

            var ffiStart = FrameTrace.BeginFfiCall();
            try
            {
                return action(handle);
            }
            finally
            {
                FrameTrace.EndFfiCall(ffiStart);
            }
        }
    }

    /// <summary>
    /// 账号已配置但凭据不可用时的 fail-closed 出口：不初始化核心/NetManager，
    /// 进入 UI 可展示的 Degraded 状态，并把可操作的原因写入 LastError。
    /// </summary>
    private void FailClosedForCredentials(string logReason)
    {
        _logger?.LogWarning("Refusing to initialize core: {Reason}", logReason);
        LastError = CredentialsUnavailableMessage;
        Diagnostics.LastInitSummary = "Core degraded: account credentials unavailable.";
        SetState(CoreState.Degraded);
    }

    /// <summary>
    /// 一次性清理旧布局遗留的空目录（`%LOCALAPPDATA%\ClipBridge\{core,logs,shell}`）。
    ///
    /// 这些目录是历史 `BuildPaths` 创建的，但运行中的 core 从未使用（data_dir 一直是
    /// LocalAppData\ClipBridge 根目录）。只删除**完全为空**的目录；任何包含文件的目录
    /// 都保留（可能是用户的旧数据，不能替用户做决定）。异常一律吞掉——清理属于尽力而为。
    /// </summary>
    private void TryRemoveEmptyLegacyDirs(string appRoot)
    {
        foreach (var name in new[] { "core", "logs", "shell" })
        {
            try
            {
                var dir = Path.Combine(appRoot, name);
                if (!Directory.Exists(dir))
                {
                    continue;
                }

                // 递归检查：只要有任何文件/子项就不动它。
                if (Directory.EnumerateFileSystemEntries(dir, "*", SearchOption.AllDirectories).Any())
                {
                    continue;
                }

                Directory.Delete(dir, recursive: true);
                _logger?.LogDebug("Removed empty legacy data directory {Dir}", dir);
            }
            catch (Exception ex)
            {
                _logger?.LogDebug(ex, "Failed to remove legacy data directory {Name}", name);
            }
        }
    }

    private void SetState(CoreState s)
    {
        var oldState = State;
        if (oldState == s)
        {
            return;
        }
        State = s;
        StateChanged?.Invoke(s);
    }

    /// <summary>
    /// 统一的 Core envelope 解析入口：{"ok": true, "data": ...} 或 {"error": {"code", "message"}}。
    /// 只有 <c>ok</c> 为 true 才返回成功；此时 <paramref name="data"/> 为 data 节点反序列化的结果
    /// （没有 data 节点时为默认值）。
    /// </summary>
    private static bool TryParseEnvelope<T>(string? json, out T? data, out string? errorCode, out string? errorMessage)
    {
        data = default;
        errorCode = null;
        errorMessage = null;

        if (string.IsNullOrWhiteSpace(json))
        {
            errorMessage = "Empty JSON envelope";
            return false;
        }

        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;

            if (root.ValueKind == JsonValueKind.Object
                && root.TryGetProperty("ok", out var okEl)
                && okEl.ValueKind == JsonValueKind.True)
            {
                if (root.TryGetProperty("data", out var dataEl))
                {
                    data = dataEl.Deserialize<T>(_parseOpts);
                }
                return true;
            }

            if (root.ValueKind == JsonValueKind.Object
                && root.TryGetProperty("error", out var errEl))
            {
                if (errEl.TryGetProperty("code", out var c)) errorCode = c.GetString();
                if (errEl.TryGetProperty("message", out var m)) errorMessage = m.GetString();
            }
            return false;
        }
        catch (Exception ex)
        {
            errorCode = "GEN_INVALID_MESSAGE";
            errorMessage = $"Invalid JSON envelope from core: {ex.Message}";
            return false;
        }
    }

    /// <summary>
    /// 要求 envelope 为成功；否则抛出带 Core 错误码/消息的统一异常。
    /// </summary>
    private static void RequireOk(string? json, string operation)
    {
        if (!TryParseEnvelope<JsonElement>(json, out _, out var errorCode, out var errorMessage))
        {
            throw new InvalidOperationException(
                $"{operation} failed: {errorCode ?? "UNKNOWN"} - {errorMessage ?? "Unknown error"}");
        }
    }

    /// <summary>
    /// 从 init envelope 的 data 节点提取句柄（对象形式或兼容的数字形式）。
    /// </summary>
    private static IntPtr ExtractHandle(JsonElement dataEl, out string? error)
    {
        error = null;

        if (dataEl.ValueKind == JsonValueKind.Object
            && dataEl.TryGetProperty("handle", out var handleElement)
            && handleElement.ValueKind == JsonValueKind.Number)
        {
            return new IntPtr(handleElement.GetInt64());
        }

        if (dataEl.ValueKind == JsonValueKind.Number)
        {
            return new IntPtr(dataEl.GetInt64());
        }

        error = "Missing or invalid 'handle' in data object";
        return IntPtr.Zero;
    }

    /// <summary>
    /// cb_list_peers 返回的原始设备 DTO。字段名与 Core JSON 一致，再映射到 <see cref="Core.Models.Events.PeerMetaPayload"/>。
    /// </summary>
    private sealed class PeerDto
    {
        public PeerDto()
        {
        }

        [JsonPropertyName("device_id")]
        public string? DeviceId
        {
            get; set;
        }

        [JsonPropertyName("device_name")]
        public string? DeviceName
        {
            get; set;
        }

        [JsonPropertyName("state")]
        public string? State
        {
            get; set;
        }

        [JsonPropertyName("last_seen_ts_ms")]
        public long? LastSeenTsMs
        {
            get; set;
        }

        [JsonPropertyName("share_to_peer")]
        public bool? ShareToPeer
        {
            get; set;
        }

        [JsonPropertyName("accept_from_peer")]
        public bool? AcceptFromPeer
        {
            get; set;
        }
    }

    // [FIX] 确保 FindCoreDll 被 InitializeAsync 调用
    private static string? FindCoreDll(string dllName)
    {
        var current = AppContext.BaseDirectory.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        const int maxLevels = 5;
        for (var level = 0; level <= maxLevels && !string.IsNullOrEmpty(current); level++)
        {
            var direct = Path.Combine(current, dllName);
            if (File.Exists(direct)) return direct;
            var winx64 = Path.Combine(current, "win-x64", dllName);
            if (File.Exists(winx64)) return winx64;
            current = Directory.GetParent(current)?.FullName;
        }
        return null;
    }

    public string GetDiagnosticsText()
    {
        return Diagnostics.ToClipboardText();
    }

    public async Task<bool> IngestLocalCopy(string snapshotJson)
    {
        if (State != CoreState.Ready)
            return false;

        return await Task.Run(() =>
        {
            try
            {
                var ptr = IntPtr.Zero;
                if (!TryUseHandle(h =>
                    {
                        // 调用 Core FFI；返回指针在锁外读取并释放
                        ptr = CoreInterop.cb_ingest_local_copy(h, snapshotJson);
                        return true;
                    }))
                {
                    return false;
                }

                // Core 返回值是 {"ok":true,"data":{"meta":...}} 或
                // {"ok":false,"error":{"code":...,"message":...}}；之前直接释放导致拒绝被静默吞掉。
                var envelopeJson = CoreInterop.PtrToStringAndFree(ptr);
                if (!TryParseEnvelope<JsonElement>(envelopeJson, out _, out var errorCode, out var errorMessage))
                {
                    _logger?.LogWarning(
                        "IngestLocalCopy rejected by core: {Code} - {Message}",
                        errorCode ?? "UNKNOWN",
                        errorMessage ?? "Unknown error");
                    System.Diagnostics.Debug.WriteLine(
                        $"[CoreHostService] IngestLocalCopy rejected: {errorCode} - {errorMessage}");
                    return false;
                }

                return true;
            }
            catch (Exception ex)
            {
                _logger?.LogWarning(ex, "IngestLocalCopy failed");
                System.Diagnostics.Debug.WriteLine($"IngestLocalCopy failed: {ex}");
                return false;
            }
        });
    }

    public async Task<HistoryPage> ListHistoryAsync(HistoryQuery query)
    {
        if (State != CoreState.Ready)
        {
            return new HistoryPage();
        }

        return await Task.Run(() =>
        {
            try
            {
                var queryJson = JsonSerializer.Serialize(query, _jsonOpts);
                var ptr = IntPtr.Zero;
                if (!TryUseHandle(h =>
                    {
                        ptr = CoreInterop.cb_list_history(h, queryJson);
                        return true;
                    }))
                {
                    return new HistoryPage();
                }

                var json = CoreInterop.PtrToStringAndFree(ptr);
                if (string.IsNullOrEmpty(json))
                {
                    return new HistoryPage();
                }

                return TryParseEnvelope<HistoryPage>(json, out var page, out _, out _) && page is not null
                    ? page
                    : new HistoryPage();
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"ListHistoryAsync failed: {ex.Message}");
                return new HistoryPage();
            }
        });
    }

    /// <summary>
    /// 从设置中读取 DeviceId，如果没有则生成一个新的并保存
    /// </summary>
    private async Task<string> GetOrCreateDeviceIdAsync()
    {
        const string Key = "Core_DeviceId";

        // 尝试读取
        var id = await _localSettingsService.ReadSettingAsync<string>(Key);

        // 如果为空，说明是第一次运行，生成一个新的
        if (string.IsNullOrEmpty(id))
        {
            id = Guid.NewGuid().ToString(); // 生成 UUID
            await _localSettingsService.SaveSettingAsync(Key, id); // 保存回去
        }

        return id;
    }

    /// <summary>
    /// 获取当前语言设置
    /// </summary>
    private string GetCurrentLanguage()
    {
        try
        {
            var loc = Localizer.Get();
            return loc.GetCurrentLanguage();
        }
        catch
        {
            return "en-US";
        }
    }

    public async Task<string> EnsureContentCachedAsync(string itemId, string? fileId = null)
    {
        return await Task.Run(() =>
        {
            var req = new
            {
                item_id = itemId,
                file_id = fileId,
                part = "content"
            };
            var requestJson = JsonSerializer.Serialize(req, _jsonOpts);

            var resPtr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    resPtr = CoreInterop.cb_ensure_content_cached(h, requestJson);
                    return true;
                }))
            {
                throw new InvalidOperationException("ensure_content_cached failed: Core is not ready");
            }

            var resJson = CoreInterop.PtrToStringAndFree(resPtr);

            if (!TryParseEnvelope<JsonElement>(resJson, out var dataEl, out var errCode, out var errMsg))
            {
                throw new Exception($"ensure_content_cached failed: {errCode} - {errMsg}: {resJson}");
            }

            if (dataEl.ValueKind != JsonValueKind.Object
                || !dataEl.TryGetProperty("transfer_id", out var tidEl)
                || string.IsNullOrWhiteSpace(tidEl.GetString()))
            {
                throw new Exception($"ensure_content_cached missing transfer_id: {resJson}");
            }

            return tidEl.GetString()!;
        });
    }

    /// <summary>
    /// 查询设备列表（从核心）
    /// </summary>
    public List<Core.Models.Events.PeerMetaPayload> ListPeers()
    {
        if (State != CoreState.Ready)
        {
            return new List<Core.Models.Events.PeerMetaPayload>();
        }

        try
        {
            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_list_peers(h);
                    return true;
                }))
            {
                return new List<Core.Models.Events.PeerMetaPayload>();
            }

            var json = CoreInterop.PtrToStringAndFree(ptr);

            if (string.IsNullOrEmpty(json))
                return new List<Core.Models.Events.PeerMetaPayload>();

            if (!TryParseEnvelope<List<PeerDto>>(json, out var dtos, out _, out _) || dtos is null)
                return new List<Core.Models.Events.PeerMetaPayload>();

            var peers = new List<Core.Models.Events.PeerMetaPayload>(dtos.Count);
            foreach (var dto in dtos)
            {
                // 与旧的逐字段解析保持一致：缺失字段走默认值，state == "Online" 即在线。
                var stateStr = dto.State ?? "Offline";
                peers.Add(new Core.Models.Events.PeerMetaPayload
                {
                    DeviceId = dto.DeviceId ?? string.Empty,
                    Name = dto.DeviceName ?? "Unknown",
                    IsOnline = stateStr == "Online",
                    LastSeen = dto.LastSeenTsMs ?? 0,
                    ShareToPeer = dto.ShareToPeer ?? true,
                    AcceptFromPeer = dto.AcceptFromPeer ?? true,
                    ConnectionState = stateStr,
                });
            }

            return peers;
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"ListPeers failed: {ex}");
            return new List<Core.Models.Events.PeerMetaPayload>();
        }
    }

    /// <summary>
    /// 设置单个设备策略
    /// </summary>
    public void SetPeerPolicy(string peerId, bool? shareTo, bool? acceptFrom)
    {
        if (State != CoreState.Ready)
        {
            throw new InvalidOperationException("Core is not ready");
        }

        try
        {
            var policy = new
            {
                peer_id = peerId,
                share_to_peer = shareTo,
                accept_from_peer = acceptFrom
            };

            var json = JsonSerializer.Serialize(policy, _jsonOpts);

            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_set_peer_policy(h, json);
                    return true;
                }))
            {
                throw new InvalidOperationException("Core is not ready");
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "SetPeerPolicy");
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"SetPeerPolicy failed: {ex}");
            throw;
        }
    }

    public void ClearPeerFingerprint(string peerId)
    {
        try
        {
            var request = new
            {
                peer_id = peerId
            };

            var json = JsonSerializer.Serialize(request, _jsonOpts);

            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_clear_peer_fingerprint(h, json);
                    return true;
                }))
            {
                throw new InvalidOperationException("Core not initialized");
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "ClearPeerFingerprint");
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"ClearPeerFingerprint failed: {ex}");
            throw;
        }
    }

    public void ClearLocalCert()
    {
        try
        {
            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_clear_local_cert(h);
                    return true;
                }))
            {
                throw new InvalidOperationException("Core not initialized");
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "ClearLocalCert");
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"ClearLocalCert failed: {ex}");
            throw;
        }
    }

    public Task DeleteItemLocalAsync(string itemId)
    {
        if (State != CoreState.Ready)
            throw new InvalidOperationException("Core is not ready");

        return Task.Run(() =>
        {
            var itemIdJson = JsonSerializer.Serialize(new { item_id = itemId }, _jsonOpts);

            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_delete_item_local(h, itemIdJson);
                    return true;
                }))
            {
                throw new InvalidOperationException("Core is not ready");
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "DeleteItemLocal");
        });
    }

    public Task DeleteItemGlobalAsync(string itemId)
    {
        if (State != CoreState.Ready)
            throw new InvalidOperationException("Core is not ready");

        return Task.Run(() =>
        {
            var itemIdJson = JsonSerializer.Serialize(new { item_id = itemId }, _jsonOpts);

            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_delete_item_global(h, itemIdJson);
                    return true;
                }))
            {
                throw new InvalidOperationException("Core is not ready");
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "DeleteItemGlobal");
        });
    }

    // ==========================================================
    // 9. 日志 (Logs) - UI 层通过 Core Bridge 访问，不再直接调用 CoreInterop
    // ==========================================================

    /// <summary>
    /// 调用 cb_logs_write 写入一条日志；返回核心分配的日志 ID，核心未就绪时返回 0。
    /// 线程安全，可在后台线程调用。
    /// </summary>
    public Task<long> WriteLogAsync(int level, string component, string category, string messageEn,
        string? messageZhCn = null, string? exception = null, string? propsJson = null, long tsUtc = 0)
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(0L);
        }

        return Task.Run(() =>
        {
            long id = 0;
            if (!TryUseHandle(h =>
                {
                    id = WriteLogCore(h, level, component, category, messageEn, messageZhCn, exception, propsJson, tsUtc);
                    return true;
                }))
            {
                return 0L;
            }

            return id;
        });
    }

    public Task<List<LogRow>> QueryLogsLatestAsync(int levelMin, string? like, int limit, string? lang = null)
    {
        return QueryLogsAsync("cb_logs_query_latest", h =>
        {
            var rc = CoreInterop.LogsQueryLatestRaw(h, levelMin, like, limit, lang, out var ptr);
            return (rc, ptr);
        });
    }

    public Task<List<LogRow>> QueryLogsAfterIdAsync(long afterId, int levelMin, string? like, int limit, string? lang = null)
    {
        return QueryLogsAsync("cb_logs_query_after_id", h =>
        {
            var rc = CoreInterop.LogsQueryAfterIdRaw(h, afterId, levelMin, like, limit, lang, out var ptr);
            return (rc, ptr);
        });
    }

    public Task<List<LogRow>> QueryLogsBeforeIdAsync(long beforeId, int levelMin, string? like, int limit, string? lang = null)
    {
        return QueryLogsAsync("cb_logs_query_before_id", h =>
        {
            var rc = CoreInterop.LogsQueryBeforeIdRaw(h, beforeId, levelMin, like, limit, lang, out var ptr);
            return (rc, ptr);
        });
    }

    public Task<List<LogRow>> QueryLogsRangeAsync(long startMs, long endMs, int levelMin, string? like, int limit, int offset, string? lang = null)
    {
        return QueryLogsAsync("cb_logs_query_range", h =>
        {
            var rc = CoreInterop.LogsQueryRangeRaw(h, startMs, endMs, levelMin, like, limit, offset, lang, out var ptr);
            return (rc, ptr);
        });
    }

    /// <summary>
    /// 日志查询的统一骨架：句柄锁内只做 P/Invoke 并接管返回指针，JSON 解析在锁外完成。
    /// </summary>
    private Task<List<LogRow>> QueryLogsAsync(string operation, Func<IntPtr, (int Rc, IntPtr JsonPtr)> invoke)
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(new List<LogRow>());
        }

        return Task.Run(() =>
        {
            var result = (Rc: 0, JsonPtr: IntPtr.Zero);
            if (!TryUseHandle(h =>
                {
                    result = invoke(h);
                    return true;
                }))
            {
                return new List<LogRow>();
            }

            var json = CoreInterop.PtrToStringAndFree(result.JsonPtr);
            if (result.Rc != 0)
            {
                throw new Exception($"{operation} failed: {result.Rc}");
            }

            return TryParseEnvelope<List<LogRow>>(json, out var rows, out _, out _) && rows is not null
                ? rows
                : new List<LogRow>();
        });
    }

    public Task<long> DeleteLogsBeforeAsync(long cutoffMs)
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(0L);
        }

        return Task.Run(() =>
        {
            long deleted = 0;
            var rc = 0;
            if (!TryUseHandle(h =>
                {
                    rc = CoreInterop.cb_logs_delete_before(h, cutoffMs, out deleted);
                    return true;
                }))
            {
                return 0L;
            }

            if (rc != 0)
            {
                throw new Exception($"cb_logs_delete_before failed: {rc}");
            }

            return deleted;
        });
    }

    public Task<long> DeleteLogsByIdsAsync(long[] ids)
    {
        if (ids is null)
        {
            throw new ArgumentNullException(nameof(ids));
        }

        if (State != CoreState.Ready)
        {
            return Task.FromResult(0L);
        }

        return Task.Run(() =>
        {
            var idsJson = JsonSerializer.Serialize(ids);

            long deleted = 0;
            var rc = 0;
            if (!TryUseHandle(h =>
                {
                    rc = CoreInterop.cb_logs_delete_by_ids(h, idsJson, out deleted);
                    return true;
                }))
            {
                return 0L;
            }

            if (rc != 0)
            {
                throw new Exception($"cb_logs_delete_by_ids failed: {rc}");
            }

            return deleted;
        });
    }

    public Task<LogStats> GetLogsStatsAsync()
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(new LogStats());
        }

        return Task.Run(() =>
        {
            var ptr = IntPtr.Zero;
            var rc = 0;
            if (!TryUseHandle(h =>
                {
                    rc = CoreInterop.cb_logs_stats(h, out ptr);
                    return true;
                }))
            {
                return new LogStats();
            }

            var json = CoreInterop.PtrToStringAndFree(ptr);
            if (rc != 0)
            {
                throw new Exception($"cb_logs_stats failed: {rc}");
            }

            return TryParseEnvelope<LogStats>(json, out var stats, out _, out _) && stats is not null
                ? stats
                : new LogStats();
        });
    }

    public Task<SourceStats> GetLogsSourceStatsAsync()
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(new SourceStats());
        }

        return Task.Run(() =>
        {
            var ptr = IntPtr.Zero;
            var rc = 0;
            if (!TryUseHandle(h =>
                {
                    rc = CoreInterop.cb_logs_source_stats(h, out ptr);
                    return true;
                }))
            {
                return new SourceStats();
            }

            var json = CoreInterop.PtrToStringAndFree(ptr);
            if (rc != 0)
            {
                throw new Exception($"cb_logs_source_stats failed: {rc}");
            }

            return TryParseEnvelope<SourceStats>(json, out var stats, out _, out _) && stats is not null
                ? stats
                : new SourceStats();
        });
    }

    public Task<bool> ClearLogsDbAsync()
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(false);
        }

        return Task.Run(() =>
        {
            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_clear_logs_db(h);
                    return true;
                }))
            {
                return false;
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "ClearLogsDb");
            return true;
        });
    }

    // ==========================================================
    // 10. 统计与清库 (Stats / Database) - UI 层通过 Core Bridge 访问
    // ==========================================================

    public Task<CacheStatsResult> QueryCacheStatsAsync(long startTsMs = 0, long endTsMs = 0, int bucketSec = 10)
    {
        return QueryStatsAsync("cb_query_cache_stats", startTsMs, endTsMs, bucketSec, (IntPtr h, string json) => CoreInterop.cb_query_cache_stats(h, json), () => new CacheStatsResult());
    }

    public Task<NetworkStatsResult> QueryNetStatsAsync(long startTsMs = 0, long endTsMs = 0, int bucketSec = 10)
    {
        return QueryStatsAsync("cb_query_net_stats", startTsMs, endTsMs, bucketSec, (IntPtr h, string json) => CoreInterop.cb_query_net_stats(h, json), () => new NetworkStatsResult());
    }

    public Task<ActivityStatsResult> QueryActivityStatsAsync(long startTsMs = 0, long endTsMs = 0, int bucketSec = 60)
    {
        return QueryStatsAsync("cb_query_activity_stats", startTsMs, endTsMs, bucketSec, (IntPtr h, string json) => CoreInterop.cb_query_activity_stats(h, json), () => new ActivityStatsResult());
    }

    private Task<T> QueryStatsAsync<T>(string operation, long startTsMs, long endTsMs, int bucketSec,
        Func<IntPtr, string, IntPtr> invoke, Func<T> emptyFactory)
        where T : class
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(emptyFactory());
        }

        return Task.Run(() =>
        {
            var query = new
            {
                start_ts_ms = startTsMs,
                end_ts_ms = endTsMs,
                bucket_sec = bucketSec
            };
            var queryJson = JsonSerializer.Serialize(query, _jsonOpts);

            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = invoke(h, queryJson);
                    return true;
                }))
            {
                return emptyFactory();
            }

            var json = CoreInterop.PtrToStringAndFree(ptr);
            return TryParseEnvelope<T>(json, out var stats, out _, out _) && stats is not null
                ? stats
                : emptyFactory();
        });
    }

    public Task<bool> ClearCoreDbAsync()
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(false);
        }

        return Task.Run(() =>
        {
            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_clear_core_db(h);
                    return true;
                }))
            {
                return false;
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "ClearCoreDb");
            return true;
        });
    }

    public Task<bool> ClearCacheAsync()
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(false);
        }

        return Task.Run(() =>
        {
            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_clear_cache(h);
                    return true;
                }))
            {
                return false;
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "ClearCache");
            return true;
        });
    }

    public Task<BackupInfo> ExportBackupAsync(string destDir, bool includeContent)
    {
        if (State != CoreState.Ready)
        {
            throw new InvalidOperationException("Core is not ready");
        }

        if (string.IsNullOrWhiteSpace(destDir))
        {
            throw new ArgumentException("Backup destination must not be empty.", nameof(destDir));
        }

        return Task.Run(() =>
        {
            var request = new
            {
                dest_dir = destDir,
                include_content = includeContent
            };
            var requestJson = JsonSerializer.Serialize(request, _jsonOpts);

            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_export_backup(h, requestJson);
                    return true;
                }))
            {
                throw new InvalidOperationException("Core is not ready");
            }

            var json = CoreInterop.PtrToStringAndFree(ptr);
            if (!TryParseEnvelope<BackupInfo>(json, out var info, out var errCode, out var errMsg)
                || info is null)
            {
                throw new InvalidOperationException(
                    $"ExportBackup failed: {errCode ?? "UNKNOWN"} - {errMsg ?? "Unknown error"}");
            }

            return info;
        });
    }

    public Task<bool> ClearStatsDbAsync()
    {
        if (State != CoreState.Ready)
        {
            return Task.FromResult(false);
        }

        return Task.Run(() =>
        {
            var ptr = IntPtr.Zero;
            if (!TryUseHandle(h =>
                {
                    ptr = CoreInterop.cb_clear_stats_db(h);
                    return true;
                }))
            {
                return false;
            }

            RequireOk(CoreInterop.PtrToStringAndFree(ptr), "ClearStatsDb");
            return true;
        });
    }

    private static long WriteLogCore(IntPtr handle, int level, string component, string category, string messageEn,
        string? messageZhCn, string? exception, string? propsJson, long tsUtc)
    {
        var rc = CoreInterop.LogsWriteRaw(handle, level, component, category, messageEn, messageZhCn, exception, propsJson, tsUtc, out var id);
        if (rc != 0)
        {
            throw new Exception($"cb_logs_write failed: {rc}");
        }

        return id;
    }

}
