//platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Services/LocalSettingsService.cs
using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Core.Helpers;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Models;

using Microsoft.Extensions.Options;

using Windows.ApplicationModel;
using Windows.Storage;

namespace ClipBridgeShell_CS.Services;

/// <summary>
/// 本地设置存储。
/// </summary>
/// <remarks>
/// <list type="bullet">
/// <item>MSIX：沿用 <see cref="ApplicationData.Current"/> 的 LocalSettings（行为不变）；</item>
/// <item>默认（无便携 marker）：<c>%LOCALAPPDATA%\ClipBridgeShell_CS\ApplicationData\LocalSettings.json</c>
/// （路径仍由 <see cref="LocalSettingsOptions"/> 决定，老用户不迁移）；</item>
/// <item>便携（marker 生效）：<c>&lt;dataRoot&gt;\settings.json</c>，与 core 数据同根。</item>
/// </list>
/// 便携路径在首次读写时才解析：DI 构造发生在 app 启动早期，延迟解析可以保证与
/// <c>CoreConfigBuilder</c> 的进程内缓存看到同一份结果，也不会让构造期 IO 失败影响启动。
/// </remarks>
public class LocalSettingsService : ILocalSettingsService
{
    private const string _defaultApplicationDataFolder = "ClipBridgeShell_CS/ApplicationData";
    private const string _defaultLocalSettingsFile = "LocalSettings.json";

    /// <summary>便携模式下设置文件名（固定，避免把默认路径的 LocalSettings.json 带进便携目录）。</summary>
    private const string _portableLocalSettingsFile = "settings.json";

    private readonly IFileService _fileService;
    private readonly LocalSettingsOptions _options;
    private readonly IDataRootProvider _dataRootProvider;

    private readonly string _localApplicationData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
    private readonly string _applicationDataFolder;
    private readonly string _localsettingsFile;

    /// <summary>设置文件位置：首次读写时才解析并缓存（MSIX 不经过这里）。</summary>
    private readonly Lazy<(string Folder, string FileName)> _storageLocation;

    private IDictionary<string, object> _settings;

    public event EventHandler<string>? SettingChanged;

    private bool _isInitialized;

    public LocalSettingsService(
        IFileService fileService,
        IOptions<LocalSettingsOptions> options,
        IDataRootProvider dataRootProvider)
    {
        _fileService = fileService;
        _options = options.Value;
        _dataRootProvider = dataRootProvider;

        _applicationDataFolder = Path.Combine(_localApplicationData, _options.ApplicationDataFolder ?? _defaultApplicationDataFolder);
        _localsettingsFile = _options.LocalSettingsFile ?? _defaultLocalSettingsFile;

        _storageLocation = new Lazy<(string Folder, string FileName)>(
            ResolveStorageLocation,
            LazyThreadSafetyMode.ExecutionAndPublication);

        _settings = new Dictionary<string, object>();
    }

    /// <summary>
    /// 解析设置文件位置：
    /// 便携模式 → <c>&lt;dataRoot&gt;\settings.json</c>；否则保持原有
    /// <c>%LOCALAPPDATA%\&lt;ApplicationDataFolder&gt;\&lt;LocalSettingsFile&gt;</c>。
    /// </summary>
    private (string Folder, string FileName) ResolveStorageLocation()
    {
        var dataRoot = _dataRootProvider.Current;
        if (dataRoot.IsPortable)
        {
            return (dataRoot.Root, _portableLocalSettingsFile);
        }

        return (_applicationDataFolder, _localsettingsFile);
    }

    private async Task InitializeAsync()
    {
        if (!_isInitialized)
        {
            _settings = await Task.Run(() =>
            {
                var (folder, fileName) = _storageLocation.Value;
                return _fileService.Read<IDictionary<string, object>>(folder, fileName);
            }) ?? new Dictionary<string, object>();

            _isInitialized = true;
        }
    }

    public async Task<T?> ReadSettingAsync<T>(string key)
    {
        if (RuntimeHelper.IsMSIX)
        {
            if (ApplicationData.Current.LocalSettings.Values.TryGetValue(key, out var obj))
            {
                return await Json.ToObjectAsync<T>((string)obj);
            }
        }
        else
        {
            await InitializeAsync();

            if (_settings != null && _settings.TryGetValue(key, out var obj))
            {
                return await Json.ToObjectAsync<T>((string)obj);
            }
        }

        return default;
    }

    public async Task SaveSettingAsync<T>(string key, T value)
    {
        if (RuntimeHelper.IsMSIX)
        {
            ApplicationData.Current.LocalSettings.Values[key] = await Json.StringifyAsync(value!);
            SettingChanged?.Invoke(this, key);
        }
        else
        {
            await InitializeAsync();

            _settings[key] = await Json.StringifyAsync(value!);

            await Task.Run(() =>
            {
                var (folder, fileName) = _storageLocation.Value;
                _fileService.Save(folder, fileName, _settings);
            });

            SettingChanged?.Invoke(this, key);
        }
    }

}
