// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/WindowExtensions.cs
using System;
using System.Runtime.InteropServices;

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

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
    /// 标题栏拖动 / 边框缩放期间临时摘掉窗口的背景（Mica），松手后立刻装回 <b>同一个</b> 背景实例。
    ///
    /// 因果链：Mica 不是应用自己绘制的像素，而是由 DWM（桌面合成器）直接作用在窗口目标上的背景材料。
    /// 窗口静止时这块合成结果可以缓存；一旦窗口正在移动/缩放，窗口目标每帧都在变，DWM 就必须
    /// 每帧重新合成一次 Mica —— 这份成本记在 dwm.exe 而不是本进程头上（实测拖拽时 DWM GPU 3D
    /// 2.5%→5.7%，本进程 ≈0.01%），但合成器被逐帧塞满会反压应用的 Present，最终表现为拖拽时
    /// 帧间隔变长（1% low 53ms、最差 89ms）。所以只在 WM_ENTERSIZEMOVE → WM_EXITSIZEMOVE 这个
    /// “窗口真的在动”的区间里把背景置空，静止时保持原样：拖拽拿回流畅度，静态外观与改动前一致。
    ///
    /// 机制：复用 <see cref="SetMinimumSize"/> 已经使用的 comctl32 SetWindowSubclass（不同的
    /// uIdSubclass），不替换窗口过程、也不需要自己向原 WndProc 转发；同一 HWND 上的多个子类化
    /// 会沿 DefSubclassProc 链依次调用，现有 WM_GETMINMAXINFO 最小尺寸逻辑不受影响。
    ///
    /// 环境变量：
    /// - <c>CB_NO_DRAG_BACKDROP_TOGGLE=1</c>：完全关闭本行为（连子类化都不安装），用于 A/B 测量；
    ///   未设置（默认）= 开启。
    /// - <c>CB_STARTUP_TRACE=1</c>：每次拖拽进入/退出各写一行 startup.log；默认不写，避免刷日志。
    ///
    /// 幂等/安全性：重复或嵌套的消息只保留第一次保存的实例；WM_EXITSIZEMOVE 没有配对时什么都不做；
    /// 背景本来就是 null 时直接返回；窗口在拖拽中被关闭（本应用的关闭是隐藏而非销毁）时在 WM_CLOSE
    /// 先把背景装回；窗口销毁时只清理托管状态。切换属性全部包 try/catch，失败只降级、不抛出。
    /// </summary>
    public static void SetDragBackdropToggle(this Window window)
    {
        if (!DragBackdropToggleEnabled)
        {
            // A/B 对照组：保持“拖拽时也一直挂着 Mica”的旧行为，不安装子类化。
            return;
        }

        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
        if (hwnd == IntPtr.Zero)
        {
            return;
        }

        // 子类化回调是静态的，靠 dwRefData 里的 GCHandle 把消息对回具体窗口。
        // 句柄由静态字段持有，保证在进程生命周期内有效（本应用只有一个主窗口）。
        if (!_dragBackdropWindowHandle.IsAllocated)
        {
            _dragBackdropWindowHandle = GCHandle.Alloc(window, GCHandleType.Normal);
        }

        _ = SetWindowSubclass(
            hwnd,
            DragBackdropSubclassProc,
            DragBackdropSubclassId,
            GCHandle.ToIntPtr(_dragBackdropWindowHandle));
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

    // ---- 拖拽期背景开关（见 SetDragBackdropToggle） ----

    /// <summary>用户开始拖动窗口 / 缩放窗口（进入 user32 的移动-缩放模态循环）。</summary>
    private const uint WM_ENTERSIZEMOVE = 0x0231;

    /// <summary>移动-缩放循环结束：松手、Esc 取消、Aero Snap 结束都会发这条消息。</summary>
    private const uint WM_EXITSIZEMOVE = 0x0232;

    /// <summary>窗口关闭：本应用的关闭其实是“隐藏窗口”（AppWindow.Closing 里取消销毁），必须借它收尾。</summary>
    private const uint WM_CLOSE = 0x0010;

    /// <summary>窗口过程被销毁：comctl32 会自动摘除子类化，这里只清理托管侧状态。</summary>
    private const uint WM_NCDESTROY = 0x0082;

    /// <summary>拖拽开始（背景已摘掉）时写入 startup.log 的稳定标记，便于按行聚合。</summary>
    private const string DragBackdropOffTrace = "drag-backdrop-off";

    /// <summary>拖拽结束（背景已恢复）时写入 startup.log 的稳定标记。</summary>
    private const string DragBackdropOnTrace = "drag-backdrop-on";

    /// <summary>切换失败时写入 startup.log 的标记前缀，后面接失败阶段（read/detach/restore）。</summary>
    private const string DragBackdropErrorTrace = "drag-backdrop-error";

    /// <summary>拖拽背景开关自己的 (hwnd, uIdSubclass) 标识，必须与最小尺寸的 MinSizeSubclassId 不同。</summary>
    private static readonly IntPtr DragBackdropSubclassId = (IntPtr)0x4344;

    /// <summary>与 MinMaxSubclassProc 同理：必须由静态字段持有，否则委托会被 GC 回收而留下野函数指针。</summary>
    private static readonly SubclassProc DragBackdropSubclassProc = OnDragBackdropMessage;

    /// <summary>CB_NO_DRAG_BACKDROP_TOGGLE=1 时关闭本行为；未设置（默认）= 开启。</summary>
    private static readonly bool DragBackdropToggleEnabled =
        !string.Equals(
            Environment.GetEnvironmentVariable("CB_NO_DRAG_BACKDROP_TOGGLE")?.Trim(),
            "1",
            StringComparison.Ordinal);

    /// <summary>传给 SetWindowSubclass 的 dwRefData 所对应的窗口句柄（进程内只分配一次）。</summary>
    private static GCHandle _dragBackdropWindowHandle;

    /// <summary>进入拖拽前窗口上的背景实例；退出时原样装回，复用同一个 MicaBackdrop 实例。</summary>
    private static SystemBackdrop? _savedBackdrop;

    /// <summary>当前是否处于“拖拽期背景已摘除”状态，用于吸收重复/嵌套的 WM_ENTERSIZEMOVE。</summary>
    private static bool _backdropSuspended;

    /// <summary>拖拽期背景开关的窗口过程：只观察进出移动-缩放循环的消息，其余原样交给子类化链。</summary>
    private static IntPtr OnDragBackdropMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam, IntPtr uIdSubclass, IntPtr dwRefData)
    {
        switch (msg)
        {
            case WM_ENTERSIZEMOVE:
                SuspendBackdropForDrag(dwRefData);
                break;

            case WM_EXITSIZEMOVE:
                // 松手 / Esc 取消 / Snap 结束都在这里恢复；没有配对的进入消息时是空操作。
                RestoreBackdropAfterDrag(dwRefData);
                break;

            case WM_CLOSE:
                // 拖拽过程中关闭窗口（例如 Alt+F4）：窗口会被隐藏而不是销毁，必须先装回背景，
                // 否则下一次显示窗口时会永久缺 Mica。非拖拽期间是无操作。
                RestoreBackdropAfterDrag(dwRefData);
                break;

            case WM_NCDESTROY:
                // 窗口真的销毁了：没有可恢复的目标，只清掉托管状态，
                // 避免同进程内重建窗口时沿用上一次残留的“已挂起”。
                _savedBackdrop = null;
                _backdropSuspended = false;
                break;
        }

        return DefSubclassProc(hWnd, msg, wParam, lParam);
    }

    /// <summary>
    /// 进入移动-缩放循环：保存当前 SystemBackdrop 后把它置空。保存/恢复的是同一个对象引用，
    /// 因此拖拽结束后重新装回的是 MainWindow 构造期创建的那个 MicaBackdrop，不会新建第二套 Mica。
    /// </summary>
    private static void SuspendBackdropForDrag(IntPtr dwRefData)
    {
        if (_backdropSuspended)
        {
            // 嵌套/重复的 WM_ENTERSIZEMOVE：保留第一次保存的实例，绝不能把 null 覆盖进去。
            return;
        }

        var window = ResolveDragBackdropWindow(dwRefData);
        if (window == null)
        {
            return;
        }

        SystemBackdrop? saved;
        try
        {
            saved = window.SystemBackdrop;
        }
        catch (Exception ex)
        {
            LogDragBackdropFailure("read", ex);
            return;
        }

        if (saved == null)
        {
            // 本来就没有背景（CB_NO_MICA=1、CB_FRAME_TRACE_MICA=0，或用户系统不支持 Mica）：
            // 无需切换，也不留下“已挂起”状态 —— 这正是“背景已为 null 时安全返回”的分支。
            return;
        }

        // 先落状态再改属性：SystemBackdrop 的 setter 可能同步回到窗口过程，提前置位让重入安全返回。
        _savedBackdrop = saved;
        _backdropSuspended = true;

        try
        {
            window.SystemBackdrop = null;
        }
        catch (Exception ex)
        {
            // 摘不掉只是退化成旧行为（拖拽时仍有 Mica），绝不能影响拖拽本身；状态已置位，
            // 退出时会尝试把原实例装回去（相同实例的赋值是幂等的）。
            LogDragBackdropFailure("detach", ex);
            return;
        }

        StartupTrace.Mark(DragBackdropOffTrace);
    }

    /// <summary>
    /// 退出移动-缩放循环：把进入时保存的同一个背景实例装回去。
    /// 任何失败都只降级 + 记录，绝不把异常抛回窗口过程。
    /// </summary>
    private static void RestoreBackdropAfterDrag(IntPtr dwRefData)
    {
        if (!_backdropSuspended)
        {
            // 没有配对的 WM_ENTERSIZEMOVE（或已经恢复过）：什么都不做。
            return;
        }

        // 先把状态清干净再碰属性：即使 setter 抛异常也不会卡在“永久挂起”，
        // 下一次拖拽仍能正常保存/恢复。
        var saved = _savedBackdrop;
        _savedBackdrop = null;
        _backdropSuspended = false;

        var window = ResolveDragBackdropWindow(dwRefData);
        if (window == null)
        {
            return;
        }

        try
        {
            // 装回同一个实例（MainWindow 构造里 new 的那个）。
            window.SystemBackdrop = saved;
        }
        catch (Exception ex)
        {
            LogDragBackdropFailure("restore", ex);
            return;
        }

        StartupTrace.Mark(DragBackdropOnTrace);
    }

    /// <summary>从 dwRefData 里的 GCHandle 取回窗口；句柄失效时只降级为“没有窗口”，不抛出。</summary>
    private static Window? ResolveDragBackdropWindow(IntPtr dwRefData)
    {
        if (dwRefData == IntPtr.Zero)
        {
            return null;
        }

        try
        {
            return GCHandle.FromIntPtr(dwRefData).Target as Window;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>
    /// 与 MainWindow / App 现有路径一致的失败记录：Debug 输出在 Release 下由编译器移除，
    /// 因此正常运行时不会刷日志；CB_STARTUP_TRACE=1 时再往 startup.log 补一行（StartupTrace 自身门控）。
    /// </summary>
    private static void LogDragBackdropFailure(string stage, Exception ex)
    {
        System.Diagnostics.Debug.WriteLine($"[DragBackdrop] {stage} failed: {ex}");
        StartupTrace.Mark($"{DragBackdropErrorTrace}-{stage}");
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
