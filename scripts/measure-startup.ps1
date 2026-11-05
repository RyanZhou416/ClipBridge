<#
.SYNOPSIS
    外部测量 ClipBridge Windows 外壳从"进程创建"到"主窗口就绪"的耗时，支持交错 A/B。

.DESCRIPTION
    为什么必须交错 A/B（而不是先测完 A 再测 B）：
    手动实测同一个二进制，跨会话可出现约 13% 的差异（660 / 748 / 786 ms）。
    冷/热文件缓存、CPU 调频、后台编译、杀毒扫描、DWM 状态等系统级漂移会叠加到
    每一个样本上；如果顺序测 A、再测 B，"会话漂移"会被误判成版本差异。
    本脚本按 A,B,A,B... 交错启动，并计算每对相邻 A/B 的 B-A 差值，
    让两个变体共享同一轮的热/IO/系统状态，从而抵消大部分漂移。

    测量方法（只做外部观测，不要求应用配合）：
      - 在 Start-Process 之前启动 Stopwatch；
      - 每 10 ms 轮询 MainWindowHandle -ne 0 且 MainWindowTitle -ne ''；
      - 检测到窗口后读取 WorkingSet64；
      - 超时或进程提前退出记为 -1，继续下一轮。

    焦点安全：
      - 默认设置 CB_STARTUP_NO_FOREGROUND=1（只传给子进程），窗口可见但不抢焦点；
      - 无论是否设置该变量，每次启动前捕获前台窗口，测量并清理完成后恢复；
        恢复失败只打印警告，不会中断测量。
      - 子进程环境变量的原值在 Start-Process 返回后立即恢复。

    每次启动前都会结束后台残留的 ClipBridgeShell_CS / 测试宿主，并等待 600 ms，
    避免单实例保护导致新进程立即退出；脚本会验证进程确实已结束，清理失败即报错。
    默认每次测量后结束应用；-KeepAlive 只保留最后一次成功启动的应用。

    同一个 exe 也可以用 -EnvA / -EnvB 给两个变体注入不同的子进程环境变量；
    显式提供 -EnvB（即使空表）即可启用 A/B，B 默认复用 A 的 exe 路径。

.PARAMETER ExeA
    可选。变体 A 的可执行文件路径（.exe）。省略时默认使用仓库内
    platforms\windows\ClipBridgeShell_CS\ClipBridgeShell_CS\bin\x64\Release\
    net10.0-windows10.0.26100.0\win-x64\ClipBridgeShell_CS.exe。

.PARAMETER ExeB
    可选。变体 B 的可执行文件路径；省略时只测量 A。
    显式提供 -ExeB（非空路径）或显式提供 -EnvB（即使 -EnvB @{}）会启用 A/B；
    后者用于同一个 exe、不同环境变量的对照，此时 B 默认复用 A 的路径。

.PARAMETER EnvA
    变体 A 专用的子进程环境变量（hashtable，默认 @{}）。
    键名必须是合法环境变量名，键和值都必须是字符串；只影响 A 的子进程。

.PARAMETER EnvB
    变体 B 专用的子进程环境变量（hashtable，默认 @{}）。
    显式提供本参数（即使空表）会启用 A/B 模式；只影响 B 的子进程。
    -NoForeground 仍会注入 CB_STARTUP_NO_FOREGROUND=1，但本表中的同名键优先。

.PARAMETER Runs
    每个变体的启动次数，默认 7，最少 3。

.PARAMETER TimeoutSec
    单次启动等待窗口的最长秒数，默认 20。

.PARAMETER NoForeground
    默认 $true。启动子进程时临时设置 CB_STARTUP_NO_FOREGROUND=1。
    如需观察正常抢焦点的启动行为，可使用 -NoForeground:$false；
    前置窗口恢复安全网仍然生效。

.PARAMETER DryRun
    只校验参数/路径并打印计划启动顺序；不启动、不结束任何进程，也不写 CSV。

.PARAMETER Csv
    可选。追加写入的 CSV 路径；存在时追加表头和数据行，不会覆盖历史结果。
    列为 timestamp,variant,run,window_ms,ws_mb。

.PARAMETER KeepAlive
    可选。测量结束后保留最后一次成功启动的应用；默认结束后台进程。
    注意：下一次启动前仍会结束上一个实例。

.EXAMPLE
    # 同一个 exe，用环境变量做 A/B（显式 -EnvB 以启用 B；B 默认复用 A 的 exe）
    # 在 PowerShell 会话中直接调用，哈希表才会保持为对象
    ./scripts/measure-startup.ps1 -Runs 7 -EnvA @{CB_SYNC_SHELL='1'} -EnvB @{}

