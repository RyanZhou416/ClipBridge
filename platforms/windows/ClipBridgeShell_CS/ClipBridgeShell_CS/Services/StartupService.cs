using System.Diagnostics;
using System.Linq;
using System.Security;

using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Helpers;

using Microsoft.Win32;

using Windows.ApplicationModel;

namespace ClipBridgeShell_CS.Services;

public class StartupService : IStartupService
{
    private const string RegistryRunPath = @"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    private const string RegistryKeyName = "ClipBridge";
    private const string MsixTaskId = "ClipBridgeStartupTask";
    private const string ScheduledTaskName = "ClipBridge_AutoStart";

    /// <summary>自启时传入，用于启动后直接最小化到托盘。</summary>
    public const string StartupToTrayArgument = "--startup-to-tray";

    /// <summary>当前进程是否带有“自启到托盘”参数（由注册表/计划任务传入）。</summary>
    public static bool IsStartupToTrayRequested()
    {
        var args = Environment.GetCommandLineArgs();
        return args.Any(a => string.Equals(a, StartupToTrayArgument, StringComparison.Ordinal));
    }

    public IReadOnlyList<StartupMethod> GetAvailableMethods()
    {
        // 说明：这里刻意不再提供 StartupMethod.ScheduledTask。
        // 该方式需要 schtasks /Create /SC ONLOGON，而它在非提权进程下必然返回
        // "Access is denied"，旧实现因此回落到 Verb="runas" 强制弹 UAC。
        // 一个常驻托盘、按用户登录自启的应用完全不需要管理员权限，
        // 注册表 Run 键（HKCU）足以实现同样的效果且零提权。
        // 枚举值保留仅用于兼容历史设置里可能持久化的字符串。
        var methods = new List<StartupMethod> { StartupMethod.Registry };
#if !DEBUG
        if (RuntimeHelper.IsMSIX)
        {
            methods.Insert(0, StartupMethod.MsixStartupTask);
        }
#endif
        return methods;
    }

    public async Task<StartupState> GetStateAsync(StartupMethod method)
    {
        return method switch
        {
            StartupMethod.Registry => GetRegistryState(),
            // 计划任务方式已废弃：不再查询，避免任何 schtasks 调用。
            StartupMethod.ScheduledTask => StartupState.NotSupported,
            StartupMethod.MsixStartupTask => await GetMsixStateAsync(),
            _ => StartupState.NotSupported,
        };
    }

    public async Task<StartupState> SetEnabledAsync(StartupMethod method, bool enable)
    {
        if (enable)
        {
            // 只禁用其他方式，不碰即将启用的目标方式
            await DisableMethodsExceptAsync(method);
        }

        return method switch
        {
            StartupMethod.Registry => SetRegistryEnabled(enable),
            // 计划任务方式已废弃：明确返回不支持，绝不提权。
            StartupMethod.ScheduledTask => StartupState.NotSupported,
            StartupMethod.MsixStartupTask => await SetMsixEnabledAsync(enable),
            _ => StartupState.NotSupported,
        };
    }

    public async Task DisableAllAsync()
    {
        SetRegistryEnabled(false);
        TryDeleteLegacyScheduledTask();
        if (RuntimeHelper.IsMSIX)
        {
            await SetMsixEnabledAsync(false);
        }
    }

    private async Task DisableMethodsExceptAsync(StartupMethod except)
    {
        if (except != StartupMethod.Registry)
        {
            SetRegistryEnabled(false);
        }
        if (except != StartupMethod.ScheduledTask)
        {
            TryDeleteLegacyScheduledTask();
        }
        if (except != StartupMethod.MsixStartupTask && RuntimeHelper.IsMSIX)
        {
            await SetMsixEnabledAsync(false);
        }
    }

    /// <summary>
    /// 尽力清理旧版本创建的计划任务（历史遗留）。
    /// 刻意只用非提权方式：删不掉就算了，绝不为清理旧数据弹 UAC。
    /// 该任务只会按用户登录启动 ClipBridge，不删也不影响功能或安全。
    /// </summary>
    private static void TryDeleteLegacyScheduledTask()
    {
        try
        {
            RunSchtasks($"/Delete /TN \"{ScheduledTaskName}\" /F");
        }
        catch
        {
            // 忽略：无权限/不存在都无需处理
        }
    }

    #region Registry Run Key

    private static StartupState GetRegistryState()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(RegistryRunPath, false);
            var value = key?.GetValue(RegistryKeyName);
            return value != null ? StartupState.Enabled : StartupState.Disabled;
        }
        catch (SecurityException)
        {
            return StartupState.NotSupported;
        }
        catch
        {
            return StartupState.Unknown;
        }
    }

    private static StartupState SetRegistryEnabled(bool enable)
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(RegistryRunPath, true);
            if (key == null)
            {
                return StartupState.NotSupported;
            }

            if (enable)
            {
                var exePath = Environment.ProcessPath;
                if (string.IsNullOrEmpty(exePath))
                {
                    return StartupState.NotSupported;
                }
                key.SetValue(RegistryKeyName, $"\"{exePath}\" {StartupToTrayArgument}");
                return StartupState.Enabled;
            }
            else
            {
                if (key.GetValue(RegistryKeyName) != null)
                {
                    key.DeleteValue(RegistryKeyName, false);
                }
                return StartupState.Disabled;
            }
        }
        catch (SecurityException)
        {
            return StartupState.NotSupported;
        }
        catch
        {
            return StartupState.Unknown;
        }
    }

    #endregion

    #region Legacy scheduled-task cleanup (best effort, never elevated)

    private static (int ExitCode, string Output) RunSchtasks(string arguments)
    {
        var psi = new ProcessStartInfo
        {
            FileName = "schtasks.exe",
            Arguments = arguments,
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
        };

        using var proc = Process.Start(psi);
        if (proc == null)
        {
            return (-1, string.Empty);
        }
        var output = proc.StandardOutput.ReadToEnd();
        proc.WaitForExit(10_000);
        return (proc.ExitCode, output);
    }

    #endregion

    #region MSIX StartupTask

    private static async Task<StartupState> GetMsixStateAsync()
    {
        if (!RuntimeHelper.IsMSIX)
        {
            return StartupState.NotSupported;
        }

        try
        {
            var task = await StartupTask.GetAsync(MsixTaskId);
            return MapMsixState(task.State);
        }
        catch
        {
            return StartupState.NotSupported;
        }
    }

    private static async Task<StartupState> SetMsixEnabledAsync(bool enable)
    {
        if (!RuntimeHelper.IsMSIX)
        {
            return StartupState.NotSupported;
        }

        try
        {
            var task = await StartupTask.GetAsync(MsixTaskId);

            if (enable)
            {
                var newState = await task.RequestEnableAsync();
                return MapMsixState(newState);
            }
            else
            {
                task.Disable();
                return StartupState.Disabled;
            }
        }
        catch
        {
            return StartupState.NotSupported;
        }
    }

    private static StartupState MapMsixState(StartupTaskState state)
    {
        return state switch
        {
            StartupTaskState.Enabled => StartupState.Enabled,
            StartupTaskState.Disabled => StartupState.Disabled,
            StartupTaskState.DisabledByUser => StartupState.DisabledByUser,
            StartupTaskState.DisabledByPolicy => StartupState.DisabledByUser,
            StartupTaskState.EnabledByPolicy => StartupState.Enabled,
            _ => StartupState.Unknown,
        };
    }

    #endregion
}
