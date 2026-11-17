using System;
using System.Runtime.InteropServices;

namespace ClipBridgeShell_CS.Interop;

/// <summary>
/// 显式加载原生 core DLL 以读取 FFI 版本。这里不复用 <see cref="CoreInterop"/> 的
/// DllImport 解析缓存：首次探测如果加载了错误文件/架构，必须让本次初始化明确失败，
/// 不能因为进程里已经解析过正确模块而把错误输入掩盖成成功。
/// </summary>
internal static class CoreNativeLibrary
{
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate void GetFfiVersionFn(out uint major, out uint minor);

    /// <summary>
    /// 加载 <paramref name="dllPath"/> 并调用 <c>cb_get_ffi_version</c>。
    /// 缺失文件抛 <see cref="DllNotFoundException"/>，错误架构/坏 PE 抛
    /// <see cref="BadImageFormatException"/>，缺少导出抛 <see cref="EntryPointNotFoundException"/>。
    /// </summary>
    public static (uint Major, uint Minor) ProbeFfiVersion(string dllPath)
    {
        if (string.IsNullOrWhiteSpace(dllPath))
        {
            throw new ArgumentException("Core library path must not be empty.", nameof(dllPath));
        }

        var handle = NativeLibrary.Load(dllPath);
        try
        {
            var export = NativeLibrary.GetExport(handle, "cb_get_ffi_version");
            var probe = Marshal.GetDelegateForFunctionPointer<GetFfiVersionFn>(export);
            probe(out var major, out var minor);
            return (major, minor);
        }
        finally
        {
            NativeLibrary.Free(handle);
        }
    }
}
