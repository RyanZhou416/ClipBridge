using System;
using System.IO;

namespace ClipBridgeShell_CS.Interop;

/// <summary>
/// 原生 core DLL 定位器。生产实现保持历史搜索顺序（应用输出目录及其上溯 5 层、
/// 每层的 <c>win-x64</c> 子目录）；测试可注入固定路径来覆盖缺库/坏库/错误架构。
/// </summary>
internal interface ICoreLibraryLocator
{
    /// <summary>返回存在的 DLL 绝对路径；找不到时返回 null。</summary>
    string? FindCoreLibrary();
}

/// <summary>默认实现：从 <see cref="AppContext.BaseDirectory"/> 向上搜索。</summary>
internal sealed class DefaultCoreLibraryLocator : ICoreLibraryLocator
{
    private const string DllName = "core_ffi_windows.dll";
    private const int MaxLevels = 5;

    public string? FindCoreLibrary()
    {
        var current = AppContext.BaseDirectory.TrimEnd(
            Path.DirectorySeparatorChar,
            Path.AltDirectorySeparatorChar);

        for (var level = 0; level <= MaxLevels && !string.IsNullOrEmpty(current); level++)
        {
            var direct = Path.Combine(current, DllName);
            if (File.Exists(direct))
            {
                return direct;
            }

            var winx64 = Path.Combine(current, "win-x64", DllName);
            if (File.Exists(winx64))
            {
                return winx64;
            }

            current = Directory.GetParent(current)?.FullName;
        }

        return null;
    }
}
