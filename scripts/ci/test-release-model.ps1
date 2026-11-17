<#
.SYNOPSIS
    Table-driven tests for the pure release model: inputs, versions, source
    refs, asset whitelist, SHA256SUMS, manifest, state transitions, signing
    policy, environment approval records, path safety and evidence schemas.
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
    try {
        & $Body
        Write-Host "ok   $Name"
    }
    catch {
        $script:Failed++
        $script:Failures.Add("$Name :: $($_.Exception.Message)")
        Write-Host "FAIL $Name :: $($_.Exception.Message)" -ForegroundColor Red
    }
}

function Assert-True {
    param([object]$Condition, [string]$Message = 'expected true')
    if (-not $Condition) { throw $Message }
}

function Assert-Equal {
    param([object]$Expected, [object]$Actual, [string]$Message = 'values differ')
    if ("$Expected" -cne "$Actual") { throw "$Message (expected '$Expected', actual '$Actual')" }
}

function Assert-Throws {
    param([scriptblock]$Body, [string]$Pattern = '')
    $threw = $false
    try { & $Body }
    catch {
        $threw = $true
        if ($Pattern -and $_.Exception.Message -notmatch $Pattern) {
            throw "threw the wrong error: $($_.Exception.Message)"
        }
    }
    if (-not $threw) { throw 'expected an exception but none was thrown' }
}

$script:ShaA = 'a' * 40
$script:ShaB = 'b' * 40
$script:HashA = 'a' * 64
$script:HashB = 'b' * 64

function New-TestArtifact {
    param([string]$Format, [string]$Hash, [long]$Size = 100, [switch]$Internal)
    $name = Get-ReleaseAssetName -Format $Format -Version '0.1.0'
    [pscustomobject]@{
        name    = $name
        format  = $Format
        role    = $(if ($Internal) { 'internal' } else { 'release' })
        internal = [bool]$Internal
        size    = [long]$Size
        sha256  = $Hash
        signing = [pscustomobject]@{
            authenticode   = $(if ($Format -eq 'portable-zip') { 'not-applicable' } else { 'unsigned' })
            timestamped    = $false
            payloadsSigned = $false
        }
    }
}

function New-TestEvidence {
    param([string]$Commit = $script:ShaA)
    [pscustomobject]@{
        Rust = [pscustomobject]@{
            status = 'passed'; commit = $Commit
            tests  = [pscustomobject]@{ suites = 2; discovered = 5; executed = 5; passed = 5; failed = 0; ignored = 0; filtered = 0 }
        }
        Windows = [pscustomobject]@{
            status = 'passed'; commit = $Commit; configuration = 'Release'; platform = 'x64'
            runtimeIdentifier = 'win-x64'; publishReadyToRun = $true; nativeDllSha256 = $script:HashA
            mandatorySuitesExecuted = $true
            tests = [pscustomobject]@{ discovered = 4; executed = 4; passed = 4; failed = 0; skipped = 0; inconclusive = 0 }
        }
    }
}

function New-TestManifest {
    param([string[]]$Formats = @('machine-msi', 'user-exe', 'portable-zip'), [string]$SigningMode = 'None', [string]$SigningStatus = 'unsigned', [bool]$Trusted = $false, [bool]$Timestamped = $false)
    $artifacts = @()
    foreach ($format in $Formats) {
        $artifacts += New-TestArtifact -Format $format -Hash $script:HashA
    }
    $evidence = New-TestEvidence
    New-ReleaseManifest -Version '0.1.0' -SourceCommit $script:ShaA -Formats $Formats -SigningMode $SigningMode -SigningStatus $SigningStatus -Trusted $Trusted -Timestamped $Timestamped -SourceRunId 100 -SourceRunAttempt 1 -CandidateArtifactName 'windows-release-candidate-100-1' -Artifacts $artifacts -RustEvidence $evidence.Rust -WindowsEvidence $evidence.Windows -Toolchain @{ rustc = 'rustc 1.0'; dotnet = '10.0' }
}

# ---------------------------------------------------------------------------
# source_ref / version / formats
# ---------------------------------------------------------------------------
Test-Case 'source main' {
    $r = Resolve-SourceRefKind 'main'
    Assert-Equal 'main' $r.Kind
}
Test-Case 'source full SHA normalizes to lowercase' {
    $r = Resolve-SourceRefKind ('A' * 40)
    Assert-Equal 'sha' $r.Kind
    Assert-Equal ('a' * 40) $r.Sha
}
Test-Case 'source tag extracts version' {
    $r = Resolve-SourceRefKind 'clipbridge-windows-0.1.0'
    Assert-Equal 'tag' $r.Kind
    Assert-Equal '0.1.0' $r.TagVersion
}
Test-Case 'source rejects abbreviated SHA' {
    Assert-Throws { Resolve-SourceRefKind 'abc1234' } 'full 40-hex'
}
Test-Case 'source rejects arbitrary tag' {
    Assert-Throws { Resolve-SourceRefKind 'v1.2.3' } 'clipbridge-windows'
}
Test-Case 'source rejects surrounding whitespace' {
    Assert-Throws { Resolve-SourceRefKind ' main' } 'whitespace'
}
Test-Case 'source rejects empty' {
    Assert-Throws { Resolve-SourceRefKind '' } 'must not be empty'
}
Test-Case 'version parses and compares' {
    $v = ConvertTo-ReleaseVersion '0.1.0'
    Assert-Equal 0 $v.Major
    Assert-Equal -1 (Compare-ReleaseVersion -Left '0.1.0' -Right '0.1.1')
    Assert-Equal 0 (Compare-ReleaseVersion -Left '1.2.3' -Right '1.2.3')
}
Test-Case 'version rejects two parts' { Assert-Throws { ConvertTo-ReleaseVersion '1.2' } 'three numeric' }
Test-Case 'version rejects leading zero' { Assert-Throws { ConvertTo-ReleaseVersion '01.2.3' } 'three numeric' }
Test-Case 'version rejects four parts' { Assert-Throws { ConvertTo-ReleaseVersion '1.2.3.4' } 'three numeric' }
Test-Case 'version rejects major > 255' { Assert-Throws { ConvertTo-ReleaseVersion '256.0.0' } 'exceeds' }
Test-Case 'version rejects build > 65535' { Assert-Throws { ConvertTo-ReleaseVersion '0.0.65536' } 'exceeds' }
Test-Case 'format set canonicalizes order and duplicates' {
    $r = Resolve-ReleaseFormatSet @('portable-zip', 'machine-msi', 'portable-zip')
    Assert-Equal 'machine-msi,portable-zip' ($r -join ',')
}
Test-Case 'format set rejects empty' { Assert-Throws { Resolve-ReleaseFormatSet @() } 'At least one' }
Test-Case 'format set rejects internal user MSI' { Assert-Throws { Resolve-ReleaseFormatSet @('user-msi') } 'internal' }
Test-Case 'format set rejects MSIX' { Assert-Throws { Resolve-ReleaseFormatSet @('msix') } 'Unknown' }
Test-Case 'format booleans map to selection' {
    $r = Get-ReleaseFormatsFromBooleans -MachineMsi $false -UserExe $true -PortableZip $false
    Assert-Equal 'user-exe' ($r -join ',')
}

