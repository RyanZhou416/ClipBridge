using System;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace ClipBridgeShell_CS.Interop;

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

    // ==========================================================
    // 6. 日志系统 (Logs) - 补充部分
    // 虽然头文件中未列出，但 Native.cs 中使用了这些 API，且业务逻辑强依赖。
    // 假设这些 API 仍存在于 DLL 中 (或在头文件的其他部分)。
    // ==========================================================

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_write(
        int level,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string category,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string message,
        IntPtr exception_or_null,
        IntPtr props_json_or_null,
        out long out_id);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_query_after_id(
        long after_id, int level_min,
        IntPtr like_or_null, int limit,
        out IntPtr out_json_array);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_query_range(
        long start_ms, long end_ms, int level_min,
        IntPtr like_or_null, int limit, int offset,
        out IntPtr out_json_array);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_delete_before(
        long cutoff_ms, out long out_deleted);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int cb_logs_stats(out IntPtr out_json);

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
    /// 写入日志
    /// </summary>
    public static long LogsWrite(int level, string category, string message, string? exception = null, string? propsJson = null)
    {
        // 手动处理可空字符串指针
        IntPtr exPtr = exception != null ? Marshal.StringToCoTaskMemUTF8(exception) : IntPtr.Zero;
        IntPtr propsPtr = propsJson != null ? Marshal.StringToCoTaskMemUTF8(propsJson) : IntPtr.Zero;

        try
        {
            var rc = cb_logs_write(level, category, message, exPtr, propsPtr, out long id);
            if (rc != 0) throw new Exception($"cb_logs_write failed: {rc}");
            return id;
        }
        finally
        {
            if (exPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(exPtr);
            if (propsPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(propsPtr);
        }
    }

    /// <summary>
    /// 查询增量日志 (Tail)
    /// </summary>
    public static List<ClipBridgeShell_CS.Models.LogRow> LogsQueryAfterId(long afterId, int levelMin, string? like, int limit)
    {
        IntPtr likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        IntPtr jsonPtr = IntPtr.Zero;
        try
        {
            var rc = cb_logs_query_after_id(afterId, levelMin, likePtr, limit, out jsonPtr);
            if (rc != 0) throw new Exception($"cb_logs_query_after_id failed: {rc}");

            // 使用我们新加的 Helper 读取并释放
            var json = PtrToUtf8AndFree(jsonPtr);
            return JsonSerializer.Deserialize<List<ClipBridgeShell_CS.Models.LogRow>>(json, _jsonOpts) ?? new();
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
        }
    }

    /// <summary>
    /// 查询历史日志 (Page)
    /// </summary>
    public static List<ClipBridgeShell_CS.Models.LogRow> LogsQueryRange(long startMs, long endMs, int levelMin, string? like, int limit, int offset)
    {
        IntPtr likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
        IntPtr jsonPtr = IntPtr.Zero;
        try
        {
            var rc = cb_logs_query_range(startMs, endMs, levelMin, likePtr, limit, offset, out jsonPtr);
            if (rc != 0) throw new Exception($"cb_logs_query_range failed: {rc}");

            var json = PtrToUtf8AndFree(jsonPtr);
            return JsonSerializer.Deserialize<List<ClipBridgeShell_CS.Models.LogRow>>(json, _jsonOpts) ?? new();
        }
        finally
        {
            if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
        }
    }

    /// <summary>
    /// 删除旧日志
    /// </summary>
    public static long LogsDeleteBefore(long cutoffMs)
    {
        var rc = cb_logs_delete_before(cutoffMs, out long deleted);
        if (rc != 0) throw new Exception($"cb_logs_delete_before failed: {rc}");
        return deleted;
    }

    /// <summary>
    /// 获取日志统计
    /// </summary>
    public static ClipBridgeShell_CS.Models.LogStats LogsStats()
    {
        IntPtr jsonPtr = IntPtr.Zero;
        var rc = cb_logs_stats(out jsonPtr);
        if (rc != 0) throw new Exception($"cb_logs_stats failed: {rc}");

        var json = PtrToUtf8AndFree(jsonPtr);
        return JsonSerializer.Deserialize<ClipBridgeShell_CS.Models.LogStats>(json, _jsonOpts) ?? new ClipBridgeShell_CS.Models.LogStats();
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
            System.Diagnostics.Debug.WriteLine("=====================================");
            System.Diagnostics.Debug.WriteLine($"[CoreInterop] Query: {queryJson}");
            System.Diagnostics.Debug.WriteLine($"[CoreInterop] Result: {json}");
            System.Diagnostics.Debug.WriteLine("=====================================");

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


}
