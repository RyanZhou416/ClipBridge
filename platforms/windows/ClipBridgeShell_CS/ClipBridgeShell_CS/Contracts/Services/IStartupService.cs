namespace ClipBridgeShell_CS.Contracts.Services;

public enum StartupState
{
    Enabled,
    Disabled,
    DisabledByUser,
    NotSupported,
    Unknown,
}

public enum StartupMethod
{
    None,
    Registry,
    ScheduledTask,
    MsixStartupTask,
}

public interface IStartupService
{
    IReadOnlyList<StartupMethod> GetAvailableMethods();
    Task<StartupState> GetStateAsync(StartupMethod method);
    Task<StartupState> SetEnabledAsync(StartupMethod method, bool enable);
    Task DisableAllAsync();
}
