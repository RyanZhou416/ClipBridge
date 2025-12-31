namespace ClipBridgeShell_CS.Contracts.Services;

public interface ILocalSettingsService
{
    Task<T?> ReadSettingAsync<T>(string key);

    Task SaveSettingAsync<T>(string key, T value);

    // 新增：当任何设置项被 Save 时触发，参数为 key 名称
    event EventHandler<string> SettingChanged;
}
