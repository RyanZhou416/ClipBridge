<#
.SYNOPSIS
    构建 Windows per-user Burn EXE（只构建 user 渠道）。

.DESCRIPTION
    build-windows-installers.ps1 -Only UserExe 的薄封装：
      - 构建 Rust core-ffi（可用 -SkipRust 跳过）
      - dotnet publish + heat + per-user MSI + Burn bundle（ClipBridge-<ver>-win-<arch>-user.exe）
      - 断言 ALLUSERS=2 / MSIINSTALLPERUSER=1 / ARP 隐藏 / WordCount bit3=1 / bundle 版本 / 无 pdb
    不构建 machine MSI 或便携 ZIP。
    dist 里会同时输出内部 user MSI（bundle 的载荷）和最终 user EXE。

    前置：WiX Toolset v3.14（winget install WiXToolset.WiXToolset），
    或用 -WixInstallPath 指定解压目录。

.PARAMETER Platform
    x64（默认）或 arm64。

.PARAMETER OutDir
    产物输出目录，默认 dist（相对仓库根目录）。

.PARAMETER WixInstallPath
    WiX 3.14 目录（含 candle/light/heat/WixTasks.dll）。不传则探测标准安装位置。

.PARAMETER SkipRust
    跳过 cargo 构建（要求 target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll 已存在）。

.PARAMETER KeepStage
    保留 dist/stage（默认构建结束后删除）。

.EXAMPLE
    pwsh scripts/build-windows-user-exe.ps1 -WixInstallPath $env:TEMP\wix314

.EXAMPLE
    pwsh scripts/build-windows-user-exe.ps1 -SkipRust -KeepStage
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Platform = 'x64',
    [string]$OutDir = 'dist',
    [string]$WixInstallPath,
    [switch]$SkipRust,
    [switch]$KeepStage
)

$ErrorActionPreference = 'Stop'
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$inner = Join-Path $PSScriptRoot 'build-windows-installers.ps1'

$innerArgs = @{
    Only     = 'UserExe'
    Platform = $Platform
    OutDir   = $OutDir
}
if ($PSBoundParameters.ContainsKey('WixInstallPath')) { $innerArgs.WixInstallPath = $WixInstallPath }
if ($SkipRust) { $innerArgs.SkipRust = $true }
if ($KeepStage) { $innerArgs.KeepStage = $true }

$LASTEXITCODE = 0
$exitCode = 0
try {
    & $inner @innerArgs
    if ($LASTEXITCODE -ne 0) { $exitCode = [int]$LASTEXITCODE }
} catch {
    Write-Error $_
    $exitCode = if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { [int]$LASTEXITCODE } else { 1 }
}

if ($exitCode -eq 0) {
    $repoRoot = Split-Path -Parent $PSScriptRoot
    $versionJson = & (Join-Path $PSScriptRoot 'version.ps1') -Json -WarningAction SilentlyContinue | ConvertFrom-Json
    $version = [string]$versionJson.MsiVersion
    $outRoot = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repoRoot $OutDir }

    Write-Host ''
    Write-Host '========== 渠道产物 =========='
    foreach ($name in @(
            "ClipBridge-$version-win-$Platform-user.exe",
            "ClipBridge-$version-win-$Platform-user.msi"   # bundle 的内部 MSI
        )) {
        $artifact = Join-Path $outRoot $name
        if (Test-Path -LiteralPath $artifact) {
            $f = Get-Item -LiteralPath $artifact
            Write-Host ("  {0}" -f $f.FullName)
            Write-Host ("  {0:N1} MB" -f ($f.Length / 1MB))
        } else {
            Write-Warning "未找到预期产物：$artifact"
        }
    }
}

$sw.Stop()
Write-Host ("耗时: {0:N1}s   exit code: {1}" -f $sw.Elapsed.TotalSeconds, $exitCode)
exit $exitCode
