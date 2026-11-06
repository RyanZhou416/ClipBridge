// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/WindowExtensions.cs
using System;
using System.Runtime.InteropServices;

using Microsoft.UI.Dispatching;
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
    /// 标题栏拖动 / 边框缩放期间临时关掉窗口的背景材料（Mica），松手后恢复原样。
    ///
    /// 因果链：Mica 不是应用自己绘制的像素，而是由 DWM（桌面合成器）直接作用在窗口目标上的背景材料。
    /// 窗口静止时这块合成结果可以缓存；一旦窗口正在移动/缩放，窗口目标每帧都在变，DWM 就必须
    /// 每帧重新合成一次 Mica —— 这份成本记在 dwm.exe 而不是本进程头上（实测拖拽时 DWM GPU 3D
    /// 2.5%→5.7%，本进程 ≈0.01%），但合成器被逐帧塞满会反压应用的 Present，最终表现为拖拽时
    /// 帧间隔变长（1% low 53ms、最差 89ms）。所以只在 WM_ENTERSIZEMOVE → WM_EXITSIZEMOVE 这个
    /// “窗口真的在动”的区间里把背景关掉，静止时保持原样：拖拽拿回流畅度，静态外观与改动前一致。
    /// 大窗口实测仍是 UI 线程 7.31ms/帧（117% 预算），说明还有“每帧把整棵重内容重新提交”
    /// 的固定成本；因此本方法现在还会在进入拖拽时把当前客户区像素冻结成一张覆盖层（见
    /// <see cref="DragSnapshot"/>），让拖拽期间屏幕上看到的是同一张位图，而不是每帧重画的
    /// hero 图/透明度遮罩/图表/亚克力卡片。
    ///
    /// 机制：复用 <see cref="SetMinimumSize"/> 已经使用的 comctl32 SetWindowSubclass（不同的
    /// uIdSubclass），不替换窗口过程、也不需要自己向原 WndProc 转发；同一 HWND 上的多个子类化
    /// 会沿 DefSubclassProc 链依次调用，现有 WM_GETMINMAXINFO 最小尺寸逻辑不受影响。
    ///
    /// 视觉保持：Mica 一旦消失，透明的 XAML 页面会直接透出桌面壁纸（实测同一区域从 #1C1C1E
    /// 变成 #658DA6）。所以摘掉 backdrop 的同时，用 Mica 文档规定的回退外观
    /// （SolidBackgroundFillColorBase，见 <see cref="DragBackdropFill"/>）把内容根刷成不透明；
    /// 退出时先恢复 DWMWA_SYSTEMBACKDROP_TYPE，再在下一帧 Low 优先级撤掉填充/覆盖层，
    /// 避免 DWM 异步应用属性期间闪一帧壁纸。快照覆盖层显示在内容根最上层，且抓取的是“包含
    /// Mica 的屏幕像素”，所以拖拽期间用户看到的是进入拖拽前那一帧的精确画面。
    ///
    /// 关掉 backdrop 的方式：优先
    /// DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE=38, DWMSBT_NONE=0)，
    /// 不碰 <see cref="Window.SystemBackdrop"/> —— MicaBackdrop 控制器保持存活，恢复是“瞬间”的；
    /// 只有 DWM 属性调用失败（例如 Windows 10 没有属性 38）才退回旧的 SystemBackdrop = null 路径。
    ///
    /// 环境变量与优先级（从高到低；高优先级命中后，低优先级的开关不再改变该行为）：
    /// 1. <c>CB_NO_DRAG_BACKDROP_TOGGLE=1</c>：完全关闭本行为（含快照、填充、强制状态），
    ///    连子类化都不安装 —— 与改动前逐字一致；
    /// 2. <c>CB_NO_DRAG_SNAPSHOT=1</c>：只关闭快照，保留“回退色填充 + DWM backdrop 开关”；
    /// 3. <c>CB_DRAG_FILL_ONLY=1</c>：只铺回退色、不碰 DWM backdrop（也不做快照），
    ///    用于验证“纯不透明填充”能否让 DWM 自行省掉 Mica 合成；
    /// 4. <c>CB_FORCE_DRAG_SNAPSHOT=1</c>：启动后等 Shell 布局稳定，抓一次快照并一直停留在
    ///    “快照覆盖 + 回退色 + DWMSBT_NONE”状态（用于和正常 Mica 外观做截图像素对比）；
    ///    若同时设置 <c>CB_FORCE_DRAG_FILL=1</c>，快照优先；若快照路径被 2/3 关闭，
    ///    则本开关无效（不会偷偷退回强制填充，避免误判截图属于哪种状态）；
    /// 5. <c>CB_FORCE_DRAG_FILL=1</c>：启动后一直停留在“伪拖拽”状态（填充 + DWMSBT_NONE），
    ///    不做快照，直到进程退出；
    /// 6. 以上都未设置（默认）：普通拖拽走“快照 + 填充 + DWM backdrop 开关”。
    ///
    /// 失败回退：
    /// - 快照抓取/建位图失败（Shell 尚未构建、BitBlt 失败、写位图失败等）→ 本次拖拽只走填充；
    /// - 填充也失败且没有可用的快照 → 不关 backdrop，退化为“拖拽时仍挂 Mica”；
    /// - 拖拽中途客户区尺寸变化（边框缩放）→ 第一次检测到尺寸变化就隐藏快照、改走填充，
    ///   本段手势剩余时间不再重抓；退出时照常恢复 Mica；
    /// - 有快照但填充失败时仍可安全关 backdrop（覆盖层已经证明能盖住整个客户区）。
    ///
    /// 幂等/安全性：重复或嵌套的进入只保留第一次保存的状态；没有配对的退出什么都不做；
    /// 背景本来就是 null 时直接返回（此时快照也不会被使用，视觉零变化）；拖拽中关闭（WM_CLOSE）
    /// 先恢复；窗口销毁（WM_NCDESTROY）只清理托管状态。所有 Win32/属性操作都包 try/catch，
    /// 失败只降级、不抛出。日志一律经 <c>CB_STARTUP_TRACE=1</c> 门控。
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
    /// 调试开关 <c>CB_FORCE_DRAG_FILL=1</c>：启动后让窗口一直停留在“拖拽中”的视觉状态
    /// （内容根铺 Mica 回退色 + DWM backdrop = DWMSBT_NONE），直到进程退出。
    /// 用于截一张与正常 Mica 外观对比的图；未设置时立即返回，行为零变化。
    ///
    /// 内容根要等 ActivationService 挂上占位层 / ShellPage 之后才存在，所以用
    /// DispatcherQueueTimer 周期检查：内容根被替换（占位层 → Shell）或主题变化时重新铺色，
    /// DWM 属性只设置一次。CB_DRAG_FILL_ONLY=1 同样生效（只铺色、不碰 DWM）。
    /// </summary>
    public static void EnableForcedDragFill(this Window window)
    {
        // 快照优先：两个强制开关同时设置时只进入快照状态，避免截图归属不明。
        if (!DragBackdropToggleEnabled
            || !ForceDragFillEnabled
            || ForcedSnapshotModeEnabled
            || _forcedDragFillTimer != null)
        {
            return;
        }

        var queue = window.DispatcherQueue;
        if (queue == null)
        {
            return;
        }

        // 先试一次：内容通常还没挂上，这里基本是空操作；真正的应用交给定时器。
        ApplyForcedDragFill(window);

        var timer = queue.CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(100);
        timer.IsRepeating = true;
        timer.Tick += (_, _) => ApplyForcedDragFill(window);
        _forcedDragFillTimer = timer;
        timer.Start();
    }

    /// <summary>
    /// 调试开关 <c>CB_FORCE_DRAG_SNAPSHOT=1</c>：启动后等 Shell 布局稳定，抓一次客户区快照
    /// 并一直停留在“快照覆盖 + 回退色 + DWMSBT_NONE”状态，直到进程退出。用于截一张与正常
    /// Mica 外观做像素对比的图；未设置时立即返回，行为零变化。
    ///
    /// 为什么需要定时器等：MainWindow 构造时内容根还是空/占位层，ShellPage 要等首帧之后才
    /// 以 Low 优先级构建。这里每 100ms 检查一次，命中“覆盖层已存在且已有布局尺寸”后再等
    /// <see cref="ForcedSnapshotSettleMs"/>，让 Shell 的第一帧/背景图解码落定，然后才抓图；
    /// 抓图必须在 Mica 还挂着的时候做（否则抓到的是回退色）。
    ///
    /// 失败降级：超过 <see cref="ForcedSnapshotMaxWaitMs"/> 仍抓不到，就退回强制填充
    /// （CB_FORCE_DRAG_FILL 的视觉效果）并写 trace，保证至少能截到一张确定状态的图。
    /// </summary>
    public static void EnableForcedDragSnapshot(this Window window)
    {
        if (!ForcedSnapshotModeEnabled || _forcedSnapshotTimer != null)
        {
            return;
        }

        var queue = window.DispatcherQueue;
        if (queue == null)
        {
            return;
        }

        var timer = queue.CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(100);
        timer.IsRepeating = true;
        timer.Tick += (_, _) => ApplyForcedDragSnapshot(window);
        _forcedSnapshotTimer = timer;
        timer.Start();
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

    /// <summary>客户区尺寸改变（边框缩放、最大化、DPI 变化）。拖拽中收到 = 快照会拉伸，必须放弃。</summary>
    private const uint WM_SIZE = 0x0005;

    /// <summary>窗口位置或尺寸改变；纯移动时也会收到，所以要和抓取尺寸比对后才放弃快照。</summary>
    private const uint WM_WINDOWPOSCHANGED = 0x0047;

    /// <summary>拖拽开始（背景已关掉、回退色已铺）时写入 startup.log 的稳定标记，便于按行聚合。</summary>
    private const string DragBackdropOffTrace = "drag-backdrop-off";

    /// <summary>拖拽结束（DWM 属性已恢复、填充已安排撤销）时写入 startup.log 的稳定标记。</summary>
    private const string DragBackdropOnTrace = "drag-backdrop-on";

    /// <summary>切换失败时写入 startup.log 的标记前缀，后面接失败阶段（read/fill/detach/restore/forced-fill）。</summary>
    private const string DragBackdropErrorTrace = "drag-backdrop-error";

    /// <summary>内容根铺回退色失败：此时保留 Mica、不改 DWM backdrop，绝不把壁纸露出来。</summary>
    private const string DragBackdropPaintFailedTrace = "drag-backdrop-paint-failed";

    /// <summary>DwmGetWindowAttribute(38) 失败（如 Windows 10）：仍按默认 2 继续。</summary>
    private const string DragBackdropDwmReadFailedTrace = "drag-backdrop-dwm-read-failed";

    /// <summary>DwmSetWindowAttribute(38, DWMSBT_NONE) 失败：退回 SystemBackdrop = null 路径。</summary>
    private const string DragBackdropDwmSetFailedTrace = "drag-backdrop-dwm-set-failed";

    /// <summary>退出时恢复 DWMWA_SYSTEMBACKDROP_TYPE 失败：再让 XAML 重装一次 Mica。</summary>
    private const string DragBackdropDwmRestoreFailedTrace = "drag-backdrop-dwm-restore-failed";

    /// <summary>CB_FORCE_DRAG_FILL=1 第一次真正进入“拖拽中”状态时写一行。</summary>
    private const string DragBackdropForcedFillTrace = "drag-backdrop-force-fill";

    /// <summary>回退画刷的实际来源（theme-brush / app-resource-brush / theme-color / hardcoded-*）。</summary>
    private const string DragBackdropFillSourceTrace = "drag-backdrop-fill-source";

    /// <summary>有快照但回退色铺不上：覆盖层仍可安全盖住整个客户区，因此继续关 backdrop。</summary>
    private const string DragSnapshotFillUnavailableTrace = "drag-snapshot-fill-unavailable";

    /// <summary>拖拽中途客户区尺寸变了：隐藏快照，本段手势剩余时间退回填充路径。</summary>
    private const string DragSnapshotDroppedResizeTrace = "drag-snapshot-dropped-resize";

    /// <summary>CB_FORCE_DRAG_SNAPSHOT=1 第一次真正进入“快照伪拖拽”状态时写一行。</summary>
    private const string DragSnapshotForcedTrace = "drag-snapshot-force";

    /// <summary>CB_FORCE_DRAG_SNAPSHOT=1 长时间抓不到图，降级为强制填充状态。</summary>
    private const string DragSnapshotForcedFillFallbackTrace = "drag-snapshot-force-fill-fallback";

    /// <summary>强制快照收到 WM_CLOSE 时恢复背景/隐藏覆盖层后写一行。</summary>
    private const string DragSnapshotForcedOffTrace = "drag-snapshot-force-off";

    /// <summary>强制快照看到 Shell 布局完成后再等这么久（毫秒），让首帧/背景图解码落定。</summary>
    private const int ForcedSnapshotSettleMs = 800;

    /// <summary>强制快照等待覆盖层可用的上限（毫秒）；超时降级为强制填充。</summary>
    private const int ForcedSnapshotMaxWaitMs = 10000;

    /// <summary>强制快照定时器周期（毫秒），也是就绪等待的计时步长。</summary>
    private const int ForcedSnapshotTickMs = 100;

    // DWMWA_SYSTEMBACKDROP_TYPE：Windows 11 22H2 (22621) 起公开的 DWM 窗口属性；
    // 写入/读取都用 cbAttribute=sizeof(int)=4。Windows 10 上这两个调用会失败，走旧路径。
    private const int DWMWA_SYSTEMBACKDROP_TYPE = 38;
    private const int DWMSBT_NONE = 0;

    /// <summary>DWMSBT_MAINWINDOW：Mica Base，也是读取失败时的默认恢复值。</summary>
    private const int DWMSBT_MAINWINDOW = 2;

    /// <summary>拖拽背景开关自己的 (hwnd, uIdSubclass) 标识，必须与最小尺寸的 MinSizeSubclassId 不同。</summary>
    private static readonly IntPtr DragBackdropSubclassId = (IntPtr)0x4344;

    /// <summary>与 MinMaxSubclassProc 同理：必须由静态字段持有，否则委托会被 GC 回收而留下野函数指针。</summary>
    private static readonly SubclassProc DragBackdropSubclassProc = OnDragBackdropMessage;

    /// <summary>CB_NO_DRAG_BACKDROP_TOGGLE=1 时关闭本行为（含强制填充）；未设置（默认）= 开启。</summary>
    private static readonly bool DragBackdropToggleEnabled = !IsEnvSwitch("CB_NO_DRAG_BACKDROP_TOGGLE");

    /// <summary>CB_DRAG_FILL_ONLY=1：只铺回退色，不碰 DWM backdrop。</summary>
    private static readonly bool DragFillOnlyEnabled = IsEnvSwitch("CB_DRAG_FILL_ONLY");

    /// <summary>CB_FORCE_DRAG_FILL=1：启动后一直停留在“拖拽中”状态，供截图对比。</summary>
    private static readonly bool ForceDragFillEnabled = IsEnvSwitch("CB_FORCE_DRAG_FILL");

    /// <summary>CB_FORCE_DRAG_SNAPSHOT=1：启动后一直停留在“快照伪拖拽”状态，供截图对比。</summary>
    private static readonly bool ForceDragSnapshotEnabled = IsEnvSwitch("CB_FORCE_DRAG_SNAPSHOT");

    /// <summary>
    /// 普通拖拽是否使用快照覆盖层。优先级：
    /// CB_NO_DRAG_BACKDROP_TOGGLE=1（连子类化都不安装）→ CB_NO_DRAG_SNAPSHOT=1（关快照）
    /// → CB_DRAG_FILL_ONLY=1（只填充、不碰 backdrop）→ 默认开启快照。
    /// </summary>
    private static readonly bool SnapshotPathEnabled =
        DragBackdropToggleEnabled
        && !IsEnvSwitch("CB_NO_DRAG_SNAPSHOT")
        && !DragFillOnlyEnabled;

    /// <summary>
    /// CB_FORCE_DRAG_SNAPSHOT 是否真正生效。快照路径被 <c>CB_NO_DRAG_SNAPSHOT</c> /
    /// <c>CB_DRAG_FILL_ONLY</c> 关闭时本开关无效（不偷偷退回强制填充，避免截图归属歧义）。
    /// 若同时设置 CB_FORCE_DRAG_FILL，快照优先。
    /// </summary>
    private static readonly bool ForcedSnapshotModeEnabled = SnapshotPathEnabled && ForceDragSnapshotEnabled;

    /// <summary>传给 SetWindowSubclass 的 dwRefData 所对应的窗口句柄（进程内只分配一次）。</summary>
    private static GCHandle _dragBackdropWindowHandle;

    /// <summary>进入拖拽前窗口上的背景实例；仅在 DWM 路径失败、退回 SystemBackdrop=null 时用于恢复。</summary>
    private static SystemBackdrop? _savedBackdrop;

    /// <summary>进入拖拽时读到的 DWMWA_SYSTEMBACKDROP_TYPE；读取失败时保持默认 DWMSBT_MAINWINDOW。</summary>
    private static int _savedBackdropType = DWMSBT_MAINWINDOW;

    /// <summary>本次拖拽是否真的把 DWMWA_SYSTEMBACKDROP_TYPE 设成了 DWMSBT_NONE（决定退出时走哪条恢复路径）。</summary>
    private static bool _dwmBackdropDisabled;

    /// <summary>是否走了“DWM 调用失败 → SystemBackdrop = null”的兜底路径（退出时需要装回实例）。</summary>
    private static bool _systemBackdropNulled;

    /// <summary>当前是否处于“拖拽期背景已关掉”状态，用于吸收重复/嵌套的 WM_ENTERSIZEMOVE。</summary>
    private static bool _backdropSuspended;

    /// <summary>本次拖拽是否成功显示了快照覆盖层（决定 WM_SIZE 时要不要放弃快照）。</summary>
    private static bool _dragSnapshotShown;

    /// <summary>
    /// 本次拖拽的回退色是否真的铺上了。尺寸变化放弃快照时：只有铺上了才能直接撤覆盖层，
    /// 否则撤掉会露出桌面壁纸，必须先恢复 backdrop。
    /// </summary>
    private static bool _dragFillApplied;

    /// <summary>CB_FORCE_DRAG_SNAPSHOT 是否已经生效（生效后忽略真实进出消息，保持状态）。</summary>
    private static bool _forcedSnapshotActive;

    /// <summary>CB_FORCE_DRAG_FILL 的周期检查定时器（进程生命周期内保持引用）。</summary>
    private static DispatcherQueueTimer? _forcedDragFillTimer;

    /// <summary>CB_FORCE_DRAG_SNAPSHOT 的启动等待/抓取定时器（进程生命周期内保持引用）。</summary>
    private static DispatcherQueueTimer? _forcedSnapshotTimer;

    /// <summary>强制快照：看到 Shell 布局完成后已经过去的等待时长（毫秒），用于再等几帧稳定。</summary>
    private static int _forcedSnapshotSettleMs;

    /// <summary>强制快照：从启动定时器起累计等待的时长（毫秒），超过上限就降级为强制填充。</summary>
    private static int _forcedSnapshotWaitedMs;

    /// <summary>强制快照：抓取失败次数；失败后继续重试，直到总等待超时。</summary>
    private static int _forcedSnapshotCaptureFailures;

    /// <summary>强制填充是否已写过那唯一一行 trace。</summary>
    private static bool _forcedDragFillApplied;

    /// <summary>强制填充是否已经设置过 DWM backdrop（只设一次）。</summary>
    private static bool _forcedDwmDisabled;

    private static bool IsEnvSwitch(string name) =>
        string.Equals(Environment.GetEnvironmentVariable(name)?.Trim(), "1", StringComparison.Ordinal);

    /// <summary>拖拽期背景开关的窗口过程：只观察进出移动-缩放循环的消息，其余原样交给子类化链。</summary>
    private static IntPtr OnDragBackdropMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam, IntPtr uIdSubclass, IntPtr dwRefData)
    {
        switch (msg)
        {
            case WM_ENTERSIZEMOVE:
                if (_forcedSnapshotActive)
                {
                    // CB_FORCE_DRAG_SNAPSHOT：已经固定在快照状态，真实拖动也保持不动。
                    break;
                }

                SuspendBackdropForDrag(dwRefData);
                break;

            case WM_EXITSIZEMOVE:
                if (_forcedSnapshotActive)
                {
                    // 强制快照状态要一直保持到进程退出，不因一次真实拖动而恢复 Mica。
                    break;
                }

                // 松手 / Esc 取消 / Snap 结束都在这里恢复；没有配对的进入消息时是空操作。
                RestoreBackdropAfterDrag(dwRefData);
                break;

            case WM_SIZE:
            case WM_WINDOWPOSCHANGED:
                // 边框缩放（以及最大化/DPI 变化）会让客户区尺寸在移动-缩放循环里变化：
                // 快照会被 Stretch 拉花，第一次检测到就放弃快照，本段手势剩余时间退回填充。
                // 强制快照是静态截图状态（不参与真实拖拽），这里不拆它。
                if (_backdropSuspended && !_forcedSnapshotActive)
                {
                    DropSnapshotIfResized(dwRefData);
                }

                break;

            case WM_CLOSE:
                // 拖拽过程中关闭窗口（例如 Alt+F4）：窗口会被隐藏而不是销毁，必须先装回背景，
                // 否则下一次显示窗口时会永久缺 Mica。非拖拽期间是无操作。
                if (_forcedSnapshotActive)
                {
                    ExitForcedSnapshot(dwRefData);
                }
                else
                {
                    RestoreBackdropAfterDrag(dwRefData);
                }

                // 关闭路径上窗口马上隐藏：DWM 已经先恢复，同步撤覆盖层不会产生可见跳变，
                // 也避免“隐藏后 Low 回调没跑、下次显示还挂着旧快照”。
                DragSnapshot.HideNow();
                break;

            case WM_NCDESTROY:
                // 窗口真的销毁了：没有可恢复的目标，只清掉托管状态，
                // 避免同进程内重建窗口时沿用上一次残留的“已挂起”/填充记录。
                ResetDragBackdropState();
                DragBackdropFill.Reset();
                StopForcedDragFill();
                StopForcedDragSnapshot();
                DragSnapshot.Reset();
                _forcedSnapshotActive = false;
                break;
        }

        return DefSubclassProc(hWnd, msg, wParam, lParam);
    }

    /// <summary>
    /// 进入移动-缩放循环：
    /// ① 读并记住当前 DWMWA_SYSTEMBACKDROP_TYPE（读取失败按文档默认值 DWMSBT_MAINWINDOW）；
    /// ② 用 Mica 文档规定的回退色铺满内容根；
    /// ③ 把 DWM backdrop 设为 DWMSBT_NONE —— 不碰 SystemBackdrop，MicaBackdrop 控制器保持存活。
    /// 只有 DWM 设置失败时才退回旧的 SystemBackdrop = null 路径。
    /// </summary>
    private static void SuspendBackdropForDrag(IntPtr dwRefData)
    {
        if (_backdropSuspended)
        {
            // 嵌套/重复的 WM_ENTERSIZEMOVE：保留第一次保存的状态，绝不能覆盖。
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
            // 本来就没有背景（CB_NO_MICA=1、CB_FRAME_TRACE_MICA=0，或系统不支持 Mica）：
            // 没有可关的 backdrop；这时候铺回退色反而会改变外观，所以直接返回。
            return;
        }

        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);

        if (!TryGetDwmBackdropType(hwnd, out var savedType))
        {
            // Windows 10 没有属性 38；读不到就按文档默认的 Mica Base 恢复。
            // 后面设置 DWMSBT_NONE 通常也会失败，从而走 SystemBackdrop = null 的旧路径。
            savedType = DWMSBT_MAINWINDOW;
            StartupTrace.Mark(DragBackdropDwmReadFailedTrace);
        }

        // 先落状态再改属性：属性 setter / DWM 调用如果同步回到窗口过程，提前置位让重入安全返回。
        _savedBackdrop = saved;
        _savedBackdropType = savedType;
        _dwmBackdropDisabled = false;
        _systemBackdropNulled = false;
        _backdropSuspended = true;
        _dragSnapshotShown = false;

        // ① 先抓快照并显示覆盖层 —— 必须在 DWM backdrop 切到 NONE 之前：
        //    快照抓的是屏幕像素（含 Mica），切完 backdrop 再抓就只能抓到回退色了。
        //    抓取失败（Shell 还没构建 / BitBlt 失败 / 位图创建失败）只记录，不抛出。
        if (SnapshotPathEnabled)
        {
            _dragSnapshotShown = DragSnapshot.TryCaptureAndShow(window);
        }

        // ② 再铺回退色：它是覆盖层下面的兜底（覆盖层若因尺寸/重绘出现缝隙也不会露出壁纸），
        //    也是尺寸变化后放弃快照时立即生效的降级外观。
        bool painted;
        try
        {
            painted = DragBackdropFill.TryApply(window);
        }
        catch (Exception ex)
        {
            painted = false;
            LogDragBackdropFailure("fill", ex);
        }

        if (!painted && !_dragSnapshotShown)
        {
            // 既没有回退色也没有快照：不要再关 backdrop，否则会重新露出壁纸。
            // 退化为“拖拽时仍挂 Mica”：只损失流畅度，不改变外观。
            StartupTrace.Mark(DragBackdropPaintFailedTrace);
            ResetDragBackdropState();
            return;
        }

        _dragFillApplied = painted;

        if (!painted)
        {
            // 有快照时回退色铺不上是安全的：覆盖层已经证明能盖住整个客户区。
            // 但尺寸变化必须放弃快照时，下面的 DropSnapshotIfResized 会先恢复 backdrop 再撤覆盖层。
            StartupTrace.Mark(DragSnapshotFillUnavailableTrace);
        }
        else
        {
            // 诊断：记录实际用的回退画刷来源（资源缺失时会退到 hardcoded-*）。
            StartupTrace.Mark($"{DragBackdropFillSourceTrace}={DragBackdropFill.LastBrushSource}");
        }

        if (DragFillOnlyEnabled)
        {
            // A/B：只验证“不透明填充”本身能否让 DWM 省掉合成，不动 backdrop。
            // （SnapshotPathEnabled 在本模式下为 false，所以这里不会残留覆盖层。）
            StartupTrace.Mark(DragBackdropOffTrace);
            return;
        }

        if (TrySetDwmBackdropType(hwnd, DWMSBT_NONE))
        {
            _dwmBackdropDisabled = true;
            StartupTrace.Mark(DragBackdropOffTrace);
            return;
        }

        // DWM 属性不可用（Windows 10 / 旧 build）：退回改动前的 SystemBackdrop = null 路径。
        StartupTrace.Mark(DragBackdropDwmSetFailedTrace);
        try
        {
            window.SystemBackdrop = null;
            _systemBackdropNulled = true;
        }
        catch (Exception ex)
        {
            // 两条路都关不掉只是退化成旧行为（拖拽时仍有 Mica）；
            // 回退色已经铺上，退出时照常清掉，绝不影响拖拽本身。
            LogDragBackdropFailure("detach", ex);
        }

        StartupTrace.Mark(DragBackdropOffTrace);
    }

    /// <summary>
    /// 退出移动-缩放循环：先恢复 DWMWA_SYSTEMBACKDROP_TYPE（或兜底的 SystemBackdrop 实例），
    /// 再把回退色填充安排到下一轮 Low 优先级撤销。
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
        var savedBackdrop = _savedBackdrop;
        var savedType = _savedBackdropType;
        var dwmDisabled = _dwmBackdropDisabled;
        var systemBackdropNulled = _systemBackdropNulled;
        ResetDragBackdropState();

        var window = ResolveDragBackdropWindow(dwRefData);

        if (!DragFillOnlyEnabled && window != null)
        {
            if (dwmDisabled)
            {
                var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
                if (!TrySetDwmBackdropType(hwnd, savedType))
                {
                    StartupTrace.Mark(DragBackdropDwmRestoreFailedTrace);

                    // 极低概率的兜底：让 XAML 重新把 MicaBackdrop 装回去。
                    // 先置 null 再赋原实例：相同实例直接赋值可能被 setter 判成 no-op。
                    try
                    {
                        window.SystemBackdrop = null;
                        window.SystemBackdrop = savedBackdrop;
                    }
                    catch (Exception ex)
                    {
                        LogDragBackdropFailure("restore", ex);
                    }
                }
            }
            else if (systemBackdropNulled)
            {
                try
                {
                    // 装回同一个实例（MainWindow 构造里 new 的那个）。
                    window.SystemBackdrop = savedBackdrop;
                }
                catch (Exception ex)
                {
                    LogDragBackdropFailure("restore", ex);
                }
            }
        }

        // 顺序要求：原 backdrop 已经恢复（上面是同步 DwmSetWindowAttribute），
        // 覆盖层才排到下一轮 Low 撤销 —— 让 DWM 先应用恢复后的 Mica，再撤遮罩。
        // 与填充同一优先级：同一帧内先隐藏覆盖层、再清填充，两次移除都在下一帧呈现之前。
        // 位图保留在缓存里，下次拖拽同尺寸可复用。window 为 null 时立即隐藏。
        DragSnapshot.ScheduleHide(window);

        // 最后才撤填充，且排到下一轮 Low：让 DWM 先异步应用刚恢复的 backdrop，
        // 否则会闪一帧桌面壁纸。window 为 null 时 ScheduleClear 会用内部持有的窗口。
        DragBackdropFill.ScheduleClear(window);
        StartupTrace.Mark(DragBackdropOnTrace);
    }

    /// <summary>
    /// 拖拽中途检测客户区尺寸变化：第一次发现快照尺寸与当前客户区不一致时，隐藏快照并放弃，
    /// 本段手势剩余时间退回“填充 + 无 backdrop”的旧路径（快照拉伸比重新渲染更难解释）。
    /// 纯移动（尺寸不变）不会触发任何动作。
    /// </summary>
    private static void DropSnapshotIfResized(IntPtr dwRefData)
    {
        if (!_dragSnapshotShown)
        {
            return;
        }

        var window = ResolveDragBackdropWindow(dwRefData);
        if (window == null)
        {
            return;
        }

        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
        if (hwnd == IntPtr.Zero)
        {
            return;
        }

        if (!GetClientRect(hwnd, out var client))
        {
            return;
        }

        if (client.Right == DragSnapshot.CapturedWidth && client.Bottom == DragSnapshot.CapturedHeight)
        {
            return;
        }

        if (!_dragFillApplied && !TryRestoreBackdropForResize(window, hwnd))
        {
            // 极罕见：回退色没铺上、原 backdrop 也恢复不了。此时撤掉覆盖层就会露出壁纸，
            // 宁可让快照继续拉伸（视觉暂时变形），也不能出现空白/壁纸窗口。
            StartupTrace.Mark($"{DragSnapshotDroppedResizeTrace}-no-fill-keep");
            return;
        }

        // 先落状态再隐藏：隐藏动作可能同步触发布局/渲染回调，重入时不应再走一次。
        _dragSnapshotShown = false;
        DragSnapshot.HideNow();
        StartupTrace.Mark(DragSnapshotDroppedResizeTrace);
    }

    /// <summary>
    /// 尺寸变化放弃快照、但没有回退色可兜底时的补救：把本次拖拽关掉的 backdrop 先恢复，
    /// 再让调用方隐藏覆盖层。恢复成功后退出路径不会再重复恢复（标志已清）。
    /// 返回 false 表示两条恢复路径都失败，调用方必须保留覆盖层。
    /// </summary>
    private static bool TryRestoreBackdropForResize(Window window, IntPtr hwnd)
    {
        try
        {
            if (_dwmBackdropDisabled)
            {
                if (TrySetDwmBackdropType(hwnd, _savedBackdropType))
                {
                    _dwmBackdropDisabled = false;
                    StartupTrace.Mark("drag-snapshot-resize-restore-backdrop");
                    return true;
                }

                // DWM 属性不可用：退回 SystemBackdrop 实例路径。
                try
                {
                    window.SystemBackdrop = null;
                    window.SystemBackdrop = _savedBackdrop;
                    _dwmBackdropDisabled = false;
                    _systemBackdropNulled = false;
                    StartupTrace.Mark("drag-snapshot-resize-restore-backdrop");
                    return true;
                }
                catch (Exception ex)
                {
                    LogDragBackdropFailure("resize-restore", ex);
                    return false;
                }
            }

            if (_systemBackdropNulled)
            {
                try
                {
                    window.SystemBackdrop = _savedBackdrop;
                    _systemBackdropNulled = false;
                    StartupTrace.Mark("drag-snapshot-resize-restore-backdrop");
                    return true;
                }
                catch (Exception ex)
                {
                    LogDragBackdropFailure("resize-restore", ex);
                    return false;
                }
            }

            // 本来就没关 backdrop（例如两条关闭路径都失败）：撤覆盖层不会露壁纸。
            return true;
        }
        catch (Exception ex)
        {
            LogDragBackdropFailure("resize-restore", ex);
            return false;
        }
    }

    /// <summary>把拖拽状态字段复位；不碰任何 UI / Win32 状态。</summary>
    private static void ResetDragBackdropState()
    {
        _savedBackdrop = null;
        _savedBackdropType = DWMSBT_MAINWINDOW;
        _dwmBackdropDisabled = false;
        _systemBackdropNulled = false;
        _backdropSuspended = false;
        _dragSnapshotShown = false;
        _dragFillApplied = false;
    }

    /// <summary>CB_FORCE_DRAG_FILL 的周期应用：内容根/主题变化时重新铺色，DWM 只设一次。</summary>
    private static void ApplyForcedDragFill(Window window)
    {
        try
        {
            if (!DragBackdropFill.EnsureApplied(window))
            {
                return;
            }

            if (!DragFillOnlyEnabled && !_forcedDwmDisabled)
            {
                var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
                if (TrySetDwmBackdropType(hwnd, DWMSBT_NONE))
                {
                    _forcedDwmDisabled = true;
                }
                else
                {
                    StartupTrace.Mark(DragBackdropDwmSetFailedTrace);
                }
            }

            if (!_forcedDragFillApplied)
            {
                _forcedDragFillApplied = true;
                StartupTrace.Mark(DragBackdropForcedFillTrace);
            }
        }
        catch (Exception ex)
        {
            LogDragBackdropFailure("forced-fill", ex);
        }
    }

    private static void StopForcedDragFill()
    {
        if (_forcedDragFillTimer == null)
        {
            return;
        }

        try
        {
            _forcedDragFillTimer.Stop();
        }
        catch
        {
        }

        _forcedDragFillTimer = null;
    }

    /// <summary>
    /// CB_FORCE_DRAG_SNAPSHOT 的周期步骤：等 Shell 的覆盖层有布局尺寸 → 再稳定
    /// <see cref="ForcedSnapshotSettleMs"/> → 抓图（此时 Mica 还挂着）→ 铺回退色 →
    /// 保存并关闭 DWM backdrop → 固定状态并停表。失败/超时降级为强制填充。
    /// </summary>
    private static void ApplyForcedDragSnapshot(Window window)
    {
        if (!ForcedSnapshotModeEnabled || _forcedSnapshotActive)
        {
            StopForcedDragSnapshot();
            return;
        }

        try
        {
            _forcedSnapshotWaitedMs += ForcedSnapshotTickMs;

            if (!DragSnapshot.IsOverlayReady(window))
            {
                // Shell 还没构建完（占位层是空 Grid，没有覆盖层）。继续等，直到上限。
                if (_forcedSnapshotWaitedMs >= ForcedSnapshotMaxWaitMs)
                {
                    EnterForcedFillFallback(window);
                }

                return;
            }

            // Shell 已经有布局尺寸，但首帧/背景图解码可能还在进行；再等几帧，
            // 避免把“占位层刚被替换”的半成品抓成快照。
            if (_forcedSnapshotSettleMs < ForcedSnapshotSettleMs)
            {
                _forcedSnapshotSettleMs += ForcedSnapshotTickMs;
                return;
            }

            if (!DragSnapshot.TryCaptureAndShow(window))
            {
                _forcedSnapshotCaptureFailures++;
                if (_forcedSnapshotCaptureFailures == 1)
                {
                    // DragSnapshot 已经写过失败阶段；总等待超时后再降级。
                    StartupTrace.Mark($"drag-snapshot-force-capture-retry={_forcedSnapshotWaitedMs}");
                }

                if (_forcedSnapshotWaitedMs >= ForcedSnapshotMaxWaitMs)
                {
                    EnterForcedFillFallback(window);
                }

                return;
            }

            // 抓图成功，且抓的是“Mica 还挂着”的画面。下面按正式拖拽同样的顺序收尾：
            // 先铺回退色（覆盖层下面），再保存状态并关闭 DWM backdrop。
            var painted = false;
            try
            {
                painted = DragBackdropFill.TryApply(window);
            }
            catch (Exception ex)
            {
                LogDragBackdropFailure("forced-snapshot-fill", ex);
            }

            if (!painted)
            {
                StartupTrace.Mark(DragSnapshotFillUnavailableTrace);
            }
            else
            {
                StartupTrace.Mark($"{DragBackdropFillSourceTrace}={DragBackdropFill.LastBrushSource}");
            }

            SystemBackdrop? saved;
            try
            {
                saved = window.SystemBackdrop;
            }
            catch (Exception ex)
            {
                LogDragBackdropFailure("read", ex);
                DragSnapshot.HideNow();
                DragBackdropFill.ClearNow();
                StopForcedDragSnapshot();
                return;
            }

            if (saved == null)
            {
                // 没有 Mica（CB_NO_MICA=1 等）：这套快照对比没有意义，也不该改变外观。
                // 覆盖层和刚才铺上的回退色都撤掉，回到启动时的原样。
                DragSnapshot.HideNow();
                DragBackdropFill.ClearNow();
                StopForcedDragSnapshot();
                StartupTrace.Mark($"{DragBackdropErrorTrace}-forced-snapshot-no-backdrop");
                return;
            }

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
            if (!TryGetDwmBackdropType(hwnd, out var savedType))
            {
                savedType = DWMSBT_MAINWINDOW;
                StartupTrace.Mark(DragBackdropDwmReadFailedTrace);
            }

            _savedBackdrop = saved;
            _savedBackdropType = savedType;
            _dwmBackdropDisabled = false;
            _systemBackdropNulled = false;
            _backdropSuspended = true;

            if (TrySetDwmBackdropType(hwnd, DWMSBT_NONE))
            {
                _dwmBackdropDisabled = true;
            }
            else
            {
                StartupTrace.Mark(DragBackdropDwmSetFailedTrace);
                try
                {
                    window.SystemBackdrop = null;
                    _systemBackdropNulled = true;
                }
                catch (Exception ex)
                {
                    LogDragBackdropFailure("forced-snapshot-detach", ex);
                }
            }

            _forcedSnapshotActive = true;
            StartupTrace.Mark(DragSnapshotForcedTrace);
            StopForcedDragSnapshot();
        }
        catch (Exception ex)
        {
            LogDragBackdropFailure("forced-snapshot", ex);

            // 半成品状态必须收回：绝不留一个“悬起但没恢复”的 backdrop / 可见覆盖层。
            if (_backdropSuspended && !_forcedSnapshotActive)
            {
                var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
                if (_dwmBackdropDisabled)
                {
                    _ = TrySetDwmBackdropType(hwnd, _savedBackdropType);
                }
                else if (_systemBackdropNulled)
                {
                    try
                    {
                        window.SystemBackdrop = _savedBackdrop;
                    }
                    catch
                    {
                    }
                }

                ResetDragBackdropState();
            }

            DragSnapshot.HideNow();
            DragBackdropFill.ScheduleClear(window);
            StopForcedDragSnapshot();
        }
    }

    /// <summary>强制快照等不到可用覆盖层/抓图持续失败时，退回强制填充的视觉状态。</summary>
    private static void EnterForcedFillFallback(Window window)
    {
        StartupTrace.Mark(DragSnapshotForcedFillFallbackTrace);
        DragSnapshot.HideNow();
        StopForcedDragSnapshot();
        ApplyForcedDragFill(window);
    }

    private static void StopForcedDragSnapshot()
    {
        _forcedSnapshotSettleMs = 0;
        _forcedSnapshotWaitedMs = 0;

        if (_forcedSnapshotTimer == null)
        {
            return;
        }

        try
        {
            _forcedSnapshotTimer.Stop();
        }
        catch
        {
        }

        _forcedSnapshotTimer = null;
    }

    /// <summary>
    /// 强制快照状态结束（目前只在 WM_CLOSE 调用）：走与普通拖拽完全相同的恢复顺序 ——
    /// 先恢复 DWM backdrop / SystemBackdrop，再安排隐藏覆盖层。
    /// </summary>
    private static void ExitForcedSnapshot(IntPtr dwRefData)
    {
        if (!_forcedSnapshotActive)
        {
            return;
        }

        _forcedSnapshotActive = false;
        RestoreBackdropAfterDrag(dwRefData);
        StartupTrace.Mark(DragSnapshotForcedOffTrace);
    }

    /// <summary>读 DWMWA_SYSTEMBACKDROP_TYPE；hwnd 无效 / 系统不支持（Windows 10）时返回 false。</summary>
    private static bool TryGetDwmBackdropType(IntPtr hwnd, out int backdropType)
    {
        backdropType = DWMSBT_MAINWINDOW;
        if (hwnd == IntPtr.Zero)
        {
            return false;
        }

        try
        {
            return DwmGetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, out backdropType, sizeof(int)) >= 0;
        }
        catch
        {
            backdropType = DWMSBT_MAINWINDOW;
            return false;
        }
    }

    /// <summary>写 DWMWA_SYSTEMBACKDROP_TYPE；失败（例如 Windows 10）返回 false，由调用方降级。</summary>
    private static bool TrySetDwmBackdropType(IntPtr hwnd, int backdropType)
    {
        if (hwnd == IntPtr.Zero)
        {
            return false;
        }

        try
        {
            var value = backdropType;
            return DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, ref value, sizeof(int)) >= 0;
        }
        catch
        {
            return false;
        }
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

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("comctl32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowSubclass(IntPtr hWnd, SubclassProc pfnSubclass, IntPtr uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);

    // DWM 返回 HRESULT：>= 0 表示成功（SUCCEEDED），负数（如 E_INVALIDARG）表示失败。
    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(IntPtr hwnd, int dwAttribute, out int pvAttribute, int cbAttribute);

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int dwAttribute, ref int pvAttribute, int cbAttribute);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hWnd);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetClientRect(IntPtr hWnd, out Rect lpRect);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetForegroundWindow(IntPtr hWnd);
}