.EXAMPLE
    pwsh -NoProfile -File scripts/measure-startup.ps1 -Runs 7

.EXAMPLE
    pwsh -NoProfile -File scripts/measure-startup.ps1 `
      -ExeA '.\build\old\ClipBridgeShell_CS.exe' `
      -ExeB '.\build\new\ClipBridgeShell_CS.exe' `
      -Runs 9 -Csv '.\artifacts\startup.csv'

.EXAMPLE
    pwsh -NoProfile -File scripts/measure-startup.ps1 -Runs 7 -DryRun

.NOTES
    计时包含 Start-Process 返回前的时间；窗口标题/heuristic 只表示主窗口已创建且有标题，
    不保证 DWM 已完成首帧呈现。对比结果请优先看 per-pair delta 的中位数。
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0, HelpMessage = '变体 A 的 exe 路径（可选，默认仓库内 Release 外壳）')]
    [string]$ExeA,

    [Parameter(Position = 1, HelpMessage = '变体 B 的 exe 路径（可选）')]
    [string]$ExeB,

    [hashtable]$EnvA = @{},

    [hashtable]$EnvB = @{},

    [ValidateRange(3, [int]::MaxValue)]
    [int]$Runs = 7,

    [ValidateRange(1, 600)]
    [int]$TimeoutSec = 20,

    [bool]$NoForeground = $true,

    [switch]$DryRun,

    [string]$Csv,

    [switch]$KeepAlive
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:Invariant = [Globalization.CultureInfo]::InvariantCulture
$script:SettleMs = 600
$script:ForegroundNativeReady = $false
$script:RepoRoot = if ([string]::IsNullOrWhiteSpace($PSScriptRoot)) {
    (Get-Location).Path
}
else {
    Split-Path -Parent $PSScriptRoot
}
$script:DefaultExePath = Join-Path $script:RepoRoot 'platforms\windows\ClipBridgeShell_CS\ClipBridgeShell_CS\bin\x64\Release\net10.0-windows10.0.26100.0\win-x64\ClipBridgeShell_CS.exe'
$script:TargetProcessNames = @(
    'ClipBridgeShell_CS',
    'ClipBridgeShell_CS.Tests.MSTest',
    'testhost',
    'testhost.x86',
    'vstest.console'
)

function Format-Number {
    param(
        [Parameter(Mandatory = $true)][double]$Value,
        [string]$Format = 'F1'
    )
    return $Value.ToString($Format, $script:Invariant)
}

function Format-SignedNumber {
    param([Parameter(Mandatory = $true)][double]$Value)
    $text = Format-Number -Value $Value
    if ($Value -gt 0) { return "+$text" }
    return $text
}

function Resolve-MeasurementExe {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $resolved = $null
    try {
        $resolved = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).ProviderPath
    }
    catch {
        throw "变体 $Label 的 exe 不存在或无法解析：$Path"
    }

    $item = Get-Item -LiteralPath $resolved -ErrorAction Stop
    if ($item.PSIsContainer) { throw "变体 $Label 的路径是目录，不是 exe：$resolved" }
    if ($item.Extension -ine '.exe') { throw "变体 $Label 必须是 .exe：$resolved" }
    return $item.FullName
}

function Resolve-DefaultShellExe {
    if (-not (Test-Path -LiteralPath $script:DefaultExePath -PathType Leaf)) {
        throw "默认的 ClipBridge Release 外壳不存在：$script:DefaultExePath`n请显式传入 -ExeA，或先构建该 Release 配置。"
    }
    return Resolve-MeasurementExe -Path $script:DefaultExePath -Label 'A/default'
}

function Get-ValidatedEnvironment {
    param(
        [AllowNull()][hashtable]$Table,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $result = @{}
    if ($null -eq $Table) { return $result }

    foreach ($key in @($Table.Keys)) {
        if ($null -eq $key -or $key -isnot [string]) {
            throw "变体 $Label 的环境变量名必须是字符串。"
        }
        if ([string]::IsNullOrEmpty($key) -or $key -cnotmatch '^[A-Za-z_][A-Za-z0-9_]*$') {
            throw "变体 $Label 的环境变量名无效：'$key'（只允许字母、数字、下划线，且不能以数字开头）。"
        }

        $value = $Table[$key]
        if ($null -eq $value) {
            throw "变体 $Label 的环境变量 '$key' 的值不能为 null。"
        }
        if ($value -isnot [string]) {
            throw "变体 $Label 的环境变量 '$key' 的值必须是字符串。"
        }
        $result[$key] = $value
    }

    return $result
}

function Format-VariantEnvironment {
    param([hashtable]$Table)

    if ($null -eq $Table -or $Table.Count -eq 0) { return '(none)' }
    $parts = foreach ($key in @($Table.Keys | Sort-Object)) {
        '{0}={1}' -f $key, $Table[$key]
    }
    return ($parts -join ', ')
}

function Write-VariantDefinition {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$ExePath,
        [Parameter(Mandatory = $true)][hashtable]$VariantEnv
    )
    Write-Output ("Variant {0} : {1}  env: {2}" -f $Label, $ExePath, (Format-VariantEnvironment -Table $VariantEnv))
}

