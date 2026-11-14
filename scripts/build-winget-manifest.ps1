<#
.SYNOPSIS
    生成 winget 多文件清单三件套（version / installer / defaultLocale）。

.DESCRIPTION
    输入是 dist/ 下已经构建好的两个 winget 交付物：
      - ClipBridge-<ver>-win-x64-machine.msi  （WiX MSI，per-machine）
      - ClipBridge-<ver>-win-x64-user.exe     （WiX Burn EXE，per-user）

    版本唯一真值仍然是 platforms/windows/ClipBridgeShell_CS/Version.props，
    通过 scripts/version.ps1 -Json 派生；不在本脚本里重复解析版本文件。

    输出（默认 dist/winget/）：
      - Ryan416.ClipBridge.yaml                   (ManifestType: version)
      - Ryan416.ClipBridge.installer.yaml         (ManifestType: installer)
      - Ryan416.ClipBridge.locale.en-US.yaml      (ManifestType: defaultLocale)

    清单里所有 URL / 文件名 / SHA256 都指向上面的真实产物；脚本末尾会做一次
    结构自检（必需字段、URL 文件名、SHA256 与磁盘文件一致），不依赖 winget CLI。

    已知限制（winget 1.29.x）：
      - winget validate 必须对完整目录执行；对单个 yaml 会报 multi file manifest is incomplete。
      - Burn 不需要显式 InstallerSwitches.Uninstall：winget 默认卸载即 /uninstall /quiet，schema 1.6.0 也没有该字段。

.PARAMETER Version
    覆盖 Version.props 派生出的版本（默认不传，使用 version.ps1 的结果）。
    仅用于本地试验；正式生成必须与安装器文件名一致。

.PARAMETER ReleaseUrlBase
    GitHub Release 资产目录前缀，默认
    https://github.com/RyanZhou416/ClipBridge/releases/download

.PARAMETER ReleaseTag
    GitHub Release 的 tag，拼成 <ReleaseUrlBase>/<ReleaseTag>/<asset>。
    默认 v<Version>（本仓库 app 发布尚未定 tag 约定时的通用值）。
    若你的 tag 不带 v / 带组件前缀，显式传入即可；传空字符串则直接拼在 base 下。

.PARAMETER OutDir
    清单输出目录，默认 dist/winget（相对仓库根目录）。

.EXAMPLE
    pwsh scripts/build-winget-manifest.ps1

.EXAMPLE
    pwsh scripts/build-winget-manifest.ps1 -Version 0.2.0 -ReleaseTag platforms/windows/ClipBridgeShell_CS-0.2.0
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$ReleaseUrlBase = 'https://github.com/RyanZhou416/ClipBridge/releases/download',
    [string]$ReleaseTag,
    [string]$OutDir = 'dist/winget'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot

# ---------- 版本 ----------
$versionJson = & (Join-Path $PSScriptRoot 'version.ps1') -Json | ConvertFrom-Json
$derivedVersion = [string]$versionJson.SemVer
if (-not $derivedVersion) { throw 'version.ps1 没返回 SemVer' }
if ($Version) {
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "-Version 必须是 3 段 semver，实际: $Version" }
    if ($Version -ne $derivedVersion) {
        Write-Warning "显式 -Version $Version 与 Version.props 的 $derivedVersion 不一致；按 $Version 生成清单。"
    }
} else {
    $Version = $derivedVersion
}

# ---------- tag / URL 前缀 ----------
if (-not $PSBoundParameters.ContainsKey('ReleaseTag')) { $ReleaseTag = "clipbridge-windows-$Version" }
$base = $ReleaseUrlBase.TrimEnd('/')
$urlRoot = if ($ReleaseTag) { "$base/$($ReleaseTag.Trim('/'))" } else { $base }

$packageIdentifier = 'Ryan416.ClipBridge'
$publisher = 'Ryan416'
$displayName = 'ClipBridge'
$manifestVersion = '1.6.0'   # winget 1.29 已验证可用
$repoUrl = 'https://github.com/RyanZhou416/ClipBridge'
$licenseUrl = "$repoUrl/blob/main/LICENSE"
$issuesUrl = "$repoUrl/issues"

$machineUpgradeCode = '{A1B2C3D4-E5F6-7890-ABCD-EF1234567891}'

