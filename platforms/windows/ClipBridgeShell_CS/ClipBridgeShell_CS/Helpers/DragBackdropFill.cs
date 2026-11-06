// platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Helpers/DragBackdropFill.cs
using System;

using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

using Windows.UI;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>
/// 拖拽/缩放期间给窗口内容根铺一层不透明的 Mica 回退色。
///
/// 为什么需要它：拖拽开始后 <see cref="WindowExtensions"/> 会把 DWM 的
/// DWMWA_SYSTEMBACKDROP_TYPE 切到 DWMSBT_NONE（见 SetDragBackdropToggle 的注释）。Mica 消失后，
/// XAML 页面（ShellPage / 占位层）本来就是透明的，窗口会直接透出桌面壁纸 —— 实测同一区域
/// 从 #1C1C1E 变成 #658DA6。Microsoft 文档给出的 Mica 回退外观是
/// SolidBackgroundFillColorBase（Mica Alt 是 SolidBackgroundFillColorBaseAlt），因此在摘掉
/// backdrop 的这段时间里，把内容根刷成该回退色，视觉上就等价于“Mica 的回退态”，
/// 而拖拽结束后再把这层填充撤掉（见 ScheduleClear 的延迟一帧说明）。
///
/// 落在哪个元素上（本应用实际走的路径）：
/// - 正常启动后 <c>MainWindow.Content</c> 是 ShellPage（Page）。WinUI 3 的继承链是
///   Page : UserControl : Control，Background 来自 Control —— 所以 Page 本身就有
///   Background 属性，直接保存/设置/恢复 Page.Background 即可。这是实际路径（direct-page）。
/// - 启动最初的占位层是 Grid（Panel），同样直接设置 Background（direct-panel）。
/// - 若内容根既不是 Panel/Control/Border、下探也找不到带 Background 的元素，才退回兜底：
///   用一个临时 Border 包住原内容（见 TryWrapWithBorder）。当前 Shell 不会走到这里。
///
/// 原 Background（通常是 null）原样保存、退出时恢复；主题按元素的 ActualTheme 选择。
/// 整个类只在 UI 线程消息回调里使用，静态字段因此无需加锁。
/// </summary>
internal static class DragBackdropFill
{
    /// <summary>Microsoft 文档中 Mica 的回退画刷资源键（WinUI 主题资源）。</summary>
    private const string FallbackBrushKey = "SolidBackgroundFillColorBaseBrush";

    /// <summary>回退画刷不可用时，Mica 回退色对应的颜色资源键。</summary>
    private const string FallbackColorKey = "SolidBackgroundFillColorBase";

    /// <summary>资源字典 / 内容树的最大下探层数，防止异常的内容嵌套造成死循环。</summary>
    private const int MaxDescendDepth = 8;

    /// <summary>
    /// WinUI 主题字典的键：深色主题通常同时存在 "Dark" 与作为兜底的 "Default"，
    /// 浅色主题用 "Light"。按顺序尝试，避免只认一种键时取不到资源。
    /// </summary>
    private static readonly string[] DarkThemeKeys = { "Dark", "Default" };
    private static readonly string[] LightThemeKeys = { "Light" };

    /// <summary>文档里 Mica 回退色在深色主题下的近似值（资源都取不到时使用）。</summary>
    private static readonly Color DarkFallbackColor = Color.FromArgb(255, 0x20, 0x20, 0x20);

    /// <summary>文档里 Mica 回退色在浅色主题下的近似值（资源都取不到时使用）。</summary>
    private static readonly Color LightFallbackColor = Color.FromArgb(255, 0xF3, 0xF3, 0xF3);

    /// <summary>当前被填充的目标元素；null 表示没有填充。</summary>
    private static FrameworkElement? _target;

    /// <summary>目标元素在填充前的 Background（通常是 null），恢复时原样写回。</summary>
    private static Brush? _originalBackground;

    /// <summary>兜底 Border 路径里原本的窗口内容；非 null 表示当前是“包 Border”状态。</summary>
    private static FrameworkElement? _wrappedRoot;

    /// <summary>当前持有窗口引用，用于延迟清填充时取 DispatcherQueue（窗口可能已隐藏）。</summary>
    private static Window? _window;

