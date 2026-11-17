<#
.SYNOPSIS
    Run the Rust workspace quality checks with locked dependencies and test evidence.
.PARAMETER OutDir
    Directory for logs and rust-summary.json. Relative to the repository root.
.NOTES
    Runs no application UI or installer. Linux is the CI host; target integration belongs to platform jobs.
#>
[CmdletBinding()]
param([string]$OutDir = 'dist/ci/rust')

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'rust-report.psm1') -Force
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$out = [IO.Path]::GetFullPath($(if ([IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repo $OutDir }))
$null = New-Item -ItemType Directory -Force -Path $out
$env:CARGO_TERM_COLOR = 'never'
$checks = [System.Collections.Generic.List[object]]::new()
$summary = [ordered]@{ schemaVersion = 1; status = 'running'; commit = ''; checks = $checks; tests = $null }
$exitCode = 1

function Invoke-Check {
    param([string]$Name, [string]$Executable, [string[]]$Arguments)
    $log = Join-Path $out "$Name.log"
    Write-Host "[$Name] $Executable $($Arguments -join ' ')"
    & $Executable @Arguments 2>&1 | Tee-Object -FilePath $log | Out-Host
    $code = $LASTEXITCODE
    $checks.Add([pscustomobject]@{ name = $Name; exitCode = $code; log = "$Name.log" })
    if ($code -ne 0) { throw "$Name failed (exit $code). See $log" }
}

Push-Location $repo
try {
    $commit = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-fA-F0-9]{40}$') { throw 'Cannot resolve source commit.' }
    $summary.commit = [string]$commit
    Invoke-Check 'toolchain' 'rustc' @('--version', '--verbose')
    Invoke-Check 'format' 'cargo' @('fmt', '--all', '--', '--check')
    Invoke-Check 'clippy' 'cargo' @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
    Invoke-Check 'test' 'cargo' @('test', '--workspace', '--locked', '--', '--color', 'never')
    $summary.tests = Get-CargoTestSummary -Text ([IO.File]::ReadAllText((Join-Path $out 'test.log')))
    Invoke-Check 'ffi-surface' 'pwsh' @('-NoProfile', '-File', (Join-Path $repo 'scripts/verify-ffi-surface.ps1'))

    # Install only into this run's output directory; never depend on a developer's global cargo-deny.
    $denyRoot = Join-Path $out 'tools/cargo-deny'
    Invoke-Check 'install-cargo-deny' 'cargo' @('install', 'cargo-deny', '--version', '0.20.2', '--locked', '--root', $denyRoot)
    $deny = Join-Path $denyRoot ('bin/cargo-deny' + $(if ($IsWindows) { '.exe' } else { '' }))
    Invoke-Check 'dependencies' $deny @('check', 'advisories', 'bans', 'licenses', 'sources')
    $summary.status = 'passed'
    $exitCode = 0
} catch {
    $summary.status = 'failed'
    $summary.error = $_.Exception.Message
    Write-Host "FAIL: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    $summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $out 'rust-summary.json') -Encoding utf8
    Pop-Location
}
exit $exitCode