# ---------------------------------------------------------------------------
# asset whitelist
# ---------------------------------------------------------------------------
Test-Case 'asset names are fixed' {
    Assert-Equal 'ClipBridge-0.1.0-win-x64-machine.msi' (Get-ReleaseAssetName -Format machine-msi -Version '0.1.0')
    Assert-Equal 'ClipBridge-0.1.0-win-x64-user.exe' (Get-ReleaseAssetName -Format user-exe -Version '0.1.0')
    Assert-Equal 'ClipBridge-0.1.0-win-x64-portable.zip' (Get-ReleaseAssetName -Format portable-zip -Version '0.1.0')
}
Test-Case 'public asset set includes metadata' {
    $set = Get-ReleasePublicAssetNames -Version '0.1.0' -Formats @('portable-zip')
    Assert-Equal 'ClipBridge-0.1.0-win-x64-portable.zip,SHA256SUMS.txt,release-manifest.json' ($set -join ',')
}
Test-Case 'asset set rejects extra file' {
    Assert-Throws { Assert-ReleaseAssetSet -ActualNames @('extra.txt') -Version '0.1.0' -Formats @('portable-zip') } 'not in the fixed'
}
Test-Case 'asset set rejects internal user MSI explicitly' {
    Assert-Throws { Assert-ReleaseAssetSet -ActualNames @('ClipBridge-0.1.0-win-x64-user.msi') -Version '0.1.0' -Formats @('user-exe') } 'Internal user MSI'
}
Test-Case 'asset set rejects PDB' {
    Assert-Throws { Assert-ReleaseAssetSet -ActualNames @('x.pdb') -Version '0.1.0' -Formats @('portable-zip') } 'outside the public whitelist'
}
Test-Case 'asset set rejects MSIX' {
    Assert-Throws { Assert-ReleaseAssetSet -ActualNames @('x.msix') -Version '0.1.0' -Formats @('portable-zip') } 'outside the public whitelist'
}
Test-Case 'asset set rejects path separators' {
    Assert-Throws { Assert-ReleaseAssetSet -ActualNames @('..\evil.exe') -Version '0.1.0' -Formats @('portable-zip') } 'plain file name'
}
Test-Case 'asset set allows missing for a partial draft' {
    $r = Assert-ReleaseAssetSet -ActualNames @('release-manifest.json') -Version '0.1.0' -Formats @('portable-zip') -AllowMissing
    Assert-True (-not $r.Complete)
    Assert-Equal 'ClipBridge-0.1.0-win-x64-portable.zip,SHA256SUMS.txt' ($r.Missing -join ',')
}
Test-Case 'asset set fails missing without AllowMissing' {
    Assert-Throws { Assert-ReleaseAssetSet -ActualNames @('release-manifest.json') -Version '0.1.0' -Formats @('portable-zip') } 'missing required assets'
}

# ---------------------------------------------------------------------------
# SHA256SUMS
# ---------------------------------------------------------------------------
Test-Case 'SHA256SUMS is sorted and covers manifest, never itself' {
    $entries = @(
        [pscustomobject]@{ name = 'release-manifest.json'; sha256 = $script:HashB },
        [pscustomobject]@{ name = 'ClipBridge-0.1.0-win-x64-portable.zip'; sha256 = $script:HashA }
    )
    $text = New-Sha256SumsText -Entries $entries
    Assert-True ($text -match "(?m)^$script:HashA  ClipBridge-0\.1\.0-win-x64-portable\.zip$")
    Assert-True ($text -match "(?m)^$script:HashB  release-manifest\.json$")
    Assert-True ($text -notmatch 'SHA256SUMS\.txt')
    $parsed = Test-Sha256SumsText -Text $text -ExpectedEntries $entries
    Assert-Equal 2 $parsed.Entries.Count
}
Test-Case 'SHA256SUMS rejects itself' {
    Assert-Throws { New-Sha256SumsText -Entries @([pscustomobject]@{ name = 'SHA256SUMS.txt'; sha256 = $script:HashA }, [pscustomobject]@{ name = 'release-manifest.json'; sha256 = $script:HashB }) } 'must not hash itself'
}
Test-Case 'SHA256SUMS rejects duplicate' {
    Assert-Throws { New-Sha256SumsText -Entries @([pscustomobject]@{ name = 'a.msi'; sha256 = $script:HashA }, [pscustomobject]@{ name = 'a.msi'; sha256 = $script:HashA }, [pscustomobject]@{ name = 'release-manifest.json'; sha256 = $script:HashB }) } 'duplicated'
}
Test-Case 'SHA256SUMS requires manifest' {
    Assert-Throws { New-Sha256SumsText -Entries @([pscustomobject]@{ name = 'a.msi'; sha256 = $script:HashA }) } 'must cover release-manifest.json'
}
Test-Case 'SHA256SUMS detects tampered package hash' {
    $text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  release-manifest.json`nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  a.msi`n"
    Assert-Throws { Test-Sha256SumsText -Text $text -ExpectedEntries @([pscustomobject]@{ name = 'release-manifest.json'; sha256 = $script:HashA }, [pscustomobject]@{ name = 'a.msi'; sha256 = $script:HashA }) } 'hash mismatch'
}
Test-Case 'SHA256SUMS rejects unexpected entry' {
    $text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  release-manifest.json`nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  extra.msi`n"
    Assert-Throws { Test-Sha256SumsText -Text $text -ExpectedEntries @([pscustomobject]@{ name = 'release-manifest.json'; sha256 = $script:HashA }) } 'unexpected entry'
}
Test-Case 'SHA256SUMS rejects malformed line' {
    Assert-Throws { Test-Sha256SumsText -Text "not-a-checksum`n" } 'Malformed'
}

