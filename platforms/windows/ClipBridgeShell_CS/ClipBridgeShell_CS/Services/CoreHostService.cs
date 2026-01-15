using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Services;
using ClipBridgeShell_CS.Interop;
using ClipBridgeShell_CS.Stores;
using Windows.Storage;
using Microsoft.Extensions.Logging;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Services;

public sealed class CoreHostService : ICoreHostService
{
    private readonly CoreConfigBuilder _cfgBuilder = new();
    private CoreInterop.CbOnEventFn? _onEventThunk;
    private GCHandle _selfHandle;
    private IntPtr _coreHandle = IntPtr.Zero;
    private static bool _isResolverSet = false;
    private readonly EventPumpService _eventPump;
    private readonly ILocalSettingsService _localSettingsService;
    private readonly ILogger<CoreHostService>? _logger;
    // 定义 JSON 序列化选项
    private readonly JsonSerializerOptions _jsonOpts;

    public CoreState State { get; private set; } = CoreState.NotLoaded;
    public CoreDiagnostics Diagnostics { get; } = new();
    public event Action<CoreState>? StateChanged;
    public string? LastError
    {
        get; private set;
    }

    public CoreHostService(EventPumpService eventPump, ILocalSettingsService localSettingsService, ILoggerFactory? loggerFactory = null)
    {
        _eventPump = eventPump;
        _localSettingsService = localSettingsService;
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

        var deviceId = await GetOrCreateDeviceIdAsync();
        
        // 1. 路径构建
        var paths = _cfgBuilder.BuildPaths("ClipBridge");
        Diagnostics.AppDataDir = paths.AppDataDir;
        Diagnostics.CoreDataDir = paths.CoreDataDir;
        Diagnostics.CacheDir = paths.CacheDir;
        Diagnostics.LogDir = paths.LogDir;

        var localFolder = ApplicationData.Current.LocalFolder.Path;
        var cacheFolder = ApplicationData.Current.LocalCacheFolder.Path;

        var config = new
        {
            device_id = deviceId, // 手动写成 snake_case 属性名，或者依赖 Policy
            device_name = System.Environment.MachineName,
            account_uid = "default_user", // 示例
            account_tag = "default_tag",
            data_dir = localFolder,
            cache_dir = cacheFolder,

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
            // 3. 配置 Resolver (仅一次)
            if (!_isResolverSet)
            {
                CoreInterop.ConfigureDllPath(dllFullPath);
                _isResolverSet = true;
            }

            // 4. 检查 ABI
            CoreInterop.cb_get_ffi_version(out var major, out var minor);
            Diagnostics.FfiAbiMajor = major;
            Diagnostics.FfiAbiMinor = minor;

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
            
            IntPtr outHandle;
            // [FIX] 必须使用 Task.Run 避免阻塞 UI 线程，虽然 init 应该很快
            await Task.Run(() =>
            {
                IntPtr envelopePtr = CoreInterop.cb_init(configJson, _onEventThunk, userData);
                var envelopeJson = CoreInterop.PtrToStringAndFree(envelopePtr);
                Diagnostics.LastInitEnvelopeJson = envelopeJson;

                // [FIX 7] 解析结果并提取 Handle
                // 假设成功时 JSON 为: {"ok": true, "data": 12345678} (data 为句柄地址)
                if (!IsEnvelopeOkAndGetHandle(envelopeJson, out var handle, out var errCode, out var errMsg))
                {
                    throw new Exception($"Core error: {errCode} - {errMsg}");
                }

                if (handle == IntPtr.Zero)
                {
                    throw new Exception("Init OK but returned handle is NULL/Zero.");
                }

                _coreHandle = handle;
            });

            _logger?.LogInformation("Core initialized successfully, handle: {Handle}", _coreHandle);
            Diagnostics.LastInitSummary = "Init OK";
            SetState(CoreState.Ready);
            
            // 写入"核心开"日志到核心（使用较早的时间戳，使其出现在核心初始化日志之前）
            try
            {
                var currentLang = GetCurrentLanguage();
                var messageEn = "-------------Core Started-----------";
                var messageZhCn = "-------------核心开-----------";
                CoreInterop.LogsWrite(_coreHandle, 2, "System", "Separator", messageEn, messageZhCn, null, null, initStartTime);
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

    public async Task ShutdownAsync(CancellationToken ct = default)
    {
        if (State == CoreState.NotLoaded)
        {
            return;
        }

        SetState(CoreState.ShuttingDown);

        if (_coreHandle != IntPtr.Zero)
        {
            // 写入"核心关"日志到核心（在关闭前）
            try
            {
                var currentLang = GetCurrentLanguage();
                var messageEn = "-------------Core Stopped-----------";
                var messageZhCn = "-------------核心关-----------";
                CoreInterop.LogsWrite(_coreHandle, 2, "System", "Separator", messageEn, messageZhCn);
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[CoreHostService] Failed to write 'Core Stopped' log: {ex}");
            }
            
            try
            {
                // [FIX] 放到后台线程防止阻塞
                await Task.Run(() => CoreInterop.cb_shutdown(_coreHandle));
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[CoreHostService] ShutdownAsync: Shutdown error: {ex}");
            }
            finally
            {
                _coreHandle = IntPtr.Zero;
            }
        }

        if (_selfHandle.IsAllocated)
        {
            _selfHandle.Free();
        }

        SetState(CoreState.NotLoaded);
    }

    public IntPtr GetHandle()
    {
        return _coreHandle;
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

    private bool IsEnvelopeOkAndGetHandle(string json, out IntPtr handle, out string errCode, out string errMsg)
    {
        handle = IntPtr.Zero;
        errCode = string.Empty;
        errMsg = string.Empty;

        if (string.IsNullOrWhiteSpace(json))
        {
            errMsg = "Empty JSON envelope";
            return false;
        }

        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var root = doc.RootElement;

            // 1. 检查 "ok" 字段
            if (root.TryGetProperty("ok", out var okElement) && okElement.GetBoolean())
            {
                // 2. 尝试获取 "data"
                if (root.TryGetProperty("data", out var dataElement))
                {
                    // 情况 A: data 是对象 {"handle": 123} (当前 Rust 实现)
                    if (dataElement.ValueKind == System.Text.Json.JsonValueKind.Object
                        && dataElement.TryGetProperty("handle", out var handleElement)
                        && handleElement.ValueKind == System.Text.Json.JsonValueKind.Number)
                    {
                        long val = handleElement.GetInt64();
                        handle = new IntPtr(val);
                        return val != 0; // 只有非0才算成功
                    }
                    // 情况 B: data 直接是数字 123 (兼容旧版/简化版)
                    else if (dataElement.ValueKind == System.Text.Json.JsonValueKind.Number)
                    {
                        long val = dataElement.GetInt64();
                        handle = new IntPtr(val);
                        return val != 0;
                    }
                }

                // 如果走到这里，说明 "ok": true 但没读到有效的 handle
                errMsg = "Missing or invalid 'handle' in data object";
                return false;
            }
            else
            {
                // 处理错误情况
                if (root.TryGetProperty("error", out var errorElement))
                {
                    if (errorElement.TryGetProperty("code", out var codeEl))
                        errCode = codeEl.GetString();
                    if (errorElement.TryGetProperty("message", out var msgEl))
                        errMsg = msgEl.GetString();
                }
                return false;
            }
        } catch (Exception ex)
        {
            errMsg = $"JSON parse error: {ex.Message}";
            return false;
        }
    }

    private static bool IsEnvelopeOk(string json, out string? errCode, out string? errMsg)
    {
        errCode = null;
        errMsg = null;
        try
        {
            using var doc = JsonDocument.Parse(json);
            if (doc.RootElement.TryGetProperty("ok", out var okEl) && okEl.ValueKind == JsonValueKind.True)
                return true;
            if (doc.RootElement.TryGetProperty("error", out var errEl))
            {
                if (errEl.TryGetProperty("code", out var c)) errCode = c.GetString();
                if (errEl.TryGetProperty("message", out var m)) errMsg = m.GetString();
            }
            return false;
        }
        catch
        {
            errCode = "GEN_INVALID_MESSAGE";
            errMsg = "Invalid JSON envelope from core.";
            return false;
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

    public async Task IngestLocalCopy(string snapshotJson)
    {
        if (State != CoreState.Ready || _coreHandle == IntPtr.Zero)
            return;

        await Task.Run(() =>
        {
            try
            {
                // 调用 Core FFI
                var ptr = CoreInterop.cb_ingest_local_copy(_coreHandle, snapshotJson);
                // 释放返回值内存（Core返回通常是确认信息或空，这里暂不处理返回值内容，但必须释放）
                CoreInterop.cb_free_string(ptr);
            } catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"IngestLocalCopy failed: {ex}");
                // 可以在这里通过 EventPump 发送一个 error 事件给 UI，或者记录日志
            }
        });
    }

    public async Task<HistoryPage> ListHistoryAsync(HistoryQuery query)
    {
        if (State != CoreState.Ready || _coreHandle == IntPtr.Zero)
        {
            return new HistoryPage();
        }

        return await Task.Run(() =>
        {
            try
            {
                return CoreInterop.ListHistory(_coreHandle, query);
            } catch (Exception ex)
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

    public async Task IngestLocalCopyAsync(ClipboardSnapshot snapshot)
    {
        if (State != CoreState.Ready)
            return;

        await Task.Run(() =>
        {
            try
            {
                // 构造符合 Rust TextDto 结构的匿名对象
                var textDto = new
                {
                    utf8 = snapshot.Data,      // 对应 Rust: utf8: String
                    mime = snapshot.MimeType   // 对应 Rust: mime: Option<String> (可选)
                };

                var ingestDto = new
                {
                    // 1. 对应 Rust: #[serde(rename = "type")] ty
                    type = "ClipboardSnapshot",

                    // 2. 对应 Rust: ShareMode enum (snake_case)
                    share_mode = "local_only",

                    // 3. 对应 Rust: ts_ms
                    ts_ms = snapshot.Timestamp,

                    // 4. 对应 Rust: SnapshotKind enum (snake_case)
                    kind = "text", // 暂时只处理文本，图片逻辑需另外写

                    // 5. 对应 Rust: text: Option<TextDto>
                    text = textDto
                };

                // 序列化
                var json = JsonSerializer.Serialize(ingestDto, _jsonOpts);

                // 调用 Core
                var resPtr = CoreInterop.cb_ingest_local_copy(_coreHandle, json);

                // 获取结果
                var resJson = CoreInterop.PtrToStringAndFree(resPtr);
            } catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[CoreHostService] Ingest failed: {ex}");
            }
        });
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
            var json = JsonSerializer.Serialize(req, _jsonOpts);


            var resPtr = CoreInterop.cb_ensure_content_cached(_coreHandle, json);
            var resJson = CoreInterop.PtrToStringAndFree(resPtr);


            using var doc = JsonDocument.Parse(resJson);
            var root = doc.RootElement;

            if (!root.TryGetProperty("ok", out var okEl) || !okEl.GetBoolean())
                throw new Exception($"ensure_content_cached failed: {resJson}");

            var tid = root.GetProperty("data").GetProperty("transfer_id").GetString();
            if (string.IsNullOrWhiteSpace(tid))
                throw new Exception($"ensure_content_cached missing transfer_id: {resJson}");

            return tid!;
        });
    }

    /// <summary>
    /// 查询设备列表（从核心）
    /// </summary>
    public List<Core.Models.Events.PeerMetaPayload> ListPeers()
    {
        if (State != CoreState.Ready || _coreHandle == IntPtr.Zero)
        {
            return new List<Core.Models.Events.PeerMetaPayload>();
        }

        try
        {
            var ptr = CoreInterop.cb_list_peers(_coreHandle);
            var json = CoreInterop.PtrToStringAndFree(ptr);

            if (string.IsNullOrEmpty(json))
                return new List<Core.Models.Events.PeerMetaPayload>();

            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;

            if (!root.TryGetProperty("ok", out var okEl) || !okEl.GetBoolean())
                return new List<Core.Models.Events.PeerMetaPayload>();

            if (!root.TryGetProperty("data", out var dataEl))
                return new List<Core.Models.Events.PeerMetaPayload>();

            var peers = new List<Core.Models.Events.PeerMetaPayload>();
            if (dataEl.ValueKind == JsonValueKind.Array)
            {
                foreach (var peerEl in dataEl.EnumerateArray())
                {
                    var peer = new Core.Models.Events.PeerMetaPayload
                    {
                        DeviceId = peerEl.TryGetProperty("device_id", out var did) ? did.GetString() ?? string.Empty : string.Empty,
                        Name = peerEl.TryGetProperty("device_name", out var name) ? name.GetString() : "Unknown",
                        IsOnline = peerEl.TryGetProperty("state", out var stateEl) 
                            ? stateEl.GetString() == "Online" 
                            : false,
                        LastSeen = peerEl.TryGetProperty("last_seen_ts_ms", out var lastSeen) 
                            ? lastSeen.GetInt64() 
                            : 0,
                        ShareToPeer = peerEl.TryGetProperty("share_to_peer", out var shareTo) 
                            ? shareTo.GetBoolean() 
                            : true,
                        AcceptFromPeer = peerEl.TryGetProperty("accept_from_peer", out var acceptFrom) 
                            ? acceptFrom.GetBoolean() 
                            : true,
                    };

                    // 转换连接状态
                    if (peerEl.TryGetProperty("state", out var state))
                    {
                        var stateStr = state.GetString() ?? "Offline";
                        peer.ConnectionState = stateStr;
                        peer.IsOnline = stateStr == "Online";
                    }

                    peers.Add(peer);
                }
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
        if (State != CoreState.Ready || _coreHandle == IntPtr.Zero)
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

            var ptr = CoreInterop.cb_set_peer_policy(_coreHandle, json);
            var result = CoreInterop.PtrToStringAndFree(ptr);

            using var doc = JsonDocument.Parse(result);
            var root = doc.RootElement;

            if (!root.TryGetProperty("ok", out var okEl) || !okEl.GetBoolean())
            {
                var errMsg = "Unknown error";
                if (root.TryGetProperty("error", out var errEl))
                {
                    if (errEl.TryGetProperty("message", out var msgEl))
                        errMsg = msgEl.GetString() ?? errMsg;
                }
                throw new Exception($"SetPeerPolicy failed: {errMsg}");
            }
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
            if (_coreHandle == IntPtr.Zero)
                throw new InvalidOperationException("Core not initialized");

            var request = new
            {
                peer_id = peerId
            };

            var json = JsonSerializer.Serialize(request, _jsonOpts);
            var ptr = CoreInterop.cb_clear_peer_fingerprint(_coreHandle, json);
            var result = CoreInterop.PtrToStringAndFree(ptr);

            using var doc = JsonDocument.Parse(result);
            var root = doc.RootElement;

            if (!root.TryGetProperty("ok", out var okEl) || !okEl.GetBoolean())
            {
                var errMsg = "Unknown error";
                if (root.TryGetProperty("error", out var errEl))
                {
                    if (errEl.TryGetProperty("message", out var msgEl))
                        errMsg = msgEl.GetString() ?? errMsg;
                }
                throw new Exception($"ClearPeerFingerprint failed: {errMsg}");
            }
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
            if (_coreHandle == IntPtr.Zero)
                throw new InvalidOperationException("Core not initialized");

            var ptr = CoreInterop.cb_clear_local_cert(_coreHandle);
            var result = CoreInterop.PtrToStringAndFree(ptr);

            using var doc = JsonDocument.Parse(result);
            var root = doc.RootElement;

            if (!root.TryGetProperty("ok", out var okEl) || !okEl.GetBoolean())
            {
                var errMsg = "Unknown error";
                if (root.TryGetProperty("error", out var errEl))
                {
                    if (errEl.TryGetProperty("message", out var msgEl))
                        errMsg = msgEl.GetString() ?? errMsg;
                }
                throw new Exception($"ClearLocalCert failed: {errMsg}");
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"ClearLocalCert failed: {ex}");
            throw;
        }
    }

}
