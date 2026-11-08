using System;
using System.Collections.Generic;
using System.Reflection;
using System.Runtime.InteropServices;
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

/// <summary>
/// 原始 P/Invoke 层。只负责 FFI 调用与字符串/指针封送，
/// JSON envelope 解析、状态保护与错误处理统一由 CoreHostService（Core Bridge）负责。
/// UI 层（ViewModels/Views）不得直接引用本类型。
/// </summary>
internal static class CoreInterop
{
    // 注意：Windows 下实际文件可能是 core_ffi_windows.dll
    private const string DllName = "core_ffi_windows.dll";
    private static string? _customDllPath;
    // _customDllPath 由 DllImportResolver（首次 P/Invoke 时在任意线程触发）和 ConfigureDllPath 共享，
    // 统一用这把锁保护读写，避免未同步的静态字段读取。
    private static readonly object _dllPathLock = new();
    private static bool _dllPathConfigured;

    // 静态构造函数：配置 DllImportResolver (只运行一次)
    static CoreInterop()
    {
        NativeLibrary.SetDllImportResolver(Assembly.GetExecutingAssembly(), (name, assembly, path) =>
        {
            if (name != DllName)
            {
                // 返回 Zero 让它走默认逻辑
                return IntPtr.Zero;
            }

            string? customPath;
            lock (_dllPathLock)
            {
                customPath = _customDllPath;
            }

            return string.IsNullOrEmpty(customPath) ? IntPtr.Zero : NativeLibrary.Load(customPath);
        });
    }

