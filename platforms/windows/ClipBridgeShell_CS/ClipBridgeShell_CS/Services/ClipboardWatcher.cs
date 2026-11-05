using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using Microsoft.UI.Xaml.Controls;   
using Microsoft.UI.Xaml;
using Microsoft.Extensions.Logging;

namespace ClipBridgeShell_CS.Services;

public class ClipboardWatcher
{
    private readonly IClipboardService _clipboardService;
    private readonly ICoreHostService _coreHostService;
    private readonly IngestPolicy _policy;
    private readonly ILocalSettingsService _settingsService;
    private readonly ILogger<ClipboardWatcher>? _logger;

    private bool _isListening = false;
    private bool _isInitialized = false;
    // 采集开关的权威状态：剪贴板采集只由本类作为唯一入口，开关必须在这里生效
    private bool _isCaptureEnabled = false;
    private CancellationTokenSource? _debounceCts;

    // 最近一次剪贴板变化的快照。由 ClipboardService 在写回锁定内容之前捕获并通过事件传入，
    // 防抖结束后直接使用它，避免再次读取系统剪贴板（一次 GetContent + 全文哈希）。
    private ClipboardSnapshot? _pendingSnapshot;

    // 配置：防抖时间 (ms)
    private const int DebounceMs = 200;
    // 配置：设置键名
    private const string CaptureEnabledKey = "IsClipboardCaptureEnabled";

    public ClipboardWatcher(
        IClipboardService clipboardService,
        ICoreHostService coreHostService,
        ILocalSettingsService settingsService,
        ILoggerFactory? loggerFactory = null)
    {
        _clipboardService = clipboardService;
        _coreHostService = coreHostService;
        _settingsService = settingsService;
        _logger = loggerFactory?.CreateLogger<ClipboardWatcher>();

        // 策略类是纯逻辑，可以直接在这里new，也可以注入。
        // 为了简单，如果策略不依赖其他复杂服务，直接new。
        // 但鉴于我们Phase 2让策略依赖了 IClipboardService，最好通过构造函数注入或这里手动new。
        _policy = new IngestPolicy(_clipboardService);
    }

    public void SetCaptureState(bool isEnabled)
    {
        // 先记录开关状态，再切换订阅：即使有防抖中的事件在飞，ProcessClipboardChange 也会据此提前返回
        _isCaptureEnabled = isEnabled;
        if (isEnabled) Start();
        else Stop();
    }

    private void Start()
    {
        if (_isListening)
            return; // 已经在听了，别重复加 +=

        _clipboardService.ContentChanged += OnClipboardContentChanged;
        _isListening = true;
        _logger?.LogInformation("Clipboard monitoring started");
    }

    private void Stop()
    {
        if (!_isListening)
            return; // 已经停了，别重复减 -=
        _clipboardService.ContentChanged -= OnClipboardContentChanged;
        _debounceCts?.Cancel(); // 取消正在进行的防抖任务
        _isListening = false;
        _logger?.LogInformation("Clipboard monitoring stopped");
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


    private async void OnClipboardContentChanged(object? sender, ClipboardSnapshot snapshot)
    {
        // 采集关闭时直接忽略（即使在 Stop 之前已进入本方法）
        if (!_isCaptureEnabled)
            return;

        // 记录本次快照；防抖期间若又来新事件，后者会覆盖前者（保留最新）
        _pendingSnapshot = snapshot;

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
        // 采集开关是最终权威：关闭时绝不 ingest（即便事件已排队或订阅尚未摘除）
        if (!_isCaptureEnabled)
            return;

        // 确保 Core 准备好了
        if (_coreHostService.State != CoreState.Ready)
            return;

        try
        {
            _logger?.LogInformation("Clipboard content changed, processing...");

            // A. 取事件随附的快照（在写回锁定内容之前捕获），不再二次读取系统剪贴板。
            //    这样锁定剪贴板时的“复制内容不丢失”语义得以保留，同时省掉一次 GetContent + 全文哈希。
            var snapshot = _pendingSnapshot;
            _pendingSnapshot = null;
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

                _logger?.LogInformation("Clipboard ingest allowed, type: {Type}", snapshot.MimeType);
            }
            else
            {
                _logger?.LogWarning("Clipboard ingest denied by policy: {Reason}", decision.Reason);
            }
        } catch (Exception ex)
        {
            _logger?.LogError(ex, "Failed to process clipboard change");
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
        App.MainWindow?.DispatcherQueue.TryEnqueue(async () =>
        {
            try
            {
                var dialog = new ContentDialog
                {
                    Title = "Watcher 状态已更新",
                    Content = $"收到回调！\n\n新状态: {(isEnabled ? "✅ 监听开启" : "⛔ 监听暂停")}\n\n(此弹窗仅供调试验证)",
                    CloseButtonText = "我知道了",
                    XamlRoot = App.MainWindow?.Content?.XamlRoot
                };
                await dialog.ShowAsync();
            } catch
            {
                // 如果已有弹窗打开，可能会抛异常，忽略即可
            }
        });
    }
}
