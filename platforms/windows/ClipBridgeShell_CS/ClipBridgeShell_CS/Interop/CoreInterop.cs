using System;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Interop;

/// <summary>
/// 来源统计（component 和 category 的列表及计数）
/// </summary>
public class SourceStats
{
    [JsonPropertyName("components")]
    public Dictionary<string, long> Components { get; set; } = new();
    
    [JsonPropertyName("categories")]
    public Dictionary<string, long> Categories { get; set; } = new();
}

internal static class CoreInterop
{
    // 注意：Windows 下实际文件可能是 core_ffi_windows.dll
    private const string DllName = "core_ffi_windows.dll";
    private static string? _customDllPath;
    private static readonly JsonSerializerOptions _jsonOpts = new()
    {
        PropertyNameCaseInsensitive = true,
        NumberHandling = JsonNumberHandling.AllowReadingFromString
    };
    // 静态构造函数：配置 DllImportResolver (只运行一次)
    static CoreInterop()
    {
        NativeLibrary.SetDllImportResolver(Assembly.GetExecutingAssembly(), (name, assembly, path) =>
        {
            if (name == DllName && !string.IsNullOrEmpty(_customDllPath))
            {
                return NativeLibrary.Load(_customDllPath);
            }
            // 返回 Zero 让它走默认逻辑（或者在这里处理默认路径）
            return IntPtr.Zero;
        });
    }

    /// <summary>
    /// 在 CoreHostService 初始化时调用，指定 DLL 的绝对路径
    /// </summary>
    public static void ConfigureDllPath(string absPath)
    {
        _customDllPath = absPath;
    }

    // ==========================================================
    // 1. 生命周期与基础 (对应 clipbridge_core.h)
    // ==========================================================

    // 回调委托定义
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void CbOnEventFn(IntPtr json_utf8, IntPtr user_data);

    // CB_API const char* cb_init(const char* cfg_json, cb_on_event_fn on_event, void* user_data);
    // 返回值是 JSON Envelope 指针 (const char*)
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_init(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string cfg_json,
        CbOnEventFn on_event,
        IntPtr user_data
    );

    // CB_API const char* cb_shutdown(cb_handle* h);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_shutdown(IntPtr h);

    // CB_API void cb_free_string(const char* s);
    // 用于释放所有返回 const char* 的 API 产生的内存
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void cb_free_string(IntPtr s);

    // ==========================================================
    // 2. 剪贴板采集 (Ingest)
    // ==========================================================

    // CB_API const char* cb_plan_local_ingest(cb_handle* h, const char* snapshot_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_plan_local_ingest(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string snapshot_json);

    // CB_API const char* cb_ingest_local_copy(cb_handle* h, const char* snapshot_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_ingest_local_copy(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string snapshot_json);

    // ==========================================================
    // 3. 状态与设备 (Status & Peers)
    // ==========================================================

    // CB_API const char* cb_list_peers(cb_handle* h);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_list_peers(IntPtr h);

    // CB_API const char* cb_get_status(cb_handle* h);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_get_status(IntPtr h);

    // CB_API const char* cb_set_peer_policy(cb_handle* h, const char* policy_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_set_peer_policy(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string policy_json);

    // CB_API const char* cb_clear_peer_fingerprint(cb_handle* h, const char* peer_id_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_clear_peer_fingerprint(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string peer_id_json);

    // CB_API const char* cb_clear_local_cert(cb_handle* h);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_clear_local_cert(IntPtr h);

    // ==========================================================
    // 4. 传输与内容 (Transfer & Content)
    // ==========================================================

    // CB_API const char* cb_ensure_content_cached(cb_handle* h, const char* req_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_ensure_content_cached(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string req_json);

    // CB_API const char* cb_cancel_transfer(cb_handle* h, const char* transfer_id_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_cancel_transfer(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string transfer_id_json);

    // ==========================================================
    // 5. 历史记录 (History)
    // ==========================================================

    // CB_API const char* cb_list_history(cb_handle* h, const char* query_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_list_history(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string query_json);