# ---------------------------------------------------------------------------
# manifest
# ---------------------------------------------------------------------------
Test-Case 'manifest builds, serializes and reads back' {
    $m = New-TestManifest
    $json = ConvertTo-ReleaseManifestJson -Manifest $m
    $parsed = Read-ReleaseManifestText -Text $json
    Assert-Equal $m.version $parsed.Manifest.version
    Assert-Equal 64 $parsed.Digest.Length
    Assert-True $parsed.Complete
}
Test-Case 'manifest digest is deterministic' {
    $a = ConvertTo-ReleaseManifestJson -Manifest (New-TestManifest)
    $b = ConvertTo-ReleaseManifestJson -Manifest (New-TestManifest)
    Assert-Equal $a $b
    Assert-Equal (Get-ReleaseManifestDigest -ManifestText $a) (Get-ReleaseManifestDigest -ManifestText $b)
}
Test-Case 'manifest rejects zero executed tests' {
    $evidence = New-TestEvidence
    $evidence.Rust.tests.executed = 0
    Assert-Throws {
        New-ReleaseManifest -Version '0.1.0' -SourceCommit $script:ShaA -Formats @('portable-zip') -SigningMode None -SigningStatus unsigned -Trusted $false -Timestamped $false -SourceRunId 1 -SourceRunAttempt 1 -CandidateArtifactName 'windows-release-candidate-1-1' -Artifacts @(New-TestArtifact portable-zip $script:HashA) -RustEvidence $evidence.Rust -WindowsEvidence $evidence.Windows
    } 'zero executed'
}
Test-Case 'manifest rejects unsigned but trusted' {
    Assert-Throws { New-TestManifest -SigningStatus unsigned -Trusted $true } 'cannot be trusted'
}
Test-Case 'manifest rejects internal artifact' {
    $internal = New-TestArtifact -Format 'user-msi' -Hash $script:HashA -Internal
    Assert-Throws {
        New-ReleaseManifest -Version '0.1.0' -SourceCommit $script:ShaA -Formats @('user-exe') -SigningMode None -SigningStatus unsigned -Trusted $false -Timestamped $false -SourceRunId 1 -SourceRunAttempt 1 -CandidateArtifactName 'windows-release-candidate-1-1' -Artifacts @((New-TestArtifact user-exe $script:HashA), $internal) -RustEvidence (New-TestEvidence).Rust -WindowsEvidence (New-TestEvidence).Windows
    } 'internal'
}
Test-Case 'manifest rejects unselected format' {
    Assert-Throws {
        New-ReleaseManifest -Version '0.1.0' -SourceCommit $script:ShaA -Formats @('portable-zip') -SigningMode None -SigningStatus unsigned -Trusted $false -Timestamped $false -SourceRunId 1 -SourceRunAttempt 1 -CandidateArtifactName 'windows-release-candidate-1-1' -Artifacts @(New-TestArtifact machine-msi $script:HashA) -RustEvidence (New-TestEvidence).Rust -WindowsEvidence (New-TestEvidence).Windows
    } 'not a selected'
}
Test-Case 'manifest rejects absolute path in toolchain' {
    $m = New-TestManifest
    $m.toolchain = [ordered]@{ rustc = 'C:\Users\someone\rustc.exe' }
    Assert-Throws { Assert-NoUnsafeManifestContent -Value $m } 'absolute path'
}
Test-Case 'manifest reader rejects sourceRun workflow mismatch' {
    $m = New-TestManifest
    $m.sourceRun.workflowPath = '.github/workflows/other.yml'
    Assert-Throws { Read-ReleaseManifestText -Text (ConvertTo-ReleaseManifestJson -Manifest $m) } 'manual main release workflow'
}
Test-Case 'manifest reader rejects duplicate package entries' {
    $m = New-TestManifest -Formats @('machine-msi', 'portable-zip')
    $m.artifacts = @($m.artifacts[0], $m.artifacts[0], $m.artifacts[1])
    Assert-Throws { Read-ReleaseManifestText -Text (ConvertTo-ReleaseManifestJson -Manifest $m) } 'duplicate package'
}
Test-Case 'manifest reader rejects missing selected artifact' {
    $m = New-TestManifest -Formats @('machine-msi', 'portable-zip')
    $m.artifacts = @($m.artifacts | Where-Object { $_.name -match 'machine' })
    Assert-Throws { Read-ReleaseManifestText -Text (ConvertTo-ReleaseManifestJson -Manifest $m) } 'missing selected package'
}
Test-Case 'candidate identity matches and detects signing change' {
    $parsed = Read-ReleaseManifestText -Text (ConvertTo-ReleaseManifestJson -Manifest (New-TestManifest))
    $id = Get-CandidateIdentity -Manifest $parsed.Manifest
    Assert-True (Test-CandidateIdentity -Expected $id -Actual $id -ExpectedDigest $parsed.Digest -ActualDigest $parsed.Digest).Ok
    $other = Get-CandidateIdentity -Manifest $parsed.Manifest
    $other.signingPolicy = 'Certificate/signed/trusted=True/timestamped=True'
    Assert-True (-not (Test-CandidateIdentity -Expected $id -Actual $other).Ok)
}

# ---------------------------------------------------------------------------
# state transitions
# ---------------------------------------------------------------------------
$formats = @('machine-msi', 'user-exe', 'portable-zip')
Test-Case 'no release + draft -> build-new, no publish' {
    $t = Get-ReleaseTransition -State none -ReleaseKind draft -Signed $false -Complete $true -RequestedFormats $formats
    Assert-Equal 'build-new' $t.Mode
    Assert-Equal 'none' $t.Publish
}
Test-Case 'no release + unsigned prerelease without flag is rejected' {
    Assert-Throws { Get-ReleaseTransition -State none -ReleaseKind prerelease -Signed $false -Complete $true -RequestedFormats $formats } 'allow_unsigned_prerelease'
}
Test-Case 'no release + unsigned prerelease with flag is allowed' {
    $t = Get-ReleaseTransition -State none -ReleaseKind prerelease -Signed $false -Complete $true -RequestedFormats $formats -AllowUnsignedPrerelease $true
    Assert-Equal 'build-new' $t.Mode
    Assert-Equal 'prerelease' $t.Publish
}
Test-Case 'no release + unsigned stable is rejected' {
    Assert-Throws { Get-ReleaseTransition -State none -ReleaseKind stable -Signed $false -Complete $true -RequestedFormats $formats } 'production-signed'
}
Test-Case 'complete draft + draft -> reuse, no publish' {
    $t = Get-ReleaseTransition -State draft -ReleaseKind draft -Signed $false -Complete $true -ExistingFormats $formats -RequestedFormats $formats
    Assert-Equal 'reuse-candidate' $t.Mode
    Assert-Equal 'none' $t.Publish
}
Test-Case 'draft + prerelease unsigned with flag -> reuse publish' {
    $t = Get-ReleaseTransition -State draft -ReleaseKind prerelease -Signed $false -Complete $true -ExistingFormats $formats -RequestedFormats $formats -AllowUnsignedPrerelease $true
    Assert-Equal 'reuse-candidate' $t.Mode
    Assert-Equal 'prerelease' $t.Publish
}
Test-Case 'draft + stable unsigned is rejected' {
    Assert-Throws { Get-ReleaseTransition -State draft -ReleaseKind stable -Signed $false -Complete $true -ExistingFormats $formats -RequestedFormats $formats } 'new version'
}
Test-Case 'signed draft + stable -> reuse with approval' {
    $t = Get-ReleaseTransition -State draft -ReleaseKind stable -Signed $true -Complete $true -ExistingFormats $formats -RequestedFormats $formats
    Assert-Equal 'reuse-candidate' $t.Mode
    Assert-Equal 'stable' $t.Publish
    Assert-True $t.RequiresApproval
}
Test-Case 'published prerelease + prerelease -> already-complete' {
    $t = Get-ReleaseTransition -State prerelease -ReleaseKind prerelease -Signed $true -Complete $true -ExistingFormats $formats -RequestedFormats $formats
    Assert-Equal 'already-complete' $t.Mode
}
Test-Case 'unsigned published prerelease + stable is rejected' {
    Assert-Throws { Get-ReleaseTransition -State prerelease -ReleaseKind stable -Signed $false -Complete $true -ExistingFormats $formats -RequestedFormats $formats } 'new version'
}
Test-Case 'signed published prerelease + stable -> reuse with approval' {
    $t = Get-ReleaseTransition -State prerelease -ReleaseKind stable -Signed $true -Complete $true -ExistingFormats $formats -RequestedFormats $formats
    Assert-Equal 'reuse-candidate' $t.Mode
    Assert-Equal 'stable' $t.Publish
    Assert-True $t.RequiresApproval
}
Test-Case 'published stable + stable -> already-complete' {
    $t = Get-ReleaseTransition -State stable -ReleaseKind stable -Signed $true -Complete $true -ExistingFormats $formats -RequestedFormats $formats
    Assert-Equal 'already-complete' $t.Mode
}
Test-Case 'published stable + prerelease is rejected' {
    Assert-Throws { Get-ReleaseTransition -State stable -ReleaseKind prerelease -Signed $true -Complete $true -ExistingFormats $formats -RequestedFormats $formats } 'downgrade'
}
Test-Case 'published stable + draft is rejected' {
    Assert-Throws { Get-ReleaseTransition -State stable -ReleaseKind draft -Signed $true -Complete $true -ExistingFormats $formats -RequestedFormats $formats } 'hide'
}
Test-Case 'published prerelease + draft is rejected' {
    Assert-Throws { Get-ReleaseTransition -State prerelease -ReleaseKind draft -Signed $true -Complete $true -ExistingFormats $formats -RequestedFormats $formats } 'hide'
}
Test-Case 'format set change is rejected' {
    Assert-Throws { Get-ReleaseTransition -State draft -ReleaseKind draft -Signed $false -Complete $true -ExistingFormats @('portable-zip') -RequestedFormats $formats } 'cannot be added or removed'
}
Test-Case 'published incomplete release is rejected' {
    Assert-Throws { Get-ReleaseTransition -State prerelease -ReleaseKind prerelease -Signed $true -Complete $false -ExistingFormats $formats -RequestedFormats $formats } 'missing assets'
}

