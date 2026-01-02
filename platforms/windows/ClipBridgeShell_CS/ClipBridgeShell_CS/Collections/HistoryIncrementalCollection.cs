using System.Collections.ObjectModel;
using Microsoft.UI.Xaml.Data;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Models;
using ClipBridgeShell_CS.Core.Models.Events;
using System.Runtime.InteropServices.WindowsRuntime;

namespace ClipBridgeShell_CS.Collections;

public class HistoryIncrementalCollection : ObservableCollection<ItemMetaPayload>, ISupportIncrementalLoading
{
    private readonly ICoreHostService _coreService;

    // 状态管理
    private long? _currentCursor = null; // 上一页最后一条的 sort_ts_ms
    private bool _hasMoreItems = true;
    private bool _isLoading = false;
    private HistoryFilter? _activeFilter = null;

    // ISupportIncrementalLoading 接口属性
    public bool HasMoreItems => _hasMoreItems;

    public HistoryIncrementalCollection(ICoreHostService coreService)
    {
        _coreService = coreService;
    }

    /// <summary>
    /// 当 ListView 滚动到底部时，WinUI 会自动调用此方法
    /// </summary>
    public Windows.Foundation.IAsyncOperation<LoadMoreItemsResult> LoadMoreItemsAsync(uint count)
    {
        // 使用 AsyncInfo.Run 将 Task 转为 WinRT 需要的 IAsyncOperation
        return AsyncInfo.Run(async c =>
        {
            if (_isLoading || !_hasMoreItems)
            {
                return new LoadMoreItemsResult { Count = 0 };
            }

            _isLoading = true;

            try
            {
                // 1. 构造查询参数
                var query = new HistoryQuery
                {
                    // 限制最大请求数量，防止 UI 一次请求过多导致卡顿
                    Limit = Math.Clamp((int)count, 20, 50),
                    Cursor = _currentCursor,
                    Filter = _activeFilter
                };

                // 2. 调用 Core API (我们在第一步封装的方法)
                var page = await _coreService.ListHistoryAsync(query);

                // 3. 将新数据加入集合
                // 注意：ObservableCollection 会自动触发 UI 更新
                foreach (var item in page.Items)
                {
                    Add(item);
                }

                // 4. 更新游标状态
                _currentCursor = page.NextCursor;

                // 如果 NextCursor 为 null，或者返回条目数为 0，说明到底了
                if (page.NextCursor == null || page.Items.Count == 0)
                {
                    _hasMoreItems = false;
                }

                return new LoadMoreItemsResult { Count = (uint)page.Items.Count };
            } catch (Exception ex)
            {
                // 实际生产中可能需要通知 UI 显示错误条
                System.Diagnostics.Debug.WriteLine($"Error loading history: {ex}");
                return new LoadMoreItemsResult { Count = 0 };
            } finally
            {
                _isLoading = false;
            }
        });
    }

    /// <summary>
    /// 重置列表（用于刷新或改变搜索条件时）
    /// </summary>
    public void Refresh(HistoryFilter? newFilter = null)
    {
        if (newFilter != null)
        {
            _activeFilter = newFilter;
        }

        // 清空当前数据，这会触发 UI 清空
        Clear();

        // 重置游标
        _currentCursor = null;
        _hasMoreItems = true;
        _isLoading = false;

        // 注意：这里不需要手动调 LoadMoreItemsAsync。
        // 因为 Clear() 后 ListView 变空，只要 HasMoreItems=true，UI 布局会自动触发 LoadMoreItemsAsync。
    }
}