# ---------- 产物 ----------
$distDir = Join-Path $repoRoot 'dist'
$machineName = "ClipBridge-$Version-win-x64-machine.msi"
$userName    = "ClipBridge-$Version-win-x64-user.exe"
$machineMsi  = Join-Path $distDir $machineName
$userExe     = Join-Path $distDir $userName

$missing = @($machineMsi, $userExe) | Where-Object { -not (Test-Path -LiteralPath $_) }
if ($missing) {
    throw ("找不到安装器产物，请先跑 scripts/build-windows-installers.ps1：`n  " + ($missing -join "`n  "))
}

# 统一大写，避免同一清单里大小写混用。
$machineSha = (Get-FileHash -LiteralPath $machineMsi -Algorithm SHA256).Hash.ToUpperInvariant()
$userSha    = (Get-FileHash -LiteralPath $userExe -Algorithm SHA256).Hash.ToUpperInvariant()
$machineUrl = "$urlRoot/$machineName"
$userUrl    = "$urlRoot/$userName"

# ---------- 输出目录 ----------
if (-not [System.IO.Path]::IsPathRooted($OutDir)) { $OutDir = Join-Path $repoRoot $OutDir }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$versionFile  = Join-Path $OutDir "$packageIdentifier.yaml"
$installerFile = Join-Path $OutDir "$packageIdentifier.installer.yaml"
$localeFile   = Join-Path $OutDir "$packageIdentifier.locale.en-US.yaml"