# ---------------------------------------------------------------------------
# signing policy
# ---------------------------------------------------------------------------
Test-Case 'signing: None draft allowed' {
    Assert-True (Get-ReleaseSigningPolicy -ReleaseKind draft -SigningMode None -SigningStatus unsigned).Allowed
}
Test-Case 'signing: None prerelease requires explicit flag' {
    Assert-Throws { Get-ReleaseSigningPolicy -ReleaseKind prerelease -SigningMode None -SigningStatus unsigned } 'allow_unsigned_prerelease'
}
Test-Case 'signing: None prerelease with flag allowed and marked unsigned' {
    $p = Get-ReleaseSigningPolicy -ReleaseKind prerelease -SigningMode None -SigningStatus unsigned -AllowUnsignedPrerelease $true
    Assert-True $p.UnsignedPublic
}
Test-Case 'signing: None stable rejected' {
    Assert-Throws { Get-ReleaseSigningPolicy -ReleaseKind stable -SigningMode None -SigningStatus unsigned } 'production signature'
}
Test-Case 'signing: Certificate signed/trusted/timestamped stable allowed' {
    Assert-True (Get-ReleaseSigningPolicy -ReleaseKind stable -SigningMode Certificate -SigningStatus signed -Trusted $true -Timestamped $true).Allowed
}
Test-Case 'signing: Certificate unsigned is a silent-downgrade error' {
    Assert-Throws { Get-ReleaseSigningPolicy -ReleaseKind draft -SigningMode Certificate -SigningStatus unsigned } 'silent downgrade'
}
Test-Case 'signing: Certificate signed but untrusted rejected' {
    Assert-Throws { Get-ReleaseSigningPolicy -ReleaseKind stable -SigningMode Certificate -SigningStatus signed -Trusted $false -Timestamped $true } 'silent downgrade'
}

# ---------------------------------------------------------------------------
# approval records
# ---------------------------------------------------------------------------
function New-Review {
    param(
        [string]$State = 'approved',
        [string]$Login = 'alice',
        [string]$Comment = '',
        [string]$Environment = 'windows-release-stable'
    )
    if (-not $Comment) {
        $Comment = "nonce=$($script:NonceA) candidate=$($script:HashA) run_id=500 run_attempt=2 result=pass record=https://example.invalid/acceptance/1"
    }
    # Deliberately the real public approval shape: state/comment/environments/user.
    # It has no review timestamp and no run_attempt field.
    return [pscustomobject]@{
        state        = $State
        comment      = $Comment
        environments = @([pscustomobject]@{ name = $Environment })
        user         = [pscustomobject]@{ login = $Login }
    }
}
$script:StageAt = '2026-11-17T06:00:00+00:00'
$script:GateAt = '2026-11-17T06:10:00+00:00'
$script:NonceA = 'a' * 64
$script:NonceB = 'b' * 64
$ApprovalArgs = @{
    RequiredReviewerLogins = @('alice')
    CandidateDigest        = $script:HashA
    ApprovalNonce          = $script:NonceA
    RunId                  = 500
    RunAttempt             = 2
    StageCompletedAt       = $script:StageAt
    GateJobStartedAt       = $script:GateAt
}

