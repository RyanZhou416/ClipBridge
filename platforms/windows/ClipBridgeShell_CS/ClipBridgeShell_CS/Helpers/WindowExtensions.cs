// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/WindowExtensions.cs
using System;
using System.Runtime.InteropServices;

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 普通 <see cref="Window"/> 缺少的窗口便利方法。
///
/// 这些方法逐一替代原先由 <c>WinUIEx.WindowEx</c> 基类 / WinUIEx 扩展方法提供的同名能力，
/// 只保留本应用真正用到的部分：为了几个薄封装继续承担 WindowEx 基类构造
/// （WindowManager + 消息监视器，实测约 87ms）并不划算。
/// </summary>
internal static class WindowExtensions
{
    /// <summary>
    /// 隐藏窗口（替代 WinUIEx 的 <c>Hide()</c>，其内部就是 ShowWindow(SW_HIDE)）。
    /// 隐藏后窗口会从任务栏和 Alt+Tab 列表消失；再次 <see cref="Show"/> 时保留原尺寸与位置。
    /// </summary>
    public static void Hide(this Window window) => window.AppWindow.Hide();

    /// <summary>
    /// 显示窗口（替代 WinUIEx 的 <c>Show()</c>，其内部就是 ShowWindow(SW_SHOW)）。
    /// </summary>
    public static void Show(this Window window) => window.AppWindow.Show();

    /// <summary>
    /// 把窗口置前（替代 WinUIEx.WindowEx.BringToFront()：原实现只是 SetForegroundWindow(hwnd)，
    /// 不会显示或激活已隐藏的窗口 —— 这里保持完全一致）。
    /// </summary>
    public static void BringToFront(this Window window)
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
        if (hwnd != IntPtr.Zero)
        {
            _ = SetForegroundWindow(hwnd);
        }
    }

    /// <summary>
    /// 设置最小窗口尺寸（单位：DIP），替代 XAML 里 <c>WindowEx.MinWidth/MinHeight</c> 的行为。
    ///
    /// WinUIEx 的实现是：WindowManager 用 SetWindowSubclass 截获 WM_GETMINMAXINFO，
    /// 把 DIP 按当前窗口 DPI 换算成物理像素后取 max 写进 ptMinTrackSize。这里复刻同一机制。
    ///
    /// 没有使用 OverlappedPresenter.PreferredMinimumWidth/PreferredMinimumHeight（本 SDK 版本已有）：
    /// 那两个属性不做 DPI 换算（见 microsoft-ui-xaml#10475），在缩放显示器上得到的下限会比
    /// 改动前更小，与“保持 MinWidth/MinHeight 行为”的要求不符。
    /// </summary>
    public static void SetMinimumSize(this Window window, int widthDip, int heightDip)
    {
        _minWidthDip = widthDip;
        _minHeightDip = heightDip;

        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
        if (hwnd == IntPtr.Zero)
        {
            return;
        }

        _ = SetWindowSubclass(hwnd, MinMaxSubclassProc, MinSizeSubclassId, IntPtr.Zero);
    }

    /// <summary>
    /// 显示一个纯文本提示对话框（替代 WinUIEx.WindowEx.ShowMessageDialogAsync(content, title)：
    /// 原实现也是用 ContentDialog + 窗口 Content 的 XamlRoot/RequestedTheme 展示文本）。
    /// 现有调用点只用于 TODO 占位提示。
    /// </summary>
    public static async Task ShowMessageDialogAsync(this Window window, string content, string title)
    {
        if (window.Content is not FrameworkElement root)
        {
            return;
        }

        var dialog = new ContentDialog
        {
            XamlRoot = root.XamlRoot,
            RequestedTheme = root.RequestedTheme,
            Title = title,
            Content = new TextBlock { Text = content, TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = "OK",
            CloseButtonText = "Close",
        };

        await dialog.ShowAsync();
    }

    private const uint WM_GETMINMAXINFO = 0x0024;

    // SetWindowSubclass 用的 (hwnd, id) 组合标识；本应用只有一个主窗口，用固定值即可。
    private static readonly IntPtr MinSizeSubclassId = (IntPtr)0x4342; // 'CB'

    // 必须由静态字段持有：SetWindowSubclass 只保存函数指针，不会阻止委托被 GC 回收。
    private static readonly SubclassProc MinMaxSubclassProc = OnMinMaxInfo;

    private static int _minWidthDip;
    private static int _minHeightDip;

    private static IntPtr OnMinMaxInfo(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam, IntPtr uIdSubclass, IntPtr dwRefData)
    {
        if (msg == WM_GETMINMAXINFO && lParam != IntPtr.Zero)
        {
            var dpi = GetDpiForWindow(hWnd);
            if (dpi == 0)
            {
                dpi = 96;
            }

            var info = Marshal.PtrToStructure<MinMaxInfo>(lParam);
            // 与 WinUIEx.WindowManager 的算法逐字一致：max(系统当前下限, DIP * dpi / 96)。
            info.MinTrackSize.X = Math.Max((int)(_minWidthDip * dpi / 96.0), info.MinTrackSize.X);
            info.MinTrackSize.Y = Math.Max((int)(_minHeightDip * dpi / 96.0), info.MinTrackSize.Y);
            Marshal.StructureToPtr(info, lParam, false);
        }

        return DefSubclassProc(hWnd, msg, wParam, lParam);
    }

    private delegate IntPtr SubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam, IntPtr uIdSubclass, IntPtr dwRefData);

    [StructLayout(LayoutKind.Sequential)]
    private struct Point
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MinMaxInfo
    {
        public Point Reserved;
        public Point MaxSize;
        public Point MaxPosition;
        public Point MinTrackSize;
        public Point MaxTrackSize;
    }

    [DllImport("comctl32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowSubclass(IntPtr hWnd, SubclassProc pfnSubclass, IntPtr uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetForegroundWindow(IntPtr hWnd);
}
