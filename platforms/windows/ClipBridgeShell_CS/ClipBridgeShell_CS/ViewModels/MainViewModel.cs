using System.Collections.ObjectModel;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Stores;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace ClipBridgeShell_CS.ViewModels;

public partial class MainViewModel : ObservableRecipient
{
    // 持有 Store 的引用
    private readonly HistoryStore _historyStore;

    // 直接暴露 Store 的集合供 UI 绑定
    // 这样当 Store 更新时，UI 会自动刷新
    public ObservableCollection<ItemMetaPayload> HistoryItems => _historyStore.Items;

    // [测试用] 手动模拟 Core 发事件的命令
    public IRelayCommand TestAddEventCommand
    {
        get;
    }

    // 构造函数注入
    public MainViewModel(HistoryStore historyStore, Services.EventPumpService pump)
    {
        _historyStore = historyStore;

        // 创建一个测试按钮命令：点击后模拟 Core 发来一条数据
        TestAddEventCommand = new RelayCommand(() =>
        {
            var randomId = (ulong)Random.Shared.Next(1000, 9999);
            var json = $$"""
            {
                "type": "item_added",
                "payload": {
                    "id": {{randomId}},
                    "timestamp": {{DateTimeOffset.UtcNow.ToUnixTimeMilliseconds()}},
                    "mime_type": "text/plain",
                    "preview_text": "Simulated Item #{{randomId}}",
                    "device_id": "myself"
                }
            }
            """;
            // 直接往泵里灌数据，假装是 Core 发来的
            pump.Enqueue(json);
        });
    }
}
