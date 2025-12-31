using System;
using System.Diagnostics;
using System.Threading.Tasks;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Views;

using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Navigation;

namespace ClipBridgeShell_CS.ViewModels;

public partial class ShellViewModel : ObservableRecipient
{
    // --- 原本就有的：导航状态 ---
    [ObservableProperty]
    private bool isBackEnabled;

    [ObservableProperty]
    private object? selected;

    public INavigationService NavigationService
    {
        get;
    }
    public INavigationViewService NavigationViewService
    {
        get;
    }

    // --- 新增：Core 降级呈现（给 ShellPage 的 InfoBar x:Bind 用） ---
    private readonly ICoreHostService _coreHost;
    private readonly IClipboardService _clipboard;

    [ObservableProperty]
    private bool isCoreDegraded;

    [ObservableProperty]
    private string? coreDegradedMessage;

    public IAsyncRelayCommand RetryCoreInitCommand
    {
        get;
    }
    public IRelayCommand CopyDiagnosticsCommand
    {
        get;
    }
    public IRelayCommand OpenLogsCommand
    {
        get;
    }

    public ShellViewModel(INavigationService navigationService, INavigationViewService navigationViewService)
    {
        NavigationService = navigationService;
        NavigationService.Navigated += OnNavigated;
        NavigationViewService = navigationViewService;

        // 关键点：不改你原来的构造函数签名（避免破坏 TemplateStudio/DI），
        // 但我们从 App Host 里拿到 CoreHost / Clipboard
        _coreHost = App.GetService<ICoreHostService>();
        _clipboard = App.GetService<IClipboardService>();

        RetryCoreInitCommand = new AsyncRelayCommand(RetryCoreInitAsync, CanRetryCoreInit);
        CopyDiagnosticsCommand = new RelayCommand(CopyDiagnostics);
        OpenLogsCommand = new RelayCommand(OpenLogsFolder);

        // 初始化一次 UI 状态 + 订阅状态变化
        UpdateDegradedBanner(_coreHost.State);
        _coreHost.StateChanged += OnCoreStateChanged;
    }

    private void OnNavigated(object sender, NavigationEventArgs e)
    {
        IsBackEnabled = NavigationService.CanGoBack;

        if (e.SourcePageType == typeof(SettingsPage))
        {
            Selected = NavigationViewService.SettingsItem;
            return;
        }

        var selectedItem = NavigationViewService.GetSelectedItem(e.SourcePageType);
        if (selectedItem != null)
        {
            Selected = selectedItem;
        }
    }

    private void OnCoreStateChanged(CoreState state)
    {
        // CoreHost 可能在后台线程触发事件；这里确保回到 UI 线程更新绑定属性
        DispatcherQueue? dq = App.MainWindow?.DispatcherQueue;
        if (dq is not null && !dq.HasThreadAccess)
        {
            dq.TryEnqueue(() =>
            {
                UpdateDegradedBanner(state);
                RetryCoreInitCommand.NotifyCanExecuteChanged();
            });
            return;
        }

        UpdateDegradedBanner(state);
        RetryCoreInitCommand.NotifyCanExecuteChanged();
    }

    private void UpdateDegradedBanner(CoreState state)
    {
        IsCoreDegraded = state == CoreState.Degraded;

        if (!IsCoreDegraded)
        {
            CoreDegradedMessage = null;
            return;
        }

        // 尽量给用户“可操作”的信息：优先 LastError，其次 Diagnostics 摘要
        var msg = _coreHost.LastError;
        if (string.IsNullOrWhiteSpace(msg))
        {
            msg = _coreHost.Diagnostics?.LastInitSummary;
        }
        if (string.IsNullOrWhiteSpace(msg))
        {
            msg = "Core unavailable. Please retry or copy diagnostics.";
        }

        CoreDegradedMessage = msg;
    }

    private bool CanRetryCoreInit()
    {
        // 注意：新版 CoreState 没有 Stopped/Starting，而是 NotLoaded/Loading
        return _coreHost.State is not CoreState.Loading
            && _coreHost.State is not CoreState.Ready
            && _coreHost.State is not CoreState.ShuttingDown;
    }

    private async Task RetryCoreInitAsync()
    {
        try
        {
            await _coreHost.InitializeAsync();
        }
        catch (Exception ex)
        {
            // 如果实现层抛异常，也要降级并可复制诊断
            CoreDegradedMessage = $"Init failed: {ex.GetType().Name}: {ex.Message}";
            IsCoreDegraded = true;
        }
        finally
        {
            RetryCoreInitCommand.NotifyCanExecuteChanged();
        }
    }

    private async void CopyDiagnostics()
    {
        try
        {
            var text = _coreHost.GetDiagnosticsText();
            await _clipboard.SetTextAsync(text);
        }
        catch (Exception ex)
        {
            CoreDegradedMessage = $"Copy diagnostics failed: {ex.Message}";
            IsCoreDegraded = true;
        }
    }

    private void OpenLogsFolder()
    {
        try
        {
            var dir = _coreHost.Diagnostics?.LogDir
                      ?? _coreHost.Diagnostics?.AppDataDir
                      ?? _coreHost.Diagnostics?.CoreDataDir;

            if (string.IsNullOrWhiteSpace(dir))
            {
                CoreDegradedMessage = "No log directory available in diagnostics.";
                IsCoreDegraded = true;
                return;
            }

            Process.Start(new ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"\"{dir}\"",
                UseShellExecute = true
            });
        }
        catch (Exception ex)
        {
            CoreDegradedMessage = $"Open logs failed: {ex.Message}";
            IsCoreDegraded = true;
        }
    }
}
