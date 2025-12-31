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

namespace ClipBridgeShell_CS.Services;

public sealed class CoreHostService : ICoreHostService
{
    private readonly CoreConfigBuilder _cfgBuilder = new();
    private CoreInterop.CbOnEventFn? _onEventThunk;
    private GCHandle _selfHandle;
    private IntPtr _coreHandle = IntPtr.Zero;
    private static bool _isResolverSet = false;
    private readonly EventPumpService _eventPump;

    public CoreState State { get; private set; } = CoreState.NotLoaded;
    public CoreDiagnostics Diagnostics { get; } = new();
    public event Action<CoreState>? StateChanged;
    public string? LastError
    {
        get; private set;
    }

    public CoreHostService(EventPumpService eventPump)
    {
        _eventPump = eventPump;
    }

    public async Task InitializeAsync(CancellationToken ct = default)
    {
        if (State is CoreState.Loading or CoreState.Ready) return;

        LastError = null;
        SetState(CoreState.Loading);

        // 1. 路径构建
        var paths = _cfgBuilder.BuildPaths("ClipBridge");
        Diagnostics.AppDataDir = paths.AppDataDir;
        Diagnostics.CoreDataDir = paths.CoreDataDir;
        Diagnostics.CacheDir = paths.CacheDir;
        Diagnostics.LogDir = paths.LogDir;

        var configJson = _cfgBuilder.BuildConfigJson(paths);

        // 2. 查找 DLL
        var dllFullPath = FindCoreDll("core_ffi_windows.dll");
        Diagnostics.DllPath = dllFullPath;

        if (string.IsNullOrWhiteSpace(dllFullPath) || !File.Exists(dllFullPath))
        {
            Diagnostics.DllLoadError = "DLL not found in app output directories.";
            Diagnostics.LastInitSummary = "Core degraded: DLL missing.";
            LastError = Diagnostics.DllLoadError;
            SetState(CoreState.Degraded);
            return;
        }

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

            Diagnostics.LastInitSummary = "Init OK";
            SetState(CoreState.Ready);
        }
        catch (DllNotFoundException ex)
        {
            Diagnostics.DllLoadError = $"DllNotFound: {ex.Message}";
            Diagnostics.LastInitSummary = "Core degraded: DLL load failed.";
            LastError = ex.Message;
            SetState(CoreState.Degraded);
        }
        catch (BadImageFormatException ex)
        {
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
        if (State == CoreState.NotLoaded) return;

        SetState(CoreState.ShuttingDown);

        if (_coreHandle != IntPtr.Zero)
        {
            try
            {
                // [FIX] 放到后台线程防止阻塞
                await Task.Run(() => CoreInterop.cb_shutdown(_coreHandle));
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"Shutdown error: {ex}");
            }
            finally
            {
                _coreHandle = IntPtr.Zero;
            }
        }

        if (_selfHandle.IsAllocated) _selfHandle.Free();

        SetState(CoreState.NotLoaded);
    }

    private void SetState(CoreState s)
    {
        State = s;
        StateChanged?.Invoke(s);
    }

    private static bool IsEnvelopeOkAndGetHandle(string json, out IntPtr handle, out string? errCode, out string? errMsg)
    {
        handle = IntPtr.Zero;
        errCode = null;
        errMsg = null;
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;

            // 1. 检查 "ok"
            if (root.TryGetProperty("ok", out var okEl) && okEl.ValueKind == JsonValueKind.True)
            {
                // 2. 成功：从 "data" 提取句柄 (假设 Core 返回的是 int64 地址)
                if (root.TryGetProperty("data", out var dataEl))
                {
                    if (dataEl.ValueKind == JsonValueKind.Number)
                    {
                        long ptrVal = dataEl.GetInt64();
                        handle = new IntPtr(ptrVal);
                        return true;
                    }
                    // 兼容性：如果 data 是 null 或者不是数字，可能 Core 是单例设计，暂且认为是 Zero
                    return true;
                }
                return true;
            }

            // 3. 失败：解析 error
            if (root.TryGetProperty("error", out var errEl))
            {
                if (errEl.TryGetProperty("code", out var c)) errCode = c.GetString();
                if (errEl.TryGetProperty("message", out var m)) errMsg = m.GetString();
            }
            return false;
        }
        catch
        {
            errCode = "JSON_PARSE_ERR";
            errMsg = "Invalid JSON envelope.";
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
}
