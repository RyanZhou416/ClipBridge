using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using Microsoft.UI.Xaml.Controls;   
using Microsoft.UI.Xaml;

namespace ClipBridgeShell_CS.Services;

public class ClipboardWatcher
{
    private readonly IClipboardService _clipboardService;
    private readonly ICoreHostService _coreHostService;
    private readonly IngestPolicy _policy;
    private readonly ILocalSettingsService _settingsService;

    private bool _isListening = false;
    private bool _isInitialized = false;
    private CancellationTokenSource? _debounceCts;

    // 配置：防抖时间 (ms)
    private const int DebounceMs = 200;
    // 配置：设置键名
    private const string CaptureEnabledKey = "IsClipboardCaptureEnabled";

    public ClipboardWatcher(
        IClipboardService clipboardService,
        ICoreHostService coreHostService,
        ILocalSettingsService settingsService)
    {
        _clipboardService = clipboardService;
        _coreHostService = coreHostService;
        _settingsService = settingsService;

        // 策略类是纯逻辑，可以直接在这里new，也可以注入。
        // 为了简单，如果策略不依赖其他复杂服务，直接new。
        // 但鉴于我们Phase 2让策略依赖了 IClipboardService，最好通过构造函数注入或这里手动new。
        _policy = new IngestPolicy(_clipboardService);
    }

    public void SetCaptureState(bool isEnabled)
    {
        if (isEnabled) Start();
        else Stop();
    }

    private void Start()
    {
        if (_isListening)
            return; // 已经在听了，别重复加 +=

        _clipboardService.ContentChanged += OnClipboardContentChanged;
        _isListening = true;
        System.Diagnostics.Debug.WriteLine("[Watcher] Clipboard Monitor Started.");
    }

    private void Stop()
    {
        if (!_isListening)
            return; // 已经停了，别重复减 -=
        _clipboardService.ContentChanged -= OnClipboardContentChanged;
        _debounceCts?.Cancel(); // 取消正在进行的防抖任务
        _isListening = false;
        System.Diagnostics.Debug.WriteLine("[Watcher] Clipboard Monitor Stopped.");
    }

    public void Shutdown()
    {
        _settingsService.SettingChanged -= OnSettingChanged;
        Stop();
    }

    public async void Initialize()
    {
        if (_isInitialized)
            return;
        _settingsService.SettingChanged += OnSettingChanged;
        _isInitialized = true;

        var isEnabled = await _settingsService.ReadSettingAsync<bool?>(CaptureEnabledKey) ?? true;
        SetCaptureState(isEnabled);
    }


    private async void OnClipboardContentChanged(object? sender, EventArgs e)
    {

        // 1. 防抖 (Debounce)
        _debounceCts?.Cancel();
        _debounceCts = new CancellationTokenSource();
        var token = _debounceCts.Token;

        try
        {
            await Task.Delay(DebounceMs, token);
        } catch (TaskCanceledException)
        {
            return; // 新的事件来了，取消当前的
        }

        if (token.IsCancellationRequested)
            return;

        // 2. 真正的处理逻辑
        await ProcessClipboardChange();
    }

    private async Task ProcessClipboardChange()
    {
        // 确保 Core 准备好了
        if (_coreHostService.State != CoreState.Ready)
            return;

        try
        {
            // A. 获取快照
            var snapshot = await _clipboardService.GetSnapshotAsync();
            if (snapshot == null)
                return;

            // B. 策略判断
            var decision = _policy.Decide(snapshot);

            if (decision.Type == IngestDecisionType.Allow)
            {
                // C. 调用 Core
                var json = snapshot.ToJson();
                await _coreHostService.IngestLocalCopy(json);

                // D. 更新策略状态 (用于连续去重)
                _policy.OnIngestSuccess(snapshot);

                System.Diagnostics.Debug.WriteLine($"[Watcher] Ingested: {snapshot.MimeType}, FP={snapshot.Fingerprint}");
            }
            else
            {
                System.Diagnostics.Debug.WriteLine($"[Watcher] Denied: {decision.Reason}");
            }
        } catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[Watcher] Error: {ex.Message}");
        }
    }

    private async void OnSettingChanged(object? sender, string key)
    {
        // 只关心我们要的 Key
        if (key == CaptureEnabledKey)
        {
            // 重新读取最新值
            var isEnabled = await _settingsService.ReadSettingAsync<bool?>(CaptureEnabledKey) ?? true;
            // 动态切换钩子
            SetCaptureState(isEnabled);
#if DEBUG
            ShowDebugPopup(isEnabled);
#endif
        }
    }

    // --- 调试辅助方法 ---
    private void ShowDebugPopup(bool isEnabled)
    {
        // 因为 Watcher 可能在后台线程运行，必须调度到 UI 线程
        App.MainWindow.DispatcherQueue.TryEnqueue(async () =>
        {
            try
            {
                var dialog = new ContentDialog
                {
                    Title = "Watcher 状态已更新",
                    Content = $"收到回调！\n\n新状态: {(isEnabled ? "✅ 监听开启" : "⛔ 监听暂停")}\n\n(此弹窗仅供调试验证)",
                    CloseButtonText = "我知道了",
                    XamlRoot = App.MainWindow.Content.XamlRoot
                };
                await dialog.ShowAsync();
            } catch
            {
                // 如果已有弹窗打开，可能会抛异常，忽略即可
            }
        });
    }
}
