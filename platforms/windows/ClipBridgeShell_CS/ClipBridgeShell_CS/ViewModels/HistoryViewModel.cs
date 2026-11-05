using System;
using System.Diagnostics;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Collections;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
namespace ClipBridgeShell_CS.ViewModels;

public partial class HistoryViewModel : ObservableRecipient, IDisposable
{
    private readonly ICoreHostService _coreService;
    private readonly ClipboardApplyService _clipboardApply;
    private readonly HistoryStore _historyStore;
    private readonly IClipboardService _clipboardService;
    private readonly MainViewModel _mainViewModel;
    private bool _disposed;

    public IAsyncRelayCommand<ItemMetaPayload> CopyCommand { get; }
    public IAsyncRelayCommand<ItemMetaPayload> DeleteItemCommand { get; }
    public IAsyncRelayCommand<ItemMetaPayload> GlobalDeleteItemCommand { get; }

    /// <summary>当前锁定的项 Id，用于历史项锁定图标与首页同步。</summary>
    public string? LockedItemId => _clipboardService.LockedItemId;

    /// <summary>首页当前选中项 Id，与 MainViewModel 同步，用于历史行选中效果。</summary>
    public string? SelectedItemId => _mainViewModel.SelectedItemId;

    public bool IsItemLocked(string itemId) => _clipboardService.LockedItemId == itemId;

    public bool IsItemSelected(string itemId) => _mainViewModel.SelectedItemId == itemId;

    private string? _toastMessage;
    public string? ToastMessage
    {
        get => _toastMessage;
        set => SetProperty(ref _toastMessage, value);
    }

    private bool _isToastOpen;
    public bool IsToastOpen
    {
        get => _isToastOpen;
        set => SetProperty(ref _isToastOpen, value);
    }

    private bool _isToastSuccess;
    public bool IsToastSuccess
    {
        get => _isToastSuccess;
        set => SetProperty(ref _isToastSuccess, value);
    }

    private void ShowToast(string message, bool success)
    {
        IsToastSuccess = success;
        ToastMessage = message;
        IsToastOpen = true;
        _ = HideToastAfterDelayAsync();
    }

    private async Task HideToastAfterDelayAsync()
    {
        await Task.Delay(2000);
        IsToastOpen = false;
    }

    // 这是我们的"无限滚动"数据源，UI 的 ListView 将绑定到它
    public HistoryIncrementalCollection Source
    {
        get;
    }

    [ObservableProperty]
    public partial string FilterText { get; set; }

    [ObservableProperty]
    public partial string? SelectedKind { get; set; }

    public HistoryViewModel(ICoreHostService coreService, ClipboardApplyService clipboardApply, HistoryStore historyStore, IClipboardService clipboardService, MainViewModel mainViewModel)
    {
        _coreService = coreService;
        _clipboardApply = clipboardApply;
        _historyStore = historyStore;
        _clipboardService = clipboardService;
        _mainViewModel = mainViewModel;
        CopyCommand = new AsyncRelayCommand<ItemMetaPayload>(CopyAsync);
        DeleteItemCommand = new AsyncRelayCommand<ItemMetaPayload>(DeleteItemAsync);
        GlobalDeleteItemCommand = new AsyncRelayCommand<ItemMetaPayload>(GlobalDeleteItemAsync);
        Source = new HistoryIncrementalCollection(_coreService);
        FilterText = string.Empty;

        _historyStore.Items.CollectionChanged += OnHistoryStoreItemsChanged;

        _clipboardService.ClipboardLockChanged += OnClipboardLockChanged;
        _mainViewModel.PropertyChanged += OnMainViewModelPropertyChanged;
        SyncPinnedItem();
    }

