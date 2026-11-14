<#
.SYNOPSIS
    从 Version.props 派生各产物版本号，并做边界校验。

.DESCRIPTION
    唯一真值：platforms/windows/ClipBridgeShell_CS/Version.props 的 <Version>（semver 3 段）。
      - MSI ProductVersion 只接受 3 段：major.minor.build（major/minor ≤255，build ≤65535）。
      - MSIX 需要 4 段：Major 不能为 0、Revision 必须为 0 → max(1,major).minor.build.0。
      - Burn Bundle 接受 3 段（与 MSI 相同）。
    越界/格式错误时直接失败，避免静默产出非法包。

.EXAMPLE
    pwsh scripts/version.ps1
    pwsh scripts/version.ps1 -Json
#>
[CmdletBinding()]
param(
    [string]$VersionProps = '',
    [switch]$Json,
    # 计算 MSIX 版本时要求 major>=1（Store 规则）。
    [switch]$RequireAppx
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $VersionProps) {
    $VersionProps = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/Version.props'
}
if (-not (Test-Path $VersionProps)) { throw "找不到版本文件: $VersionProps" }

[xml]$xml = Get-Content $VersionProps
$version = [string]$xml.Project.PropertyGroup.Version
if (-not $version) { throw "Version.props 缺少 <Version>" }
if ($version -notmatch '^(\d+)\.(\d+)\.(\d+)$') {
    throw "Version 必须是 3 段 semver（如 1.0.9），实际: $version"
}
$major = [int]$Matches[1]; $minor = [int]$Matches[2]; $patch = [int]$Matches[3]

if ($major -gt 255 -or $minor -gt 255 -or $patch -gt 65535) {
    throw "版本越界：MSI 要求 major/minor<=255、build<=65535，实际 $major.$minor.$patch"
}

# MSIX：Major 不能为 0、Revision 必须为 0。
# 0.x 期间不可发 Store（否则映射出的版本会高于将来真正的 1.0.0，Store 不允许降级），
# 因此这里不硬造映射，只返回 $null；需要 MSIX 的调用方必须显式 -RequireAppx 并先把版本抬到 >=1.0.0。
$appxVersion = $null
if ($major -eq 0) {
    Write-Warning "Version 的 major 为 0：可构建 MSI/Bundle，但不可发 MSIX。首发 Store 前请把版本抬到 1.0.0。"
    if ($RequireAppx) { throw "MSIX 需要 major>=1（当前 $version）；请先把 Version.props 抬到 1.0.0 或更高。" }
} else {
    $appxVersion = "$major.$minor.$patch.0"
}

$result = [ordered]@{
    SemVer        = "$major.$minor.$patch"
    MsiVersion    = "$major.$minor.$patch"
    BundleVersion = "$major.$minor.$patch"
    AppxVersion   = $appxVersion
}

if ($Json) {
    $result | ConvertTo-Json
} else {
    "SemVer:        $($result.SemVer)"
    "MsiVersion:    $($result.MsiVersion)"
    "BundleVersion: $($result.BundleVersion)"
    "AppxVersion:   $($result.AppxVersion)"
}
