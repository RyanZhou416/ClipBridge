<#
.SYNOPSIS
    Local fixture test for New-ReleaseCandidate.ps1: assembles a candidate from
    fake package-summary/evidence inputs, then verifies the manifest, checksum
    file, whitelist and internal-artifact exclusion without any network or
    compiler.
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Import-Module (Join-Path $repo 'scripts/release/ReleaseModel.psm1') -Force
Import-Module (Join-Path $repo 'scripts/release/ReleaseRuntime.psm1') -Force

$script:Total = 0
$script:Failed = 0
$script:Failures = [System.Collections.Generic.List[string]]::new()
function Test-Case {
    param([string]$Name, [scriptblock]$Body)
    $script:Total++
    try { & $Body; Write-Host "ok   $Name" }
    catch {
        $script:Failed++
        $script:Failures.Add("$Name :: $($_.Exception.Message)")
        Write-Host "FAIL $Name :: $($_.Exception.Message)" -ForegroundColor Red
    }
}
function Assert-True { param([object]$Condition, [string]$Message = 'expected true') if (-not $Condition) { throw $Message } }
function Assert-Equal { param([object]$Expected, [object]$Actual, [string]$Message = 'values differ') if ("$Expected" -cne "$Actual") { throw "$Message (expected '$Expected', actual '$Actual')" } }
function Assert-Throws {
    param([scriptblock]$Body, [string]$Pattern = '')
    $threw = $false
    try { & $Body } catch {
        $threw = $true
        if ($Pattern -and $_.Exception.Message -notmatch $Pattern) { throw "threw the wrong error: $($_.Exception.Message)" }
    }
    if (-not $threw) { throw 'expected an exception but none was thrown' }
}

$script:Sha = 'a' * 40
$script:Dirty = New-ReleaseTempDirectory -Prefix 'cb-candidate-test'

function New-Fixture {
    param([string]$Root, [switch]$Certificate)
    $packageDir = Join-Path $Root 'packages'
    $null = New-Item -ItemType Directory -Force -Path $packageDir
    $formats = @('machine-msi', 'user-exe', 'portable-zip')
    $artifacts = [System.Collections.Generic.List[object]]::new()
    foreach ($format in $formats) {
        $name = Get-ReleaseAssetName -Format $format -Version '0.1.0'
        $path = Join-Path $packageDir $name
        Set-Content -Path $path -Value "content of $name" -NoNewline
        $hash = (Get-FileHash -Algorithm SHA256 $path).Hash.ToLowerInvariant()
        $size = (Get-Item $path).Length
        $artifacts.Add([ordered]@{
                name = $name; format = $format; role = 'release'; internal = $false; size = $size; sha256 = $hash
                signing = [ordered]@{ authenticode = $(if ($format -eq 'portable-zip') { 'not-applicable' } else { if ($Certificate) { 'signed' } else { 'unsigned' } }); timestamped = [bool]$Certificate; payloadsSigned = $(if ($format -eq 'portable-zip') { [bool]$Certificate } else { $null }) }
            })
    }
    $internalName = Get-ReleaseAssetName -Format 'user-msi' -Version '0.1.0'
    $internalPath = Join-Path $packageDir $internalName
    Set-Content -Path $internalPath -Value 'internal user msi' -NoNewline
    $artifacts.Add([ordered]@{
            name = $internalName; format = 'user-msi'; role = 'internal'; internal = $true; size = (Get-Item $internalPath).Length
            sha256 = (Get-FileHash -Algorithm SHA256 $internalPath).Hash.ToLowerInvariant()
            signing = [ordered]@{ authenticode = $(if ($Certificate) { 'signed' } else { 'unsigned' }); timestamped = [bool]$Certificate; payloadsSigned = $null }
        })
    $summary = [ordered]@{
        schemaVersion = 1; product = 'ClipBridge'; version = '0.1.0'; platform = 'x64'; formats = $formats
        nativeEvidence = [ordered]@{ inputDllSha256 = ('b' * 64); stageDllSha256 = ('b' * 64); sourceReusedVerified = $true }
        signing = [ordered]@{ mode = $(if ($Certificate) { 'Certificate' } else { 'None' }); status = $(if ($Certificate) { 'signed' } else { 'unsigned' }); trusted = [bool]$Certificate; timestamped = [bool]$Certificate }
        generatedAtUtc = '2026-11-17T00:00:00Z'
        artifacts = [object[]]$artifacts.ToArray()
    }
    Write-JsonFile -Path (Join-Path $packageDir 'package-summary.json') -Value $summary
    $rust = [ordered]@{
        schemaVersion = 1; status = 'passed'; commit = $script:Sha
        checks = @([ordered]@{ name = 'format'; exitCode = 0; log = 'format.log' }, [ordered]@{ name = 'test'; exitCode = 0; log = 'test.log' })
        tests = [ordered]@{ suites = 1; discovered = 3; executed = 3; passed = 3; failed = 0; ignored = 0; filtered = 0 }
    }
    $windows = [ordered]@{
        schemaVersion = 1; status = 'passed'; exitCode = 0; commit = $script:Sha; dirty = $false; configuration = 'Release'; platform = 'x64'; runtimeIdentifier = 'win-x64'; publishReadyToRun = $true
        nativeEvidence = [ordered]@{ locked = $true; dllSha256 = ('b' * 64); dllSizeBytes = 10; machine = '0x8664'; cargoExitCode = 0; dotnetSkipRustBuild = $false; dotnetRequireCoreFfi = $true; buildExitCode = 0 }
        formatEvidence = [ordered]@{ verified = $true; projects = @([ordered]@{ name = 'core'; exitCode = 0 }, [ordered]@{ name = 'shell'; exitCode = 0 }) }
        nugetEvidence = [ordered]@{ includeTransitive = $true; blockedSeverities = @('High', 'Critical'); projects = @([ordered]@{ name = 'p'; topLevelPackageCount = 1 }); totalPackagesScanned = 3; totalTransitivePackages = 2; vulnerablePackageCount = 0; violations = @(); problems = @(); queryExitCodes = @(0, 0) }
        testsEvidence = [ordered]@{ testCommandExitCode = 0; buildExitCode = 0; discovered = 2; executed = 2; passed = 2; failed = 0; skipped = 0; inconclusive = 0; mandatorySuites = @([ordered]@{ name = 'CoreHost'; discovered = 2; executed = 2; passed = 2; failed = 0; skipped = 0 }); allMandatoryExecuted = $true; problems = @() }
        checks = @([ordered]@{ name = 'build-shell'; exitCode = 0 })
        failure = $null; error = $null
    }
    Write-JsonFile -Path (Join-Path $Root 'rust-summary.json') -Value $rust
    Write-JsonFile -Path (Join-Path $Root 'windows-summary.json') -Value $windows
    return [pscustomobject]@{
        PackageDir = $packageDir
        PackageSummary = (Join-Path $packageDir 'package-summary.json')
        RustSummary = (Join-Path $Root 'rust-summary.json')
        WindowsSummary = (Join-Path $Root 'windows-summary.json')
        InternalName = $internalName
    }
}

