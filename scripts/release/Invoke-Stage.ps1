#Requires -Version 7.0
<#
.SYNOPSIS
    Stage a release candidate: verify the exact build artifact, create/fill a
    draft release and emit the immutable candidate summary used for approval.

.DESCRIPTION
    stage deliberately distinguishes "build skipped because this is a reuse"
    from "build failed": it receives the actual job results and refuses to run
    for an unexpected skip. Existing assets are never overwritten; a public
    release is read-only and no asset is uploaded to it. All final assets are
    re-downloaded and hashed before the stage summary is emitted.
#>
[CmdletBinding()]
param([ValidateSet('Live', 'DryRun')][string]$Mode = 'Live')

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$scriptDir = $PSScriptRoot
Import-Module (Join-Path $scriptDir 'GitHubReleaseApi.psm1') -Force
# The API module imports ReleaseModel for its internal helpers; re-import it so the
# entrypoint also gets the model commands (a -Force module import replaces the
# caller's reference). Keep this order if you edit imports.
Import-Module (Join-Path $scriptDir 'ReleaseModel.psm1') -Force
Import-Module (Join-Path $scriptDir 'ReleaseRuntime.psm1') -Force

function Get-ExpectedAssetHash {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$ManifestText,
        [Parameter(Mandatory)][string]$ManifestDigest,
        [object[]]$ManifestArtifacts,
        [Parameter(Mandatory)][string]$Directory
    )
    if ($Name -ceq 'release-manifest.json') {
        $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($ManifestText)
        return [pscustomobject]@{ Size = [long]$bytes.Length; Sha256 = $ManifestDigest }
    }
    $path = Join-Path $Directory $Name
    if ($Name -ceq 'SHA256SUMS.txt') {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw (New-ReleaseModelError -Code 'missing-asset' -Message 'SHA256SUMS.txt is missing from the candidate.') }
        return [pscustomobject]@{ Size = [long](Get-Item -LiteralPath $path).Length; Sha256 = (Get-FileSha256 -Path $path) }
    }
    $record = @($ManifestArtifacts | Where-Object { [string](Get-PropValue $_ 'name' '') -ceq $Name })
    if ($record.Count -ne 1) { throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "No unique manifest record for '$Name'.") }
    return [pscustomobject]@{ Size = [long](Get-PropValue $record[0] 'size' 0); Sha256 = ([string](Get-PropValue $record[0] 'sha256' '')).ToLowerInvariant() }
}

function Assert-DownloadedAsset {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][object]$Asset,
        [Parameter(Mandatory)][string]$Directory,
        [Parameter(Mandatory)][string]$ExpectedSha256,
        [Parameter(Mandatory)][long]$ExpectedSize
    )
    $name = [string](Get-PropValue $Asset 'name' '')
    $id = [long](Get-PropValue $Asset 'id' 0)
    $path = Join-Path $Directory (($name -replace '[^A-Za-z0-9._-]', '_') + '.verify')
    $null = Save-GitHubReleaseAsset -Context $Context -AssetId $id -OutFile $path
    $hash = Get-FileSha256 -Path $path
    $size = [long](Get-Item -LiteralPath $path).Length
    if ($hash -cne $ExpectedSha256) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Release asset '$name' does not match the candidate hash (no clobber allowed).")
    }
    if ($size -ne $ExpectedSize) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Release asset '$name' size does not match the candidate.")
    }
    $apiDigest = [string](Get-PropValue $Asset 'digest' '')
    if ($apiDigest -cmatch '^sha256:([0-9a-f]{64})$' -and $Matches[1] -cne $ExpectedSha256) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Release asset '$name' API digest disagrees with the downloaded bytes.")
    }
    return [pscustomobject]@{ Name = $name; Id = $id; Size = $size; Sha256 = $hash }
}

