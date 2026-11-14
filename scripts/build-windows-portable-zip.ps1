<#
.SYNOPSIS
    构建 Windows 便携 ZIP（只构建 portable 渠道）。

.DESCRIPTION
    build-windows-installers.ps1 -Only PortableZip 的薄封装：
      - 构建 Rust core-ffi（可用 -SkipRust 跳过）
      - dotnet publish 到 dist/stage（去 pdb/xml，不经过 WiX）
      - 打包 dist/ClipBridge-<ver>-win-<arch>-portable.zip：
        stage 全部内容 + 空标记 ClipBridge.portable + version.txt，zip 内顶层目录
        与产物同名，解压即用。
      - 断言 marker、core_ffi_windows.dll、无 pdb、zip 顶层目录正确。
    不构建 MSI 或 Burn bundle。便携渠道不需要 WiX；-WixInstallPath 会被忽略。

.PARAMETER Platform
    x64（默认）或 arm64。

.PARAMETER OutDir
    产物输出目录，默认 dist（相对仓库根目录）。

.PARAMETER WixInstallPath
    便携渠道不使用 WiX。为与其他渠道脚本保持相同参数而保留，传入会被忽略。

.PARAMETER SkipRust
    跳过 cargo 构建（要求 target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll 已存在）。

.PARAMETER KeepStage
    保留 dist/stage（默认构建结束后删除）。

.EXAMPLE
    pwsh scripts/build-windows-portable-zip.ps1

.EXAMPLE
    pwsh scripts/build-windows-portable-zip.ps1 -SkipRust -OutDir dist
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
    Only     = 'PortableZip'
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
    $artifact = Join-Path $outRoot "ClipBridge-$version-win-$Platform-portable.zip"
    if (Test-Path -LiteralPath $artifact) {
        $f = Get-Item -LiteralPath $artifact
        Write-Host ("  {0}" -f $f.FullName)
        Write-Host ("  {0:N1} MB" -f ($f.Length / 1MB))
    } else {
        Write-Warning "未找到预期产物：$artifact"
    }
}

$sw.Stop()
Write-Host ("耗时: {0:N1}s   exit code: {1}" -f $sw.Elapsed.TotalSeconds, $exitCode)
exit $exitCode
