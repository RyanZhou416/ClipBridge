// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/DragSnapshot.cs
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 拖拽期“冻结画面”快照：用 GDI 从屏幕 DC 上 BitBlt 抓窗口客户区，存成一张 BGRA 位图，
/// 铺到 ShellPage 根 Grid 最上层那个 <c>DragSnapshotOverlay</c> Image 上。
///
/// 为什么抓屏幕而不是窗口内容：
/// - <c>PrintWindow</c> 只让应用自己的视觉树重绘一遍，系统绘制的 Mica 背景不会出现
///   （Mica 属于 DWM 的窗口背景材料，不在应用的视觉树里），抓出来是透明/黑色。
/// - <c>Windows.Graphics.Capture</c> 首帧有约 100ms 延迟，鼠标按下那一刻来不及。
/// - 桌面 DC 的 <c>BitBlt</c> 拿到的就是用户当前看到的合成结果，既包含 Mica，也没有首帧延迟。
///
/// 抓哪块矩形：<c>GetClientRect</c> + <c>ClientToScreen</c>。覆盖层是 ShellPage（内容根）里的
/// XAML 子元素，它的布局盒正好等于窗口客户区；窗口开了 ExtendsContentIntoTitleBar，客户区就是
/// 用户看到的整块内容（标题栏图标/文字也在 XAML 客户区里）。不用
/// <c>DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)</c>：那个矩形包含不可见的缩放边框
/// （每边数像素），比覆盖层的布局盒大一圈，贴到 Image 上会被 Stretch 拉伸并产生偏移。
///
/// 一次拖拽只抓一次：用户在 WM_ENTERSIZEMOVE 时还没开始移动，此刻抓到的就是静止画面。
/// 位图（<see cref="WriteableBitmap"/>）按尺寸缓存，再次进入拖拽且尺寸没变时复用，不重新分配；
/// 像素缓冲（byte[]）同样复用。只有冷启动后第一次拖拽会分配。
///
/// 限制（已知且接受）：
/// - SDR 精度：屏幕 DC 给出的是 8bit BGRA；HDR 显示器上 DWM 的色调映射结果未必等于实际 HDR
///   输出，但拖拽期间只要求“和用户看到的 SDR 外观一致”。
/// - 多显示器/混合 DPI：抓的是物理像素矩形，Image 按 DIP 布局等比铺满客户区，1:1 像素映射；
///   当窗口跨越不同 DPI 的显示器时，系统会对窗口做 DPI 缩放，快照内容会在拖拽开始那一刻定格
///   （这正是本功能的目的）。
/// - 被其它窗口遮挡的部分会被一起抓进来：拖拽中的窗口在按下标题栏时已经被提到最前，
///   只有窗口部分移出桌面时才可能在边缘抓到桌面/黑边。
///
/// 所有入口都不抛出；失败时返回 false，由 <see cref="WindowExtensions"/> 退回到原有的
/// “回退色填充 + 关 DWM 背景”路径，绝不会把空/黑覆盖层留在屏幕上。
/// </summary>
internal static class DragSnapshot
{
    /// <summary>ShellPage.xaml 里覆盖层的 x:Name；用 FindName 取，避免 ShellPage 反向依赖本类。</summary>
    private const string OverlayName = "DragSnapshotOverlay";

    /// <summary>成功抓取 + 建位图的总耗时（毫秒），日志标记名。</summary>
    private const string CaptureTrace = "drag-snapshot-capture";

    /// <summary>抓取/建位图失败的标记前缀，后接失败阶段。</summary>
    private const string CaptureFailedTrace = "drag-snapshot-capture-failed";

    /// <summary>覆盖层真正显示出来（参数为位图尺寸）。</summary>
    private const string OverlayShownTrace = "drag-snapshot-overlay-shown";

    /// <summary>覆盖层被隐藏。与 shown 成对出现，冒烟测试据此判断 plumbing。</summary>
    private const string OverlayHiddenTrace = "drag-snapshot-overlay-hidden";

    private const uint SRCCOPY = 0x00CC0020;
    private const uint DIB_RGB_COLORS = 0;
    private const uint BI_RGB = 0;