function Write-YamlFile {
    param([string]$Path, [string]$Content)
    $text = $Content.TrimEnd("`r", "`n") + "`n"
    # 无 BOM 的 UTF-8：winget 对 BOM 不敏感，但提交到 winget-pkgs 的清单通常不带。
    [System.IO.File]::WriteAllText($Path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

# ---------- version 清单 ----------
$versionYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.$manifestVersion.schema.json
PackageIdentifier: $packageIdentifier
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $manifestVersion
"@

# ---------- installer 清单 ----------
$installerYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.$manifestVersion.schema.json
PackageIdentifier: $packageIdentifier
PackageVersion: $Version
Platform:
  - Windows.Desktop
MinimumOSVersion: 10.0.0.0
InstallModes:
  - interactive
  - silent
  - silentWithProgress
Installers:
  # per-machine WiX MSI：安装到 Program Files，需要 UAC 提权。
  - Architecture: x64
    InstallerType: msi
    Scope: machine
    ElevationRequirement: elevatesSelf
    InstallerUrl: $machineUrl
    InstallerSha256: $machineSha
    InstallerSwitches:
      Silent: /qn
      SilentWithProgress: /qb
    AppsAndFeaturesEntries:
      - DisplayName: $displayName
        Publisher: $publisher
        UpgradeCode: "$machineUpgradeCode"
  # per-user Burn EXE：安装到 %LOCALAPPDATA%\Programs，禁止提权。
  - Architecture: x64
    InstallerType: burn
    Scope: user
    ElevationRequirement: elevationProhibited
    InstallerUrl: $userUrl
    InstallerSha256: $userSha
    InstallerSwitches:
      Silent: /quiet
      SilentWithProgress: /passive
    AppsAndFeaturesEntries:
      - DisplayName: $displayName
        Publisher: $publisher
ManifestType: installer
ManifestVersion: $manifestVersion
"@

# ---------- defaultLocale 清单 ----------
$localeYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.$manifestVersion.schema.json
PackageIdentifier: $packageIdentifier
PackageVersion: $Version
PackageLocale: en-US
Publisher: $publisher
PublisherUrl: https://github.com/RyanZhou416
PublisherSupportUrl: $issuesUrl
PackageName: $displayName
PackageUrl: $repoUrl
License: MIT
LicenseUrl: $licenseUrl
ShortDescription: Cross-platform clipboard sync for Windows, Android, and macOS.
Description: ClipBridge keeps clipboard content in sync across Windows, Android, and macOS devices on the same local network. Content is transferred peer-to-peer and encrypted end to end, with explicit policies for what is allowed to leave each device.
Tags:
  - clipboard
  - clipboard-sync
  - cross-platform
  - sync
ManifestType: defaultLocale
ManifestVersion: $manifestVersion
"@

Write-YamlFile -Path $versionFile -Content $versionYaml
Write-YamlFile -Path $installerFile -Content $installerYaml
Write-YamlFile -Path $localeFile -Content $localeYaml

# ---------- 自检（不依赖 winget CLI） ----------
$errors = @()

function Test-Content {
    param([string]$Path, [string[]]$Required, [string]$Label)
    $text = Get-Content -LiteralPath $Path -Raw
    foreach ($r in $Required) {
        if (-not $text.Contains($r)) { $script:errors += "$Label 缺少: $r" }
    }
}

Test-Content -Path $versionFile -Label 'version 清单' -Required @(
    "PackageIdentifier: $packageIdentifier",
    "PackageVersion: $Version",
    'DefaultLocale: en-US',
    'ManifestType: version',
    "ManifestVersion: $manifestVersion"
)
Test-Content -Path $installerFile -Label 'installer 清单' -Required @(
    "PackageIdentifier: $packageIdentifier",
    "PackageVersion: $Version",
    'InstallerType: msi',
    'InstallerType: burn',
    'Scope: machine',
    'Scope: user',
    'ElevationRequirement: elevatesSelf',
    'ElevationRequirement: elevationProhibited',
    "InstallerUrl: $machineUrl",
    "InstallerUrl: $userUrl",
    "InstallerSha256: $machineSha",
    "InstallerSha256: $userSha",
    'Silent: /qn',
    'SilentWithProgress: /qb',
    'Silent: /quiet',
    'SilentWithProgress: /passive',
    "UpgradeCode: `"$machineUpgradeCode`"",
    'ManifestType: installer',
    "ManifestVersion: $manifestVersion"
)
Test-Content -Path $localeFile -Label 'locale 清单' -Required @(
    "PackageIdentifier: $packageIdentifier",
    "PackageVersion: $Version",
    'PackageLocale: en-US',
    "Publisher: $publisher",
    "PackageName: $displayName",
    'License: MIT',
    'ShortDescription:',
    "PackageUrl: $repoUrl",
    'ManifestType: defaultLocale',
    "ManifestVersion: $manifestVersion"
)

# SHA256 与磁盘文件再核对一次（防止手改清单后没重算）。
foreach ($pair in @(
        @{ Path = $machineMsi; Sha = $machineSha; Name = $machineName },
        @{ Path = $userExe; Sha = $userSha; Name = $userName })) {
    $actual = (Get-FileHash -LiteralPath $pair.Path -Algorithm SHA256).Hash.ToUpperInvariant()
    if ($actual -ne $pair.Sha) { $errors += "$($pair.Name) SHA256 不一致：清单 $($pair.Sha)，磁盘 $actual" }
}
foreach ($u in @($machineUrl, $userUrl)) {
    if ($u -notmatch '^https://') { $errors += "InstallerUrl 不是 https: $u" }
}

if ($errors.Count -gt 0) {
    $errors | ForEach-Object { Write-Error $_ }
    throw "winget 清单自检失败 $($errors.Count) 项（文件已保留在 $OutDir）"
}

# ---------- 输出 ----------
Write-Host ''
Write-Host '========== winget 清单 =========='
Write-Host "输出目录: $OutDir"
Get-ChildItem -LiteralPath $OutDir -Filter '*.yaml' | Sort-Object Name | ForEach-Object {
    Write-Host ("  {0,-46} {1,7} bytes" -f $_.Name, $_.Length)
}
Write-Host ''
Write-Host "PackageIdentifier: $packageIdentifier"
Write-Host "PackageVersion:    $Version   (ManifestVersion $manifestVersion)"
Write-Host ''
Write-Host "machine (msi, scope machine, elevatesSelf):"
Write-Host "  URL:    $machineUrl"
Write-Host "  SHA256: $machineSha"
Write-Host "  file:   $machineMsi"
Write-Host ''
Write-Host "user (burn, scope user, elevationProhibited):"
Write-Host "  URL:    $userUrl"
Write-Host "  SHA256: $userSha"
Write-Host "  file:   $userExe"
Write-Host ''
Write-Host "release tag: '$ReleaseTag'  (base: $ReleaseUrlBase)"
Write-Host ''
Write-Host '结构自检通过：必需字段、两条 Installer 的 URL/文件名、SHA256 均与磁盘产物一致。'
Write-Host '已知限制（winget 1.29.x）：'
Write-Host '  - validate 必须对完整目录执行；对单个 yaml 会报 multi file manifest is incomplete（预期）。'
Write-Host '  - Burn 无需显式 Uninstall 开关：winget 默认 /uninstall /quiet；CI validate 为阻断门禁。'
