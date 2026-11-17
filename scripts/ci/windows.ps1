<#
.SYNOPSIS
    Windows native + C# CI entry point (Release / x64 / win-x64 / R2R).
.PARAMETER OutDir
    Directory for logs, test reports and windows-summary.json. Relative to the repository root.
.NOTES
    Requires PowerShell 7 and the .NET 10 SDK (dotnet package list --format json --output-version 1).
    The Rust core DLL is built once with locked dependencies, then restore/build/test run with
    -p:SkipRustBuild=true and -p:RequireCoreFfi=true so a missing/stale native library fails the
    job instead of producing a warning. No MSIX packaging, deployment, signing or secrets are used.
#>
[CmdletBinding()]
param([string]$OutDir = 'dist/ci/windows')

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'windows-report.psm1') -Force

$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$out = [IO.Path]::GetFullPath($(if ([IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repo $OutDir }))
$null = New-Item -ItemType Directory -Force -Path $out
$logDir = Join-Path $out 'logs'
$testResultsDir = Join-Path $out 'test-results'
$nugetDir = Join-Path $out 'nuget'
$summaryPath = Join-Path $out 'windows-summary.json'
$null = New-Item -ItemType Directory -Force -Path $logDir, $testResultsDir, $nugetDir

$configuration = 'Release'
$platform = 'x64'
$runtimeIdentifier = 'win-x64'
$publishReadyToRun = $true
$rustTarget = 'x86_64-pc-windows-msvc'
$cargoTargetRoot = Join-Path $repo 'target'
$rustTargetDir = Join-Path $cargoTargetRoot "$rustTarget/release"
$coreDll = Join-Path $rustTargetDir 'core_ffi_windows.dll'

$shellProject = Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/ClipBridgeShell_CS.csproj'
$coreProject = Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Core/ClipBridgeShell_CS.Core.csproj'
$testProject = Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/ClipBridgeShell_CS.Tests.MSTest.csproj'
$projects = [ordered]@{ core = $coreProject; shell = $shellProject; tests = $testProject }

# Mandatory suites are identified by TRX class name. They cover the native positive lifecycle,
# data isolation, native negative paths and migration/credential regressions; the total number of
# tests is deliberately NOT pinned here.
$requiredSuites = @(
    'ClipBridgeShell_CS.Tests.MSTest.CoreHostSmokeTests',
    'ClipBridgeShell_CS.Tests.MSTest.CoreHostDataIsolationTests',
    'ClipBridgeShell_CS.Tests.MSTest.CoreNativeLoadNegativeTests',
    'ClipBridgeShell_CS.Tests.MSTest.MsixLegacyImportTests',
    'ClipBridgeShell_CS.Tests.MSTest.AccountFailClosedTests',
    'ClipBridgeShell_CS.Tests.MSTest.CredentialStoreTests'
)

$configProps = @(
    "-p:Configuration=$configuration",
    "-p:Platform=$platform",
    "-p:RuntimeIdentifier=$runtimeIdentifier",
    "-p:PublishReadyToRun=$($publishReadyToRun.ToString().ToLowerInvariant())"
)
$strictNativeProps = @(
    '-p:SkipRustBuild=true',
    '-p:RequireCoreFfi=true',
    "-p:RustTargetDir=$rustTargetDir"
)
$noPackagingProps = @(
    '-p:GenerateAppxPackageOnBuild=false',
    '-p:AppxPackageSigningEnabled=false',
    '-p:AppxSymbolPackageEnabled=false'
)

$checks = [System.Collections.Generic.List[object]]::new()
$summary = [ordered]@{
    schemaVersion        = 1
    status               = 'running'
    exitCode             = 1
    commit               = ''
    commitShort          = ''
    dirty                = $true
    startedAtUtc         = [DateTime]::UtcNow.ToString('o')
    finishedAtUtc        = $null
    configuration        = $configuration
    platform             = $platform
    runtimeIdentifier    = $runtimeIdentifier
    publishReadyToRun    = $publishReadyToRun
    toolchain            = [ordered]@{}
    nativeEvidence       = [ordered]@{}
    formatEvidence       = [ordered]@{ verified = $false; command = 'dotnet format --verify-no-changes'; projects = @() }
    nugetEvidence        = [ordered]@{}
    testsEvidence        = [ordered]@{}
    failure              = $null
    error                = $null
    artifacts            = [ordered]@{ logsDirectory = $logDir; testResultsDirectory = $testResultsDir; summaryPath = $summaryPath }
}
$currentStep = 'initialization'
$exitCode = 1

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @()
    )

    $log = Join-Path $logDir "$Name.log"
    Write-Host ''
    Write-Host "==> [$Name] $FilePath $($Arguments -join ' ')" -ForegroundColor Cyan

    $psi = [System.Diagnostics.ProcessStartInfo]::new($FilePath)
    foreach ($arg in $Arguments) { $psi.ArgumentList.Add($arg) }
    $psi.WorkingDirectory = $repo
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    $psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $psi
    if (-not $process.Start()) { throw "Failed to start $FilePath" }

    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    $code = [int]$process.ExitCode
    $process.Dispose()

    $logText = $stdout
    if ($stderr) { $logText += "`n--- stderr ---`n$stderr" }
    [IO.File]::WriteAllText($log, $logText, [Text.UTF8Encoding]::new($false))

    if ($stdout) { Write-Host $stdout.TrimEnd() }
    if ($stderr) { Write-Host $stderr.TrimEnd() }
    Write-Host "[$Name] exit=$code" -ForegroundColor $(if ($code -eq 0) { 'Green' } else { 'Red' })

    $check = [pscustomobject]@{ name = $Name; exitCode = $code; log = $log }
    $checks.Add($check)
    return [pscustomobject]@{
        Name     = $Name
        ExitCode = $code
        StdOut   = $stdout
        StdErr   = $stderr
        Log      = $log
    }
}