    private void OnMainViewModelPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_disposed) return;
        if (e.PropertyName == nameof(MainViewModel.SelectedItemId))
            OnPropertyChanged(nameof(SelectedItemId));
    }

    private void OnClipboardLockChanged(object? sender, bool isLocked)
    {
        if (_disposed) return;
        App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
        {
            if (_disposed) return;
            OnPropertyChanged(nameof(LockedItemId));
            SyncPinnedItem();
            ApplyFilters();
        });
    }

    /// <summary>与首页一致的锁定/解锁，锁定项会置顶。</summary>
    public async Task ToggleLockItemAsync(ItemMetaPayload item)
    {
        await _mainViewModel.ToggleLockItemAsync(item);
    }

    /// <summary>
    /// 删除锁定项后必须清空剪贴板锁定文本，否则已删除的内容会持续覆盖后续复制（直到重启）。
    /// 解锁统一走 <see cref="IClipboardService.UnlockClipboard"/>，两处 UI 状态由 ClipboardLockChanged 同步。
    /// </summary>
    private void UnlockIfDeletedItemWasLocked(string itemId)
    {
        if (_clipboardService.LockedItemId == itemId)
            _clipboardService.UnlockClipboard();
    }

    private void SyncPinnedItem()
    {
        var lockedId = _clipboardService.LockedItemId;
        if (string.IsNullOrEmpty(lockedId))
        {
            Source.SetPinnedItem(null);
            return;
        }

        var item = Source.FirstOrDefault(i => i.ItemId == lockedId)
                   ?? _historyStore.Items.FirstOrDefault(i => i.ItemId == lockedId)
                   ?? _mainViewModel.LockedItemMeta;
        Source.SetPinnedItem(item);
    }

    private void OnHistoryStoreItemsChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        if (_disposed) return;

        // 新项只做单条增量插入，避免整表 Refresh（Refresh 会 Clear 并重新拉取第一页）
        if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Add && e.NewItems != null)
        {
            foreach (ItemMetaPayload newItem in e.NewItems)
            {
                // 检查新项是否匹配当前的过滤条件
                if (MatchesFilter(newItem))
                {
                    // 锁定项固定在 index 0，由 InsertItem 负责去重与排序
                    Source.InsertItem(newItem);
                }
            }
        }
        // 删除项直接从列表中移除，无需重查
        else if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Remove && e.OldItems != null)
        {
            foreach (ItemMetaPayload removedItem in e.OldItems)
            {
                Source.RemoveItem(removedItem.ItemId);
            }
        }
        // 更新项：已显示的原地替换，不再匹配过滤条件的移除
        else if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Replace && e.NewItems != null)
        {
            foreach (ItemMetaPayload updatedItem in e.NewItems)
            {
                if (!MatchesFilter(updatedItem))
                {
                    Source.RemoveItem(updatedItem.ItemId);
                    continue;
                }

                if (Source.FirstOrDefault(i => i.ItemId == updatedItem.ItemId) != null)
                {
                    Source.InsertItem(updatedItem);
                }
            }
        }
        else
        {
            // Reset 等少见操作：走一次完整刷新，保证与 Store 一致
            ApplyFilters();
        }
    }

    /// <summary>
    /// 检查项是否匹配当前的过滤条件
    /// </summary>
    private bool MatchesFilter(ItemMetaPayload item)
    {
        // 检查类型过滤
        if (!string.IsNullOrEmpty(SelectedKind) && item.Kind != SelectedKind)
        {
            return false;
        }

        // 检查文本过滤
        if (!string.IsNullOrWhiteSpace(FilterText))
        {
            var filterLower = FilterText.ToLowerInvariant();
            // 检查预览文本
            if (item.Preview?.Text != null && item.Preview.Text.ToLowerInvariant().Contains(filterLower))
            {
                return true;
            }
            // 检查类型
            if (item.Kind != null && item.Kind.ToLowerInvariant().Contains(filterLower))
            {
                return true;
            }
            // 检查设备ID
            if (item.SourceDeviceId != null && item.SourceDeviceId.ToLowerInvariant().Contains(filterLower))
            {
                return true;
            }
            // 如果都不匹配，返回 false
            return false;
        }

        return true;
    }

    /// <summary>
    /// 当 FilterText 或 SelectedKind 发生变化时，自动调用此方法刷新列表
    /// (利用 CommunityToolkit.Mvvm 的特性，属性变化可触发逻辑)
    /// </summary>
    async partial void OnFilterTextChanged(string value)
    {
        ApplyFilters();
    }

    async partial void OnSelectedKindChanged(string? value)
    {
        ApplyFilters();
    }

    private void ApplyFilters()
    {
        // 构造新的过滤条件
        var filter = new HistoryFilter
        {
            FilterText = string.IsNullOrWhiteSpace(FilterText) ? null : FilterText,
            Kind = SelectedKind
        };

        // 调用集合的刷新方法，这会清空列表并重新触发第一页的加载
        Source.Refresh(filter);
    }

    /// <summary>
    /// 手动刷新命令（供 UI 的刷新按钮使用）
    /// </summary>
    [RelayCommand]
    public void Refresh()
    {
        ApplyFilters();
    }

    // TODO: 后续步骤将在此处添加“ItemClick”或“Copy”命令
    // [RelayCommand]
    // public async Task CopyItemAsync(ItemMetaPayload item) { ... }

    private async Task CopyAsync(ItemMetaPayload? meta)
    {
        Debug.WriteLine($"[History.Copy] invoked item_id={meta?.ItemId}");
        if (meta == null)
            return;

        if (_clipboardService.IsClipboardLocked && meta.ItemId != _clipboardService.LockedItemId)
        {
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_ClipboardIsLocked");
            ShowToast(string.IsNullOrEmpty(msg) || msg == "Main_ClipboardIsLocked" ? "Clipboard is locked" : msg, false);
            return;
        }

        try
        {
            await _clipboardApply.ApplyMetaToSystemClipboardAsync(meta);
            _mainViewModel.SelectedItemId = meta.ItemId;
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_CopySuccess");
            ShowToast(string.IsNullOrEmpty(msg) || msg == "Main_CopySuccess" ? "Copied!" : msg, true);
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[History.Copy] failed: {ex}");
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_CopyFailed");
            ShowToast(string.IsNullOrEmpty(msg) || msg == "Main_CopyFailed" ? "Copy failed" : msg, false);
        }
    }

    public async Task DeleteItemAsync(ItemMetaPayload? item)
    {
        if (item == null) return;
        try
        {
            await _coreService.DeleteItemLocalAsync(item.ItemId);
            UnlockIfDeletedItemWasLocked(item.ItemId);
            ApplyFilters();
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_ItemDeleted");
            ShowToast(string.IsNullOrEmpty(msg) || msg == "Main_ItemDeleted" ? "Deleted" : msg, true);
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[HistoryViewModel] DeleteItemAsync failed: {ex}");
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_DeleteFailed");
            ShowToast(string.IsNullOrEmpty(msg) || msg == "Main_DeleteFailed" ? "Delete failed" : msg, false);
        }
    }

    public async Task GlobalDeleteItemAsync(ItemMetaPayload? item)
    {
        if (item == null) return;
        try
        {
            await _coreService.DeleteItemGlobalAsync(item.ItemId);
            UnlockIfDeletedItemWasLocked(item.ItemId);
            ApplyFilters();
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_ItemDeletedGlobal");
            ShowToast(string.IsNullOrEmpty(msg) || msg == "Main_ItemDeletedGlobal" ? "Deleted from all devices" : msg, true);
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[HistoryViewModel] GlobalDeleteItemAsync failed: {ex}");
            var loc = WinUI3Localizer.Localizer.Get();
            var msg = loc.GetLocalizedString("Main_DeleteFailed");
            ShowToast(string.IsNullOrEmpty(msg) || msg == "Main_DeleteFailed" ? "Delete failed" : msg, false);
        }
    }

    /// <summary>
    /// 取消对 Store / Clipboard / MainViewModel 的事件订阅（幂等）。
    /// HistoryViewModel 未实现 INavigationAware，导航不会自动调用，需要页面在 OnNavigatedFrom 中显式调用。
    /// </summary>
    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;

        _historyStore.Items.CollectionChanged -= OnHistoryStoreItemsChanged;
        _clipboardService.ClipboardLockChanged -= OnClipboardLockChanged;
        _mainViewModel.PropertyChanged -= OnMainViewModelPropertyChanged;
    }
}