function Get-TargetProcesses {
    return @(Get-Process -Name $script:TargetProcessNames -ErrorAction SilentlyContinue)
}

function Stop-TargetProcesses {
    [CmdletBinding()]
    param([switch]$Quiet)

    $targets = @(Get-TargetProcesses)
    if ($targets.Count -eq 0) { return $true }

    foreach ($target in $targets) {
        try {
            Stop-Process -Id $target.Id -Force -ErrorAction Stop
            try { [void]$target.WaitForExit(2000) } catch { }
        }
        catch {
            # “拒绝访问”通常表示进程已经在退出过程中；此时继续等待即可，不应直接判失败。
            $denied = $_.Exception.Message -match '拒绝访问|Access is denied'
            if (-not $denied -and -not $Quiet) {
                throw "无法结束进程 $($target.ProcessName) (PID=$($target.Id))：$($_.Exception.Message)"
            }
        }
    }

    # 强制终止 WinUI 进程偶尔需要数秒（DWM/COM 拆除），并把"仍在退出中"的情况算作成功路径。
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    $reKilled = $false
    while ([DateTime]::UtcNow -lt $deadline) {
        if (@(Get-TargetProcesses).Count -eq 0) { return $true }
        if (-not $reKilled -and ([DateTime]::UtcNow -gt $deadline.AddSeconds(-7))) {
            # 中途补一刀，处理第一次 Stop-Process 丢失的进程
            $reKilled = $true
            foreach ($t in @(Get-TargetProcesses)) {
                try { Stop-Process -Id $t.Id -Force -ErrorAction Stop } catch { }
            }
        }
        Start-Sleep -Milliseconds 50
    }

    $remaining = @(Get-TargetProcesses)
    if ($remaining.Count -eq 0) { return $true }

    $details = ($remaining | ForEach-Object { "$($_.ProcessName)(PID=$($_.Id))" }) -join ', '
    $message = "清理失败：仍有目标进程存活：$details"
    if ($Quiet) {
        Write-Warning $message
        return $false
    }
    throw $message
}

function Get-Median {
    param([double[]]$Values)

    if ($null -eq $Values -or $Values.Count -eq 0) { return $null }
    $sorted = @($Values | Sort-Object)
    $count = $sorted.Count
    if (($count % 2) -eq 1) {
        return [double]$sorted[[int](($count - 1) / 2)]
    }
    $left = [double]$sorted[([int]($count / 2)) - 1]
    $right = [double]$sorted[[int]($count / 2)]
    return ($left + $right) / 2.0
}

function Get-VariantStats {
    param([object[]]$Rows)

    $valid = @($Rows | Where-Object { $_.WindowMs -ge 0 })
    $values = [double[]]@($valid | ForEach-Object { $_.WindowMs })

    $median = $null
    $min = $null
    $max = $null
    if ($values.Count -gt 0) {
        $median = Get-Median -Values $values
        $min = ($values | Measure-Object -Minimum).Minimum
        $max = ($values | Measure-Object -Maximum).Maximum
    }

    return [pscustomobject]@{
        Attempts     = $Rows.Count
        ValidCount   = $valid.Count
        FailureCount = $Rows.Count - $valid.Count
        Median       = $median
        Min          = $min
        Max          = $max
    }
}

function Initialize-ForegroundNative {
    if ($script:ForegroundNativeReady) { return }

    $existing = [System.Management.Automation.PSTypeName]::new('ClipBridgeStartupFocus').Type
    if ($null -eq $existing) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class ClipBridgeStartupFocus
{
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
}
'@ -ErrorAction Stop
    }

    $script:ForegroundNativeReady = $true
}

function Get-ForegroundWindowHandle {
    Initialize-ForegroundNative
    return [ClipBridgeStartupFocus]::GetForegroundWindow()
}

