using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using ClipBridgeShell_CS.Collections;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Models.Events;

namespace ClipBridgeShell_CS.ViewModels;

public partial class HistoryViewModel : ObservableRecipient
{
    private readonly ICoreHostService _coreService;

    // 这是我们的“无限滚动”数据源，UI 的 ListView 将绑定到它
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

    public HistoryViewModel(ICoreHostService coreService)
    {
        _coreService = coreService;

        // 初始化增量集合
        Source = new HistoryIncrementalCollection(_coreService);

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
}
