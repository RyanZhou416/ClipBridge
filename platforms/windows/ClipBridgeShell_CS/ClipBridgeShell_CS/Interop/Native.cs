using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Collections.Generic;
using System.Security;

namespace ClipBridge.Interop
{
    internal static class Native
    {
        // ★ 修改成你的实际 DLL 名（Windows：cb_core.dll，调试时放到 exe 同目录或 PATH）
        private const string Dll = "cb_core";

        // 错误码（和你的 cb_ffi.h 对齐即可；若命名不同，改这里的常量值）
        public const int CB_OK = 0;

        // ------------------- FFI 导入（与 cb_ffi.h 一致） -------------------
        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        private static extern int cb_logs_write(
            int level,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string category,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string message,
            IntPtr exception_or_null,
            IntPtr props_json_or_null,
            out long out_id);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        private static extern int cb_logs_query_after_id(
            long after_id, int level_min,
            IntPtr like_or_null, int limit,
            out IntPtr out_json_array);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        private static extern int cb_logs_query_range(
            long start_ms, long end_ms, int level_min,
            IntPtr like_or_null, int limit, int offset,
            out IntPtr out_json_array);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        private static extern int cb_logs_delete_before(
            long cutoff_ms, out long out_deleted);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        private static extern int cb_logs_stats(out IntPtr out_json);

        // 你的 FFI 里应该已有统一释放函数（名字可能是 cb_free/ffi_free 等）
        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        private static extern void cb_free(IntPtr ptr);

        // ------------------- 托管包装（给 VM 调用） -------------------
        private static readonly JsonSerializerOptions JsonOpts = new()
        {
            PropertyNameCaseInsensitive = true,
            NumberHandling = JsonNumberHandling.AllowReadingFromString
        };

        public static long LogsWrite(int level, string category, string message, string? exception = null, string? propsJson = null)
        {
            IntPtr exPtr = exception != null ? Marshal.StringToCoTaskMemUTF8(exception) : IntPtr.Zero;
            IntPtr propsPtr = propsJson != null ? Marshal.StringToCoTaskMemUTF8(propsJson) : IntPtr.Zero;
            try
            {
                var rc = cb_logs_write(level, category, message, exPtr, propsPtr, out long id);
                if (rc != CB_OK) throw new ExternalException($"cb_logs_write failed: {rc}");
                return id;
            }
            finally
            {
                if (exPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(exPtr);
                if (propsPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(propsPtr);
            }
        }

        public static List<Models.LogRow> LogsQueryAfterId(long afterId, int levelMin, string? like, int limit)
        {
            IntPtr likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
            IntPtr jsonPtr = IntPtr.Zero;
            try
            {
                var rc = cb_logs_query_after_id(afterId, levelMin, likePtr, limit, out jsonPtr);
                if (rc != CB_OK) throw new ExternalException($"cb_logs_query_after_id failed: {rc}");
                var json = PtrToUtf8(jsonPtr);
                return JsonSerializer.Deserialize<List<Models.LogRow>>(json, JsonOpts) ?? new();
            }
            finally
            {
                if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
                if (jsonPtr != IntPtr.Zero) cb_free(jsonPtr);
            }
        }

        public static List<Models.LogRow> LogsQueryRange(long startMs, long endMs, int levelMin, string? like, int limit, int offset)
        {
            IntPtr likePtr = like != null ? Marshal.StringToCoTaskMemUTF8(like) : IntPtr.Zero;
            IntPtr jsonPtr = IntPtr.Zero;
            try
            {
                var rc = cb_logs_query_range(startMs, endMs, levelMin, likePtr, limit, offset, out jsonPtr);
                if (rc != CB_OK) throw new ExternalException($"cb_logs_query_range failed: {rc}");
                var json = PtrToUtf8(jsonPtr);
                return JsonSerializer.Deserialize<List<Models.LogRow>>(json, JsonOpts) ?? new();
            }
            finally
            {
                if (likePtr != IntPtr.Zero) Marshal.FreeCoTaskMem(likePtr);
                if (jsonPtr != IntPtr.Zero) cb_free(jsonPtr);
            }
        }

        public static long LogsDeleteBefore(long cutoffMs)
        {
            var rc = cb_logs_delete_before(cutoffMs, out long deleted);
            if (rc != CB_OK) throw new ExternalException($"cb_logs_delete_before failed: {rc}");
            return deleted;
        }

        public static Models.LogStats LogsStats()
        {
            IntPtr jsonPtr = IntPtr.Zero;
            try
            {
                var rc = cb_logs_stats(out jsonPtr);
                if (rc != CB_OK) throw new ExternalException($"cb_logs_stats failed: {rc}");
                var json = PtrToUtf8(jsonPtr);
                return JsonSerializer.Deserialize<Models.LogStats>(json, JsonOpts) ?? new Models.LogStats();
            }
            finally
            {
                if (jsonPtr != IntPtr.Zero) cb_free(jsonPtr);
            }
        }

        private static string PtrToUtf8(IntPtr ptr)
        {
            if (ptr == IntPtr.Zero) return string.Empty;
            // 读取以 '\0' 结尾的 UTF-8
            int len = 0;
            while (Marshal.ReadByte(ptr, len) != 0) len++;
            var bytes = new byte[len];
            Marshal.Copy(ptr, bytes, 0, len);
            return Encoding.UTF8.GetString(bytes);
        }
    }
}