function Restore-ForegroundWindow {
    param([IntPtr]$Handle)

    if ($Handle -eq [IntPtr]::Zero) { return $true }

    try {
        if ([ClipBridgeStartupFocus]::GetForegroundWindow() -eq $Handle) { return $true }
        if (-not [ClipBridgeStartupFocus]::SetForegroundWindow($Handle)) {
            Write-Warning ("无法恢复前台窗口 (HWND=0x{0:X})；请手动切回原来的窗口。" -f $Handle.ToInt64())
            return $false
        }
        return $true
    }
    catch {
        Write-Warning ("恢复前台窗口失败 (HWND=0x{0:X})：{1}" -f $Handle.ToInt64(), $_.Exception.Message)
        return $false
    }
}

function Measure-OneLaunch {
    param(
        [Parameter(Mandatory = $true)][string]$ExePath,
        [Parameter(Mandatory = $true)][int]$TimeoutSec,
        [Parameter(Mandatory = $true)][bool]$NoForeground,
        [Parameter(Mandatory = $true)][hashtable]$VariantEnv
    )

    $timeoutMs = [double]$TimeoutSec * 1000.0
    $workingDirectory = Split-Path -Parent $ExePath

    # 合成本次子进程要用的环境：先放默认的 no-focus，再用变体表覆盖（变体优先）。
    $childEnvironment = @{}
    if ($NoForeground) {
        $childEnvironment['CB_STARTUP_NO_FOREGROUND'] = '1'
        # 光"不抢焦点"还不够：窗口仍会出现在屏幕上，连续测几十次会让用户看到反复闪烁。
        # CB_STARTUP_OFFSCREEN=1 把窗口显示到虚拟桌面之外（渲染照常，测量语义不变）。
        $childEnvironment['CB_STARTUP_OFFSCREEN'] = '1'
    }
    foreach ($key in $VariantEnv.Keys) {
        $childEnvironment[$key] = $VariantEnv[$key]
    }

    # 保存所有将被覆盖的父进程变量，Start-Process 返回后立刻恢复，绝不泄漏。
    $previousEnvironment = @{}
    foreach ($key in $childEnvironment.Keys) {
        $previousEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
    }

    # 必须在 Start-Process 之前启动，才能把进程创建时间算进去。
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    $process = $null
    try {
        foreach ($key in $childEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $childEnvironment[$key], 'Process')
        }

        $process = Start-Process -FilePath $ExePath -WorkingDirectory $workingDirectory -PassThru -ErrorAction Stop
    }
    catch {
        $stopwatch.Stop()
        return [pscustomobject]@{
            WindowMs  = -1.0
            WsMb      = $null
            Error     = "启动失败：$($_.Exception.Message)"
            ProcessId = $null
        }
    }
    finally {
        foreach ($key in $childEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $previousEnvironment[$key], 'Process')
        }
    }

    if ($null -eq $process) {
        $stopwatch.Stop()
        return [pscustomobject]@{
            WindowMs  = -1.0
            WsMb      = $null
            Error     = 'Start-Process 未返回进程对象'
            ProcessId = $null
        }
    }

    $processId = $process.Id
    $exited = $false
    $windowReady = $false

    try {
        while ($stopwatch.Elapsed.TotalMilliseconds -lt $timeoutMs) {
            Start-Sleep -Milliseconds 10

            try {
                $process.Refresh()
                $exited = $process.HasExited
            }
            catch {
                $exited = $true
            }

            if ($exited) { break }

            $handle = [IntPtr]::Zero
            $title = ''
            try {
                $handle = $process.MainWindowHandle
                $title = $process.MainWindowTitle
            }
            catch {
                $exited = $true
                break
            }

            if ($handle -ne [IntPtr]::Zero -and -not [string]::IsNullOrWhiteSpace($title)) {
                $windowReady = $true
                break
            }
        }
    }
    finally {
        $stopwatch.Stop()
    }

    $elapsedMs = $stopwatch.Elapsed.TotalMilliseconds

    if ($windowReady) {
        $workingSetMb = $null
        try {
            $process.Refresh()
            if (-not $process.HasExited) {
                $workingSetMb = [math]::Round($process.WorkingSet64 / 1MB, 1)
            }
        }
        catch {
            $workingSetMb = $null
        }

        return [pscustomobject]@{
            WindowMs  = [math]::Round($elapsedMs, 1)
            WsMb      = $workingSetMb
            Error     = $null
            ProcessId = $processId
        }
    }

    if ($exited) {
        $exitText = 'unknown'
        try {
            if ($process.HasExited) { $exitText = $process.ExitCode.ToString($script:Invariant) }
        }
        catch { }
        return [pscustomobject]@{
            WindowMs  = -1.0
            WsMb      = $null
            Error     = "进程在窗口就绪前退出（exit=$exitText）"
            ProcessId = $processId
        }
    }

    return [pscustomobject]@{
        WindowMs  = -1.0
        WsMb      = $null
        Error     = "超时 ${TimeoutSec}s 未检测到有标题的主窗口"
        ProcessId = $processId
    }
}

