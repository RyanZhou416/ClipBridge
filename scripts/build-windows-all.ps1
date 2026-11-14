<#
.SYNOPSIS
    构建全部 Windows 安装器渠道（machine MSI + user EXE + 便携 ZIP）。

.DESCRIPTION
    build-windows-installers.ps1 -Only All 的薄封装：
      - machine MSI（PerMachine，需要管理员实机安装验证）
      - per-user MSI + Burn EXE（免 UAC，ARP 隐藏）
      - 便携 ZIP（ClipBridge.portable）
      - 全部结构断言
    不自动生成 winget 清单：清单里固化产物 SHA256，签名前生成会在后续重构建/签名后失效。
    发布 Release（上传产物）之后，再单独运行：
      pwsh scripts/build-windows-winget.ps1

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
            Write-Host ("  {0}" -f $f.FullName)
            Write-Host ("  {0:N1} MB" -f ($f.Length / 1MB))
        } else {
            Write-Warning "未找到预期产物：$artifact"
        }
    }

    Write-Host ''
    Write-Host '下一步：'
    Write-Host '  1. 签名（若已配置证书）。'
    Write-Host '  2. 发布 GitHub Release 并上传上面 4 个产物。'
    Write-Host '  3. 生成 winget 清单：pwsh scripts/build-windows-winget.ps1'
    Write-Host '说明：本脚本不自动生成 winget 清单，避免签名前固化产物 SHA256。'
}

$sw.Stop()
Write-Host ("耗时: {0:N1}s   exit code: {1}" -f $sw.Elapsed.TotalSeconds, $exitCode)
exit $exitCode