    /// <summary>是否已经铺过填充。</summary>
    private static bool _applied;

    /// <summary>填充时的 ActualTheme，用于主题变化后重新取色。</summary>
    private static ElementTheme _appliedTheme = ElementTheme.Default;

    /// <summary>
    /// 每次应用/清除都自增；退出时调度的 Low 优先级清除只对“自己那一代”生效，
    /// 避免“退出后立刻又进入拖拽”时，旧的延迟回调把新一代填充误清掉。
    /// </summary>
    private static long _generation;

    /// <summary>最近一次解析回退画刷的来源（theme-brush / app-resource-brush / theme-color / …），仅诊断用。</summary>
    public static string LastBrushSource { get; private set; } = "none";

    /// <summary>
    /// 给窗口内容根铺上当前主题的 Mica 回退色。成功返回 true；找不到可绘制元素 /
    /// 资源解析或属性写入抛异常时返回 false（由调用方决定降级，本方法不抛）。
    /// </summary>
    public static bool TryApply(Window window)
    {
        if (window == null)
        {
            return false;
        }

        FrameworkElement? root;
        try
        {
            root = window.Content as FrameworkElement;
        }
        catch
        {
            return false;
        }

        if (root == null)
        {
            return false;
        }

        var target = FindPaintableElement(root, 0);
        var themeElement = target ?? root;
        var theme = themeElement.ActualTheme;
        var brush = ResolveFallbackBrush(themeElement);

        if (target == null)
        {
            // 兜底路径：内容根没有任何带 Background 的元素（当前 Shell 不会走到）。
            return TryWrapWithBorder(window, root, brush, theme);
        }

        try
        {
            if (_applied && ReferenceEquals(_target, target))
            {
                // 同一目标再次进入（例如强制填充的周期检查 / 退出清除前又进入）：
                // 不覆盖保存的原值，只让旧的延迟清除失效；主题变了就换一张画刷。
                _generation++;
                _window = window;
                if (!Equals(_appliedTheme, theme))
                {
                    SetBackground(target, brush);
                    _appliedTheme = theme;
                }

                return true;
            }

            if (_applied)
            {
                // 内容根被替换（占位层 → Shell）：先把旧目标恢复干净，再铺到新目标上。
                ClearNow();
            }

            var original = GetBackground(target);
            SetBackground(target, brush);

            _window = window;
            _target = target;
            _originalBackground = original;
            _wrappedRoot = null;
            _appliedTheme = theme;
            _applied = true;
            _generation++;
            return true;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>
    /// 强制填充模式（CB_FORCE_DRAG_FILL=1）的周期检查：目标存在且主题没变时什么都不做，
    /// 否则复走一次 <see cref="TryApply"/>。返回当前是否已铺好填充。
    /// </summary>
    public static bool EnsureApplied(Window window)
    {
        if (_applied && _target != null)
        {
            try
            {
                if (ReferenceEquals(_target, FindPaintableElement(window.Content as FrameworkElement, 0)) &&
                    Equals(_appliedTheme, _target.ActualTheme))
                {
                    return true;
                }
            }
            catch
            {
                // 落到下面的 TryApply 重试。
            }
        }

        return TryApply(window);
    }

    /// <summary>
    /// 在下一轮 dispatcher（Low 优先级）清除填充。
    ///
    /// 不能同步清：刚把 DWMWA_SYSTEMBACKDROP_TYPE 设回 Mica 后，DWM 是异步应用该属性的，
    /// 同一帧里如果填充已经消失，就会闪一帧桌面壁纸。留到 Low 优先级回调里，
    /// 让 DWM 先完成恢复，再撤掉这层回退色。
    /// </summary>
    public static void ScheduleClear(Window? window)
    {
        if (!_applied)
        {
            return;
        }

        var generation = _generation;
        var queue = (window ?? _window)?.DispatcherQueue;
        if (queue == null)
        {
            // 拿不到 dispatcher（窗口已销毁 / 测试环境）：同步清掉，避免永久残留。
            ClearNow();
            return;
        }

        bool enqueued;
        try
        {
            enqueued = queue.TryEnqueue(DispatcherQueuePriority.Low, () => ClearIfGeneration(generation));
        }
        catch
        {
            enqueued = false;
        }

        if (!enqueued)
        {
            ClearNow();
        }
    }

    /// <summary>立即恢复目标原来的 Background（可能用兜底 Border 包过则拆回原内容）。</summary>
    public static void ClearNow()
    {
        var target = _target;
        var original = _originalBackground;
        var wrappedRoot = _wrappedRoot;
        var window = _window;

        _target = null;
        _originalBackground = null;
        _wrappedRoot = null;
        _window = null;
        _appliedTheme = ElementTheme.Default;
        _applied = false;
        _generation++;

        if (target == null)
        {
            return;
        }

        try
        {
            if (wrappedRoot != null && target is Border wrapper)
            {
                if (ReferenceEquals(wrapper.Child, wrappedRoot))
                {
                    wrapper.Child = null;
                }

                if (window != null && ReferenceEquals(window.Content, wrapper))
                {
                    window.Content = wrappedRoot;
                }

                wrapper.Background = null;
            }
            else
            {
                SetBackground(target, original);
            }
        }
        catch
        {
            // 恢复填充绝不能把异常抛回窗口过程；最坏情况是保留一帧回退色。
        }
    }

    /// <summary>窗口销毁时丢弃托管状态（此时没有可恢复的 UI 元素）。</summary>
    public static void Reset()
    {
        _target = null;
        _originalBackground = null;
        _wrappedRoot = null;
        _window = null;
        _appliedTheme = ElementTheme.Default;
        _applied = false;
        _generation++;
    }

    private static void ClearIfGeneration(long generation)
    {
        if (!_applied || _generation != generation)
        {
            // 已经清过，或者期间又进入了一次拖拽（新一代填充不能清）。
            return;
        }

        ClearNow();
    }

    /// <summary>
    /// 判断窗口内容根本身能不能设置 Background：
    /// Panel / Control / Border 都有 Background；WinUI 3 里 Page、UserControl 都继承 Control，
    /// 所以 ShellPage 会直接命中这里的 Control 分支（实际路径）。
    /// ContentPresenter 没有 Background，只有它需要沿 Content 下探。
    /// </summary>
    private static FrameworkElement? FindPaintableElement(FrameworkElement? element, int depth)
    {
        if (element == null || depth > MaxDescendDepth)
        {
            return null;
        }

        switch (element)
        {
            case Panel:
            case Control:
            case Border:
                return element;

            case ContentPresenter presenter:
                return FindPaintableElement(presenter.Content as FrameworkElement, depth + 1);

            default:
                return null;
        }
    }

    private static Brush? GetBackground(FrameworkElement element) => element switch
    {
        Panel panel => panel.Background,
        Control control => control.Background,
        Border border => border.Background,
        _ => null
    };

    private static void SetBackground(FrameworkElement element, Brush? brush)
    {
        switch (element)
        {
            case Panel panel:
                panel.Background = brush;
                break;

            case Control control:
                control.Background = brush;
                break;

            case Border border:
                border.Background = brush;
                break;

            default:
                throw new InvalidOperationException($"Element {element.GetType().Name} has no Background property.");
        }
    }

    /// <summary>
    /// 兜底路径：内容根没有任何带 Background 的元素时，用临时 Border 把原内容包起来
    /// （Border 在视觉树中位于原内容“后面”）。当前 ShellPage 的 Page 继承 Control、本身就有
    /// Background，所以正常路径不会进这里；保留它只是为了“内容根换成自定义 FrameworkElement”
    /// 时不至于失效。
    /// </summary>
    private static bool TryWrapWithBorder(Window window, FrameworkElement root, Brush brush, ElementTheme theme)
    {
        Border? border = null;
        try
        {
            border = new Border { Background = brush };

            // Window.Content 只能有一个子元素：先摘下来，再放进 Border，最后把 Border 挂回去。
            window.Content = null;
            border.Child = root;
            window.Content = border;

            _window = window;
            _target = border;
            _originalBackground = null;
            _wrappedRoot = root;
            _appliedTheme = theme;
            _applied = true;
            _generation++;
            return true;
        }
        catch
        {
            // 尽力把原内容挂回去，失败则放弃填充（调用方会降级并记录 paint-failed）。
            try
            {
                if (border != null && ReferenceEquals(border.Child, root))
                {
                    border.Child = null;
                }

                if (ReferenceEquals(window.Content, null))
                {
                    window.Content = root;
                }
                else if (border != null && ReferenceEquals(window.Content, border))
                {
                    window.Content = root;
                }
            }
            catch
            {
            }

            return false;
        }
    }

    /// <summary>
    /// 按元素当前主题解析 Mica 回退画刷，顺序：
    /// ① 主题字典里按 ActualTheme 取 SolidBackgroundFillColorBaseBrush（最贴合当前主题）；
    /// ② Application.Resources["SolidBackgroundFillColorBaseBrush"]（任务要求的首选入口）；
    /// ③ 主题字典 / Application.Resources 里的 SolidBackgroundFillColorBase 颜色；
    /// ④ 硬编码 #202020（Dark）/ #F3F3F3（Light）。
    /// 任何一步取不到或抛异常都继续下一步，绝不因资源缺失而中断拖拽；
    /// 实际来源写进 <see cref="LastBrushSource"/>，供 CB_STARTUP_TRACE 下的诊断行使用。
    /// </summary>
    private static Brush ResolveFallbackBrush(FrameworkElement themeElement)
    {
        var isDark = themeElement.ActualTheme == ElementTheme.Dark;
        var themeKeys = isDark ? DarkThemeKeys : LightThemeKeys;
        var appResources = Application.Current?.Resources;

        try
        {
            if (FindThemeResource(appResources, themeKeys, FallbackBrushKey, 0) is Brush themedBrush)
            {
                LastBrushSource = "theme-brush";
                return themedBrush;
            }
        }
        catch
        {
        }

        try
        {
            if (appResources != null &&
                appResources.TryGetValue(FallbackBrushKey, out var brushValue) &&
                brushValue is Brush appBrush)
            {
                LastBrushSource = "app-resource-brush";
                return appBrush;
            }
        }
        catch
        {
        }

        try
        {
            if (FindThemeResource(appResources, themeKeys, FallbackColorKey, 0) is Color themeColor)
            {
                LastBrushSource = "theme-color";
                return new SolidColorBrush(themeColor);
            }
        }
        catch
        {
        }

        try
        {
            if (appResources != null &&
                appResources.TryGetValue(FallbackColorKey, out var colorValue) &&
                colorValue is Color appColor)
            {
                LastBrushSource = "app-resource-color";
                return new SolidColorBrush(appColor);
            }
        }
        catch
        {
        }

        LastBrushSource = isDark ? "hardcoded-dark" : "hardcoded-light";
        return new SolidColorBrush(isDark ? DarkFallbackColor : LightFallbackColor);
    }

    /// <summary>
    /// 在 (含合并的) 资源字典里按 ThemeDictionaries 的键找资源。
    /// Application.Resources 的索引器不感知元素级 RequestedTheme，而本应用允许
    /// ThemeSelectorService 给内容根单独设主题，所以先按 ActualTheme 精确取一次。
    /// </summary>
    private static object? FindThemeResource(ResourceDictionary? dictionary, string[] themeKeys, string key, int depth)
    {
        if (dictionary == null || depth > MaxDescendDepth)
        {
            return null;
        }

        try
        {
            foreach (var pair in dictionary.ThemeDictionaries)
            {
                if (pair.Key is not string candidate || pair.Value is not ResourceDictionary themeDictionary)
                {
                    continue;
                }

                foreach (var themeKey in themeKeys)
                {
                    if (string.Equals(candidate, themeKey, StringComparison.OrdinalIgnoreCase) &&
                        themeDictionary.TryGetValue(key, out var value))
                    {
                        return value;
                    }
                }
            }
        }
        catch
        {
        }

        foreach (var merged in dictionary.MergedDictionaries)
        {
            var found = FindThemeResource(merged, themeKeys, key, depth + 1);
            if (found != null)
            {
                return found;
            }
        }

        return null;
    }
}