function Add-CsvRow {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Timestamp,
        [Parameter(Mandatory = $true)][string]$Variant,
        [Parameter(Mandatory = $true)][int]$Run,
        [Parameter(Mandatory = $true)][double]$WindowMs,
        [AllowNull()][object]$WorkingSetMb
    )

    $needsHeader = $false
    if (-not (Test-Path -LiteralPath $Path)) {
        $needsHeader = $true
    }
    elseif ((Get-Item -LiteralPath $Path -ErrorAction Stop).Length -eq 0) {
        $needsHeader = $true
    }

    if ($needsHeader) {
        Add-Content -LiteralPath $Path -Value 'timestamp,variant,run,window_ms,ws_mb' -Encoding utf8
    }

    $wsText = ''
    if ($null -ne $WorkingSetMb) {
        $wsText = ([double]$WorkingSetMb).ToString('F1', $script:Invariant)
    }

    $line = '{0},{1},{2},{3},{4}' -f `
        $Timestamp,
        $Variant,
        $Run,
        $WindowMs.ToString('F1', $script:Invariant),
        $wsText

    Add-Content -LiteralPath $Path -Value $line -Encoding utf8
}

function Write-VariantTable {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$ExePath,
        [Parameter(Mandatory = $true)][object[]]$Rows
    )

    Write-Output ''
    Write-Output ("=== variant {0}: {1} ===" -f $Label, [IO.Path]::GetFileName($ExePath))
    Write-Output ("{0,4}  {1,12}  {2,9}" -f 'run', 'window_ms', 'ws_mb')
    foreach ($row in $Rows) {
        if ($row.WindowMs -lt 0) {
            Write-Output ("{0,4}  {1,12}  {2,9}" -f $row.Run, '-1', '-')
            if (-not [string]::IsNullOrWhiteSpace($row.Error)) {
                Write-Output ("      ! {0}" -f $row.Error)
            }
        }
        else {
            $ws = if ($null -eq $row.WsMb) { '-' } else { Format-Number -Value $row.WsMb }
            Write-Output ("{0,4}  {1,12}  {2,9}" -f $row.Run, (Format-Number -Value $row.WindowMs), $ws)
        }
    }
}

function Get-RoughConfidence {
    param([double[]]$PairDeltas)

    if ($null -eq $PairDeltas -or $PairDeltas.Count -eq 0) {
        return 'n/a (没有完整的 A/B 配对)'
    }

    $count = $PairDeltas.Count
    $negative = @($PairDeltas | Where-Object { $_ -lt 0 }).Count
    $positive = @($PairDeltas | Where-Object { $_ -gt 0 }).Count
    $zero = $count - $negative - $positive

    if ($zero -eq $count) {
        return 'low (所有配对差值都是 0 ms，无法区分)'
    }

    $majority = [math]::Max($negative, $positive)
    $fraction = $majority / [double]$count

    if ($count -lt 3) {
        return "very low (只有 $count 对，样本太少)"
    }
    if (($negative -eq $count) -or ($positive -eq $count)) {
        if ($count -ge 5) {
            return "high (全部 $count 对同向，$majority/$count)"
        }
        return "moderate (全部 $count 对同向，但样本量小)"
    }
    if ($fraction -ge 0.8) {
        return "moderate ($majority/$count 对同向；存在反向样本)"
    }
    if ($fraction -ge 0.65) {
        return "low-moderate ($majority/$count 对同向；方向不稳定)"
    }
    return "low/inconclusive ($majority/$count 对同向；正负混杂)"
}

# ---------------------------------------------------------------------------
# 参数与路径准备
# ---------------------------------------------------------------------------
$envAValidated = Get-ValidatedEnvironment -Table $EnvA -Label 'A'
$envBValidated = Get-ValidatedEnvironment -Table $EnvB -Label 'B'

$exeBExplicit = -not [string]::IsNullOrWhiteSpace($ExeB)
$envBExplicit = $PSBoundParameters.ContainsKey('EnvB')
$hasVariantB = $exeBExplicit -or $envBExplicit

if ([string]::IsNullOrWhiteSpace($ExeA)) {
    $resolvedA = Resolve-DefaultShellExe
}
else {
    $resolvedA = Resolve-MeasurementExe -Path $ExeA -Label 'A'
}

$resolvedB = $null
if ($hasVariantB) {
    if ($exeBExplicit) {
        $resolvedB = Resolve-MeasurementExe -Path $ExeB -Label 'B'
    }
    else {
        # 同一个 exe 的环境变量对照：B 默认复用 A。
        $resolvedB = $resolvedA
    }
}

$csvPath = $null
$csvDirectory = $null
if (-not [string]::IsNullOrWhiteSpace($Csv)) {
    if ([IO.Path]::IsPathRooted($Csv)) {
        $csvPath = [IO.Path]::GetFullPath($Csv)
    }
    else {
        $csvPath = [IO.Path]::GetFullPath((Join-Path -Path (Get-Location).Path -ChildPath $Csv))
    }

    if (Test-Path -LiteralPath $csvPath -PathType Container) {
        throw "CSV 路径是目录，不是文件：$csvPath"
    }

    $csvDirectory = Split-Path -Parent $csvPath
    if (-not [string]::IsNullOrWhiteSpace($csvDirectory) -and
        (Test-Path -LiteralPath $csvDirectory -PathType Leaf)) {
        throw "CSV 的父路径是文件，无法创建目录：$csvDirectory"
    }
}

$variantEnvs = @{ A = $envAValidated }
if ($null -ne $resolvedB) { $variantEnvs['B'] = $envBValidated }

$variants = [ordered]@{ A = $resolvedA }
if ($null -ne $resolvedB) { $variants['B'] = $resolvedB }

# 变体显式设置 CB_STARTUP_NO_FOREGROUND 时，变体值优先于脚本默认值，只告警一次。
if ($NoForeground) {
    foreach ($label in $variants.Keys) {
        $overrideKey = @($variantEnvs[$label].Keys | Where-Object { $_ -ieq 'CB_STARTUP_NO_FOREGROUND' }) | Select-Object -First 1
        if ($null -ne $overrideKey) {
            Write-Warning ("Variant {0} env sets {1}={2}; the variant value overrides the harness default 1." -f `
                $label, $overrideKey, $variantEnvs[$label][$overrideKey])
        }
    }
}