function Invoke-Candidate {
    param([object]$Fixture, [string]$OutDir, [string[]]$Formats = @('machine-msi', 'user-exe', 'portable-zip'), [string]$ArtifactName = 'windows-release-candidate-100-1', [string]$SourceRunAttempt = '1')
    & (Join-Path $repo 'scripts/release/New-ReleaseCandidate.ps1') `
        -OutDir $OutDir `
        -Version '0.1.0' `
        -SourceCommit $script:Sha `
        -Formats $Formats `
        -SourceRunId 100 `
        -SourceRunAttempt $SourceRunAttempt `
        -ArtifactName $ArtifactName `
        -PackageSummaryPath $Fixture.PackageSummary `
        -RustSummaryPath $Fixture.RustSummary `
        -WindowsSummaryPath $Fixture.WindowsSummary
}

Test-Case 'candidate assembly produces only the public whitelist plus evidence' {
    $root = Join-Path $script:Dirty 'case-valid'
    $fixture = New-Fixture -Root $root
    $out = Join-Path $root 'candidate'
    Invoke-Candidate -Fixture $fixture -OutDir $out
    $rootFiles = @(Get-ChildItem -LiteralPath $out -File | ForEach-Object { $_.Name })
    $expected = Get-ReleasePublicAssetNames -Version '0.1.0' -Formats @('machine-msi', 'user-exe', 'portable-zip')
    Assert-Equal (($expected | Sort-Object) -join ',') (($rootFiles | Sort-Object) -join ',')
    Assert-True (-not ($rootFiles -contains $fixture.InternalName))
    Assert-Equal 0 @(Get-ChildItem -LiteralPath $out -Recurse -File -Filter '*-user.msi').Count
    Assert-True (Test-Path (Join-Path $out 'evidence/package-summary.json'))
    Assert-True (Test-Path (Join-Path $out 'evidence/rust-summary.json'))
    Assert-True (Test-Path (Join-Path $out 'evidence/windows-summary.json'))
    $manifestText = Read-Utf8Text -Path (Join-Path $out 'release-manifest.json')
    $parsed = Read-ReleaseManifestText -Text $manifestText
    Assert-Equal '0.1.0' $parsed.Manifest.version
    Assert-Equal $script:Sha $parsed.Manifest.commit
    Assert-Equal 'None' $parsed.Manifest.signing.mode
    Assert-True $parsed.Complete
    Assert-Equal 3 $parsed.Manifest.artifacts.Count
    Assert-True ($parsed.Manifest.artifacts.name -notcontains $fixture.InternalName)
    $sums = Test-Sha256SumsText -Text (Read-Utf8Text -Path (Join-Path $out 'SHA256SUMS.txt'))
    Assert-Equal 4 $sums.Entries.Count
    Assert-Equal $parsed.Digest ($sums.Entries | Where-Object { $_.name -eq 'release-manifest.json' }).sha256
    Assert-True ($manifestText -notmatch 'user\.msi')
}
Test-Case 'candidate rejects certificate mode with unsigned package evidence' {
    $root = Join-Path $script:Dirty 'case-cert-unsigned'
    $fixture = New-Fixture -Root $root
    # Force package-summary signing to Certificate/unsigned, which must fail.
    $summary = Read-JsonFile -Path $fixture.PackageSummary
    $summary.signing.mode = 'Certificate'
    $summary.signing.status = 'unsigned'
    Write-JsonFile -Path $fixture.PackageSummary -Value $summary
    Assert-Throws { Invoke-Candidate -Fixture $fixture -OutDir (Join-Path $root 'candidate') } 'signed/trusted/timestamped'
}
Test-Case 'candidate rejects wrong native input/stage hashes' {
    $root = Join-Path $script:Dirty 'case-native-wrong'
    $fixture = New-Fixture -Root $root
    $summary = Read-JsonFile -Path $fixture.PackageSummary
    $summary.nativeEvidence.inputDllSha256 = ('c' * 64)
    Write-JsonFile -Path $fixture.PackageSummary -Value $summary
    Assert-Throws { Invoke-Candidate -Fixture $fixture -OutDir (Join-Path $root 'candidate') } 'inputDllSha256'
}
Test-Case 'candidate rejects missing nativeEvidence' {
    $root = Join-Path $script:Dirty 'case-native-missing'
    $fixture = New-Fixture -Root $root
    $summary = Read-JsonFile -Path $fixture.PackageSummary
    $summary.PSObject.Properties.Remove('nativeEvidence')
    Write-JsonFile -Path $fixture.PackageSummary -Value $summary
    Assert-Throws { Invoke-Candidate -Fixture $fixture -OutDir (Join-Path $root 'candidate') } 'nativeEvidence'
}
Test-Case 'candidate rejects unverified native source reuse' {
    $root = Join-Path $script:Dirty 'case-native-unverified'
    $fixture = New-Fixture -Root $root
    $summary = Read-JsonFile -Path $fixture.PackageSummary
    $summary.nativeEvidence.sourceReusedVerified = $false
    Write-JsonFile -Path $fixture.PackageSummary -Value $summary
    Assert-Throws { Invoke-Candidate -Fixture $fixture -OutDir (Join-Path $root 'candidate') } 'sourceReusedVerified'
}
Test-Case 'candidate rejects a non-empty output directory' {
    $root = Join-Path $script:Dirty 'case-nonempty'
    $fixture = New-Fixture -Root $root
    $out = Join-Path $root 'candidate'
    $null = New-Item -ItemType Directory -Force -Path $out
    Set-Content -Path (Join-Path $out 'old.txt') -Value 'old'
    Assert-Throws { Invoke-Candidate -Fixture $fixture -OutDir $out } 'must be empty'
}
Test-Case 'candidate rejects tampered package hash' {
    $root = Join-Path $script:Dirty 'case-tamper'
    $fixture = New-Fixture -Root $root
    $name = Get-ReleaseAssetName -Format 'machine-msi' -Version '0.1.0'
    $path = Join-Path $fixture.PackageDir $name
    Add-Content -Path $path -Value 'tampered'
    Assert-Throws { Invoke-Candidate -Fixture $fixture -OutDir (Join-Path $root 'candidate') } 'does not match package-summary'
}
Test-Case 'candidate manifest rejects absolute path in toolchain' {
    $root = Join-Path $script:Dirty 'case-toolchain'
    $fixture = New-Fixture -Root $root
    # Toolchain is generated internally, so this exercises the model guard directly.
    $m = Read-ReleaseManifestText -Text (Read-Utf8Text -Path (Join-Path $script:Dirty 'case-valid/candidate/release-manifest.json'))
    $m.Manifest.toolchain = [ordered]@{ rustc = 'C:\Users\x\rustc.exe' }
    Assert-Throws { Assert-NoUnsafeManifestContent -Value $m.Manifest } 'absolute path'
}

if (Test-Path $script:Dirty) { Remove-Item -LiteralPath $script:Dirty -Recurse -Force -ErrorAction SilentlyContinue }
Write-Host ''
Write-Host "$($script:Total - $script:Failed)/$($script:Total) candidate tests passed."
if ($script:Failed -gt 0) {
    $script:Failures | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}
exit 0
