#!/usr/bin/env pwsh
# =============================================================================
# 在独立（隐藏）桌面上运行被测程序，用于测量"窗口可见之后"的阶段。
#
# 为什么需要它：
#   - CB_STARTUP_OFFSCREEN=1 把窗口移到虚拟桌面之外，虽然用户看不见，但 DWM 会判定窗口被遮挡
#     并降低渲染优先级，导致"可见之后"的工作（Shell 构建、首页 Loaded）被显著拖慢 ——
#     这些数据不能用来衡量可交互时间。
#   - 而在屏幕上真实显示又会反复弹窗打断用户。
#   - CreateDesktop + CreateProcess(lpDesktop) 可以把进程整体放到一个不可见的桌面里：
#     窗口真实显示与合成（测量保真），但用户屏幕上什么都看不到，也不抢焦点。
#
# 用法：
#   pwsh scripts/measure-hidden-desktop.ps1 -Exe <path> -Runs 5 [-Env @{CB_SYNC_SHELL='1'}]
# 读数：解析 %LOCALAPPDATA%\ClipBridge\startup.log 中该 pid 段落里的相位打点，
#       输出 window-activated / mainpage-loaded 及其差值（可见→可用）。
# =============================================================================

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [int]$Runs = 5,
    [hashtable]$Env = @{},
    [int]$WaitMs = 3000,
    [string]$Desktop = 'cbmeasure'
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Exe)) { throw "找不到可执行文件：$Exe" }
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$ExeDir = Split-Path -Parent $Exe
$LogPath = Join-Path $env:LOCALAPPDATA 'ClipBridge\startup.log'

Add-Type -Namespace CBMeasure -Name HiddenDesktop -MemberDefinition @'
[StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
public struct SI { public int cb; public string lpReserved; public string lpDesktop; public string lpTitle;
  public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
  public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError; }
[StructLayout(LayoutKind.Sequential)]
public struct PI { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }
[DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
public static extern IntPtr CreateDesktop(string name, IntPtr dev, IntPtr devmode, int flags, uint access, IntPtr sa);
[DllImport("user32.dll", SetLastError=true)] public static extern bool CloseDesktop(IntPtr h);
[DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
public static extern bool CreateProcess(string app, string cmd, IntPtr pa, IntPtr ta, bool inherit, int flags, IntPtr env, string cwd, ref SI si, out PI pi);
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool CloseHandle(IntPtr h);
public static IntPtr NewDesktop(string name) {
  var h = CreateDesktop(name, IntPtr.Zero, IntPtr.Zero, 0, 0x0F01FF, IntPtr.Zero);
  if (h == IntPtr.Zero) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
  return h; }
public static int Launch(string exe, string cwd, string desktop) {
  var si = new SI(); si.cb = Marshal.SizeOf(si); si.lpDesktop = desktop; si.dwFlags = 1; si.wShowWindow = 4;
  PI pi;
  if (!CreateProcess(null, "\"" + exe + "\"", IntPtr.Zero, IntPtr.Zero, false, 0, IntPtr.Zero, cwd, ref si, out pi))
    throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
  CloseHandle(pi.hThread); CloseHandle(pi.hProcess); return pi.dwProcessId; }
'@

function Median($values) {
    if (-not $values) { return $null }
    $sorted = $values | Sort-Object
    return [math]::Round($sorted[[int][math]::Floor($sorted.Count / 2)], 1)
}

$deskHandle = [CBMeasure.HiddenDesktop]::NewDesktop($Desktop)
Write-Host "隐藏桌面 '$Desktop' 已创建（句柄 $deskHandle）。窗口不会出现在屏幕上，也不会抢焦点。"

$results = @()
try {
    for ($i = 1; $i -le $Runs; $i++) {
        $env:CB_STARTUP_TRACE = '1'
        foreach ($key in $Env.Keys) { Set-Item -Path "env:$key" -Value $Env[$key] }

        $appPid = [CBMeasure.HiddenDesktop]::Launch($Exe, $ExeDir, $Desktop)
        Start-Sleep -Milliseconds $WaitMs
        Get-Process -Id $appPid -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 400

        Remove-Item env:CB_STARTUP_TRACE -ErrorAction SilentlyContinue
        foreach ($key in $Env.Keys) { Remove-Item "env:$key" -ErrorAction SilentlyContinue }

        $visible = $null; $ready = $null; $shellReady = $null
        if (Test-Path -LiteralPath $LogPath) {
            $lines = Get-Content -LiteralPath $LogPath
            $start = -1
            for ($j = 0; $j -lt $lines.Count; $j++) {
                if ($lines[$j] -match "^--- run .* pid=$appPid ---") { $start = $j }
            }
            if ($start -ge 0) {
                for ($j = $start + 1; $j -lt $lines.Count; $j++) {
                    if ($lines[$j] -match '^\s*([\d.]+)\t[\d:.]+\twindow-activated$') { $visible = [double]$Matches[1] }
                    if ($lines[$j] -match '^\s*([\d.]+)\t[\d:.]+\tshell-built-after-first-frame$') { $shellReady = [double]$Matches[1] }
                    if ($lines[$j] -match '^\s*([\d.]+)\t[\d:.]+\tmainpage-loaded$') { $ready = [double]$Matches[1]; break }
                }
            }
        }

        $usable = if ($visible -and $ready) { $ready - $visible } else { $null }
        $shellAfter = if ($visible -and $shellReady) { $shellReady - $visible } else { $null }
        $results += [pscustomobject]@{ Run = $i; Visible = $visible; ShellAfterVisible = $shellAfter; UsableAfterVisible = $usable }
        Write-Host ("  run {0}: 可见={1}ms  可见后 Shell={2}ms  可见后可用={3}ms" -f `
            $i, $(if ($visible) { [math]::Round($visible, 1) } else { 'n/a' }), `
            $(if ($shellAfter) { [math]::Round($shellAfter, 1) } else { 'n/a' }), `
            $(if ($usable) { [math]::Round($usable, 1) } else { 'n/a' }))
    }
}
finally {
    [void][CBMeasure.HiddenDesktop]::CloseDesktop($deskHandle)
    Get-Process -Name (Split-Path $Exe -LeafBase) -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

$results | Format-Table -AutoSize | Out-String | Write-Host
$visibleVals = @($results | Where-Object Visible | ForEach-Object Visible)
$usableVals = @($results | Where-Object UsableAfterVisible | ForEach-Object UsableAfterVisible)
$shellVals = @($results | Where-Object ShellAfterVisible | ForEach-Object ShellAfterVisible)
Write-Host ("可见时间           median={0}ms  min={1}  max={2}" -f (Median $visibleVals), (($visibleVals | Measure-Object -Minimum).Minimum), (($visibleVals | Measure-Object -Maximum).Maximum))
Write-Host ("可见后 Shell 完成  median={0}ms" -f (Median $shellVals))
Write-Host ("可见后可用         median={0}ms" -f (Median $usableVals))