$exitCode = 1
$summary = [System.Collections.Generic.List[string]]::new()
$tempDir = $null
try {
    $ref = Get-RequiredEnv 'GITHUB_REF'
    if ($ref -cne 'refs/heads/main') { throw (New-ReleaseModelError -Code 'invalid-workflow-ref' -Message "stage must run from refs/heads/main; got '$ref'.") }
    $plan = Read-ReleasePlanEnvironment
    $planMode = [string](Get-PropValue $plan 'mode' '')
    if ($planMode -cnotin @('build-new', 'reuse-candidate')) {
        throw (New-ReleaseModelError -Code 'invalid-plan' -Message "stage received unsupported plan mode '$planMode'.")
    }
    $runId = [long](Get-RequiredEnv 'GITHUB_RUN_ID')
    $runAttempt = [int](Get-RequiredEnv 'GITHUB_RUN_ATTEMPT')
    $verifyResult = Get-RequiredEnv 'VERIFY_RESULT'
    $buildResult = Get-RequiredEnv 'BUILD_RESULT'
    $sourceSha = [string](Get-PropValue $plan 'sourceSha' '')
    if ($sourceSha -cnotmatch '^[0-9a-f]{40}$') { throw (New-ReleaseModelError -Code 'invalid-plan' -Message 'Plan sourceSha is invalid.') }
    $version = [string](Get-PropValue $plan 'version' '')
    $tag = [string](Get-PropValue $plan 'tag' '')
    if ($tag -cne (Get-ReleaseTag -Version $version)) { throw (New-ReleaseModelError -Code 'invalid-plan' -Message 'Plan tag/version mismatch.') }
    $formats = Resolve-ReleaseFormatSet -Formats ([string[]]@(Get-PropValue $plan 'formats' @()))
    $releaseKind = [string](Get-PropValue $plan 'releaseKind' '')
    $allowUnsigned = [bool](Get-PropValue $plan 'allowUnsignedPrerelease' $false)
    if ($releaseKind -cnotin @('draft', 'prerelease', 'stable')) { throw (New-ReleaseModelError -Code 'invalid-plan' -Message 'Plan releaseKind is invalid.') }

    $api = New-ReleaseApiContextFromEnv -Mode $Mode
    if ($planMode -ceq 'build-new') {
        if ($verifyResult -cne 'success' -or $buildResult -cne 'success') {
            throw (New-ReleaseModelError -Code 'gate-bypass' -Message "build-new requires verify and build success; got verify=$verifyResult build=$buildResult.")
        }
        $artifactName = "windows-release-candidate-$runId-$runAttempt"
        $artifactId = [long](Get-RequiredEnv 'BUILD_ARTIFACT_ID')
        $expectedManifestDigest = ''
        $expectedSourceRunId = $runId
        $expectedSourceRunAttempt = $runAttempt
    }
    else {
        if ($verifyResult -cne 'success') {
            throw (New-ReleaseModelError -Code 'gate-bypass' -Message "reuse-candidate requires verify success; got verify=$verifyResult.")
        }
        if ($buildResult -cne 'skipped') {
            throw (New-ReleaseModelError -Code 'gate-bypass' -Message "reuse-candidate requires the build job to be intentionally skipped; got build=$buildResult.")
        }
        $candidatePlan = Get-PropValue $plan 'candidate' $null
        if ($null -eq $candidatePlan) { throw (New-ReleaseModelError -Code 'invalid-plan' -Message 'reuse-candidate plan has no candidate provenance.') }
        $artifactId = [long](Get-PropValue $candidatePlan 'artifactId' 0)
        $artifactName = [string](Get-PropValue $candidatePlan 'artifactName' '')
        $expectedManifestDigest = [string](Get-PropValue $candidatePlan 'manifestSha256' '')
        $expectedSourceRunId = [long](Get-PropValue $candidatePlan 'runId' 0)
        $expectedSourceRunAttempt = [int](Get-PropValue $candidatePlan 'runAttempt' 0)
        if ($artifactId -le 0 -or [string]::IsNullOrWhiteSpace($artifactName)) {
            throw (New-ReleaseModelError -Code 'invalid-plan' -Message 'reuse-candidate plan has an invalid artifact id/name.')
        }
        if ($expectedManifestDigest -cnotmatch '^[0-9a-f]{64}$') {
            throw (New-ReleaseModelError -Code 'invalid-plan' -Message 'reuse-candidate plan has no valid manifest digest.')
        }
    }

    $tempDir = New-ReleaseTempDirectory -Prefix 'cb-stage'
    $candidateDir = Join-Path $tempDir 'candidate'
    $zipPath = Join-Path $tempDir 'candidate.zip'
    $artifact = Get-GitHubArtifact -Context $api -ArtifactId $artifactId
    $null = Test-CandidateArtifactRecord -Artifact $artifact -ExpectedId $artifactId -ExpectedName $artifactName -ExpectedRunId $expectedSourceRunId
    $artifactRun = Get-PropValue $artifact 'workflow_run' $null
    if ($null -ne $artifactRun -and [long](Get-PropValue $artifactRun 'id' 0) -ne $expectedSourceRunId) {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message 'Candidate artifact belongs to a different workflow run.')
    }
    $null = Save-GitHubArtifactZip -Context $api -ArtifactId $artifactId -OutFile $zipPath
    $null = Expand-ZipSafe -ZipPath $zipPath -Destination $candidateDir

    $manifestPath = Join-Path $candidateDir 'release-manifest.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message 'Candidate artifact has no release-manifest.json at its root.')
    }
    $manifestText = Read-Utf8Text -Path $manifestPath
    $manifestDigest = Get-ReleaseManifestDigest -ManifestText $manifestText
    if ($expectedManifestDigest -and $manifestDigest -cne $expectedManifestDigest) {
        throw (New-ReleaseModelError -Code 'candidate-digest-mismatch' -Message "Candidate manifest digest '$manifestDigest' does not match the plan '$expectedManifestDigest'.")
    }
    $parsed = Read-ReleaseManifestText -Text $manifestText
    $manifest = $parsed.Manifest
    if ($manifest.commit -cne $sourceSha) { throw (New-ReleaseModelError -Code 'candidate-conflict' -Message 'Candidate commit does not match the plan.') }
    if ($manifest.version -cne $version) { throw (New-ReleaseModelError -Code 'candidate-conflict' -Message 'Candidate version does not match the plan.') }
    if (($parsed.Formats -join ',') -cne ($formats -join ',')) { throw (New-ReleaseModelError -Code 'candidate-conflict' -Message 'Candidate formats do not match the plan.') }
    $sourceRun = Get-PropValue $manifest 'sourceRun' $null
    if ([long](Get-PropValue $sourceRun 'runId' 0) -ne $expectedSourceRunId -or [int](Get-PropValue $sourceRun 'runAttempt' 0) -ne $expectedSourceRunAttempt) {
        throw (New-ReleaseModelError -Code 'candidate-conflict' -Message 'Candidate manifest source run/attempt does not match the plan.')
    }
    if ([string](Get-PropValue $sourceRun 'artifactName' '') -cne $artifactName) {
        throw (New-ReleaseModelError -Code 'candidate-conflict' -Message 'Candidate manifest artifact name does not match the downloaded artifact.')
    }

    $packageSummaryPath = Join-Path $candidateDir 'evidence/package-summary.json'
    $rustSummaryPath = Join-Path $candidateDir 'evidence/rust-summary.json'
    $windowsSummaryPath = Join-Path $candidateDir 'evidence/windows-summary.json'
    foreach ($required in @($packageSummaryPath, $rustSummaryPath, $windowsSummaryPath, (Join-Path $candidateDir 'SHA256SUMS.txt'))) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message "Candidate artifact is missing required evidence '$([System.IO.Path]::GetFileName($required))'.")
        }
    }
    $packageSummary = Read-JsonFile -Path $packageSummaryPath
    $package = Test-PackageSummary -Summary $packageSummary -Version $version -Formats $formats
    $rustEvidence = Test-RustSummary -Summary (Read-JsonFile -Path $rustSummaryPath) -ExpectedCommit $sourceSha
    $windowsEvidence = Test-WindowsSummary -Summary (Read-JsonFile -Path $windowsSummaryPath) -ExpectedCommit $sourceSha -Formats $formats
    $nativeEvidence = Test-ReleaseNativeEvidence -PackageNativeEvidence (Get-PropValue $packageSummary 'nativeEvidence' $null) -ExpectedWindowsDllSha256 $windowsEvidence.nativeDllSha256

    $manifestArtifacts = @(Get-PropValue $manifest 'artifacts' @())
    $manifestByName = @{}
    foreach ($record in $manifestArtifacts) { $manifestByName[[string](Get-PropValue $record 'name' '')] = $record }
    if ($package.ReleaseArtifacts.Count -ne $manifestArtifacts.Count) {
        throw (New-ReleaseModelError -Code 'candidate-conflict' -Message 'package-summary release artifacts and manifest artifacts have different counts.')
    }
    foreach ($record in $package.ReleaseArtifacts) {
        if (-not $manifestByName.ContainsKey($record.name)) {
            throw (New-ReleaseModelError -Code 'candidate-conflict' -Message "package-summary artifact '$($record.name)' is missing from the manifest.")
        }
        $manifestRecord = $manifestByName[$record.name]
        if ([long](Get-PropValue $manifestRecord 'size' 0) -ne $record.size -or ([string](Get-PropValue $manifestRecord 'sha256' '')).ToLowerInvariant() -cne $record.sha256) {
            throw (New-ReleaseModelError -Code 'candidate-conflict' -Message "package-summary and manifest disagree on '$($record.name)' size/hash.")
        }
        $path = Join-Path $candidateDir $record.name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message "Candidate file '$($record.name)' is missing.")
        }
        $actualHash = Get-FileSha256 -Path $path
        $actualSize = [long](Get-Item -LiteralPath $path).Length
        if ($actualHash -cne $record.sha256 -or $actualSize -ne $record.size) {
            throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Candidate file '$($record.name)' does not match package-summary.")
        }
    }
    if (@(Get-ChildItem -LiteralPath $candidateDir -Recurse -File -Filter '*-user.msi').Count -gt 0) {
        throw (New-ReleaseModelError -Code 'forbidden-asset' -Message 'Candidate artifact contains the internal user MSI; it must never be a public candidate file.')
    }

    $manifestSigning = Get-PropValue $manifest 'signing' $null
    $signingMode = [string](Get-PropValue $manifestSigning 'mode' '')
    $signingStatus = [string](Get-PropValue $manifestSigning 'status' '')
    $signingTrusted = [bool](Get-PropValue $manifestSigning 'trusted' $false)
    $signingTimestamped = [bool](Get-PropValue $manifestSigning 'timestamped' $false)
    $planSigning = Get-PropValue $plan 'signing' $null
    if ([string](Get-PropValue $planSigning 'mode' '') -cne $signingMode -or
        [string](Get-PropValue $planSigning 'status' '') -cne $signingStatus -or
        [bool](Get-PropValue $planSigning 'trusted' $false) -ne $signingTrusted -or
        [bool](Get-PropValue $planSigning 'timestamped' $false) -ne $signingTimestamped) {
        throw (New-ReleaseModelError -Code 'signing-mismatch' -Message 'Candidate signing state does not match the preflight plan.')
    }
    if ($signingMode -cne $package.Signing.mode -or $signingStatus -cne $package.Signing.status -or $signingTrusted -ne $package.Signing.trusted -or $signingTimestamped -ne $package.Signing.timestamped) {
        throw (New-ReleaseModelError -Code 'signing-mismatch' -Message 'package-summary signing state disagrees with the manifest.')
    }
    $null = Get-ReleaseSigningPolicy -ReleaseKind $releaseKind -SigningMode $signingMode -SigningStatus $signingStatus -Trusted $signingTrusted -Timestamped $signingTimestamped -AllowUnsignedPrerelease $allowUnsigned
    $null = Assert-ReleaseArtifactSignatures -Directory $candidateDir -Artifacts $manifestArtifacts -SigningStatus $signingStatus

    # Verify the manifest's filtered verification evidence matches the raw summaries.
    $manifestVerification = Get-PropValue $manifest 'verification' $null
    $manifestRust = Get-PropValue $manifestVerification 'rust' $null
    $manifestRustTests = Get-PropValue $manifestRust 'tests' $null
    foreach ($field in @('status', 'commit')) {
        if ([string](Get-PropValue $manifestRust $field '') -cne [string](Get-PropValue $rustEvidence $field '')) {
            throw (New-ReleaseModelError -Code 'verification-mismatch' -Message "Manifest rust $field does not match rust-summary.")
        }
    }
    foreach ($field in @('suites', 'discovered', 'executed', 'passed', 'failed', 'ignored', 'filtered')) {
        if ([long](Get-PropValue $manifestRustTests $field 0) -ne [long](Get-PropValue (Get-PropValue $rustEvidence 'tests' $null) $field 0)) {
            throw (New-ReleaseModelError -Code 'verification-mismatch' -Message "Manifest rust test field '$field' does not match rust-summary.")
        }
    }
    $manifestWindows = Get-PropValue $manifestVerification 'windows' $null
    $manifestWindowsTests = Get-PropValue $manifestWindows 'tests' $null
    foreach ($field in @('status', 'commit', 'configuration', 'platform', 'runtimeIdentifier', 'nativeDllSha256')) {
        if ([string](Get-PropValue $manifestWindows $field '') -cne [string](Get-PropValue $windowsEvidence $field '')) {
            throw (New-ReleaseModelError -Code 'verification-mismatch' -Message "Manifest windows $field does not match windows-summary.")
        }
    }
    foreach ($field in @('discovered', 'executed', 'passed', 'failed', 'skipped', 'inconclusive')) {
        if ([long](Get-PropValue $manifestWindowsTests $field 0) -ne [long](Get-PropValue (Get-PropValue $windowsEvidence 'tests' $null) $field 0)) {
            throw (New-ReleaseModelError -Code 'verification-mismatch' -Message "Manifest windows test field '$field' does not match windows-summary.")
        }
    }
    if ([bool](Get-PropValue $manifestWindows 'publishReadyToRun' $false) -ne [bool](Get-PropValue $windowsEvidence 'publishReadyToRun' $false)) {
        throw (New-ReleaseModelError -Code 'verification-mismatch' -Message 'Manifest windows PublishReadyToRun does not match windows-summary.')
    }

    $checksumEntries = [System.Collections.Generic.List[object]]::new()
    foreach ($record in $manifestArtifacts) {
        $checksumEntries.Add([pscustomobject]@{ name = [string](Get-PropValue $record 'name' ''); sha256 = [string](Get-PropValue $record 'sha256' '') })
    }
    $checksumEntries.Add([pscustomobject]@{ name = 'release-manifest.json'; sha256 = $manifestDigest })
    $null = Test-Sha256SumsText -Text (Read-Utf8Text -Path (Join-Path $candidateDir 'SHA256SUMS.txt')) -ExpectedEntries ([object[]]$checksumEntries.ToArray())
    $candidateRootFiles = @(Get-ChildItem -LiteralPath $candidateDir -File | ForEach-Object { $_.Name })
    $expectedRootFiles = Get-ReleasePublicAssetNames -Version $version -Formats $formats
    foreach ($file in $candidateRootFiles) {
        if ($expectedRootFiles -cnotcontains $file) {
            throw (New-ReleaseModelError -Code 'forbidden-asset' -Message "Candidate root contains unexpected file '$file'.")
        }
    }
    foreach ($file in $expectedRootFiles) {
        if ($candidateRootFiles -cnotcontains $file) {
            throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message "Candidate root is missing '$file'.")
        }
    }

    # Locate/verify the target release. A state change after preflight is a stop, not an override.
    $releasePlan = Get-PropValue $plan 'release' $null
    $release = $null
    if ($null -ne $releasePlan) {
        $releaseId = [long](Get-PropValue $releasePlan 'id' 0)
        $release = Get-GitHubRelease -Context $api -ReleaseId $releaseId
        if ($null -eq $release) { throw (New-ReleaseModelError -Code 'release-disappeared' -Message "Preflighted release $releaseId no longer exists.") }
        $currentState = Get-ReleaseState -Release $release
        $expectedState = [string](Get-PropValue $releasePlan 'state' '')
        if ($currentState -cne $expectedState) {
            throw (New-ReleaseModelError -Code 'release-state-changed' -Message "Release state changed from '$expectedState' to '$currentState' after preflight; stop and re-run preflight.")
        }
    }
    else {
        $appeared = Get-GitHubReleaseByTag -Context $api -Tag $tag
        if ($null -ne $appeared) {
            throw (New-ReleaseModelError -Code 'release-state-changed' -Message 'A release appeared after preflight; refusing to adopt it in this run.')
        }
        $body = Get-ReleaseBody -Version $version -SourceCommit $sourceSha -Formats $formats -SigningStatus $signingStatus -CandidateDigest $manifestDigest -ReleaseKind $releaseKind
        $created = New-GitHubDraftRelease -Context $api -TagName $tag -TargetCommitish $sourceSha -Name (Get-ReleaseName -Version $version) -Body $body
        if ($null -eq $created -or -not [bool](Get-PropValue $created 'draft' $false)) {
            throw (New-ReleaseModelError -Code 'release-create-failed' -Message 'Draft release creation did not return a draft release.')
        }
        $releaseId = [long](Get-PropValue $created 'id' 0)
        $release = Get-GitHubRelease -Context $api -ReleaseId $releaseId
        if ($null -eq $release) { throw (New-ReleaseModelError -Code 'release-create-failed' -Message 'Draft release could not be re-read after creation.') }
        $currentState = Get-ReleaseState -Release $release
        if ($currentState -cne 'draft') { throw (New-ReleaseModelError -Code 'release-create-failed' -Message "Newly created release is '$currentState', not draft.") }
    }
    if ([string](Get-PropValue $release 'tag_name' '') -cne $tag) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Target release tag does not match the plan.')
    }
    $targetCommitish = [string](Get-PropValue $release 'target_commitish' '')
    $tagSha = Get-GitHubTagRef -Context $api -Tag $tag
    if ($tagSha -and $tagSha -cne $sourceSha) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message "Tag '$tag' points to '$tagSha', not '$sourceSha'; tags are never moved.")
    }
    if ($targetCommitish -cmatch '^[0-9a-f]{40}$' -and $targetCommitish -cne $sourceSha) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Release target_commitish is not the fixed source SHA.')
    }
    if ($currentState -cne 'draft' -and -not $tagSha) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Published release has no tag pointing at the candidate.')
    }

    $assets = @(Get-GitHubReleaseAssets -Context $api -ReleaseId $releaseId)
    $assetMap = @{}
    foreach ($asset in $assets) {
        $assetName = [string](Get-PropValue $asset 'name' '')
        if ($assetMap.ContainsKey($assetName)) { throw (New-ReleaseModelError -Code 'invalid-release' -Message "Duplicate release asset '$assetName'.") }
        $assetMap[$assetName] = $asset
    }
    $assetSet = Assert-ReleaseAssetSet -ActualNames @($assetMap.Keys) -Version $version -Formats $formats -AllowMissing:($currentState -eq 'draft')

    $expectedHashes = @{}
    foreach ($expectedName in (Get-ReleasePublicAssetNames -Version $version -Formats $formats)) {
        $expectedHashes[$expectedName] = Get-ExpectedAssetHash -Name $expectedName -ManifestText $manifestText -ManifestDigest $manifestDigest -ManifestArtifacts $manifestArtifacts -Directory $candidateDir
    }
    foreach ($existingName in @($assetMap.Keys)) {
        $expected = $expectedHashes[$existingName]
        $null = Assert-DownloadedAsset -Context $api -Asset $assetMap[$existingName] -Directory $tempDir -ExpectedSha256 $expected.Sha256 -ExpectedSize $expected.Size
    }

    if ($currentState -eq 'draft') {
        foreach ($uploadName in (Get-ReleaseUploadOrder -Version $version -Formats $formats)) {
            if ($assetMap.ContainsKey($uploadName)) { continue }
            $uploadPath = Join-Path $candidateDir $uploadName
            if (-not (Test-Path -LiteralPath $uploadPath -PathType Leaf)) {
                throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message "Required upload '$uploadName' is missing from the candidate.")
            }
            $expected = $expectedHashes[$uploadName]
            $response = Publish-GitHubReleaseAsset -Context $api -ReleaseId $releaseId -Name $uploadName -InFile $uploadPath
            $responseName = [string](Get-PropValue $response 'name' '')
            $responseSize = [long](Get-PropValue $response 'size' 0)
            $responseDigest = [string](Get-PropValue $response 'digest' '')
            if ($responseName -cne $uploadName -or $responseSize -ne $expected.Size) {
                throw (New-ReleaseModelError -Code 'upload-mismatch' -Message "Uploaded asset '$uploadName' metadata does not match the candidate.")
            }
            if ($responseDigest -cmatch '^sha256:([0-9a-f]{64})$' -and $Matches[1] -cne $expected.Sha256) {
                throw (New-ReleaseModelError -Code 'upload-mismatch' -Message "Uploaded asset '$uploadName' digest does not match the candidate.")
            }
            $assetMap[$uploadName] = $response
        }
    }
    elseif (-not $assetSet.Complete) {
        throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message 'Published release is missing assets; automatic repair is not allowed.')
    }

    $finalAssets = @(Get-GitHubReleaseAssets -Context $api -ReleaseId $releaseId)
    $finalMap = @{}
    foreach ($asset in $finalAssets) {
        $assetName = [string](Get-PropValue $asset 'name' '')
        if ($finalMap.ContainsKey($assetName)) { throw (New-ReleaseModelError -Code 'invalid-release' -Message "Duplicate release asset '$assetName' after upload.") }
        $finalMap[$assetName] = $asset
    }
    $finalSet = Assert-ReleaseAssetSet -ActualNames @($finalMap.Keys) -Version $version -Formats $formats
    $finalEvidence = [System.Collections.Generic.List[object]]::new()
    foreach ($expectedName in (Get-ReleasePublicAssetNames -Version $version -Formats $formats)) {
        $expected = $expectedHashes[$expectedName]
        $verified = Assert-DownloadedAsset -Context $api -Asset $finalMap[$expectedName] -Directory $tempDir -ExpectedSha256 $expected.Sha256 -ExpectedSize $expected.Size
        $finalEvidence.Add([pscustomobject]@{ name = $verified.Name; id = $verified.Id; size = $verified.Size; sha256 = $verified.Sha256 })
    }

    $approvalNonce = New-ApprovalNonce
    if ($approvalNonce -cnotmatch '^[0-9a-f]{64}$') { throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Failed to generate a 256-bit approval nonce.') }
    $stageCompletedAt = Get-UtcNowString
    $stage = [ordered]@{
        schemaVersion    = 1
        releaseId        = $releaseId
        releaseUrl       = [string](Get-PropValue $release 'html_url' '')
        tag              = $tag
        sourceSha        = $sourceSha
        version          = $version
        formats          = [string[]]$formats
        releaseKind      = $releaseKind
        allowUnsignedPrerelease = [bool]$allowUnsigned
        signing          = [ordered]@{ mode = $signingMode; status = $signingStatus; trusted = $signingTrusted; timestamped = $signingTimestamped }
        candidateDigest  = $manifestDigest
        approvalNonce    = $approvalNonce
        manifestSha256   = $manifestDigest
        assets           = [object[]]$finalEvidence.ToArray()
        releaseState     = [ordered]@{ state = (Get-ReleaseState -Release $release); draft = [bool](Get-PropValue $release 'draft' $false); prerelease = [bool](Get-PropValue $release 'prerelease' $false) }
        stageCompletedAt = $stageCompletedAt
        approval         = [ordered]@{ required = [bool]($releaseKind -eq 'stable'); environment = 'windows-release-stable' }
    }
    $stageJson = $stage | ConvertTo-Json -Depth 20 -Compress
    Write-GitHubOutput -Name 'stage_json' -Value $stageJson
    Write-GitHubOutput -Name 'release_id' -Value ([string]$releaseId)
    Write-GitHubOutput -Name 'release_url' -Value ([string]$stage.releaseUrl)
    Write-GitHubOutput -Name 'candidate_digest' -Value $manifestDigest
    Write-GitHubOutput -Name 'approval_nonce' -Value $approvalNonce
    Write-GitHubOutput -Name 'stage_completed_at' -Value $stageCompletedAt
    Write-GitHubOutput -Name 'signed' -Value ([string]($signingStatus -eq 'signed')).ToLowerInvariant()

    $summary.Add(("Candidate **{0}** for `{1}` is staged." -f $version, $tag))
    $summary.Add('')
    $summary.Add("| field | value |")
    $summary.Add('|---|---|')
    $summary.Add("| source SHA | ``$sourceSha`` |")
    $summary.Add("| release | id $releaseId, state $(Get-ReleaseState -Release $release) |")
    $summary.Add("| signing | $signingMode/$signingStatus, trusted=$signingTrusted, timestamped=$signingTimestamped |")
    $summary.Add("| candidate manifest SHA256 | ``$manifestDigest`` |")
    $assetNames = @($stage.assets | ForEach-Object { $_.name })
    $summary.Add("| assets | $($assetNames -join ', ') |")
    $summary.Add('')
    if ($releaseKind -eq 'stable') {
        $summary.Add('Next: `windows-release-stable` environment approval. The approval comment must contain this fresh stage nonce and candidate binding:')
        $summary.Add('```')
        $summary.Add("nonce=$approvalNonce")
        $summary.Add("candidate=$manifestDigest")
        $summary.Add("run_id=$runId")
        $summary.Add("run_attempt=$runAttempt")
        $summary.Add('result=pass')
        $summary.Add('record=<https URL of the isolated-environment acceptance record>')
        $summary.Add('```')
    }
    else {
        $summary.Add('Draft staged; no public change is made by stage.')
    }
    Write-ReleaseSummary -Heading 'Windows release stage' -Lines ([string[]]$summary.ToArray())
    $exitCode = 0
}
catch {
    $errorText = Get-ReleaseErrorText -ErrorRecord $_
    Write-ReleaseSummary -Heading 'Windows release stage failed' -Lines @('```', $errorText, '```')
    Write-Host "STAGE FAILED: $errorText" -ForegroundColor Red
    $exitCode = 1
}
finally {
    if ($tempDir -and (Test-Path -LiteralPath $tempDir)) {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
exit $exitCode
