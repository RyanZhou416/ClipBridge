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

    private long? _currentCursor = null;
    private bool _hasMoreItems = true;
    private bool _isLoading = false;
    private HistoryFilter? _activeFilter = null;

    // 增量插入时的上限，避免长时间运行后集合无限增长
    private const int MaxIncrementalItems = 2000;

    public string? PinnedItemId { get; set; }

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

                // 2. 调用 Core API (我们在第一步封装的方法)；await 保持在后台线程
                var page = await _coreService.ListHistoryAsync(query);

                // 3. ObservableCollection 不是线程安全的：集合修改与游标更新统一回到 UI 线程
                var added = 0;
                await RunOnUiThreadAsync(() =>
                {
                    foreach (var item in page.Items)
                    {
                        if (!string.IsNullOrEmpty(PinnedItemId) && item.ItemId == PinnedItemId)
                            continue;
                        Add(item);
                        added++;
                    }

                    // 4. 更新游标状态
                    _currentCursor = page.NextCursor;

                    // 如果 NextCursor 为 null，或者返回条目数为 0，说明到底了
                    if (page.NextCursor == null || page.Items.Count == 0)
                    {
                        _hasMoreItems = false;
                    }
                });

                // Count 反映实际添加到集合中的条目数（跳过锁定项）
                return new LoadMoreItemsResult { Count = (uint)added };
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

    public void Refresh(HistoryFilter? newFilter = null)
    {
        if (newFilter != null)
        {
            _activeFilter = newFilter;
        }

        _currentCursor = null;
        _hasMoreItems = true;
        _isLoading = false;

        // Clear/Add 同样必须在 UI 线程执行
        RunOnUiThread(() =>
        {
            Clear();

            // Insert pinned item at position 0 before incremental loading starts
            if (_pinnedItem != null)
            {
                Add(_pinnedItem);
            }
        });
    }

    /// <summary>
    /// 单条增量插入（新历史项到达时使用，避免整表 Refresh）。
    /// 列表按 CreatedTsMs 降序，锁定项固定在 index 0。
    /// </summary>
    public void InsertItem(ItemMetaPayload item)
    {
        if (item == null) return;

        RunOnUiThread(() =>
        {
            // 锁定项固定在第 0 位，不重复插入
            if (!string.IsNullOrEmpty(PinnedItemId) && item.ItemId == PinnedItemId)
            {
                return;
            }

            // 已存在则原地更新，保持现有位置
            var existing = IndexOfItem(item.ItemId);
            if (existing >= 0)
            {
                this[existing] = item;
                return;
            }

            var startIndex = !string.IsNullOrEmpty(PinnedItemId) && Count > 0 && this[0].ItemId == PinnedItemId
                ? 1
                : 0;

            var index = startIndex;
            while (index < Count && this[index].CreatedTsMs >= item.CreatedTsMs)
            {
                index++;
            }

            Insert(index, item);

            // 上限保护：超出时裁掉最旧的尾部项
            while (Count > MaxIncrementalItems)
            {
                RemoveAt(Count - 1);
            }
        });
    }

    /// <summary>按 ItemId 移除单条项（历史项删除时使用）。</summary>
    public void RemoveItem(string? itemId)
    {
        if (string.IsNullOrEmpty(itemId)) return;

        RunOnUiThread(() =>
        {
            var index = IndexOfItem(itemId);
            if (index >= 0)
            {
                RemoveAt(index);
            }
        });
    }

    private int IndexOfItem(string? itemId)
    {
        if (string.IsNullOrEmpty(itemId)) return -1;

        for (var i = 0; i < Count; i++)
        {
            if (this[i].ItemId == itemId) return i;
        }

        return -1;
    }

    /// <summary>确保集合修改发生在 UI 线程上（窗口尚未创建时直接执行）。</summary>
    private static void RunOnUiThread(Action action)
    {
        var dispatcher = App.MainWindow?.DispatcherQueue;
        if (dispatcher == null || dispatcher.HasThreadAccess)
        {
            action();
            return;
        }

        dispatcher.TryEnqueue(() => action());
    }

    /// <summary>同上，但等待 UI 线程执行完成，便于调用方拿到准确的添加数量。</summary>
    private static Task RunOnUiThreadAsync(Action action)
    {
        var dispatcher = App.MainWindow?.DispatcherQueue;
        if (dispatcher == null || dispatcher.HasThreadAccess)
        {
            action();
            return Task.CompletedTask;
        }

        var tcs = new TaskCompletionSource<object?>(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!dispatcher.TryEnqueue(() =>
        {
            try
            {
                action();
                tcs.TrySetResult(null);
            }
            catch (Exception ex)
            {
                tcs.TrySetException(ex);
            }
        }))
        {
            // 入队失败（窗口正在销毁）：不再修改集合
            tcs.TrySetResult(null);
        }

        return tcs.Task;
    }

    private ItemMetaPayload? _pinnedItem;

    public void SetPinnedItem(ItemMetaPayload? item)
    {
        _pinnedItem = item;
        PinnedItemId = item?.ItemId;
    }
}
