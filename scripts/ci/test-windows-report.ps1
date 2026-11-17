#Requires -Version 7.0
<#
.SYNOPSIS
    Offline contract tests for scripts/ci/windows-report.psm1.
.DESCRIPTION
    Exercises the failure inputs the CI must catch even when the test command exits 0:
    missing/zero/invalid TRX, all-skipped or missing mandatory suites and failed results;
    NuGet High/Critical/unknown severity and empty scan graphs. No dotnet/rust toolchain needed.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'windows-report.psm1') -Force

$base = Join-Path ([IO.Path]::GetTempPath()) ("ClipBridgeShell_CS.Tests.WindowsReport." + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Force -Path $base

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw "ASSERT FAILED: $Message" }
}

function Assert-Problem {
    param($Evidence, [string]$Needle)
    $problems = @($Evidence.problems)
    if (-not ($problems | Where-Object { $_ -like "*$Needle*" })) {
        throw "ASSERT FAILED: expected a problem containing '$Needle', got: $($problems -join ' | ')"
    }
}

function New-Trx {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Tests
    )

    $definitions = foreach ($test in $Tests) {
        "<UnitTest name=`"$($test.Name)`" storage=`"test.dll`" id=`"$($test.Id)`"><TestMethod codeBase=`"test.dll`" adapterTypeName=`"x`" className=`"$($test.Class)`" name=`"$($test.Name)`" /></UnitTest>"
    }
    $results = foreach ($test in $Tests) {
        "<UnitTestResult testName=`"$($test.Name)`" testId=`"$($test.Id)`" outcome=`"$($test.Outcome)`" />"
    }
    $xml = "<?xml version=`"1.0`" encoding=`"utf-8`"?><TestRun xmlns=`"http://microsoft.com/schemas/VisualStudio/TeamTest/2010`"><TestDefinitions>$($definitions -join '')</TestDefinitions><Results>$($results -join '')</Results></TestRun>"
    [IO.File]::WriteAllText($Path, $xml, [Text.UTF8Encoding]::new($false))
}

$caseDir = Join-Path $base 'happy'
$null = New-Item -ItemType Directory -Force -Path $caseDir
New-Trx -Path (Join-Path $caseDir 'happy.trx') -Tests @(
    [pscustomobject]@{ Id = 't1'; Class = 'Suite.A'; Name = 'A1'; Outcome = 'Passed' },
    [pscustomobject]@{ Id = 't2'; Class = 'Suite.A'; Name = 'A2'; Outcome = 'Passed' },
    [pscustomobject]@{ Id = 't3'; Class = 'Suite.B'; Name = 'B1'; Outcome = 'Passed' }
)
$happy = Get-TestEvidence -ResultsDirectory $caseDir -RequiredSuites @('Suite.A', 'Suite.B')
Assert-True ($happy.problems.Count -eq 0) "happy path must have no problems: $($happy.problems -join ' | ')"
Assert-True ($happy.discovered -eq 3 -and $happy.executed -eq 3 -and $happy.passed -eq 3) 'happy path counts must be 3/3/3'
Assert-True ($happy.allMandatoryExecuted) 'happy path must mark mandatory suites executed'

$missing = Get-TestEvidence -ResultsDirectory (Join-Path $base 'does-not-exist') -RequiredSuites @('Suite.A')
Assert-Problem $missing 'No TRX result report'

$zeroDir = Join-Path $base 'zero'
$null = New-Item -ItemType Directory -Force -Path $zeroDir
New-Trx -Path (Join-Path $zeroDir 'zero.trx') -Tests @()
$zero = Get-TestEvidence -ResultsDirectory $zeroDir -RequiredSuites @()
Assert-Problem $zero 'zero discovered'
Assert-Problem $zero 'zero executed'

$skippedDir = Join-Path $base 'skipped'
$null = New-Item -ItemType Directory -Force -Path $skippedDir
New-Trx -Path (Join-Path $skippedDir 'skipped.trx') -Tests @(
    [pscustomobject]@{ Id = 's1'; Class = 'Suite.Required'; Name = 'S1'; Outcome = 'NotExecuted' }
)
$skipped = Get-TestEvidence -ResultsDirectory $skippedDir -RequiredSuites @('Suite.Required')
Assert-True ($skipped.executed -eq 0) 'all-skipped TRX must report zero executed'
Assert-Problem $skipped 'executed none'
Assert-Problem $skipped 'zero executed'

$missingSuiteDir = Join-Path $base 'missing-suite'
$null = New-Item -ItemType Directory -Force -Path $missingSuiteDir
New-Trx -Path (Join-Path $missingSuiteDir 'missing.trx') -Tests @(
    [pscustomobject]@{ Id = 'm1'; Class = 'Suite.Other'; Name = 'M1'; Outcome = 'Passed' }
)
$missingSuite = Get-TestEvidence -ResultsDirectory $missingSuiteDir -RequiredSuites @('Suite.Required')
Assert-Problem $missingSuite 'Mandatory suite(s) missing'
Assert-True (-not $missingSuite.allMandatoryExecuted) 'missing mandatory suite must not be treated as executed'