    // CB_API const char* cb_get_item_meta(cb_handle* h, const char* item_id_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_get_item_meta(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string item_id_json);

    // 统计查询接口
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_query_cache_stats(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string query_json);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_query_net_stats(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string query_json);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_query_activity_stats(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string query_json);

    // ==========================================================
    // 6. 日志系统 (Logs) - 补充部分
    // 虽然头文件中未列出，但 Native.cs 中使用了这些 API，且业务逻辑强依赖。
    // 假设这些 API 仍存在于 DLL 中 (或在头文件的其他部分)。
    // ==========================================================

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_write(
        IntPtr h,
        int level,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string component,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string category,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string message_en,
        IntPtr message_zh_cn_or_null,
        IntPtr exception_or_null,
        IntPtr props_json_or_null,
        long ts_utc,
        out long out_id);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_query_after_id(
        IntPtr h,
        long after_id, int level_min,
        IntPtr like_or_null, int limit,
        IntPtr lang_or_null,
        out IntPtr out_json_array);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_query_before_id(
        IntPtr h,
        long before_id, int level_min,
        IntPtr like_or_null, int limit,
        IntPtr lang_or_null,
        out IntPtr out_json_array);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_query_range(
        IntPtr h,
        long start_ms, long end_ms, int level_min,
        IntPtr like_or_null, int limit, int offset,
        IntPtr lang_or_null,
        out IntPtr out_json_array);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_delete_before(
        IntPtr h,
        long cutoff_ms, out long out_deleted);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_delete_by_ids(
        IntPtr h,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string ids_json,
        out long out_deleted);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_stats(IntPtr h, out IntPtr out_json);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_source_stats(IntPtr h, out IntPtr out_json);

    // 数据库清空接口
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_clear_core_db(IntPtr h);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_clear_logs_db(IntPtr h);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_clear_stats_db(IntPtr h);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_clear_cache(IntPtr h);

