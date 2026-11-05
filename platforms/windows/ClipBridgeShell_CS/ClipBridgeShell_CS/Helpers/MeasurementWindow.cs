// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/MeasurementWindow.cs
using System;
using System.Runtime.InteropServices;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 启动耗时测量专用的窗口显示策略（只在设置环境变量时生效，未设置时完全不介入）。
///
/// 背景：连续测量启动耗时要启动同一进程 5~7 次以上。
/// - <c>CB_STARTUP_NO_FOREGROUND=1</c>：不调用 <c>Activate()</c>，避免反复把用户正在用的窗口抢到前台。
/// - <c>CB_STARTUP_OFFSCREEN=1</c>（需同时设置上面那个）：把窗口先移到虚拟桌面之外再显示。
///   渲染照常发生，所以"到窗口可见"的测量语义不变；但屏幕上什么都不出现，
///   连续测量时用户不会看到窗口反复闪烁。同时打 WS_EX_TOOLWINDOW，避免出现在 Alt+Tab 列表里。
/// </summary>
internal static class MeasurementWindow
{
    public static readonly bool SuppressForeground =
        string.Equals(Environment.GetEnvironmentVariable("CB_STARTUP_NO_FOREGROUND"), "1", StringComparison.Ordinal);

    public static readonly bool OffScreen =
        string.Equals(Environment.GetEnvironmentVariable("CB_STARTUP_OFFSCREEN"), "1", StringComparison.Ordinal);

    private const int SW_SHOWNOACTIVATE = 4;
    private const int OffScreenX = -32000;
    private const int OffScreenY = -32000;

    private const int GWL_EXSTYLE = -20;
    private const long WS_EX_TOOLWINDOW = 0x00000080L;

    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_NOZORDER = 0x0004;
    private const uint SWP_NOACTIVATE = 0x0010;
    private const uint SWP_SHOWWINDOW = 0x0040;

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW", SetLastError = true)]
    private static extern IntPtr GetWindowLongPtr(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW", SetLastError = true)]
    private static extern IntPtr SetWindowLongPtr(IntPtr hWnd, int nIndex, IntPtr dwNewLong);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);

    /// <summary>
    /// 让窗口可见但不激活；<see cref="OffScreen"/> 时移到虚拟桌面之外。
    /// 返回实际走的分支名，供 StartupTrace 打点区分。
    /// </summary>
    public static string ShowWithoutFocus(IntPtr hwnd)
    {
        if (OffScreen)
        {
            var exStyle = GetWindowLongPtr(hwnd, GWL_EXSTYLE).ToInt64();
            _ = SetWindowLongPtr(hwnd, GWL_EXSTYLE, new IntPtr(exStyle | WS_EX_TOOLWINDOW));
            _ = SetWindowPos(hwnd, IntPtr.Zero, OffScreenX, OffScreenY, 0, 0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            return "window-shown-offscreen";
        }

        _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        return "window-shown-noactivate";
    }
}