# 计划顺序用于 DryRun 与实际执行，二者保持一致。
$plannedOrder = [System.Collections.Generic.List[string]]::new()
for ($run = 1; $run -le $Runs; $run++) {
    foreach ($label in $variants.Keys) { $plannedOrder.Add($label) }
}

if ($DryRun) {
    $mode = if ($null -ne $resolvedB) { "interleaved A/B ($Runs pairs)" } else { 'single-binary mode' }
    Write-Output 'ClipBridge startup measurement - DRY RUN (nothing will be started or killed)'
    foreach ($label in $variants.Keys) {
        Write-VariantDefinition -Label $label -ExePath $variants[$label] -VariantEnv $variantEnvs[$label]
    }
    Write-Output ("Runs         : {0} per variant | timeout: {1}s | settle: {2}ms" -f $Runs, $TimeoutSec, $script:SettleMs)
    Write-Output ("Plan         : {0}; {1} launches" -f $mode, $plannedOrder.Count)
    Write-Output ("NoForeground : {0} (CB_STARTUP_NO_FOREGROUND={1} for child only)" -f [bool]$NoForeground, $(if ($NoForeground) { '1' } else { '<unset>' }))
    Write-Output 'Focus        : capture foreground before each launch; restore after cleanup (warnings only)'
    Write-Output ("KeepAlive    : {0}" -f [bool]$KeepAlive)
    if ($null -ne $csvPath) {
        $csvState = if (Test-Path -LiteralPath $csvDirectory) { 'parent exists' } else { 'parent will be created' }
        Write-Output ("CSV          : {0} ({1}; append one row per launch)" -f $csvPath, $csvState)
    }
    Write-Output ("Order        : {0}" -f ($plannedOrder -join ','))
    Write-Output 'Pairs        :'
    for ($run = 1; $run -le $Runs; $run++) {
        if ($null -ne $resolvedB) {
            Write-Output ("  pair {0}: A -> B" -f $run)
        }
        else {
            Write-Output ("  run  {0}: A" -f $run)
        }
    }
    Write-Output ''
    Write-Output ("DryRun OK: validated {0} executable path(s), {1} planned launches in {2}; nothing was started or killed." -f $variants.Count, $plannedOrder.Count, $mode)
    exit 0
}

if ($null -ne $csvDirectory -and -not (Test-Path -LiteralPath $csvDirectory)) {
    New-Item -ItemType Directory -Path $csvDirectory -Force | Out-Null
}

$results = @{}
foreach ($label in $variants.Keys) {
    $results[$label] = [System.Collections.Generic.List[object]]::new()
}

$keepProcessId = $null
$totalFailures = 0
$fatalError = $null
$cleanupFailed = $false
$exitCode = 0
$verdict = $null