Test-Case 'approval real API shape without timestamp passes with fresh nonce' {
    $review = New-Review
    Assert-True (-not ($review.PSObject.Properties.Name -contains 'submitted_at'))
    Assert-True (-not ($review.PSObject.Properties.Name -contains 'run_attempt'))
    $r = Test-ApprovalRecords -Reviews @($review) @ApprovalArgs
    Assert-True $r.Ok
    Assert-Equal 'alice' $r.Approvals[0].reviewer
    Assert-Equal $script:NonceA $r.Nonce
    Assert-Equal 500 $r.RunId
    Assert-Equal 2 $r.RunAttempt
    Assert-Equal $script:GateAt $r.GateJobStartedAt
}
Test-Case 'approval missing records fail closed' {
    Assert-Throws { Test-ApprovalRecords -Reviews @() @ApprovalArgs } 'missing'
}
Test-Case 'approval rejection blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -State 'rejected') @ApprovalArgs } 'rejected'
}
Test-Case 'approval for a different environment does not count' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Environment 'other') @ApprovalArgs } 'No environment review'
}
Test-Case 'approval from a non-required reviewer blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Login 'mallory') @ApprovalArgs } 'not from a configured required reviewer'
}
Test-Case 'approval without a nonce blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "candidate=$($script:HashA) run_id=500 run_attempt=2 result=pass record=https://example.invalid/x") @ApprovalArgs } 'current fresh nonce'
}
Test-Case 'approval with an old nonce (same candidate) blocks replay' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "nonce=$($script:NonceB) candidate=$($script:HashA) run_id=500 run_attempt=2 result=pass record=https://example.invalid/x") @ApprovalArgs } 'current fresh nonce'
}
Test-Case 'mixed old rejected/approved records plus one current approval passes' {
    $oldApproved = New-Review -Comment "nonce=$($script:NonceB) candidate=$($script:HashA) run_id=499 run_attempt=1 result=pass record=https://example.invalid/old"
    $oldRejected = New-Review -State 'rejected' -Comment "nonce=$($script:NonceB) candidate=$($script:HashA) run_id=498 run_attempt=1 result=fail record=https://example.invalid/old-reject"
    $current = New-Review
    $r = Test-ApprovalRecords -Reviews @($oldApproved, $oldRejected, $current) @ApprovalArgs
    Assert-True $r.Ok
    Assert-Equal 1 $r.Approvals.Count
    Assert-Equal $script:NonceA $r.Approvals[0].nonce
}
Test-Case 'old invalid-reviewer approval does not block a current valid approval' {
    $old = New-Review -Login 'mallory' -Comment "nonce=$($script:NonceB) candidate=$($script:HashA) run_id=499 run_attempt=1 result=pass record=https://example.invalid/old"
    $r = Test-ApprovalRecords -Reviews @($old, (New-Review)) @ApprovalArgs
    Assert-True $r.Ok
    Assert-Equal 'alice' $r.Approvals[0].reviewer
}
Test-Case 'only old records (old attempt same candidate) are rejected' {
    $old = New-Review -Comment "nonce=$($script:NonceB) candidate=$($script:HashA) run_id=500 run_attempt=1 result=pass record=https://example.invalid/old"
    Assert-Throws { Test-ApprovalRecords -Reviews @($old) @ApprovalArgs } 'current fresh nonce'
}
Test-Case 'current nonce from an unauthorized reviewer blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Login 'mallory') @ApprovalArgs } 'not from a configured required reviewer'
}
Test-Case 'rejection carrying the current nonce blocks even with a current approval' {
    $current = New-Review
    $rejected = New-Review -State 'rejected'
    Assert-Throws { Test-ApprovalRecords -Reviews @($current, $rejected) @ApprovalArgs } 'rejected the current approval nonce'
}
Test-Case 'multiple current approvals are all accepted' {
    $r = Test-ApprovalRecords -Reviews @((New-Review), (New-Review)) @ApprovalArgs
    Assert-True $r.Ok
    Assert-Equal 2 $r.Approvals.Count
}
Test-Case 'approval with a stale candidate digest blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "nonce=$($script:NonceA) candidate=$($script:HashB) run_id=500 run_attempt=2 result=pass record=https://example.invalid/x") @ApprovalArgs } 'different candidate'
}
Test-Case 'approval for a different run id blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "nonce=$($script:NonceA) candidate=$($script:HashA) run_id=499 run_attempt=2 result=pass record=https://example.invalid/x") @ApprovalArgs } 'run_id=500'
}
Test-Case 'approval for a different run attempt blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "nonce=$($script:NonceA) candidate=$($script:HashA) run_id=500 run_attempt=1 result=pass record=https://example.invalid/x") @ApprovalArgs } 'run_attempt=2'
}
Test-Case 'approval without result=pass blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "nonce=$($script:NonceA) candidate=$($script:HashA) run_id=500 run_attempt=2 record=https://example.invalid/x") @ApprovalArgs } 'result=pass'
}
Test-Case 'approval without https record link blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "nonce=$($script:NonceA) candidate=$($script:HashA) run_id=500 run_attempt=2 result=pass record=http://example.invalid/x") @ApprovalArgs } 'record='
}
Test-Case 'approval gate started before stage blocks' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review) -RequiredReviewerLogins @('alice') -CandidateDigest $script:HashA -ApprovalNonce $script:NonceA -RunId 500 -RunAttempt 2 -StageCompletedAt $script:GateAt -GateJobStartedAt $script:StageAt } 'before the candidate was staged'
}
Test-Case 'unparseable gate time fails closed' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review) -RequiredReviewerLogins @('alice') -CandidateDigest $script:HashA -ApprovalNonce $script:NonceA -RunId 500 -RunAttempt 2 -StageCompletedAt $script:StageAt -GateJobStartedAt 'not-a-time' } 'gate job start'
}
Test-Case 'invalid short nonce fails closed' {
    Assert-Throws { Test-ApprovalRecords -Reviews @(New-Review -Comment "nonce=deadbeef candidate=$($script:HashA) run_id=500 run_attempt=2 result=pass record=https://example.invalid/x") -RequiredReviewerLogins @('alice') -CandidateDigest $script:HashA -ApprovalNonce 'deadbeef' -RunId 500 -RunAttempt 2 -StageCompletedAt $script:StageAt -GateJobStartedAt $script:GateAt } '128 bits'
}
function New-GateJobs {
    param(
        [string]$StageConclusion = 'success',
        [string]$GateConclusion = 'success',
        [string]$StageCompleted = '2026-11-17T06:00:00Z',
        [string]$GateStarted = '2026-11-17T06:00:00Z',
        [int]$StageAttempt = 2,
        [int]$GateAttempt = 2,
        [switch]$DropStage,
        [switch]$DropGate
    )
    $jobs = [System.Collections.Generic.List[object]]::new()
    if (-not $DropStage) {
        $jobs.Add([pscustomobject]@{ id = 1; name = 'stage'; run_id = 500; run_attempt = $StageAttempt; conclusion = $StageConclusion; completed_at = $StageCompleted })
    }
    if (-not $DropGate) {
        $jobs.Add([pscustomobject]@{ id = 2; name = 'approve-stable'; run_id = 500; run_attempt = $GateAttempt; conclusion = $GateConclusion; started_at = $GateStarted })
    }
    return [object[]]$jobs.ToArray()
}
Test-Case 'approval same-second stage completion and gate start are allowed' {
    $sameArgs = $ApprovalArgs.Clone()
    $sameArgs.StageCompletedAt = $script:GateAt
    $r = Test-ApprovalRecords -Reviews @(New-Review) @sameArgs
    Assert-True $r.Ok
}
Test-Case 'GitHub gate timing allows same-second stage/gate records' {
    $t = Get-ApprovalGateTiming -Jobs (New-GateJobs) -ExpectedRunId 500 -ExpectedRunAttempt 2
    Assert-True $t.SameSecond
    Assert-Equal '2026-11-17T06:00:00Z' $t.StageCompletedAt
}
Test-Case 'GitHub gate timing rejects missing stage job' {
    Assert-Throws { Get-ApprovalGateTiming -Jobs (New-GateJobs -DropStage) -ExpectedRunId 500 -ExpectedRunAttempt 2 } 'stage'
}
Test-Case 'GitHub gate timing rejects non-success stage job' {
    Assert-Throws { Get-ApprovalGateTiming -Jobs (New-GateJobs -StageConclusion 'failure') -ExpectedRunId 500 -ExpectedRunAttempt 2 } 'stage'
}
Test-Case 'GitHub gate timing rejects missing approve job' {
    Assert-Throws { Get-ApprovalGateTiming -Jobs (New-GateJobs -DropGate) -ExpectedRunId 500 -ExpectedRunAttempt 2 } 'approve-stable'
}
Test-Case 'GitHub gate timing rejects non-success approve job' {
    Assert-Throws { Get-ApprovalGateTiming -Jobs (New-GateJobs -GateConclusion 'failure') -ExpectedRunId 500 -ExpectedRunAttempt 2 } 'approve-stable'
}
Test-Case 'GitHub gate timing rejects old attempt records' {
    Assert-Throws { Get-ApprovalGateTiming -Jobs (New-GateJobs -StageAttempt 1) -ExpectedRunId 500 -ExpectedRunAttempt 2 } 'run attempt'
}
Test-Case 'GitHub gate timing rejects gate start before stage completion' {
    Assert-Throws { Get-ApprovalGateTiming -Jobs (New-GateJobs -StageCompleted '2026-11-17T06:01:00Z' -GateStarted '2026-11-17T06:00:00Z') -ExpectedRunId 500 -ExpectedRunAttempt 2 } 'before stage completed'
}
Test-Case 'environment without protection rules fails closed' {
    $env = [pscustomobject]@{ name = 'windows-release-stable'; protection_rules = @() }
    Assert-Throws { Get-EnvironmentReviewerPolicy -Environment $env } 'no required_reviewers'
}
Test-Case 'environment with non-reviewer rule fails closed' {
    $env = [pscustomobject]@{ name = 'windows-release-stable'; protection_rules = @([pscustomobject]@{ type = 'wait_timer'; wait_timer = 10 }) }
    Assert-Throws { Get-EnvironmentReviewerPolicy -Environment $env } 'no required_reviewers'
}
Test-Case 'environment with only team reviewers fails closed' {
    $env = [pscustomobject]@{
        name = 'windows-release-stable'
        protection_rules = @([pscustomobject]@{ type = 'required_reviewers'; reviewers = @([pscustomobject]@{ type = 'Team'; reviewer = [pscustomobject]@{ slug = 'release-team' } }) })
    }
    Assert-Throws { Get-EnvironmentReviewerPolicy -Environment $env } 'team reviewers'
}
Test-Case 'environment with required user reviewer is verifiable' {
    $env = [pscustomobject]@{
        name = 'windows-release-stable'
        protection_rules = @([pscustomobject]@{ type = 'required_reviewers'; reviewers = @([pscustomobject]@{ type = 'User'; reviewer = [pscustomobject]@{ login = 'alice' } }) })
    }
    $p = Get-EnvironmentReviewerPolicy -Environment $env
    Assert-True $p.Verifiable
    Assert-Equal 'alice' $p.RequiredReviewerLogins[0]
}
Test-Case 'stable gate rejects unsigned even with approval' {
    $approval = Test-ApprovalRecords -Reviews @(New-Review) @ApprovalArgs
    $policy = [pscustomobject]@{ Name = 'windows-release-stable'; Verifiable = $true }
    Assert-Throws { Assert-StableReleaseGate -SigningMode None -SigningStatus unsigned -Trusted $false -Timestamped $false -EnvironmentPolicy $policy -ApprovalEvidence $approval } 'requires Certificate'
}
Test-Case 'stable gate passes signed + verifiable current nonce' {
    $approval = Test-ApprovalRecords -Reviews @(New-Review) @ApprovalArgs
    $policy = [pscustomobject]@{ Name = 'windows-release-stable'; Verifiable = $true }
    $r = Assert-StableReleaseGate -SigningMode Certificate -SigningStatus signed -Trusted $true -Timestamped $true -EnvironmentPolicy $policy -ApprovalEvidence $approval
    Assert-True $r.Signed
}
Test-Case 'new approval nonces are 256-bit and fresh' {
    $a = New-ApprovalNonce
    $b = New-ApprovalNonce
    Assert-True ($a -cmatch '^[0-9a-f]{64}$')
    Assert-True ($b -cmatch '^[0-9a-f]{64}$')
    Assert-True ($a -cne $b)
}
# ---------------------------------------------------------------------------
# path safety / latest
# ---------------------------------------------------------------------------
Test-Case 'safe relative paths' {
    Assert-True (Test-SafeRelativePath 'ClipBridge-0.1.0-win-x64-portable.zip')
    Assert-True (Test-SafeRelativePath 'dir/file.txt')
    Assert-True (-not (Test-SafeRelativePath '../evil'))
    Assert-True (-not (Test-SafeRelativePath 'C:\evil'))
    Assert-True (-not (Test-SafeRelativePath '\\\\server\\share'))
    Assert-True (-not (Test-SafeRelativePath '/abs'))
    Assert-True (-not (Test-SafeRelativePath 'a\\b'))
}
Test-Case 'Assert-PathUnderRoot accepts child and rejects escape' {
    $root = Join-Path $env:TEMP 'cb-model-root'
    $child = Join-Path $root 'a/b.txt'
    $null = Assert-PathUnderRoot -Path $child -Root $root
    Assert-Throws { Assert-PathUnderRoot -Path (Join-Path $root '..\evil.txt') -Root $root } 'escapes'
}
Test-Case 'latest decision' {
    Assert-Equal 'true' (Get-PublishLatestMode -CurrentStableLatestVersion $null -NewVersion '0.1.0')
    Assert-Equal 'true' (Get-PublishLatestMode -CurrentStableLatestVersion '0.1.0' -NewVersion '0.1.0')
    Assert-Equal 'true' (Get-PublishLatestMode -CurrentStableLatestVersion '0.1.0' -NewVersion '0.2.0')
    Assert-Equal 'false' (Get-PublishLatestMode -CurrentStableLatestVersion '0.2.0' -NewVersion '0.1.0')
}

