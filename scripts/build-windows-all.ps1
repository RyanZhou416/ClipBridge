<#
.SYNOPSIS
    构建全部 Windows 安装器渠道（machine MSI + user EXE + 便携 ZIP）。

.DESCRIPTION
    build-windows-installers.ps1 -Only All 的薄封装：
      - machine MSI（PerMachine，需要管理员实机安装验证）
      - per-user MSI（内部 bundle 载荷，不单独发布）+ Burn EXE（免 UAC，ARP 隐藏）
      - 便携 ZIP（ClipBridge.portable）
      - 全部产物表断言 + package-summary.json
    只产出 machine MSI / user EXE / 便携 ZIP 三个公开发布格式；内部 user MSI 只作为
    EXE 的依赖并在 summary 中标记 internal。MSIX/winget 不在本入口内，不自动生成清单。

.PARAMETER Platform
    x64（当前唯一支持）。arm64 参数只为兼容旧调用而保留，会在构建前明确失败。

.PARAMETER OutDir
    产物输出目录，默认 dist（相对仓库根目录）。

.PARAMETER WixInstallPath
    WiX 3.14 目录（含 candle/light/heat/WixTasks.dll）。不传则探测标准安装位置。

.PARAMETER SkipRust
    跳过 cargo 构建（要求 target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll 已存在）。

.PARAMETER KeepStage
    保留 dist/stage（默认构建结束后删除）。

.EXAMPLE
    pwsh scripts/build-windows-all.ps1 -WixInstallPath $env:TEMP\wix314

.EXAMPLE
    pwsh scripts/build-windows-all.ps1 -SkipRust -KeepStage
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
    Only     = 'All'
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
            "ClipBridge-$version-win-$Platform-machine.msi",
            "ClipBridge-$version-win-$Platform-user.exe",
            "ClipBridge-$version-win-$Platform-user.msi",
            "ClipBridge-$version-win-$Platform-portable.zip"
        )) {
        $artifact = Join-Path $outRoot $name
        if (Test-Path -LiteralPath $artifact) {
            $f = Get-Item -LiteralPath $artifact
            $tag = if ($name -match '-user\.msi$') { '  [internal]' } else { '' }
            Write-Host ("  {0}{1}" -f $f.FullName, $tag)
            Write-Host ("  {0:N1} MB" -f ($f.Length / 1MB))
        } else {
            Write-Warning "未找到预期产物：$artifact"
        }
    }
    $summaryPath = Join-Path $outRoot 'package-summary.json'
    if (Test-Path -LiteralPath $summaryPath) {
        Write-Host ("  {0}  （机器可读签名/大小/SHA256，供 CI/release 复用）" -f $summaryPath)
    }

    Write-Host ''
    Write-Host '下一步：'
    Write-Host '  1. 生产发布请用主脚本 -SigningMode Certificate（本 All 入口默认不签名）。'
    Write-Host '  2. 公开发布资产只取 machine MSI / user EXE / portable ZIP；内部 user MSI 不公开。'
    Write-Host '  3. winget 清单仍是本地人工渠道流程，不在 GitHub 发布自动化内。'
}

$sw.Stop()
Write-Host ("耗时: {0:N1}s   exit code: {1}" -f $sw.Elapsed.TotalSeconds, $exitCode)
exit $exitCode