function Assert-NativeSuccess {
    param([Parameter(Mandatory = $true)]$Result)
    if ($Result.ExitCode -ne 0) {
        throw "$($Result.Name) failed with exit code $($Result.ExitCode). See $($Result.Log)"
    }
}

function Assert-PeX64 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $reader = [IO.BinaryReader]::new($stream)
        if ($reader.ReadUInt16() -ne 0x5A4D) { throw "'$Path' is not a PE image (missing MZ)." }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadInt32()
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) { throw "'$Path' is not a PE image (missing PE signature)." }
        $machine = $reader.ReadUInt16()
        if ($machine -ne 0x8664) {
            throw ("'{0}' is not an AMD64 image (machine=0x{1:X4}, expected 0x8664)." -f $Path, $machine)
        }
    } finally {
        $stream.Dispose()
    }
}

function Invoke-PackageList {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$Project,
        [switch]$Vulnerable
    )

    $arguments = @(
        'package', 'list',
        '--project', $Project,
        '--format', 'json',
        '--output-version', '1',
        '--no-restore'
    )
    if ($Vulnerable) { $arguments += '--vulnerable' }
    $arguments += '--include-transitive'
    $jsonFile = Join-Path $nugetDir "$Label.json"

    $result = Invoke-Native -Name "nuget-$Label" -FilePath 'dotnet' -Arguments $arguments
    if ($result.ExitCode -ne 0) {
        throw "NuGet $Label query failed with exit code $($result.ExitCode); vulnerability status is unknown. See $($result.Log)"
    }

    $text = $result.StdOut.TrimStart([char]0xFEFF).Trim()
    if ([string]::IsNullOrWhiteSpace($text)) {
        throw "NuGet $Label query produced no JSON; vulnerability status is unknown."
    }
    [IO.File]::WriteAllText($jsonFile, $text, [Text.UTF8Encoding]::new($false))
    try {
        $json = $text | ConvertFrom-Json -Depth 20
    } catch {
        throw "NuGet $Label query produced invalid JSON: $($_.Exception.Message). See $jsonFile"
    }
    if ($null -eq $json -or -not ($json.PSObject.Properties.Name -contains 'version') -or [int]$json.version -ne 1) {
        throw "NuGet $Label JSON has unsupported schemaVersion; expected 1. See $jsonFile"
    }
    if ($null -eq $json.projects -or @($json.projects).Count -eq 0) {
        throw "NuGet $Label JSON contains no projects; the restore graph was not scanned. See $jsonFile"
    }
    return [pscustomobject]@{ Result = $result; Json = $json; JsonFile = $jsonFile }
}