Test-Case 'unknown current latest does not overwrite latest' {
    Assert-Equal 'false' (Get-PublishLatestMode -CurrentStableLatestVersion 'not-a-windows-version' -NewVersion '0.1.0')
}
Test-Case 'source run jobs must match the exact recorded attempt' {
    $jobs = @(
        [pscustomobject]@{ id = 1; name = 'verify'; run_id = 100; run_attempt = 2; conclusion = 'success' },
        [pscustomobject]@{ id = 2; name = 'build'; run_id = 100; run_attempt = 2; conclusion = 'success' }
    )
    $r = Test-SourceRunJobs -Jobs $jobs -ExpectedRunId 100 -ExpectedRunAttempt 2
    Assert-True $r.Ok
    Assert-Throws { Test-SourceRunJobs -Jobs $jobs -ExpectedRunId 100 -ExpectedRunAttempt 3 } 'run attempt'
    $old = @(
        [pscustomobject]@{ id = 1; name = 'verify'; run_id = 100; run_attempt = 1; conclusion = 'success' },
        [pscustomobject]@{ id = 2; name = 'build'; run_id = 100; run_attempt = 1; conclusion = 'success' }
    )
    Assert-Throws { Test-SourceRunJobs -Jobs $old -ExpectedRunId 100 -ExpectedRunAttempt 2 } 'run attempt'
    $failed = @(
        [pscustomobject]@{ id = 1; name = 'verify'; run_id = 100; run_attempt = 2; conclusion = 'success' },
        [pscustomobject]@{ id = 2; name = 'build'; run_id = 100; run_attempt = 2; conclusion = 'failure' }
    )
    Assert-Throws { Test-SourceRunJobs -Jobs $failed -ExpectedRunId 100 -ExpectedRunAttempt 2 } 'build job'
}
Test-Case 'candidate artifact requires the recorded workflow_run id' {
    $artifact = [pscustomobject]@{
        id = 9; name = 'windows-release-candidate-100-2'; expired = $false; size_in_bytes = 10; digest = "sha256:$($script:HashA)"
        workflow_run = [pscustomobject]@{ id = 100; head_branch = 'main' }
    }
    $r = Test-CandidateArtifactRecord -Artifact $artifact -ExpectedId 9 -ExpectedName 'windows-release-candidate-100-2' -ExpectedRunId 100
    Assert-True $r.Ok
    Assert-Throws { Test-CandidateArtifactRecord -Artifact $artifact -ExpectedId 9 -ExpectedName 'windows-release-candidate-100-2' -ExpectedRunId 101 } 'workflow run'
    $noRun = [pscustomobject]@{ id = 9; name = 'windows-release-candidate-100-2'; expired = $false; size_in_bytes = 10 }
    Assert-Throws { Test-CandidateArtifactRecord -Artifact $noRun -ExpectedId 9 -ExpectedName 'windows-release-candidate-100-2' -ExpectedRunId 100 } 'workflow_run association'
    $expired = [pscustomobject]@{ id = 9; name = 'windows-release-candidate-100-2'; expired = $true; size_in_bytes = 10; workflow_run = [pscustomobject]@{ id = 100 } }
    Assert-Throws { Test-CandidateArtifactRecord -Artifact $expired -ExpectedId 9 -ExpectedName 'windows-release-candidate-100-2' -ExpectedRunId 100 } 'expired'
}
# ---------------------------------------------------------------------------
# verification evidence
# ---------------------------------------------------------------------------
function New-PackageSummaryFixture {
    param([string]$Version = '0.1.0', [string]$Mode = 'None', [string]$Status = 'unsigned')
    $artifacts = @()
    foreach ($format in @('machine-msi', 'user-exe', 'portable-zip')) {
        $artifacts += [pscustomobject]@{
            name = (Get-ReleaseAssetName -Format $format -Version $Version); format = $format; role = 'release'; internal = $false; size = 100; sha256 = $script:HashA
            signing = [pscustomobject]@{ authenticode = $(if ($format -eq 'portable-zip') { 'not-applicable' } else { 'unsigned' }); timestamped = $false; payloadsSigned = $false }
        }
    }
    $artifacts += [pscustomobject]@{
        name = "ClipBridge-$Version-win-x64-user.msi"; format = 'user-msi'; role = 'internal'; internal = $true; size = 100; sha256 = $script:HashA
        signing = [pscustomobject]@{ authenticode = 'unsigned'; timestamped = $false; payloadsSigned = $null }
    }
    [pscustomobject]@{ schemaVersion = 1; product = 'ClipBridge'; version = $Version; platform = 'x64'; formats = @('machine-msi', 'user-exe', 'portable-zip'); signing = [pscustomobject]@{ mode = $Mode; status = $Status; trusted = $false; timestamped = $false }; artifacts = $artifacts }
}
Test-Case 'package summary separates public/internal artifacts' {
    $r = Test-PackageSummary -Summary (New-PackageSummaryFixture) -Version '0.1.0' -Formats @('machine-msi', 'user-exe', 'portable-zip')
    Assert-Equal 3 $r.ReleaseArtifacts.Count
    Assert-Equal 1 $r.InternalArtifacts.Count
    Assert-Equal 'user-msi' $r.InternalArtifacts[0].format
}
Test-Case 'package summary rejects version mismatch' {
    Assert-Throws { Test-PackageSummary -Summary (New-PackageSummaryFixture) -Version '0.2.0' -Formats @('machine-msi', 'user-exe', 'portable-zip') } 'version does not match'
}
Test-Case 'package summary rejects Certificate mode unsigned' {
    $s = New-PackageSummaryFixture -Mode 'Certificate' -Status 'unsigned'
    Assert-Throws { Test-PackageSummary -Summary $s -Version '0.1.0' -Formats @('machine-msi', 'user-exe', 'portable-zip') } 'signed/trusted/timestamped'
}
Test-Case 'rust summary rejects zero execution' {
    $e = New-TestEvidence
    $s = [pscustomobject]@{ schemaVersion = 1; status = 'passed'; commit = $script:ShaA; checks = @([pscustomobject]@{ name = 'test'; exitCode = 0 }); tests = [pscustomobject]@{ suites = 1; discovered = 1; executed = 0; passed = 0; failed = 0; ignored = 0; filtered = 0 } }
    Assert-Throws { Test-RustSummary -Summary $s -ExpectedCommit $script:ShaA } 'zero executed'
}
Test-Case 'windows summary accepts real-shaped complete evidence' {
    $s = [pscustomobject]@{
        schemaVersion = 1; status = 'passed'; exitCode = 0; commit = $script:ShaA; dirty = $false; configuration = 'Release'; platform = 'x64'; runtimeIdentifier = 'win-x64'; publishReadyToRun = $true
        nativeEvidence = [pscustomobject]@{ locked = $true; dllSha256 = $script:HashA; dllSizeBytes = 100; machine = '0x8664'; cargoExitCode = 0; buildExitCode = 0 }
        formatEvidence = [pscustomobject]@{ verified = $true; projects = @([pscustomobject]@{ name = 'core'; exitCode = 0 }, [pscustomobject]@{ name = 'shell'; exitCode = 0 }) }
        nugetEvidence = [pscustomobject]@{ queryExitCodes = @(0, 0); totalPackagesScanned = 3; vulnerablePackageCount = 0; violations = @(); problems = @() }
        testsEvidence = [pscustomobject]@{ testCommandExitCode = 0; buildExitCode = 0; discovered = 4; executed = 4; passed = 4; failed = 0; skipped = 0; inconclusive = 0; mandatorySuites = @([pscustomobject]@{ name = 'suite'; executed = 1; failed = 0 }); allMandatoryExecuted = $true; problems = @() }
        checks = @([pscustomobject]@{ name = 'build-shell'; exitCode = 0 })
        failure = $null; error = $null
    }
    $r = Test-WindowsSummary -Summary $s -ExpectedCommit $script:ShaA -Formats @('portable-zip')
    Assert-Equal 'passed' $r.status
    Assert-Equal ($script:HashA) $r.nativeDllSha256
}
Test-Case 'windows summary rejects non-x64 machine' {
    $s = [pscustomobject]@{
        schemaVersion = 1; status = 'passed'; exitCode = 0; commit = $script:ShaA; dirty = $false; configuration = 'Release'; platform = 'x64'; runtimeIdentifier = 'win-x64'; publishReadyToRun = $true
        nativeEvidence = [pscustomobject]@{ locked = $true; dllSha256 = $script:HashA; dllSizeBytes = 100; machine = '0xAA64'; cargoExitCode = 0; buildExitCode = 0 }
        formatEvidence = [pscustomobject]@{ verified = $true; projects = @([pscustomobject]@{ name = 'core'; exitCode = 0 }) }
        nugetEvidence = [pscustomobject]@{ queryExitCodes = @(0); totalPackagesScanned = 3; vulnerablePackageCount = 0; violations = @(); problems = @() }
        testsEvidence = [pscustomobject]@{ testCommandExitCode = 0; buildExitCode = 0; discovered = 1; executed = 1; passed = 1; failed = 0; mandatorySuites = @([pscustomobject]@{ name = 's'; executed = 1; failed = 0 }); allMandatoryExecuted = $true; problems = @() }
        checks = @([pscustomobject]@{ name = 'x'; exitCode = 0 })
    }
    Assert-Throws { Test-WindowsSummary -Summary $s -ExpectedCommit $script:ShaA -Formats @('portable-zip') } 'not x64'
}
Test-Case 'windows summary rejects dirty worktree' {
    $s = [pscustomobject]@{
        schemaVersion = 1; status = 'passed'; exitCode = 0; commit = $script:ShaA; dirty = $true; configuration = 'Release'; platform = 'x64'; runtimeIdentifier = 'win-x64'; publishReadyToRun = $true
        nativeEvidence = [pscustomobject]@{ locked = $true; dllSha256 = $script:HashA; dllSizeBytes = 100; machine = '0x8664'; cargoExitCode = 0; buildExitCode = 0 }
        formatEvidence = [pscustomobject]@{ verified = $true; projects = @([pscustomobject]@{ name = 'core'; exitCode = 0 }) }
        nugetEvidence = [pscustomobject]@{ queryExitCodes = @(0); totalPackagesScanned = 3; vulnerablePackageCount = 0; violations = @(); problems = @() }
        testsEvidence = [pscustomobject]@{ testCommandExitCode = 0; buildExitCode = 0; discovered = 1; executed = 1; passed = 1; failed = 0; mandatorySuites = @([pscustomobject]@{ name = 's'; executed = 1; failed = 0 }); allMandatoryExecuted = $true; problems = @() }
        checks = @([pscustomobject]@{ name = 'x'; exitCode = 0 })
    }
    Assert-Throws { Test-WindowsSummary -Summary $s -ExpectedCommit $script:ShaA -Formats @('portable-zip') } 'dirty'
}
Test-Case 'release native evidence normalizes case and sha256 prefix' {
    $expected = ('A' * 64)
    $pkg = [pscustomobject]@{ inputDllSha256 = ('a' * 64); stageDllSha256 = "sha256:$('a' * 64)"; sourceReusedVerified = $true }
    $r = Test-ReleaseNativeEvidence -PackageNativeEvidence $pkg -ExpectedWindowsDllSha256 $expected
    Assert-Equal ('a' * 64) $r.InputDllSha256
    Assert-Equal ('a' * 64) $r.StageDllSha256
    Assert-True $r.SourceReusedVerified
}
Test-Case 'release native evidence rejects wrong/missing/unverified input' {
    Assert-Throws { Test-ReleaseNativeEvidence -PackageNativeEvidence ([pscustomobject]@{ inputDllSha256 = ('b' * 64); stageDllSha256 = ('a' * 64); sourceReusedVerified = $true }) -ExpectedWindowsDllSha256 ('a' * 64) } 'inputDllSha256'
    Assert-Throws { Test-ReleaseNativeEvidence -PackageNativeEvidence ([pscustomobject]@{ inputDllSha256 = ('a' * 64); stageDllSha256 = ('b' * 64); sourceReusedVerified = $true }) -ExpectedWindowsDllSha256 ('a' * 64) } 'stageDllSha256'
    Assert-Throws { Test-ReleaseNativeEvidence -PackageNativeEvidence ([pscustomobject]@{ inputDllSha256 = ('a' * 64); stageDllSha256 = ('a' * 64); sourceReusedVerified = $false }) -ExpectedWindowsDllSha256 ('a' * 64) } 'sourceReusedVerified'
    Assert-Throws { Test-ReleaseNativeEvidence -PackageNativeEvidence $null -ExpectedWindowsDllSha256 ('a' * 64) } 'no nativeEvidence'
    Assert-Throws { Test-ReleaseNativeEvidence -PackageNativeEvidence ([pscustomobject]@{ inputDllSha256 = ('a' * 64); stageDllSha256 = ('a' * 64); sourceReusedVerified = $true }) -ExpectedWindowsDllSha256 'not-a-hash' } '64-hex'
}
Test-Case 'release native evidence rejects truthy non-boolean reuse claims' {
    foreach ($invalid in @('false', 'true', 1)) {
        $pkg = [pscustomobject]@{ inputDllSha256 = ('a' * 64); stageDllSha256 = ('a' * 64); sourceReusedVerified = $invalid }
        Assert-Throws { Test-ReleaseNativeEvidence -PackageNativeEvidence $pkg -ExpectedWindowsDllSha256 ('a' * 64) } 'JSON boolean'
    }
}
Test-Case 'real windows report hash is accepted case-insensitively by native evidence' {
    $realPath = Join-Path $repo 'dist/ci-review/windows/windows-summary.json'
    if (-not (Test-Path -LiteralPath $realPath -PathType Leaf)) {
        Write-Host '     skip: dist/ci-review/windows/windows-summary.json is not present in this checkout'
        return
    }
    $real = Get-Content -Raw -LiteralPath $realPath | ConvertFrom-Json
    $expected = [string]$real.nativeEvidence.dllSha256
    $pkg = [pscustomobject]@{ inputDllSha256 = $expected.ToUpperInvariant(); stageDllSha256 = $expected; sourceReusedVerified = $true }
    $r = Test-ReleaseNativeEvidence -PackageNativeEvidence $pkg -ExpectedWindowsDllSha256 $expected.ToLowerInvariant()
    Assert-Equal $expected.ToLowerInvariant() $r.InputDllSha256
    Assert-Equal $expected.ToLowerInvariant() $r.StageDllSha256
}
Test-Case 'real windows-summary.json is accepted with only dirty cleared in memory' {
    $realPath = Join-Path $repo 'dist/ci-review/windows/windows-summary.json'
    if (-not (Test-Path -LiteralPath $realPath -PathType Leaf)) {
        Write-Host '     skip: dist/ci-review/windows/windows-summary.json is not present in this checkout'
        return
    }
    $before = Get-Content -Raw -LiteralPath $realPath
    $real = $before | ConvertFrom-Json
    Assert-Equal '0x8664' ([string]$real.nativeEvidence.machine)
    $real.dirty = $false
    $r = Test-WindowsSummary -Summary $real -ExpectedCommit ([string]$real.commit) -Formats @('machine-msi', 'user-exe', 'portable-zip')
    Assert-Equal 'passed' $r.status
    Assert-Equal ([string]$real.nativeEvidence.dllSha256).ToLowerInvariant() $r.nativeDllSha256
    $after = Get-Content -Raw -LiteralPath $realPath
    Assert-True ($after -ceq $before) 'the real evidence file must not be modified'
}
Test-Case 'real windows-summary non-x64 and dirty negatives' {
    $realPath = Join-Path $repo 'dist/ci-review/windows/windows-summary.json'
    if (-not (Test-Path -LiteralPath $realPath -PathType Leaf)) {
        Write-Host '     skip: dist/ci-review/windows/windows-summary.json is not present in this checkout'
        return
    }
    $dirty = Get-Content -Raw -LiteralPath $realPath | ConvertFrom-Json
    Assert-Throws { Test-WindowsSummary -Summary $dirty -ExpectedCommit ([string]$dirty.commit) -Formats @('machine-msi', 'user-exe', 'portable-zip') } 'dirty'
    $badMachine = Get-Content -Raw -LiteralPath $realPath | ConvertFrom-Json
    $badMachine.dirty = $false
    $badMachine.nativeEvidence.machine = '0xAA64'
    Assert-Throws { Test-WindowsSummary -Summary $badMachine -ExpectedCommit ([string]$badMachine.commit) -Formats @('machine-msi', 'user-exe', 'portable-zip') } 'not x64'
}

Write-Host ''
Write-Host "$($script:Total - $script:Failed)/$($script:Total) model tests passed."
if ($script:Failed -gt 0) {
    $script:Failures | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}
exit 0