    /// <summary>
    /// 在 CoreHostService 初始化时调用，指定 DLL 的绝对路径。
    /// 线程安全且幂等：只允许首次配置；重复配置相同路径返回 true，配置不同路径返回 false。
    /// </summary>
    /// <param name="absPath">DLL 的绝对路径。</param>
    /// <returns>当前解析器是否已指向 <paramref name="absPath"/>。</returns>
    public static bool ConfigureDllPath(string absPath)
    {
        if (string.IsNullOrWhiteSpace(absPath))
        {
            return false;
        }

        lock (_dllPathLock)
        {
            if (_dllPathConfigured)
            {
                return string.Equals(_customDllPath, absPath, StringComparison.OrdinalIgnoreCase);
            }

            _customDllPath = absPath;
            _dllPathConfigured = true;
            return true;
        }
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

    // CB_API const char* cb_delete_item_local(cb_handle* h, const char* item_id_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_delete_item_local(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string item_id_json);

    // CB_API const char* cb_delete_item_global(cb_handle* h, const char* item_id_json);
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_delete_item_global(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string item_id_json);

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
    internal static extern int cb_logs_query_latest(
        IntPtr h,
        int level_min,
        IntPtr like_or_null, int limit,
        IntPtr lang_or_null,
        out IntPtr out_json_array);

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

    // CB_API const char* cb_export_backup(cb_handle* h, const char* request_json);
    // 单向导出备份；request_json: {"dest_dir":"...","include_content":false}
    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr cb_export_backup(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string request_json);

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
    // 8. 原始调用辅助 (Raw marshalling helpers)
    // 只做 P/Invoke 与可空字符串指针封送，不解析 JSON、不判断 rc。
    // rc 处理与 envelope 解析属于 CoreHostService（Core Bridge）的职责。
    // ==========================================================

    /// <summary>
    /// 调用 cb_logs_write，负责可空字符串指针的封送与释放，返回原始 rc。
    /// </summary>
    internal static int LogsWriteRaw(IntPtr handle, int level, string component, string category, string messageEn,
        string? messageZhCn, string? exception, string? propsJson, long tsUtc, out long id)
    {
        // 手动处理可空字符串指针
        var zhCnPtr = messageZhCn != null ? Marshal.StringToCoTaskMemUTF8(messageZhCn) : IntPtr.Zero;
        var exPtr = exception != null ? Marshal.StringToCoTaskMemUTF8(exception) : IntPtr.Zero;
        var propsPtr = propsJson != null ? Marshal.StringToCoTaskMemUTF8(propsJson) : IntPtr.Zero;

        try
        {
            return cb_logs_write(handle, level, component, category, messageEn, zhCnPtr, exPtr, propsPtr, tsUtc, out id);
        }
        finally
        {
            if (zhCnPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(zhCnPtr);
            if (exPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(exPtr);
            if (propsPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(propsPtr);
        }
    }

    internal static int LogsQueryLatestRaw(IntPtr handle, int levelMin, string? like, int limit, string? lang, out IntPtr jsonPtr)
    {
        var likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        var langPtr = lang != null ? Marshal.StringToCoTaskMemUTF8(lang) : IntPtr.Zero;
        try
        {
            return cb_logs_query_latest(handle, levelMin, likePtr, limit, langPtr, out jsonPtr);
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
            if (langPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(langPtr);
        }
    }

    internal static int LogsQueryAfterIdRaw(IntPtr handle, long afterId, int levelMin, string? like, int limit, string? lang, out IntPtr jsonPtr)
    {
        var likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        var langPtr = lang != null ? Marshal.StringToCoTaskMemUTF8(lang) : IntPtr.Zero;
        try
        {
            return cb_logs_query_after_id(handle, afterId, levelMin, likePtr, limit, langPtr, out jsonPtr);
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
            if (langPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(langPtr);
        }
    }

    internal static int LogsQueryBeforeIdRaw(IntPtr handle, long beforeId, int levelMin, string? like, int limit, string? lang, out IntPtr jsonPtr)
    {
        var likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        var langPtr = lang != null ? Marshal.StringToCoTaskMemUTF8(lang) : IntPtr.Zero;
        try
        {
            return cb_logs_query_before_id(handle, beforeId, levelMin, likePtr, limit, langPtr, out jsonPtr);
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
            if (langPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(langPtr);
        }
    }

    internal static int LogsQueryRangeRaw(IntPtr handle, long startMs, long endMs, int levelMin, string? like, int limit, int offset, string? lang, out IntPtr jsonPtr)
    {
        var likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        var langPtr = lang != null ? Marshal.StringToCoTaskMemUTF8(lang) : IntPtr.Zero;
        try
        {
            return cb_logs_query_range(handle, startMs, endMs, levelMin, likePtr, limit, offset, langPtr, out jsonPtr);
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
            if (langPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(langPtr);
        }
    }
}

// 统计结果模型
public sealed class CacheStatsResult
{
    [JsonPropertyName("window")]
    public StatsWindow? Window
    {
        get; set;
    }

    [JsonPropertyName("series")]
    public List<CacheStatsPoint> Series { get; set; } = new();

    [JsonPropertyName("current_cache_bytes")]
    public long CurrentCacheBytes
    {
        get; set;
    }
}

public sealed class NetworkStatsResult
{
    [JsonPropertyName("window")]
    public StatsWindow? Window
    {
        get; set;
    }

    [JsonPropertyName("series")]
    public List<NetworkStatsPoint> Series { get; set; } = new();
}

public sealed class ActivityStatsResult
{
    [JsonPropertyName("window")]
    public StatsWindow? Window
    {
        get; set;
    }

    [JsonPropertyName("series")]
    public List<ActivityStatsPoint> Series { get; set; } = new();
}

/// <summary>
/// <c>cb_export_backup</c> 成功时的 data 负载（对应 Rust 的 <c>BackupInfo</c>）。
/// </summary>
public sealed class BackupInfo
{
    [JsonPropertyName("dest")]
    public string Dest { get; set; } = string.Empty;

    [JsonPropertyName("core_db_bytes")]
    public long CoreDbBytes
    {
        get; set;
    }

    [JsonPropertyName("logs_db_bytes")]
    public long LogsDbBytes
    {
        get; set;
    }

    [JsonPropertyName("content_bytes")]
    public long ContentBytes
    {
        get; set;
    }

    [JsonPropertyName("items")]
    public long Items
    {
        get; set;
    }

    [JsonPropertyName("history")]
    public long History
    {
        get; set;
    }

    [JsonPropertyName("created_at_ms")]
    public long CreatedAtMs
    {
        get; set;
    }
}

public sealed class StatsWindow
{
    [JsonPropertyName("start_ts_ms")]
    public long StartTsMs
    {
        get; set;
    }

    [JsonPropertyName("end_ts_ms")]
    public long EndTsMs
    {
        get; set;
    }

    [JsonPropertyName("bucket_sec")]
    public int BucketSec
    {
        get; set;
    }
}

public sealed class CacheStatsPoint
{
    [JsonPropertyName("ts_ms")]
    public long TsMs
    {
        get; set;
    }

    [JsonPropertyName("cache_bytes")]
    public long CacheBytes
    {
        get; set;
    }
}

public sealed class NetworkStatsPoint
{
    [JsonPropertyName("ts_ms")]
    public long TsMs
    {
        get; set;
    }

    [JsonPropertyName("bytes_sent")]
    public long BytesSent
    {
        get; set;
    }

    [JsonPropertyName("bytes_recv")]
    public long BytesRecv
    {
        get; set;
    }
}

public sealed class ActivityStatsPoint
{
    [JsonPropertyName("ts_ms")]
    public long TsMs
    {
        get; set;
    }

    [JsonPropertyName("text_count")]
    public long TextCount
    {
        get; set;
    }

    [JsonPropertyName("image_count")]
    public long ImageCount
    {
        get; set;
    }

    [JsonPropertyName("files_count")]
    public long FilesCount
    {
        get; set;
    }
}