    // 释放非 string 类型的指针 (如 logs 返回的 json 数组可能需要通用 free)
    // 如果头文件中 cb_free_string 是通用的 free，则使用 cb_free_string 即可。
    // 鉴于原 Native.cs 使用了 cb_free，这里保留一个别名以兼容 Log 模块。
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl, EntryPoint = "cb_free_string")]
    internal static extern void cb_free(IntPtr ptr);

    // ==========================================================
    // 7. 版本与辅助 (Version)
    // ==========================================================

    // 之前的 CoreNative.cs 中有此函数，用于诊断
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void cb_get_ffi_version(out uint out_major, out uint out_minor);


    /// <summary>
    /// 供 CoreHostService 使用，自动转换 string 并释放 C 侧内存
    /// </summary>
    /// <param name="ptr"></param>
    /// <returns></returns>
    public static string PtrToStringAndFree(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return string.Empty;
        try
        {
            return Marshal.PtrToStringUTF8(ptr) ?? string.Empty;
        }
        finally
        {
            // 必须调用 Core 的 free 方法
            cb_free_string(ptr);
        }
    }

    // ==========================================================
    // 8. 高级封装方法 (Wrappers)
    // 供 ViewModel 直接调用，替代原 Native.cs 的功能
    // ==========================================================

    /// <summary>
    /// 写入日志（多语言版本）
    /// </summary>
    /// <param name="tsUtc">时间戳（Unix毫秒）。如果为0或负数，使用当前时间；否则使用提供的时间戳（用于暂存日志回写）</param>
    public static long LogsWrite(IntPtr handle, int level, string component, string category, string messageEn, string? messageZhCn = null, string? exception = null, string? propsJson = null, long tsUtc = 0)
    {
        // 手动处理可空字符串指针
        IntPtr zhCnPtr = messageZhCn != null ? Marshal.StringToCoTaskMemUTF8(messageZhCn) : IntPtr.Zero;
        IntPtr exPtr = exception != null ? Marshal.StringToCoTaskMemUTF8(exception) : IntPtr.Zero;
        IntPtr propsPtr = propsJson != null ? Marshal.StringToCoTaskMemUTF8(propsJson) : IntPtr.Zero;

        try
        {
            var rc = cb_logs_write(handle, level, component, category, messageEn, zhCnPtr, exPtr, propsPtr, tsUtc, out long id);
            if (rc != 0) throw new Exception($"cb_logs_write failed: {rc}");
            return id;
        }
        finally
        {
            if (zhCnPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(zhCnPtr);
            if (exPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(exPtr);
            if (propsPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(propsPtr);
        }
    }

    /// <summary>
    /// 查询增量日志 (Tail)
    /// </summary>
    public static List<ClipBridgeShell_CS.Models.LogRow> LogsQueryAfterId(IntPtr handle, long afterId, int levelMin, string? like, int limit, string? lang = null)
    {
        IntPtr likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        IntPtr langPtr = lang != null ? Marshal.StringToCoTaskMemUTF8(lang) : IntPtr.Zero;
        IntPtr jsonPtr = IntPtr.Zero;
        try
        {
            var rc = cb_logs_query_after_id(handle, afterId, levelMin, likePtr, limit, langPtr, out jsonPtr);
            if (rc != 0) throw new Exception($"cb_logs_query_after_id failed: {rc}");

            // 使用我们新加的 Helper 读取并释放
            var json = PtrToUtf8AndFree(jsonPtr);
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreInterop] LogsQueryAfterId raw JSON: {json.Substring(0, Math.Min(500, json.Length))}");
            // #endregion
            var envelope = JsonSerializer.Deserialize<JsonElement>(json, _jsonOpts);
            if (envelope.TryGetProperty("data", out var data))
            {
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[CoreInterop] LogsQueryAfterId data property found, type: {data.ValueKind}");
                var dataText = data.GetRawText();
                System.Diagnostics.Debug.WriteLine($"[CoreInterop] LogsQueryAfterId data content: {dataText.Substring(0, Math.Min(500, dataText.Length))}");
                // #endregion
                var result = JsonSerializer.Deserialize<List<ClipBridgeShell_CS.Models.LogRow>>(dataText, _jsonOpts) ?? new();
                // #region agent log
                System.Diagnostics.Debug.WriteLine($"[CoreInterop] LogsQueryAfterId deserialized: {result.Count} items");
                // #endregion
                return result;
            }
            // #region agent log
            System.Diagnostics.Debug.WriteLine($"[CoreInterop] LogsQueryAfterId no data property, returning empty list");
            // #endregion
            return new();
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
            if (langPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(langPtr);
        }
    }

    /// <summary>
    /// 查询 before_id 之前的日志（用于向上滚动加载更早的日志）
    /// </summary>
    public static List<ClipBridgeShell_CS.Models.LogRow> LogsQueryBeforeId(IntPtr handle, long beforeId, int levelMin, string? like, int limit, string? lang = null)
    {
        IntPtr likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        IntPtr langPtr = lang != null ? Marshal.StringToCoTaskMemUTF8(lang) : IntPtr.Zero;
        IntPtr jsonPtr = IntPtr.Zero;
        try
        {
            var rc = cb_logs_query_before_id(handle, beforeId, levelMin, likePtr, limit, langPtr, out jsonPtr);
            if (rc != 0) throw new Exception($"cb_logs_query_before_id failed: {rc}");

            var json = PtrToUtf8AndFree(jsonPtr);
            var envelope = JsonSerializer.Deserialize<JsonElement>(json, _jsonOpts);
            if (envelope.TryGetProperty("data", out var data))
            {
                return JsonSerializer.Deserialize<List<ClipBridgeShell_CS.Models.LogRow>>(data.GetRawText(), _jsonOpts) ?? new();
            }
            return new();
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
            if (langPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(langPtr);
        }
    }

    /// <summary>
    /// 查询历史日志 (Page)
    /// </summary>
    public static List<ClipBridgeShell_CS.Models.LogRow> LogsQueryRange(IntPtr handle, long startMs, long endMs, int levelMin, string? like, int limit, int offset, string? lang = null)
    {
        IntPtr likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        IntPtr langPtr = lang != null ? Marshal.StringToCoTaskMemUTF8(lang) : IntPtr.Zero;
        IntPtr jsonPtr = IntPtr.Zero;
        try
        {
            var rc = cb_logs_query_range(handle, startMs, endMs, levelMin, likePtr, limit, offset, langPtr, out jsonPtr);
            if (rc != 0) throw new Exception($"cb_logs_query_range failed: {rc}");

            var json = PtrToUtf8AndFree(jsonPtr);
            var envelope = JsonSerializer.Deserialize<JsonElement>(json, _jsonOpts);
            if (envelope.TryGetProperty("data", out var data))
            {
                return JsonSerializer.Deserialize<List<ClipBridgeShell_CS.Models.LogRow>>(data.GetRawText(), _jsonOpts) ?? new();
            }
            return new();
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
            if (langPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(langPtr);
        }
    }

    /// <summary>
    /// 删除旧日志
    /// </summary>
    public static long LogsDeleteBefore(IntPtr handle, long cutoffMs)
    {
        var rc = cb_logs_delete_before(handle, cutoffMs, out long deleted);
        if (rc != 0) throw new Exception($"cb_logs_delete_before failed: {rc}");
        return deleted;
    }

    /// <summary>
    /// 按 ID 列表删除日志
    /// </summary>
    public static long LogsDeleteByIds(IntPtr handle, long[] ids)
    {
        var idsJson = JsonSerializer.Serialize(ids, _jsonOpts);
        var rc = cb_logs_delete_by_ids(handle, idsJson, out long deleted);
        if (rc != 0) throw new Exception($"cb_logs_delete_by_ids failed: {rc}");
        return deleted;
    }

    /// <summary>
    /// 获取日志统计
    /// </summary>
    public static ClipBridgeShell_CS.Models.LogStats LogsStats(IntPtr handle)
    {
        IntPtr jsonPtr = IntPtr.Zero;
        var rc = cb_logs_stats(handle, out jsonPtr);
        if (rc != 0) throw new Exception($"cb_logs_stats failed: {rc}");

        var json = PtrToUtf8AndFree(jsonPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(json, _jsonOpts);
        if (envelope.TryGetProperty("data", out var data))
        {
            return JsonSerializer.Deserialize<ClipBridgeShell_CS.Models.LogStats>(data.GetRawText(), _jsonOpts) ?? new ClipBridgeShell_CS.Models.LogStats();
        }
        return new ClipBridgeShell_CS.Models.LogStats();
    }

    /// <summary>
    /// 获取来源统计（component 和 category 的列表及计数）
    /// </summary>
    public static SourceStats LogsSourceStats(IntPtr handle)
    {
        IntPtr jsonPtr = IntPtr.Zero;
        var rc = cb_logs_source_stats(handle, out jsonPtr);
        if (rc != 0) throw new Exception($"cb_logs_source_stats failed: {rc}");

        var json = PtrToUtf8AndFree(jsonPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(json, _jsonOpts);
        if (envelope.TryGetProperty("data", out var data))
        {
            return JsonSerializer.Deserialize<SourceStats>(data.GetRawText(), _jsonOpts) ?? new SourceStats();
        }
        return new SourceStats();
    }

    /// <summary>
    /// 清空核心数据库
    /// </summary>
    public static void ClearCoreDb(IntPtr handle)
    {
        var resultPtr = cb_clear_core_db(handle);
        var result = PtrToStringAndFree(resultPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(result, _jsonOpts);
        if (!envelope.TryGetProperty("ok", out var ok) || !ok.GetBoolean())
        {
            throw new Exception("Failed to clear core database");
        }
    }

    /// <summary>
    /// 清空日志数据库
    /// </summary>
    public static void ClearLogsDb(IntPtr handle)
    {
        var resultPtr = cb_clear_logs_db(handle);
        var result = PtrToStringAndFree(resultPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(result, _jsonOpts);
        if (!envelope.TryGetProperty("ok", out var ok) || !ok.GetBoolean())
        {
            throw new Exception("Failed to clear logs database");
        }
    }

    /// <summary>
    /// 清空统计数据库
    /// </summary>
    public static void ClearStatsDb(IntPtr handle)
    {
        var resultPtr = cb_clear_stats_db(handle);
        var result = PtrToStringAndFree(resultPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(result, _jsonOpts);
        if (!envelope.TryGetProperty("ok", out var ok) || !ok.GetBoolean())
        {
            throw new Exception("Failed to clear stats database");
        }
    }

    /// <summary>
    /// 清空缓存（CAS blobs、临时文件等）
    /// </summary>
    public static void ClearCache(IntPtr handle)
    {
        var resultPtr = cb_clear_cache(handle);
        var result = PtrToStringAndFree(resultPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(result, _jsonOpts);
        if (!envelope.TryGetProperty("ok", out var ok) || !ok.GetBoolean())
        {
            throw new Exception("Failed to clear cache");
        }
    }

    // 私有辅助：读取 Core 返回的 JSON 指针转 String 并通知 Core 释放内存
    // 注意：这个逻辑和 PtrToStringAndFree 是一样的，为了代码复用，你可以直接用 PtrToStringAndFree
    private static string PtrToUtf8AndFree(IntPtr ptr)
    {
        return PtrToStringAndFree(ptr);
    }

    /// <summary>
    /// 查询历史记录 (Wrapper)
    /// </summary>
    public static ClipBridgeShell_CS.Core.Models.HistoryPage ListHistory(IntPtr handle, ClipBridgeShell_CS.Core.Models.HistoryQuery query)
    {
        if (handle == IntPtr.Zero)
            return new();

        var queryJson = JsonSerializer.Serialize(query, _jsonOpts);

        // 调用 FFI
        var jsonPtr = cb_list_history(handle, queryJson);

        try
        {
            var json = PtrToStringAndFree(jsonPtr);

            // 日志保持不变
            /*
            System.Diagnostics.Debug.WriteLine("=====================================");
            System.Diagnostics.Debug.WriteLine($"[CoreInterop] Query: {queryJson}");
            System.Diagnostics.Debug.WriteLine($"[CoreInterop] Result: {json}");
            System.Diagnostics.Debug.WriteLine("=====================================");
            */

            if (string.IsNullOrEmpty(json))
                return new();

            // 【核心修复 START】
            // 1. 解析 JSON 文档
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;

            // 2. 检查 "ok"
            if (root.TryGetProperty("ok", out var ok) && ok.GetBoolean())
            {
                // 3. 提取 "data" 节点
                if (root.TryGetProperty("data", out var data))
                {
                    // 4. 将 "data" 节点反序列化为 HistoryPage
                    // 注意：必须传入 _jsonOpts 以处理 snake_case (item_id -> ItemId)
                    var page = data.Deserialize<ClipBridgeShell_CS.Core.Models.HistoryPage>(_jsonOpts);
                    return page ?? new();
                }
            }
            // 【核心修复 END】

            return new();
        } catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"ListHistory parsing failed: {ex}");
            return new();
        }
    }

    /// <summary>
    /// 查询缓存统计
    /// </summary>
    public static CacheStatsResult QueryCacheStats(IntPtr handle, long startTsMs = 0, long endTsMs = 0, int bucketSec = 10)
    {
        var query = new
        {
            start_ts_ms = startTsMs,
            end_ts_ms = endTsMs,
            bucket_sec = bucketSec
        };
        var queryJson = JsonSerializer.Serialize(query, _jsonOpts);
        var resultPtr = cb_query_cache_stats(handle, queryJson);
        var result = PtrToStringAndFree(resultPtr);

        var envelope = JsonSerializer.Deserialize<JsonElement>(result, _jsonOpts);
        if (envelope.TryGetProperty("data", out var data))
        {
            return JsonSerializer.Deserialize<CacheStatsResult>(data.GetRawText(), _jsonOpts) ?? new CacheStatsResult();
        }
        return new CacheStatsResult();
    }

    /// <summary>
    /// 查询网络统计
    /// </summary>
    public static NetworkStatsResult QueryNetStats(IntPtr handle, long startTsMs = 0, long endTsMs = 0, int bucketSec = 10)
    {
        var query = new
        {
            start_ts_ms = startTsMs,
            end_ts_ms = endTsMs,
            bucket_sec = bucketSec
        };
        var queryJson = JsonSerializer.Serialize(query, _jsonOpts);
        var resultPtr = cb_query_net_stats(handle, queryJson);
        var result = PtrToStringAndFree(resultPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(result, _jsonOpts);
        if (envelope.TryGetProperty("data", out var data))
        {
            return JsonSerializer.Deserialize<NetworkStatsResult>(data.GetRawText(), _jsonOpts) ?? new NetworkStatsResult();
        }
        return new NetworkStatsResult();
    }

    /// <summary>
    /// 查询活动统计
    /// </summary>
    public static ActivityStatsResult QueryActivityStats(IntPtr handle, long startTsMs = 0, long endTsMs = 0, int bucketSec = 60)
    {
        var query = new
        {
            start_ts_ms = startTsMs,
            end_ts_ms = endTsMs,
            bucket_sec = bucketSec
        };
        var queryJson = JsonSerializer.Serialize(query, _jsonOpts);
        var resultPtr = cb_query_activity_stats(handle, queryJson);
        var result = PtrToStringAndFree(resultPtr);
        var envelope = JsonSerializer.Deserialize<JsonElement>(result, _jsonOpts);
        if (envelope.TryGetProperty("data", out var data))
        {
            return JsonSerializer.Deserialize<ActivityStatsResult>(data.GetRawText(), _jsonOpts) ?? new ActivityStatsResult();
        }
        return new ActivityStatsResult();
    }
}

// 统计结果模型
public sealed class CacheStatsResult
{
    [JsonPropertyName("window")]
    public StatsWindow? Window { get; set; }
    
    [JsonPropertyName("series")]
    public List<CacheStatsPoint> Series { get; set; } = new();
    
    [JsonPropertyName("current_cache_bytes")]
    public long CurrentCacheBytes { get; set; }
}

public sealed class NetworkStatsResult
{
    [JsonPropertyName("window")]
    public StatsWindow? Window { get; set; }
    
    [JsonPropertyName("series")]
    public List<NetworkStatsPoint> Series { get; set; } = new();
}

public sealed class ActivityStatsResult
{
    [JsonPropertyName("window")]
    public StatsWindow? Window { get; set; }
    
    [JsonPropertyName("series")]
    public List<ActivityStatsPoint> Series { get; set; } = new();
}

public sealed class StatsWindow
{
    [JsonPropertyName("start_ts_ms")]
    public long StartTsMs { get; set; }
    
    [JsonPropertyName("end_ts_ms")]
    public long EndTsMs { get; set; }
    
    [JsonPropertyName("bucket_sec")]
    public int BucketSec { get; set; }
}

public sealed class CacheStatsPoint
{
    [JsonPropertyName("ts_ms")]
    public long TsMs { get; set; }
    
    [JsonPropertyName("cache_bytes")]
    public long CacheBytes { get; set; }
}

public sealed class NetworkStatsPoint
{
    [JsonPropertyName("ts_ms")]
    public long TsMs { get; set; }
    
    [JsonPropertyName("bytes_sent")]
    public long BytesSent { get; set; }
    
    [JsonPropertyName("bytes_recv")]
    public long BytesRecv { get; set; }
}

public sealed class ActivityStatsPoint
{
    [JsonPropertyName("ts_ms")]
    public long TsMs { get; set; }
    
    [JsonPropertyName("text_count")]
    public long TextCount { get; set; }
    
    [JsonPropertyName("image_count")]
    public long ImageCount { get; set; }
    
    [JsonPropertyName("files_count")]
    public long FilesCount { get; set; }
}