try {
    Write-Output 'ClipBridge startup measurement (external, interleaved A/B)'
    foreach ($label in $variants.Keys) {
        Write-VariantDefinition -Label $label -ExePath $variants[$label] -VariantEnv $variantEnvs[$label]
    }
    Write-Output ("Runs        : {0} per variant | timeout: {1}s | settle: {2}ms" -f $Runs, $TimeoutSec, $script:SettleMs)
    Write-Output ("Order       : {0}" -f ($plannedOrder -join ','))
    Write-Output ("NoForeground: {0} (CB_STARTUP_NO_FOREGROUND={1} for child only)" -f [bool]$NoForeground, $(if ($NoForeground) { '1' } else { '<unset>' }))
    Write-Output ("KeepAlive   : {0}" -f [bool]$KeepAlive)
    if ($null -ne $csvPath) { Write-Output ("CSV         : {0}" -f $csvPath) }
    Write-Output 'Measure     : start process -> poll 10ms for MainWindowHandle != 0 and non-empty MainWindowTitle'

    Initialize-ForegroundNative

    # 交错执行：A,B,A,B...，而不是把所有 A 跑完再跑 B。
    for ($run = 1; $run -le $Runs; $run++) {
        foreach ($label in $variants.Keys) {
            # 单实例保护：每次启动前彻底清掉旧实例/测试宿主，并验证确实清掉了。
            $null = Stop-TargetProcesses
            Start-Sleep -Milliseconds $script:SettleMs

            $foregroundHandle = Get-ForegroundWindowHandle
            $measurement = $null
            $completed = $false

            try {
                $timestamp = [DateTimeOffset]::Now.ToString('o', $script:Invariant)
                $measurement = Measure-OneLaunch -ExePath $variants[$label] -TimeoutSec $TimeoutSec -NoForeground ([bool]$NoForeground) -VariantEnv $variantEnvs[$label]

                $row = [pscustomobject]@{
                    Run       = $run
                    Variant   = $label
                    WindowMs  = [double]$measurement.WindowMs
                    WsMb      = $measurement.WsMb
                    Error     = $measurement.Error
                    ProcessId = $measurement.ProcessId
                    Timestamp = $timestamp
                }
                $results[$label].Add($row)

                if ($measurement.WindowMs -ge 0) {
                    $keepProcessId = $measurement.ProcessId
                }
                else {
                    $keepProcessId = $null
                }

                if ($null -ne $csvPath) {
                    Add-CsvRow -Path $csvPath -Timestamp $timestamp -Variant $label -Run $run `
                        -WindowMs $measurement.WindowMs -WorkingSetMb $measurement.WsMb
                }
                $completed = $true
            }
            finally {
                try {
                    # -KeepAlive 时只保留最后一次成功启动；其余每次测量后立刻清理。
                    $leaveForKeepAlive = $KeepAlive -and $completed -and $null -ne $measurement -and $measurement.WindowMs -ge 0
                    if (-not $leaveForKeepAlive) {
                        $null = Stop-TargetProcesses
                    }
                }
                finally {
                    # 无论启动/清理是否成功，都把前台窗口还给用户。
                    $null = Restore-ForegroundWindow -Handle $foregroundHandle
                }
            }
        }
    }

    foreach ($label in $variants.Keys) {
        Write-VariantTable -Label $label -ExePath $variants[$label] -Rows @($results[$label])
    }

    $stats = @{}
    foreach ($label in $variants.Keys) {
        $stats[$label] = Get-VariantStats -Rows @($results[$label])
        $totalFailures += $stats[$label].FailureCount
    }

    Write-Output ''
    Write-Output '=== summary ==='
    foreach ($label in $variants.Keys) {
        Write-VariantDefinition -Label $label -ExePath $variants[$label] -VariantEnv $variantEnvs[$label]
    }
    foreach ($label in $variants.Keys) {
        $s = $stats[$label]
        $prefix = if ($s.FailureCount -eq 0) {
            "variant $label (n=$($s.Attempts))"
        }
        else {
            "variant $label (n=$($s.ValidCount) valid / $($s.Attempts) attempts, failed=$($s.FailureCount))"
        }

        if ($s.ValidCount -eq 0) {
            Write-Output ("{0}: median=n/a, min=n/a, max=n/a" -f $prefix)
        }
        else {
            Write-Output ("{0}: median={1} ms, min={2} ms, max={3} ms" -f `
                $prefix,
                (Format-Number -Value $s.Median),
                (Format-Number -Value $s.Min),
                (Format-Number -Value $s.Max))
        }
    }

    $pairTexts = [System.Collections.Generic.List[string]]::new()
    $pairDeltas = [System.Collections.Generic.List[double]]::new()

    if ($null -ne $resolvedB) {
        for ($run = 1; $run -le $Runs; $run++) {
            $a = @($results['A'] | Where-Object { $_.Run -eq $run }) | Select-Object -First 1
            $b = @($results['B'] | Where-Object { $_.Run -eq $run }) | Select-Object -First 1

            if ($null -ne $a -and $null -ne $b -and $a.WindowMs -ge 0 -and $b.WindowMs -ge 0) {
                $delta = [double]$b.WindowMs - [double]$a.WindowMs
                $pairDeltas.Add($delta)
                $pairTexts.Add(("{0}:{1}" -f $run, (Format-SignedNumber -Value $delta)))
            }
            else {
                $reason = if ($null -eq $a -or $a.WindowMs -lt 0) { 'A failed' } else { 'B failed' }
                $pairTexts.Add(("{0}:n/a($reason)" -f $run))
            }
        }

        if ($pairDeltas.Count -gt 0) {
            $pairMedian = Get-Median -Values ([double[]]@($pairDeltas))
            $confidence = Get-RoughConfidence -PairDeltas ([double[]]@($pairDeltas))
            $medianOfMedians = [double]$stats['B'].Median - [double]$stats['A'].Median

            Write-Output ("median delta (B-A) = {0} ms (median of per-pair deltas; negative = B faster)" -f `
                (Format-Number -Value $pairMedian))
            Write-Output ("median-of-medians delta (B-A) = {0} ms (median[B] - median[A])" -f `
                (Format-Number -Value $medianOfMedians))
            Write-Output ("interleaved pairs: {0}" -f ($pairTexts -join ', '))
            Write-Output ("rough confidence: {0}" -f $confidence)

            $verdict = "VERDICT: PASS - $($pairDeltas.Count) complete interleaved pair(s); median pairwise B-A = $(Format-Number -Value $pairMedian) ms (negative = B faster); confidence: $confidence."
        }
        else {
            Write-Output 'median delta (B-A) = n/a (没有完整的 A/B 配对)'
            Write-Output ("interleaved pairs: {0}" -f ($pairTexts -join ', '))
            Write-Output 'rough confidence: n/a (没有完整的 A/B 配对)'
        }
    }
    else {
        $medianText = if ($stats['A'].ValidCount -gt 0) { Format-Number -Value $stats['A'].Median } else { 'n/a' }
        Write-Output 'median delta (B-A) = n/a (single-binary mode)'
        Write-Output 'interleaved pairs: n/a (single-binary mode)'
        $verdict = "VERDICT: PASS - variant A measured with $($stats['A'].ValidCount)/$($stats['A'].Attempts) valid runs; median = $medianText ms."
    }

    Write-Output ("failures: A={0}, B={1} (total={2})" -f `
        $stats['A'].FailureCount,
        $(if ($null -ne $resolvedB) { $stats['B'].FailureCount } else { 0 }),
        $totalFailures)

    if ($totalFailures -gt 0) {
        $verdict = "VERDICT: FAIL - $totalFailures launch failure(s); numbers are incomplete and must not be used as a final comparison."
        $exitCode = 1
    }
}
catch {
    $fatalError = $_
    $exitCode = 1
    $verdict = "VERDICT: FAIL - harness error: $($_.Exception.Message)"
}
finally {
    $shouldKeep = $KeepAlive -and ($null -eq $fatalError) -and ($null -ne $keepProcessId)

    if ($shouldKeep) {
        $live = Get-Process -Id $keepProcessId -ErrorAction SilentlyContinue
        if ($null -ne $live) {
            Write-Output ("keepalive: leaving PID={0} running (requested by -KeepAlive)" -f $keepProcessId)
        }
        else {
            $shouldKeep = $false
        }
    }

    if (-not $shouldKeep) {
        $cleanResult = Stop-TargetProcesses -Quiet
        if (-not $cleanResult) {
            $cleanupFailed = $true
            $exitCode = 1
        }
        else {
            Write-Output 'cleanup: no ClipBridgeShell_CS / test host processes remain.'
        }
    }
}

if ($null -ne $fatalError) {
    Write-Output ''
    Write-Output ("FATAL: {0}" -f $fatalError.Exception.Message)
    Write-Output $verdict
    exit 1
}

if ($cleanupFailed) {
    Write-Output 'VERDICT: FAIL - target process cleanup did not complete; fix the leftover process before trusting any numbers.'
    exit 1
}

Write-Output ''
Write-Output $verdict
exit $exitCode