$failedDir = Join-Path $base 'failed'
$null = New-Item -ItemType Directory -Force -Path $failedDir
New-Trx -Path (Join-Path $failedDir 'failed.trx') -Tests @(
    [pscustomobject]@{ Id = 'f1'; Class = 'Suite.Required'; Name = 'F1'; Outcome = 'Failed' }
)
$failed = Get-TestEvidence -ResultsDirectory $failedDir -RequiredSuites @('Suite.Required')
Assert-True ($failed.failed -eq 1) 'failed TRX must count one failure'
Assert-Problem $failed 'failed test'

$invalidDir = Join-Path $base 'invalid'
$null = New-Item -ItemType Directory -Force -Path $invalidDir
[IO.File]::WriteAllText((Join-Path $invalidDir 'invalid.trx'), '<not-xml', [Text.UTF8Encoding]::new($false))
$invalid = Get-TestEvidence -ResultsDirectory $invalidDir -RequiredSuites @()
Assert-Problem $invalid 'not valid XML'

$multiDir = Join-Path $base 'multi'
$null = New-Item -ItemType Directory -Force -Path $multiDir
New-Trx -Path (Join-Path $multiDir 'a.trx') -Tests @([pscustomobject]@{ Id = 'a1'; Class = 'Suite.A'; Name = 'A1'; Outcome = 'Passed' })
New-Trx -Path (Join-Path $multiDir 'b.trx') -Tests @([pscustomobject]@{ Id = 'b1'; Class = 'Suite.B'; Name = 'B1'; Outcome = 'Passed' })
$multi = Get-TestEvidence -ResultsDirectory $multiDir -RequiredSuites @('Suite.A', 'Suite.B')
Assert-True ($multi.discovered -eq 2 -and $multi.executed -eq 2) 'multiple TRX files must be merged'

$graph = [pscustomobject]@{
    version  = 1
    projects = @([pscustomobject]@{
        path       = 'p.csproj'
        frameworks = @([pscustomobject]@{
            framework         = 'net10.0'
            topLevelPackages  = @([pscustomobject]@{ id = 'Top'; resolvedVersion = '1.0.0' })
            transitivePackages = @([pscustomobject]@{ id = 'Transitive'; resolvedVersion = '2.0.0' })
        })
    })
}
$scan = Get-NuGetScanSummary -Json $graph -Project 'p.csproj'
Assert-True ($scan.frameworkCount -eq 1 -and $scan.topLevelPackageCount -eq 1 -and $scan.transitivePackageCount -eq 1) 'package graph counts must be reported'
Assert-True ($scan.packages.Count -eq 2) 'direct + transitive packages must be included'

$threw = $false
try {
    Get-NuGetScanSummary -Json ([pscustomobject]@{ version = 1; projects = @([pscustomobject]@{ frameworks = @() }) }) -Project 'empty.csproj' | Out-Null
} catch {
    $threw = $true
    Assert-True ($_.Exception.Message -like '*resolved no packages*') 'empty graph must fail with an explicit message'
}
Assert-True $threw 'empty package graph must be rejected'

function New-VulnJson {
    param([string]$PackageId, [string[]]$Severities)
    $vulnerabilities = @($Severities | ForEach-Object { [pscustomobject]@{ severity = $_ } })
    return [pscustomobject]@{
        version  = 1
        projects = @([pscustomobject]@{
            frameworks = @([pscustomobject]@{
                framework        = 'net10.0'
                topLevelPackages = @([pscustomobject]@{
                    id              = $PackageId
                    resolvedVersion = '1.0.0'
                    vulnerabilities = $vulnerabilities
                })
            })
        })
    }
}

$high = Get-NuGetVulnerabilitySummary -Json (New-VulnJson -PackageId 'HighPkg' -Severities @('High')) -Project 'p'
Assert-True ($high.high -eq 1 -and $high.violations.Count -eq 1) 'High severity must block'
$critical = Get-NuGetVulnerabilitySummary -Json (New-VulnJson -PackageId 'CriticalPkg' -Severities @('Critical')) -Project 'p'
Assert-True ($critical.critical -eq 1 -and $critical.violations.Count -eq 1) 'Critical severity must block'
$low = Get-NuGetVulnerabilitySummary -Json (New-VulnJson -PackageId 'LowPkg' -Severities @('Low', 'Moderate')) -Project 'p'
Assert-True ($low.violations.Count -eq 0) 'Low/Moderate must not block'
$unknown = Get-NuGetVulnerabilitySummary -Json (New-VulnJson -PackageId 'UnknownPkg' -Severities @('', 'Unknown')) -Project 'p'
Assert-True ($unknown.unknownSeverity -eq 2 -and $unknown.violations.Count -eq 2) 'unknown severity must block'

if (Test-Path -LiteralPath $base) {
    Remove-Item -LiteralPath $base -Recurse -Force
}

Write-Host "PASS: windows-report.psm1 contract"
