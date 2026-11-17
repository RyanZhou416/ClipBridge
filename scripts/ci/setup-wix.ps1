#requires -version 7.0
<#
.SYNOPSIS
    固定版本 WiX 3.14.1 的幂等引导（下载 + SHA256 校验 + 解压），不全局安装。

.DESCRIPTION
    - 版本/URL/SHA256 来自 scripts/ci/plan.psm1 的单一 tool pin 表。
    - 已存在完整工具目录时直接复用，不重复下载。
    - 下载失败只对可识别的网络操作做有界重试（最多 3 次尝试，带退避）；
      SHA256 不匹配立即失败，绝不重试掩盖。
    - -VerifyZipPath 只验证给定 zip 的摘要，用于负路径测试，不联网。

.PARAMETER DestDir
    解压目录（默认 RUNNER_TEMP/wix314）。包含 candle.exe/light.exe/heat.exe/WixTasks.dll。

.PARAMETER ZipPath
    可选：复用已下载的 wix314-binaries.zip（仍校验摘要）。

.PARAMETER VerifyZipPath
    只校验该文件的 SHA256，成功 exit 0，失败 exit 1。

.EXAMPLE
    pwsh scripts/ci/setup-wix.ps1 -DestDir $env:RUNNER_TEMP/wix314
#>
[CmdletBinding()]
param(
    [string]$DestDir = (Join-Path ([System.IO.Path]::GetTempPath()) 'wix314'),
    [string]$ZipPath,
    [string]$VerifyZipPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-WixPins {
    Import-Module (Join-Path $PSScriptRoot 'plan.psm1') -Force
    return Get-CiToolPins
}

function Test-WixZipHash {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Expected)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
    return ($actual -ieq $Expected)
}

function Get-WixZip {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Url,
        [Parameter(Mandatory)][string]$Sha256
    )
    if (Test-Path -LiteralPath $Path -PathType Leaf) {
        if (Test-WixZipHash -Path $Path -Expected $Sha256) { return }
        # 本地缓存损坏：删除后按新下载处理；这不是对已识别网络故障的重试。
        Remove-Item -LiteralPath $Path -Force
    }
    $parent = Split-Path -Parent $Path
    if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    $attempts = 3
    for ($attempt = 1; $attempt -le $attempts; $attempt++) {
        try {
            Write-Host "downloading WiX ($attempt/$attempts): $Url"
            Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Path -MaximumRedirection 5
            if (-not (Test-WixZipHash -Path $Path -Expected $Sha256)) {
                $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
                Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
                throw "WiX zip SHA256 mismatch: expected $Sha256, got $actual"
            }
            return
        } catch {
            $message = $_.Exception.Message
            if ($message -match 'SHA256 mismatch') { throw }
            if ($attempt -eq $attempts) { throw "WiX download failed after $attempts attempts: $message" }
            Start-Sleep -Seconds ([math]::Pow(2, $attempt))
        }
    }
}

try {
    $pins = Get-WixPins
    $expected = [string]$pins.WixZipSha256
    $url = [string]$pins.WixDownloadUrl
    Write-Host "WiX pin: $($pins.WixVersion) sha256=$expected"

    if ($VerifyZipPath) {
        if (-not (Test-Path -LiteralPath $VerifyZipPath -PathType Leaf)) { throw "zip not found: $VerifyZipPath" }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $VerifyZipPath).Hash
        if ($actual -ieq $expected) {
            Write-Host "WiX zip checksum OK: $actual"
            exit 0
        }
        Write-Error "WiX zip checksum mismatch: expected $expected, got $actual"
        exit 1
    }

    $required = @('candle.exe', 'light.exe', 'heat.exe', 'WixTasks.dll', 'wix.targets')
    $destFull = [System.IO.Path]::GetFullPath($DestDir)
    $hasAll = $true
    foreach ($name in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $destFull $name) -PathType Leaf)) { $hasAll = $false; break }
    }
    if ($hasAll) {
        Write-Host "reusing WiX at $destFull"
    } else {
        $zip = if ($ZipPath) { [System.IO.Path]::GetFullPath($ZipPath) } else { Join-Path ([System.IO.Path]::GetTempPath()) 'clipbridge-ci-wix314-binaries.zip' }
        Get-WixZip -Path $zip -Url $url -Sha256 $expected
        if (Test-Path -LiteralPath $destFull) { Remove-Item -LiteralPath $destFull -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $destFull | Out-Null
        Write-Host "extracting WiX to $destFull"
        Expand-Archive -LiteralPath $zip -DestinationPath $destFull -Force
        foreach ($name in $required) {
            if (-not (Test-Path -LiteralPath (Join-Path $destFull $name) -PathType Leaf)) {
                throw "WiX extraction is incomplete: missing $name in $destFull"
            }
        }
    }

    Write-Host "wix-path=$destFull"
    if ($env:GITHUB_OUTPUT) {
        "wix-path=$destFull" | Add-Content -Path $env:GITHUB_OUTPUT
    }
    exit 0
} catch {
    Write-Error $_.Exception.Message
    exit 1
}
