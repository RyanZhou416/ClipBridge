using System.Diagnostics;
using System.Diagnostics.CodeAnalysis;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Contracts.ViewModels;
using ClipBridgeShell_CS.Helpers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace ClipBridgeShell_CS.Services;

// For more information on navigation between pages see
// https://github.com/microsoft/TemplateStudio/blob/main/docs/WinUI/navigation.md
public class NavigationService : INavigationService
{
    private readonly IPageService _pageService;
    private object? _lastParameterUsed;
    private Frame? _frame;

    public event NavigatedEventHandler? Navigated;

    public Frame? Frame
    {
        get
        {
            if (_frame == null)
            {
                _frame = App.MainWindow.Content as Frame;
                RegisterFrameEvents();
            }

            return _frame;
        }

        set
        {
            UnregisterFrameEvents();
            _frame = value;
            RegisterFrameEvents();
        }
    }

    [MemberNotNullWhen(true, nameof(Frame), nameof(_frame))]
    public bool CanGoBack => Frame != null && Frame.CanGoBack;

    public NavigationService(IPageService pageService)
    {
        _pageService = pageService;
    }

    private void RegisterFrameEvents()
    {
        if (_frame != null)
        {
            _frame.Navigated += OnNavigated;
        }
    }

    private void UnregisterFrameEvents()
    {
        if (_frame != null)
        {
            _frame.Navigated -= OnNavigated;
        }
    }

    public bool GoBack()
    {
        if (CanGoBack)
        {
            var vmBeforeNavigation = _frame.GetPageViewModel();
            _frame.GoBack();
            if (vmBeforeNavigation is INavigationAware navigationAware)
            {
                navigationAware.OnNavigatedFrom();
            }

            return true;
        }

        return false;
    }

    public bool NavigateTo(string pageKey, object? parameter = null, bool clearNavigation = false)
    {
        var pageType = _pageService.GetPageType(pageKey);

        if (_frame != null && (_frame.Content?.GetType() != pageType || (parameter != null && !parameter.Equals(_lastParameterUsed))))
        {
            _frame.Tag = clearNavigation;
            var vmBeforeNavigation = _frame.GetPageViewModel();
            var navigated = _frame.Navigate(pageType, parameter);
            if (navigated)
            {
                _lastParameterUsed = parameter;
                if (vmBeforeNavigation is INavigationAware navigationAware)
                {
                    navigationAware.OnNavigatedFrom();
                }
            }

            return navigated;
        }

        return false;
    }

    private void OnNavigated(object sender, NavigationEventArgs e)
    {
        if (sender is Frame frame)
        {
            var clearNavigation = (bool)frame.Tag;
            if (clearNavigation)
            {
                frame.BackStack.Clear();
            }

            if (frame.GetPageViewModel() is INavigationAware navigationAware)
            {
                navigationAware.OnNavigatedTo(e.Parameter);
            }

            Navigated?.Invoke(sender, e);
        }
    }

    public void RefreshCurrentPage()
    {
        Debug.WriteLine($"[{DateTime.Now:HH:mm:ss.fff}] [NAV] RefreshCurrentPage begin");

        if (Frame is null)
        {
            Debug.WriteLine($"[{DateTime.Now:HH:mm:ss.fff}] [NAV] RefreshCurrentPage skipped (no frame)");
            return;
        }

        // 记录当前页类型 & 参数
        var currentElement = Frame.Content as FrameworkElement;
        var pageType = currentElement?.GetType() ?? Frame.CurrentSourcePageType;
        var parameter = (Frame as Frame)?.GetNavigationState(); // 如果你有参数，可以自行保存你自己的参数来源

        // 导航到相同的页类型，触发完整的构建和资源解析
        var navigated = Frame.Navigate(pageType, null);
        if (navigated)
        {
            // 清空回退栈（与模板一致）
            Frame.BackStack.Clear();
            Debug.WriteLine($"[{DateTime.Now:HH:mm:ss.fff}] [NAV] RefreshCurrentPage done → {pageType.Name}");
        }
        else
        {
            Debug.WriteLine($"[{DateTime.Now:HH:mm:ss.fff}] [NAV] RefreshCurrentPage failed → {pageType?.Name}");
        }
    }

}