function Get-NuGetEvidence {
    $projectEvidence = [System.Collections.Generic.List[object]]::new()
    $queryExitCodes = [System.Collections.Generic.List[int]]::new()
    $allPackages = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $allTransitive = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $violations = [System.Collections.Generic.List[object]]::new()
    $vulnerableCount = 0

    foreach ($entry in $projects.GetEnumerator()) {
        $label = [string]$entry.Key
        $project = [string]$entry.Value

        $full = Invoke-PackageList -Label "$label.packages" -Project $project
        $queryExitCodes.Add($full.Result.ExitCode)

        $graph = Get-NuGetScanSummary -Json $full.Json -Project $project
        $frameworkCount = $graph.frameworkCount
        $topCount = $graph.topLevelPackageCount
        $transitiveCount = $graph.transitivePackageCount
        foreach ($packageKey in $graph.packages) { $null = $allPackages.Add($packageKey) }
        foreach ($packageKey in $graph.transitivePackages) { $null = $allTransitive.Add($packageKey) }

        $vuln = Invoke-PackageList -Label "$label.vulnerable" -Project $project -Vulnerable
        $queryExitCodes.Add($vuln.Result.ExitCode)
        $vulnSummary = Get-NuGetVulnerabilitySummary -Json $vuln.Json -Project $label
        $projectHigh = $vulnSummary.high
        $projectCritical = $vulnSummary.critical
        $projectUnknown = $vulnSummary.unknownSeverity
        $projectVulnerable = $vulnSummary.vulnerablePackageCount
        $vulnerableCount += $vulnSummary.vulnerablePackageCount
        foreach ($violation in $vulnSummary.violations) { $violations.Add($violation) }

        $projectEvidence.Add([ordered]@{
            name                   = $label
            path                   = $project
            frameworkCount         = $frameworkCount
            topLevelPackageCount   = $topCount
            transitivePackageCount = $transitiveCount
            vulnerablePackageCount = $projectVulnerable
            vulnerableHigh         = $projectHigh
            vulnerableCritical     = $projectCritical
            unknownSeverity        = $projectUnknown
        })
    }

    $problems = [System.Collections.Generic.List[string]]::new()
    if ($allTransitive.Count -eq 0) {
        $problems.Add('NuGet scan resolved zero transitive packages across all projects; --include-transitive input is missing.')
    }

    $evidence = [ordered]@{
        command                 = 'dotnet package list --vulnerable --include-transitive --format json --output-version 1'
        includeTransitive       = $true
        blockedSeverities       = @('High', 'Critical')
        projects                = @($projectEvidence)
        totalPackagesScanned    = $allPackages.Count
        totalTransitivePackages = $allTransitive.Count
        vulnerablePackageCount  = $vulnerableCount
        violations              = @($violations)
        queryExitCodes          = @($queryExitCodes)
        problems                = @($problems)
    }

    if ($violations.Count -gt 0) {
        $rendered = $violations | ForEach-Object { "$($_.project)/$($_.package)=$($_.severity)" }
        $evidence.problems = @($problems) + @("NuGet vulnerability gate blocked: $($rendered -join ', '). See $nugetDir")
    }

    return $evidence
}