    /// <summary>当前覆盖层元素；内容根被替换（占位层 → Shell）后重新 FindName 得到新元素。</summary>
    private static Image? _overlay;

    /// <summary>按尺寸缓存的写位图；同尺寸再次拖拽时复用。</summary>
    private static WriteableBitmap? _bitmap;

    private static int _bitmapWidth;
    private static int _bitmapHeight;

    /// <summary>按尺寸缓存的可复用像素缓冲，避免每次拖拽都分配 ~W*H*4 的 LOH 数组。</summary>
    private static byte[]? _pixels;

    /// <summary>覆盖层当前是否可见。</summary>
    public static bool IsShowing { get; private set; }

    /// <summary>最近一次成功抓取的客户区宽/高（物理像素）；0 表示尚未抓取。</summary>
    public static int CapturedWidth { get; private set; }

    public static int CapturedHeight { get; private set; }

    /// <summary>
    /// 显示代数。每次显示/隐藏都自增；<see cref="ScheduleHide"/> 的延迟回调只对“自己那一代”
    /// 生效，避免“刚松手、马上又按下”时旧的 Low 回调把新一代的快照藏掉。
    /// </summary>
    private static long _generation;

    /// <summary>
    /// 覆盖层是否已经可测地挂到已布局的 ShellPage 上（用于 CB_FORCE_DRAG_SNAPSHOT 等首帧之后
    /// 才能抓图的场景）。占位层（空 Grid）没有这个命名元素，返回 false。
    /// </summary>
    public static bool IsOverlayReady(Window window)
    {
        try
        {
            var root = window?.Content as FrameworkElement;
            if (root == null || root.XamlRoot == null || root.ActualWidth <= 0 || root.ActualHeight <= 0)
            {
                return false;
            }

            // 覆盖层平时是 Collapsed，不参与布局（ActualWidth 恒为 0），所以这里只能确认
            // “元素存在且根已经有布局尺寸”，不能拿覆盖层自己的尺寸当就绪条件。
            return root.FindName(OverlayName) is Image;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>
    /// 抓取窗口客户区的屏幕像素并显示到覆盖层。成功返回 true；任何一步失败都清理 GDI 资源、
    /// 记录失败阶段并返回 false（此时覆盖层保持原状，调用方走回退路径）。
    /// 必须在 UI 线程调用（会触碰 XAML 元素）；整个方法内只有一次 BitBlt + 一次缓冲拷贝。
    /// </summary>
    public static bool TryCaptureAndShow(Window window)
    {
        if (window == null)
        {
            return false;
        }

        var started = Stopwatch.GetTimestamp();
        IntPtr screenDc = IntPtr.Zero;
        IntPtr memDc = IntPtr.Zero;
        IntPtr dib = IntPtr.Zero;
        IntPtr previousObject = IntPtr.Zero;

        try
        {
            var root = window.Content as FrameworkElement;
            if (root == null)
            {
                MarkFailure("no-root");
                return false;
            }

            var overlay = root.FindName(OverlayName) as Image;
            if (overlay == null)
            {
                // ShellPage 尚未构建（启动最初的占位层），退回填充路径。
                MarkFailure("no-overlay");
                return false;
            }

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
            if (hwnd == IntPtr.Zero)
            {
                MarkFailure("no-hwnd");
                return false;
            }

            if (!GetClientRect(hwnd, out var client)
                || client.Right <= 0
                || client.Bottom <= 0)
            {
                MarkFailure("client-rect");
                return false;
            }

            var width = client.Right;
            var height = client.Bottom;

            // 客户区原点映射到屏幕坐标。物理像素：WinUI 3 进程是 Per-Monitor V2 DPI 感知。
            var origin = new POINT { X = 0, Y = 0 };
            if (!ClientToScreen(hwnd, ref origin))
            {
                MarkFailure("client-to-screen");
                return false;
            }

            screenDc = GetDC(IntPtr.Zero);
            if (screenDc == IntPtr.Zero)
            {
                MarkFailure("getdc");
                return false;
            }

            memDc = CreateCompatibleDC(screenDc);
            if (memDc == IntPtr.Zero)
            {
                MarkFailure("create-dc");
                return false;
            }

            // 负高度 = top-down DIB：内存里的行序与 WriteableBitmap 的 top-down BGRA 一致，
            // 省掉一次整图翻转（大窗口下那是显存/内存上的几十毫秒）。
            var info = new BITMAPINFO
            {
                bmiHeader = new BITMAPINFOHEADER
                {
                    biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>(),
                    biWidth = width,
                    biHeight = -height,
                    biPlanes = 1,
                    biBitCount = 32,
                    biCompression = BI_RGB,
                    biSizeImage = 0,
                }
            };

            dib = CreateDIBSection(memDc, ref info, DIB_RGB_COLORS, out var bits, IntPtr.Zero, 0);
            if (dib == IntPtr.Zero || bits == IntPtr.Zero)
            {
                MarkFailure("dib-section");
                return false;
            }

            previousObject = SelectObject(memDc, dib);
            if (previousObject == IntPtr.Zero || previousObject == new IntPtr(-1))
            {
                MarkFailure("select-object");
                return false;
            }

            // 唯一一次 BitBlt：把屏幕上的客户区（含 Mica）拷进 32bpp DIB。
            if (!BitBlt(memDc, 0, 0, width, height, screenDc, origin.X, origin.Y, SRCCOPY))
            {
                MarkFailure("bitblt");
                return false;
            }

            var byteCount = checked(width * height * 4);
            if (_pixels == null || _pixels.Length != byteCount)
            {
                _pixels = new byte[byteCount];
                StartupTrace.Mark(FormattableString.Invariant($"drag-snapshot-pixels-alloc={byteCount}"));
            }

            // 一次托管拷贝；BitBlt 不定义 alpha 通道（通常为 0），而 XAML Image 按预乘 alpha
            // 解释 BGRA，不强制不透明会得到一张全透明（看起来像空窗口）的图。
            Marshal.Copy(bits, _pixels, 0, byteCount);
            ForceOpaqueAlpha(_pixels);

            if (_bitmap == null || _bitmapWidth != width || _bitmapHeight != height)
            {
                _bitmap = new WriteableBitmap(width, height);
                _bitmapWidth = width;
                _bitmapHeight = height;
                StartupTrace.Mark(FormattableString.Invariant($"drag-snapshot-bitmap-alloc={width}x{height}"));
            }

            using (var stream = _bitmap.PixelBuffer.AsStream())
            {
                stream.Position = 0;
                stream.Write(_pixels, 0, byteCount);
            }

            // 顺序：先 Source 再 Visible，避免出现一帧“旧图/空图”。
            overlay.Source = _bitmap;
            overlay.Visibility = Visibility.Visible;

            _overlay = overlay;
            IsShowing = true;
            CapturedWidth = width;
            CapturedHeight = height;
            _generation++;

            var elapsedMs = (Stopwatch.GetTimestamp() - started) * 1000.0 / Stopwatch.Frequency;
            StartupTrace.Mark(FormattableString.Invariant($"{CaptureTrace}={elapsedMs:F1}"));
            StartupTrace.Mark(FormattableString.Invariant($"{OverlayShownTrace}={width}x{height}"));
            return true;
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[DragSnapshot] capture failed: {ex}");
            StartupTrace.Mark($"{CaptureFailedTrace}={ex.GetType().Name}");
            return false;
        }
        finally
        {
            // 顺序与创建相反；任何一步失败都不影响已经拷贝到托管数组的像素。
            try
            {
                if (memDc != IntPtr.Zero && previousObject != IntPtr.Zero && previousObject != new IntPtr(-1))
                {
                    _ = SelectObject(memDc, previousObject);
                }

                if (dib != IntPtr.Zero)
                {
                    _ = DeleteObject(dib);
                }

                if (memDc != IntPtr.Zero)
                {
                    _ = DeleteDC(memDc);
                }

                if (screenDc != IntPtr.Zero)
                {
                    _ = ReleaseDC(IntPtr.Zero, screenDc);
                }
            }
            catch
            {
                // GDI 清理失败只泄漏句柄，不能让窗口过程收到异常。
            }
        }
    }

    /// <summary>
    /// 立即隐藏覆盖层（同步）。用于尺寸变化后放弃快照、WM_CLOSE、以及延迟隐藏被新一代取消时。
    /// 位图本身保留在缓存里，下一次拖拽可复用。
    /// </summary>
    public static void HideNow()
    {
        var overlay = _overlay;
        _overlay = null;
        IsShowing = false;
        CapturedWidth = 0;
        CapturedHeight = 0;
        _generation++;

        if (overlay == null)
        {
            return;
        }

        try
        {
            overlay.Visibility = Visibility.Collapsed;
            StartupTrace.Mark(OverlayHiddenTrace);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[DragSnapshot] hide failed: {ex}");
        }
    }

    /// <summary>
    /// 在下一轮 dispatcher（Low 优先级）隐藏覆盖层。调用点已经先把 DWM backdrop 恢复，
    /// 延迟一帧是为了让 DWM 先应用恢复后的 Mica，再撤掉覆盖层，避免任何可见的跳变。
    ///
    /// 与 <see cref="HideNow"/> 一样，位图缓存保留。如果这期间开始了新一次拖拽
    /// （<see cref="TryCaptureAndShow"/> 会更新覆盖层），延迟回调自动失效。
    /// </summary>
    public static void ScheduleHide(Window? window)
    {
        if (!IsShowing)
        {
            return;
        }

        var generation = _generation;
        var queue = window?.DispatcherQueue;
        if (queue == null)
        {
            HideNow();
            return;
        }

        bool enqueued;
        try
        {
            enqueued = queue.TryEnqueue(
                Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
                () =>
                {
                    // 期间可能已经又开始了一次拖拽（TryCaptureAndShow 自增 _generation）：
                    // 旧的隐藏请求绝不能把新一代的快照藏掉。
                    if (_generation == generation && IsShowing)
                    {
                        HideNow();
                    }
                });
        }
        catch
        {
            enqueued = false;
        }

        if (!enqueued)
        {
            HideNow();
        }
    }

    /// <summary>窗口销毁时丢弃所有托管状态（不触碰已销毁的 XAML 元素）。</summary>
    public static void Reset()
    {
        _overlay = null;
        _generation++;
        _bitmap = null;
        _bitmapWidth = 0;
        _bitmapHeight = 0;
        _pixels = null;
        IsShowing = false;
        CapturedWidth = 0;
        CapturedHeight = 0;
    }

    /// <summary>屏幕 BitBlt 出来的 alpha 通道未定义（通常 0）；统一改成 255（不透明）。</summary>
    private static void ForceOpaqueAlpha(byte[] pixels)
    {
        for (var i = 3; i < pixels.Length; i += 4)
        {
            pixels[i] = 0xFF;
        }
    }

    private static void MarkFailure(string stage)
    {
        StartupTrace.Mark($"{CaptureFailedTrace}={stage}");
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFOHEADER
    {
        public uint biSize;
        public int biWidth;
        public int biHeight;
        public ushort biPlanes;
        public ushort biBitCount;
        public uint biCompression;
        public uint biSizeImage;
        public int biXPelsPerMeter;
        public int biYPelsPerMeter;
        public uint biClrUsed;
        public uint biClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFO
    {
        public BITMAPINFOHEADER bmiHeader;

        /// <summary>BI_RGB 32bpp 不需要调色板；保留一个 DWORD 占位，结构体大小与 Win32 一致。</summary>
        public uint bmiColors;
    }

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ClientToScreen(IntPtr hWnd, ref POINT lpPoint);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr GetDC(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern int ReleaseDC(IntPtr hWnd, IntPtr hDC);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern IntPtr CreateCompatibleDC(IntPtr hdc);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern bool DeleteDC(IntPtr hdc);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern IntPtr CreateDIBSection(
        IntPtr hdc,
        ref BITMAPINFO pbmi,
        uint usage,
        out IntPtr ppvBits,
        IntPtr hSection,
        uint offset);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern IntPtr SelectObject(IntPtr hdc, IntPtr hgdiobj);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern bool DeleteObject(IntPtr hObject);

    [DllImport("gdi32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool BitBlt(
        IntPtr hdcDest,
        int xDest,
        int yDest,
        int width,
        int height,
        IntPtr hdcSrc,
        int xSrc,
        int ySrc,
        uint rasterOp);
}
