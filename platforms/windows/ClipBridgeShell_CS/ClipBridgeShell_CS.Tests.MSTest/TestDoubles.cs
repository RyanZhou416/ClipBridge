using ClipBridgeShell_CS.Contracts.Services;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 测试用内存 <see cref="ILocalSettingsService"/>：不落盘，避免测试依赖用户目录。
/// </summary>
internal sealed class InMemoryLocalSettingsService : ILocalSettingsService
{
    private readonly Dictionary<string, object?> _values = new();

    public event EventHandler<string>? SettingChanged;

    public Task<T?> ReadSettingAsync<T>(string key)
    {
        return Task.FromResult(_values.TryGetValue(key, out var value) && value is T typed ? typed : default);
    }

    public Task SaveSettingAsync<T>(string key, T value)
    {
        _values[key] = value;
        SettingChanged?.Invoke(this, key);
        return Task.CompletedTask;
    }
}

/// <summary>
/// 测试用 <see cref="IAccountService"/>：直接返回预设的账号状态，不触碰 PasswordVault。
/// </summary>
internal sealed class StubAccountService : IAccountService
{
    private AccountLoadResult _state;

    public StubAccountService(AccountLoadResult state)
    {
        _state = state;
    }

    public event EventHandler<bool>? AccountStatusChanged;

    public AccountLoadResult State
    {
        get => _state;
        set
        {
            _state = value;
            AccountStatusChanged?.Invoke(this, value.Configured && value.PasswordAvailable);
        }
    }

    public Task SaveAccountAsync(string username, string password)
    {
        State = AccountLoadResult.Available(username, password);
        return Task.CompletedTask;
    }

    public Task<(string username, string password)?> LoadAccountAsync()
    {
        var state = _state;
        if (state.Configured
            && state.PasswordAvailable
            && !string.IsNullOrWhiteSpace(state.Username)
            && !string.IsNullOrWhiteSpace(state.Password))
        {
            return Task.FromResult<(string username, string password)?>((state.Username, state.Password));
        }

        return Task.FromResult<(string username, string password)?>(null);
    }

    public Task<AccountLoadResult> LoadAccountStateAsync() => Task.FromResult(_state);

    public Task<bool> HasAccountAsync() => Task.FromResult(
        _state.Configured
        && _state.PasswordAvailable
        && !string.IsNullOrWhiteSpace(_state.Username));

    public Task ClearAccountAsync()
    {
        State = AccountLoadResult.NotConfigured;
        return Task.CompletedTask;
    }
}
