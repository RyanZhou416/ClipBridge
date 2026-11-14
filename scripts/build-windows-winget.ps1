<#
.SYNOPSIS
    从 dist 里已构建的产物生成 winget 三文件清单（不构建安装器）。

.DESCRIPTION
    build-winget-manifest.ps1 的薄封装：
      - 不构建、不签名任何安装器
      - 要求 dist 已同时存在：
          ClipBridge-<ver>-win-x64-machine.msi
          ClipBridge-<ver>-win-x64-user.exe
        缺失时直接给出明确错误和构建提示（exit 1）。
      - 调用 build-winget-manifest.ps1 生成：
          dist/winget/Ryan416.ClipBridge.yaml
          dist/winget/Ryan416.ClipBridge.installer.yaml
          dist/winget/Ryan416.ClipBridge.locale.en-US.yaml
      - winget CLI 可用时额外执行 winget validate（对完整清单目录）；不可用则跳过。
    清单里的 InstallerUrl 指向 GitHub Release 资产，真实 winget 安装前必须先发布 Release
    并上传上面两个产物，否则 URL 会 404。

    前置：先运行 pwsh scripts/build-windows-all.ps1（或分别构建 machine MSI 与 user EXE）。

.PARAMETER Version
    覆盖 Version.props 派生版本（默认使用 scripts/version.ps1 的结果）。
    必须与 dist 里产物的文件名版本一致。

.PARAMETER ReleaseUrlBase
    GitHub Release 资产目录前缀，默认
    https://github.com/RyanZhou416/ClipBridge/releases/download

.PARAMETER ReleaseTag
    GitHub Release tag，拼成 <ReleaseUrlBase>/<ReleaseTag>/<asset>。
    默认 clipbridge-windows-<Version>；传空字符串则直接拼在 base 下。

.PARAMETER OutDir
    清单输出目录，默认 dist/winget（相对仓库根目录）。

.EXAMPLE
    pwsh scripts/build-windows-winget.ps1

.EXAMPLE
    pwsh scripts/build-windows-winget.ps1 -Version 1.0.0 -ReleaseTag clipbridge-windows-1.0.0

.EXAMPLE
    pwsh scripts/build-windows-winget.ps1 -OutDir dist/winget-preview
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$ReleaseUrlBase,
    [string]$ReleaseTag,
    [string]$OutDir = 'dist/winget'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifestScript = Join-Path $PSScriptRoot 'build-winget-manifest.ps1'
$sw = [System.Diagnostics.Stopwatch]::StartNew()

if ($Version -and $Version -notmatch '^\d+\.\d+\.\d+$') {
    Write-Host "错误：-Version 必须是 3 段 semver（如 1.0.0），实际: $Version" -ForegroundColor Red
    exit 1
}

$derivedVersion = [string]((& (Join-Path $PSScriptRoot 'version.ps1') -Json -WarningAction SilentlyContinue | ConvertFrom-Json).SemVer)
if (-not $derivedVersion) {
    Write-Host '错误：version.ps1 没返回 SemVer。' -ForegroundColor Red
    exit 1
}
$effectiveVersion = if ($Version) { $Version } else { $derivedVersion }

# winget 清单脚本固定按 x64 产物生成（架构字段也是 x64）。
$distDir = Join-Path $repoRoot 'dist'
$machineMsi = Join-Path $distDir "ClipBridge-$effectiveVersion-win-x64-machine.msi"
$userExe = Join-Path $distDir "ClipBridge-$effectiveVersion-win-x64-user.exe"
$missing = @($machineMsi, $userExe) | Where-Object { -not (Test-Path -LiteralPath $_) }

if ($missing.Count -gt 0) {
    Write-Host ''
    Write-Host '错误：dist 里缺少 winget 清单需要的安装器产物：' -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" }
    Write-Host ''
    Write-Host '请先构建安装器产物（dist 必须同时有 machine MSI 与 per-user EXE）：'
    Write-Host '  pwsh scripts/build-windows-installers.ps1 -Only All -WixInstallPath <WiX 3.14 目录>'
    Write-Host '或分渠道构建：'
    Write-Host '  pwsh scripts/build-windows-machine-msi.ps1 -WixInstallPath <WiX 3.14 目录>'
    Write-Host '  pwsh scripts/build-windows-user-exe.ps1 -WixInstallPath <WiX 3.14 目录>'
    Write-Host ''
    Write-Host "（期望版本: $effectiveVersion；若产物版本不同，用 -Version 指定或重新构建。）"
    exit 1
}

$innerArgs = @{ OutDir = $OutDir }
if ($PSBoundParameters.ContainsKey('Version')) { $innerArgs.Version = $Version }
if ($PSBoundParameters.ContainsKey('ReleaseUrlBase')) { $innerArgs.ReleaseUrlBase = $ReleaseUrlBase }
if ($PSBoundParameters.ContainsKey('ReleaseTag')) { $innerArgs.ReleaseTag = $ReleaseTag }

$LASTEXITCODE = 0
$exitCode = 0
try {
    & $manifestScript @innerArgs
    if ($LASTEXITCODE -ne 0) { $exitCode = [int]$LASTEXITCODE }

    if ($exitCode -eq 0) {
        $manifestOut = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repoRoot $OutDir }
        $winget = Get-Command winget -ErrorAction SilentlyContinue
        if ($winget) {
            Write-Host ''
            Write-Host "winget validate $manifestOut"
            & winget validate $manifestOut
            if ($LASTEXITCODE -ne 0) { throw "winget validate 失败（exit $LASTEXITCODE）" }
        } else {
            Write-Warning '未找到 winget CLI，跳过 winget validate（清单结构自检已通过）。'
        }

        Write-Host ''
        Write-Host '========== winget 清单产物 =========='
        Get-ChildItem -LiteralPath $manifestOut -Filter '*.yaml' -File | Sort-Object Name | ForEach-Object {
            Write-Host ("  {0}" -f $_.FullName)
            Write-Host ("  {0,7} bytes" -f $_.Length)
        }
        Write-Host ''
        Write-Host '提示：真实 winget 安装前，需先发布 Release 并上传 machine MSI 与 user EXE；'
        Write-Host '      否则清单里的 InstallerUrl 会 404。'
    }
} catch {
    Write-Error $_
    $exitCode = if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { [int]$LASTEXITCODE } else { 1 }
}

$sw.Stop()
Write-Host ("耗时: {0:N1}s   exit code: {1}" -f $sw.Elapsed.TotalSeconds, $exitCode)
exit $exitCode
