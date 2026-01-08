using System.Diagnostics;
using ClipBridgeShell_CS.Collections;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Services;
using ClipBridgeShell_CS.Stores;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace ClipBridgeShell_CS.ViewModels;

public partial class HistoryViewModel : ObservableRecipient
{
    private readonly ICoreHostService _coreService;
    private readonly ClipboardApplyService _clipboardApply;
    private readonly HistoryStore _historyStore;

    public IAsyncRelayCommand<ItemMetaPayload> CopyCommand { get; }

    // 这是我们的"无限滚动"数据源，UI 的 ListView 将绑定到它
    public HistoryIncrementalCollection Source
    {
        get;
    }

    // 搜索过滤词
    [ObservableProperty]
    private string _filterText = string.Empty;

    // 过滤类型 (可选: "text", "image", "file", null=全部)
    [ObservableProperty]
    private string? _selectedKind = null;

    public HistoryViewModel(ICoreHostService coreService, ClipboardApplyService clipboardApply, HistoryStore historyStore)
    {
        _coreService = coreService;
        _clipboardApply = clipboardApply;
        _historyStore = historyStore;
        CopyCommand = new AsyncRelayCommand<ItemMetaPayload>(CopyAsync);
        // 初始化增量集合
        Source = new HistoryIncrementalCollection(_coreService);

        // 监听 HistoryStore 的变化，当有新项添加时自动刷新
        _historyStore.Items.CollectionChanged += OnHistoryStoreItemsChanged;
    }

    private void OnHistoryStoreItemsChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        // 当有新项添加时，检查是否需要刷新列表
        if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Add && e.NewItems != null)
        {
            foreach (ItemMetaPayload newItem in e.NewItems)
            {
                // 检查新项是否匹配当前的过滤条件
                if (MatchesFilter(newItem))
                {
                    // 如果匹配，刷新列表以显示新项
                    // 注意：这里使用 Refresh 会清空列表并重新加载，确保新项出现在正确位置
                    ApplyFilters();
                    break; // 只需要刷新一次
                }
            }
        }
        // 当有项更新时，也需要检查是否需要刷新
        else if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Replace && e.NewItems != null)
        {
            foreach (ItemMetaPayload updatedItem in e.NewItems)
            {
                if (MatchesFilter(updatedItem))
                {
                    // 如果更新的项在当前过滤条件下，刷新列表
                    ApplyFilters();
                    break;
                }
            }
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
        System.Diagnostics.Debug.WriteLine($"[History.Copy] invoked. meta={(meta == null ? "null" : $"{meta.ItemId} kind={meta.Kind} mime={meta.Content?.Mime} bytes={meta.Content?.TotalBytes}")}");
        if (meta == null)
            return;
        await _clipboardApply.ApplyMetaToSystemClipboardAsync(meta);
    }
}
