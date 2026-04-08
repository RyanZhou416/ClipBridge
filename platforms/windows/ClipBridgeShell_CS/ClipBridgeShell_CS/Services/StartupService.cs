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
        var methods = new List<StartupMethod> { StartupMethod.Registry, StartupMethod.ScheduledTask };
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
            StartupMethod.ScheduledTask => GetScheduledTaskState(),
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
            StartupMethod.ScheduledTask => SetScheduledTaskEnabled(enable),
            StartupMethod.MsixStartupTask => await SetMsixEnabledAsync(enable),
            _ => StartupState.NotSupported,
        };
    }

    public async Task DisableAllAsync()
    {
        SetRegistryEnabled(false);
        try { RunSchtasks($"/Delete /TN \"{ScheduledTaskName}\" /F"); } catch { }
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
            try { RunSchtasks($"/Delete /TN \"{ScheduledTaskName}\" /F"); } catch { }
        }
        if (except != StartupMethod.MsixStartupTask && RuntimeHelper.IsMSIX)
        {
            await SetMsixEnabledAsync(false);
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

    #region Scheduled Task

    private static StartupState GetScheduledTaskState()
    {
        try
        {
            var result = RunSchtasks($"/Query /TN \"{ScheduledTaskName}\" /FO CSV /NH");
            if (result.ExitCode == 0 && result.Output.Contains(ScheduledTaskName, StringComparison.OrdinalIgnoreCase))
            {
                return result.Output.Contains("Ready", StringComparison.OrdinalIgnoreCase)
                    ? StartupState.Enabled
                    : StartupState.Disabled;
            }
            return StartupState.Disabled;
        }
        catch
        {
            return StartupState.Unknown;
        }
    }

    private static StartupState SetScheduledTaskEnabled(bool enable)
    {
        try
        {
            if (enable)
            {
                var exePath = Environment.ProcessPath;
                if (string.IsNullOrEmpty(exePath))
                {
                    return StartupState.NotSupported;
                }

                // /F 覆盖已有同名任务，无需先删除；自启时加参数以便启动后最小化到托盘
                var args = $"/Create /TN \"{ScheduledTaskName}\" /TR \"\\\"{exePath}\\\" {StartupToTrayArgument}\" /SC ONLOGON /RL LIMITED /F /DELAY 0000:10";
                if (!TryRunSchtasksWithElevation(args))
                {
                    return StartupState.Unknown;
                }
                return GetScheduledTaskState();
            }
            else
            {
                TryRunSchtasksWithElevation($"/Delete /TN \"{ScheduledTaskName}\" /F");
                return StartupState.Disabled;
            }
        }
        catch
        {
            return StartupState.Unknown;
        }
    }

    /// <summary>
    /// 先以普通权限尝试，失败则通过 UAC 提权重试。
    /// </summary>
    private static bool TryRunSchtasksWithElevation(string arguments)
    {
        var result = RunSchtasks(arguments);
        if (result.ExitCode == 0)
        {
            return true;
        }
        return RunSchtasksElevated(arguments);
    }

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

    /// <summary>
    /// 以管理员权限运行 schtasks（弹出 UAC 提示）。
    /// 因为 UseShellExecute=true 不支持重定向输出，所以只返回是否成功。
    /// </summary>
    private static bool RunSchtasksElevated(string arguments)
    {
        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = "schtasks.exe",
                Arguments = arguments,
                UseShellExecute = true,
                Verb = "runas",
                WindowStyle = ProcessWindowStyle.Hidden,
            };
            using var proc = Process.Start(psi);
            if (proc == null)
            {
                return false;
            }
            proc.WaitForExit(30_000);
            return proc.ExitCode == 0;
        }
        catch (System.ComponentModel.Win32Exception)
        {
            // 用户取消了 UAC 提示
            return false;
        }
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
