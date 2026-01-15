using System;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Contracts.ViewModels;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Services;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;

namespace ClipBridgeShell_CS.ViewModels;

public partial class DevicesViewModel : ObservableRecipient, INavigationAware
{
    private readonly ICoreHostService _coreHost;
    private readonly ILocalSettingsService _localSettings;
    private readonly EventPumpService _eventPump;
    private DispatcherTimer? _refreshTimer;
    private PeerMetaPayload? _selectedDevice;

    public ObservableCollection<PeerMetaPayload> Devices { get; } = new();

    [ObservableProperty]
    private int _outboundAllowedCount;

    [ObservableProperty]
    private int _inboundAllowedCount;

    public PeerMetaPayload? SelectedDevice
    {
        get => _selectedDevice;
        set => SetProperty(ref _selectedDevice, value);
    }

    public DevicesViewModel(ICoreHostService coreHost, ILocalSettingsService localSettings, EventPumpService eventPump)
    {
        _coreHost = coreHost;
        _localSettings = localSettings;
        _eventPump = eventPump;

        // 监听设备相关事件
        _eventPump.EventReceived += OnEventReceived;
    }

    public async void OnNavigatedTo(object parameter)
    {
        await RefreshDevicesAsync();

        // 启动定时刷新（每10秒）
        _refreshTimer = new DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(10)
        };
        _refreshTimer.Tick += async (s, e) => await RefreshDevicesAsync();
        _refreshTimer.Start();
    }

    public void OnNavigatedFrom()
    {
        _refreshTimer?.Stop();
        _refreshTimer = null;
    }

    private void OnEventReceived(string eventJson)
    {
        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(eventJson);
            var root = doc.RootElement;

            if (root.TryGetProperty("type", out var typeEl))
            {
                var type = typeEl.GetString();

                if (type == "peer_found" || type == "peer_changed")
                {
                    // 设备状态变化，刷新列表
                    _ = RefreshDevicesAsync();
                }
            }
        }
        catch (Exception ex)
        {
            // 忽略解析错误
        }
    }

    private async Task RefreshDevicesAsync()
    {
        await Task.Run(() =>
        {
            try
            {
                var peers = _coreHost.ListPeers();

                // 加载本地别名
                foreach (var peer in peers)
                {
                    var aliasKey = $"DeviceAlias_{peer.DeviceId}";
                    var alias = Task.Run(async () => await _localSettings.ReadSettingAsync<string>(aliasKey)).Result;
                    peer.LocalAlias = alias;
                }

                // 更新 UI 线程上的集合
                App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
                {
                    Devices.Clear();

                    foreach (var peer in peers)
                    {
                        Devices.Add(peer);
                    }

                    // 更新统计
                    OutboundAllowedCount = Devices.Count(p => p.ShareToPeer);
                    InboundAllowedCount = Devices.Count(p => p.AcceptFromPeer);
                });
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"RefreshDevicesAsync failed: {ex}");
            }
        });
    }

    [RelayCommand]
    private async Task SetShareToDevice((PeerMetaPayload device, bool newValue)? param)
    {
        if (param == null) return;
        var (device, newValue) = param.Value;

        try
        {
            // 调用 API 更新策略（使用用户期望的新值，而不是取反）
            _coreHost.SetPeerPolicy(device.DeviceId, newValue, null);

            // 注意：不手动更新 device.ShareToPeer，让 peer_changed 事件触发 RefreshDevicesAsync 来更新状态
            // 这样可以确保状态与核心保持一致
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"SetShareToDevice failed: {ex}");
            // TODO: 显示错误提示
            // 如果失败，需要恢复 UI 状态
            App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
            {
                var deviceInCollection = Devices.FirstOrDefault(d => d.DeviceId == device.DeviceId);
                if (deviceInCollection != null)
                {
                    deviceInCollection.ShareToPeer = !newValue; // 恢复到原始值
                }
            });
        }
    }

    [RelayCommand]
    private async Task SetAcceptFromDevice((PeerMetaPayload device, bool newValue)? param)
    {
        if (param == null) return;
        var (device, newValue) = param.Value;

        try
        {
            // 调用 API 更新策略（使用用户期望的新值，而不是取反）
            _coreHost.SetPeerPolicy(device.DeviceId, null, newValue);
            
            // 注意：不手动更新 device.AcceptFromPeer，让 peer_changed 事件触发 RefreshDevicesAsync 来更新状态
            // 这样可以确保状态与核心保持一致
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"SetAcceptFromDevice failed: {ex}");
            // TODO: 显示错误提示
            // 如果失败，需要恢复 UI 状态
            App.MainWindow?.DispatcherQueue.TryEnqueue(() =>
            {
                var deviceInCollection = Devices.FirstOrDefault(d => d.DeviceId == device.DeviceId);
                if (deviceInCollection != null)
                {
                    deviceInCollection.AcceptFromPeer = !newValue; // 恢复到原始值
                }
            });
        }
    }

    [RelayCommand]
    private void CopyDeviceInfo(PeerMetaPayload? device)
    {
        if (device == null) return;

        var info = $"Device ID: {device.DeviceId}\n" +
                   $"Name: {device.Name}\n" +
                   $"State: {device.ConnectionState}\n" +
                   $"Last Seen: {DateTimeOffset.FromUnixTimeMilliseconds(device.LastSeen):yyyy-MM-dd HH:mm:ss}\n" +
                   $"Share To: {device.ShareToPeer}\n" +
                   $"Accept From: {device.AcceptFromPeer}";

        var package = new DataPackage();
        package.SetText(info);
        Clipboard.SetContent(package);
    }

    [RelayCommand]
    private async Task SetDeviceAlias(PeerMetaPayload? device)
    {
        if (device == null) return;

        // 显示 ContentDialog 让用户输入别名
        var dialog = new Microsoft.UI.Xaml.Controls.ContentDialog
        {
            Title = "Set Device Alias",
            PrimaryButtonText = "Save",
            SecondaryButtonText = "Cancel",
            DefaultButton = Microsoft.UI.Xaml.Controls.ContentDialogButton.Primary,
            XamlRoot = App.MainWindow?.Content.XamlRoot
        };

        var textBox = new Microsoft.UI.Xaml.Controls.TextBox
        {
            PlaceholderText = "Enter alias (leave empty to remove)",
            Text = device.LocalAlias ?? string.Empty,
            Margin = new Microsoft.UI.Xaml.Thickness(0, 12, 0, 0)
        };

        dialog.Content = textBox;

        var result = await dialog.ShowAsync();
        if (result == Microsoft.UI.Xaml.Controls.ContentDialogResult.Primary)
        {
            var aliasKey = $"DeviceAlias_{device.DeviceId}";
            var newAlias = textBox.Text?.Trim();
            
            if (string.IsNullOrEmpty(newAlias))
            {
                // 删除别名
                await _localSettings.SaveSettingAsync<string>(aliasKey, null);
                device.LocalAlias = null;
            }
            else
            {
                // 保存别名
                await _localSettings.SaveSettingAsync<string>(aliasKey, newAlias);
                device.LocalAlias = newAlias;
            }
        }
    }

    [RelayCommand]
    private async Task RefreshDevices()
    {
        await RefreshDevicesAsync();
    }

    [RelayCommand]
    private async Task ClearPeerFingerprint(PeerMetaPayload? device)
    {
        if (device == null) return;

        try
        {
            // 显示确认对话框
            var dialog = new Microsoft.UI.Xaml.Controls.ContentDialog
            {
                Title = "清除设备指纹",
                Content = $"确定要清除设备 \"{GetDeviceDisplayName(device)}\" 的指纹吗？\n\n这将允许该设备重新配对，但需要重新建立信任关系。",
                PrimaryButtonText = "确定",
                SecondaryButtonText = "取消",
                XamlRoot = App.MainWindow?.Content.XamlRoot
            };

            var result = await dialog.ShowAsync();
            if (result != Microsoft.UI.Xaml.Controls.ContentDialogResult.Primary)
            {
                return;
            }

            // 调用清除指纹
            _coreHost.ClearPeerFingerprint(device.DeviceId);

            // 刷新设备列表
            await RefreshDevicesAsync();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"ClearPeerFingerprint failed: {ex}");
            // TODO: 显示错误提示
        }
    }

    /// <summary>
    /// 获取设备显示名称（优先使用本地别名）
    /// </summary>
    public string GetDeviceDisplayName(PeerMetaPayload device)
    {
        return !string.IsNullOrEmpty(device.LocalAlias) ? device.LocalAlias : device.Name;
    }
}