Push-Location $repo
try {
    $currentStep = 'preflight'
    if (-not $IsWindows) { throw 'windows.ps1 must run on Windows.' }
    if (-not [Environment]::Is64BitProcess -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        throw "windows.ps1 requires an x64 process (PROCESSOR_ARCHITECTURE=$env:PROCESSOR_ARCHITECTURE)."
    }
    foreach ($requiredPath in @($shellProject, $coreProject, $testProject)) {
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) { throw "Missing project: $requiredPath" }
    }

    $commit = (& git rev-parse HEAD 2>$null)
    if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-fA-F0-9]{40}$') { throw 'Cannot resolve the source commit (git rev-parse HEAD).' }
    $summary.commit = [string]$commit
    $summary.commitShort = ([string]$commit).Substring(0, 12)
    $statusPorcelain = (& git status --porcelain 2>$null)
    $summary.dirty = -not [string]::IsNullOrWhiteSpace(($statusPorcelain -join "`n"))

    $currentStep = 'toolchain'
    $dotnetVersion = Invoke-Native -Name 'toolchain-dotnet' -FilePath 'dotnet' -Arguments @('--version')
    Assert-NativeSuccess $dotnetVersion
    $rustcVersion = Invoke-Native -Name 'toolchain-rustc' -FilePath 'rustc' -Arguments @('--version', '--verbose')
    Assert-NativeSuccess $rustcVersion
    $cargoVersion = Invoke-Native -Name 'toolchain-cargo' -FilePath 'cargo' -Arguments @('--version')
    Assert-NativeSuccess $cargoVersion
    $summary.toolchain = [ordered]@{
        dotnet = $dotnetVersion.StdOut.Trim()
        rustc  = $rustcVersion.StdOut.Trim()
        cargo  = $cargoVersion.StdOut.Trim()
        os     = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
        architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }

    # 1) Build the current commit's x64 Rust DLL once, locked, then reuse it for every dotnet step.
    $currentStep = 'cargo'
    $cargo = Invoke-Native -Name 'cargo-build' -FilePath 'cargo' -Arguments @(
        'build', '-p', 'core-ffi-windows', '--release', '--locked',
        '--target', $rustTarget, '--target-dir', $cargoTargetRoot
    )
    Assert-NativeSuccess $cargo
    if (-not (Test-Path -LiteralPath $coreDll -PathType Leaf)) {
        throw "cargo succeeded but '$coreDll' was not produced."
    }
    $dllInfo = Get-Item -LiteralPath $coreDll
    if ($dllInfo.Length -le 0) { throw "core_ffi_windows.dll is empty: $coreDll" }
    Assert-PeX64 $coreDll
    $dllHash = (Get-FileHash -LiteralPath $coreDll -Algorithm SHA256).Hash
    $summary.nativeEvidence = [ordered]@{
        crate                  = 'core-ffi-windows'
        target                 = $rustTarget
        configuration          = 'release'
        locked                 = $true
        dllPath                = $coreDll
        dllSha256              = $dllHash
        dllSizeBytes           = [long]$dllInfo.Length
        machine                = '0x8664'
        cargoExitCode          = $cargo.ExitCode
        dotnetSkipRustBuild    = $true
        dotnetRequireCoreFfi   = $true
        buildExitCode          = $null
    }

    # 2) Restore with the exact same configuration/platform/RID/R2R properties used by build/test.
    $currentStep = 'restore'
    foreach ($entry in $projects.GetEnumerator()) {
        $restore = Invoke-Native -Name "restore-$($entry.Key)" -FilePath 'dotnet' -Arguments (@('restore', [string]$entry.Value) + $configProps)
        Assert-NativeSuccess $restore
    }

    # 3) Blocking format gate for every owned Windows project (no continue-on-error).
    $currentStep = 'format'
    $formatProjects = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in $projects.GetEnumerator()) {
        $format = Invoke-Native -Name "format-$($entry.Key)" -FilePath 'dotnet' -Arguments @(
            'format', [string]$entry.Value, '--verify-no-changes', '--no-restore'
        )
        $formatProjects.Add([ordered]@{ name = [string]$entry.Key; path = [string]$entry.Value; exitCode = $format.ExitCode })
        Assert-NativeSuccess $format
    }
    $summary.formatEvidence.projects = @($formatProjects)
    $summary.formatEvidence.verified = $true

    # 4) NuGet vulnerable scan over direct + transitive dependencies, verified from JSON.
    $currentStep = 'nuget-vulnerabilities'
    $summary.nugetEvidence = Get-NuGetEvidence
    if (@($summary.nugetEvidence.problems).Count -gt 0) {
        throw (@($summary.nugetEvidence.problems) -join ' ')
    }

    # 5) Build the real application + test projects against the freshly built DLL.
    $currentStep = 'build'
    $shellBuild = Invoke-Native -Name 'build-shell' -FilePath 'dotnet' -Arguments (@(
        'build', $shellProject, '--no-restore'
    ) + $configProps + $strictNativeProps + $noPackagingProps)
    Assert-NativeSuccess $shellBuild
    $summary.nativeEvidence.buildExitCode = $shellBuild.ExitCode

    $testBuild = Invoke-Native -Name 'build-tests' -FilePath 'dotnet' -Arguments (@(
        'build', $testProject, '--no-restore'
    ) + $configProps + $strictNativeProps + $noPackagingProps)
    Assert-NativeSuccess $testBuild

    # 6) Execute MSTest and verify the TRX contract (report present, discovered/executed > 0,
    #    mandatory suites actually executed; total test count is not pinned).
    $currentStep = 'test'
    Get-ChildItem -Path $testResultsDir -Filter '*.trx' -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    # 正向 smoke 用这两个值证明实际加载的 DLL 就是本次 cargo 产物（防止从父目录捡旧 DLL）。
    $env:CLIPBRIDGE_CORE_FFI_DLL = $coreDll
    $env:CLIPBRIDGE_CORE_FFI_SHA256 = $dllHash
    $testResult = Invoke-Native -Name 'test-mstest' -FilePath 'dotnet' -Arguments (@(
        'test', $testProject,
        '--no-build', '--no-restore',
        '--logger', 'trx;LogFileName=windows.trx',
        '--results-directory', $testResultsDir
    ) + $configProps + $strictNativeProps)
    $summary.testsEvidence = Get-TestEvidence -ResultsDirectory $testResultsDir -RequiredSuites $requiredSuites
    $summary.testsEvidence.testCommandExitCode = $testResult.ExitCode
    $summary.testsEvidence.buildExitCode = $testBuild.ExitCode
    $summary.testsEvidence.expectedCoreDllPath = $coreDll
    $summary.testsEvidence.expectedCoreDllSha256 = $dllHash

    if ($summary.testsEvidence.problems.Count -gt 0) {
        throw ($summary.testsEvidence.problems -join ' ')
    }
    Assert-NativeSuccess $testResult

    $summary.status = 'passed'
    $exitCode = 0
} catch {
    $summary.status = 'failed'
    $summary.failure = [ordered]@{ step = $currentStep; message = $_.Exception.Message }
    $summary.error = $_.Exception.Message
    Write-Host ''
    Write-Host "FAIL [$currentStep]: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    $summary.finishedAtUtc = [DateTime]::UtcNow.ToString('o')
    $summary.exitCode = $exitCode
    $summary.checks = @($checks)

    if ($summary.testsEvidence.Count -gt 0) {
        $te = $summary.testsEvidence
        Write-Host ''
        Write-Host ('MSTest: discovered={0} executed={1} passed={2} failed={3} skipped={4} inconclusive={5}' -f `
            $te.discovered, $te.executed, $te.passed, $te.failed, $te.skipped, $te.inconclusive)
        foreach ($suite in @($te.mandatorySuites)) {
            Write-Host ("  suite {0}: discovered={1} executed={2} passed={3} failed={4} skipped={5}" -f `
                $suite.name, $suite.discovered, $suite.executed, $suite.passed, $suite.failed, $suite.skipped)
        }
    }

    $json = $summary | ConvertTo-Json -Depth 12
    [IO.File]::WriteAllText($summaryPath, $json, [Text.UTF8Encoding]::new($false))
    Write-Host ''
    Write-Host "windows-summary.json: $summaryPath"
    Write-Host "status=$($summary.status) exitCode=$exitCode commit=$($summary.commitShort)" -ForegroundColor $(if ($exitCode -eq 0) { 'Green' } else { 'Red' })
    Pop-Location
}

exit $exitCode
