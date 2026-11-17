#Requires -Version 7.0
<#
.SYNOPSIS
    Manual Windows release preflight: fix the source SHA, validate the version,
    inspect the existing release/asset state and emit a machine plan.

.DESCRIPTION
    This script performs read-only GitHub API calls. It never creates a
    release/tag, never uploads and never writes repository content. All write
    operations happen later in stage/publish. The API context is ReadOnly even
    though the job token has contents: write (GitHub only exposes draft releases
    to push-capable tokens); ReadOnly rejects POST/PATCH before any network call.
    The minimum viable preflight permission set is pending hosted-runner
    verification. The plan is emitted as JSON for the later jobs, which
    re-validate it rather than trusting it blindly.
#>
[CmdletBinding()]
param(
    [ValidateSet('Live', 'DryRun')][string]$Mode = 'Live',
    [string]$EventPath = $env:GITHUB_EVENT_PATH
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$scriptDir = $PSScriptRoot
Import-Module (Join-Path $scriptDir 'GitHubReleaseApi.psm1') -Force
# The API module imports ReleaseModel for its internal helpers; re-import it so the
# entrypoint also gets the model commands (a -Force module import replaces the
# caller's reference). Keep this order if you edit imports.
Import-Module (Join-Path $scriptDir 'ReleaseModel.psm1') -Force
Import-Module (Join-Path $scriptDir 'ReleaseRuntime.psm1') -Force

function Get-ReleaseAssetMap {
    param([object[]]$Assets)
    $map = @{}
    foreach ($asset in @($Assets)) {
        $name = [string](Get-PropValue $asset 'name' '')
        if ([string]::IsNullOrWhiteSpace($name)) { throw (New-ReleaseModelError -Code 'invalid-release' -Message 'Release contains an asset without a name.') }
        if ($map.ContainsKey($name)) { throw (New-ReleaseModelError -Code 'invalid-release' -Message "Release contains duplicate asset '$name'.") }
        if ([string](Get-PropValue $asset 'state' 'uploaded') -cne 'uploaded') {
            throw (New-ReleaseModelError -Code 'invalid-release' -Message "Release asset '$name' is not in the uploaded state.")
        }
        $map[$name] = $asset
    }
    return $map
}

function Get-ReleaseForTag {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][string]$Tag
    )
    $release = Get-GitHubReleaseByTag -Context $Context -Tag $Tag
    if ($null -ne $release) { return $release }
    # Draft releases are not always returned by the by-tag endpoint; list is authoritative for the token.
    $releases = Get-GitHubReleases -Context $Context
    $matches = @($releases | Where-Object { [string](Get-PropValue $_ 'tag_name' '') -ceq $Tag })
    if ($matches.Count -gt 1) { throw (New-ReleaseModelError -Code 'invalid-release' -Message "Multiple releases found for tag '$Tag'.") }
    if ($matches.Count -eq 1) { return $matches[0] }
    return $null
}

function Assert-ReleaseIdentity {
    param(
        [Parameter(Mandatory)][object]$Release,
        [Parameter(Mandatory)][string]$Tag,
        [Parameter(Mandatory)][string]$SourceSha,
        [string]$TagSha
    )
    $id = [long](Get-PropValue $Release 'id' 0)
    if ($id -le 0) { throw (New-ReleaseModelError -Code 'invalid-release' -Message 'Release response has no positive id.') }
    if ([string](Get-PropValue $Release 'tag_name' '') -cne $Tag) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message "Existing release tag does not match '$Tag'.")
    }
    $targetCommitish = [string](Get-PropValue $Release 'target_commitish' '')
    if ($targetCommitish -cmatch '^[0-9a-f]{40}$') {
        if ($targetCommitish -cne $SourceSha) {
            throw (New-ReleaseModelError -Code 'tag-mismatch' -Message "Existing release target_commitish '$targetCommitish' does not point to the fixed source SHA '$SourceSha'.")
        }
    }
    elseif ($targetCommitish -ne '') {
        if (-not $TagSha) {
            throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Existing release target_commitish is not a SHA and the tag is missing; refusing to reuse.')
        }
    }
    $state = Get-ReleaseState -Release $Release
    if ($state -ceq 'draft' -and -not $TagSha) {
        # GitHub creates the tag when a draft is published; absence is allowed for our own draft.
    }
    elseif ($TagSha -and $TagSha -cne $SourceSha) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message "Existing tag points to '$TagSha', not the fixed source SHA '$SourceSha'.")
    }
    elseif ($state -ne 'draft' -and -not $TagSha) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Published release has no tag; refusing to use it as a candidate.')
    }
    return $state
}

function Read-ReleaseAsset {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][object]$Asset,
        [Parameter(Mandatory)][string]$Directory
    )
    $name = [string](Get-PropValue $Asset 'name' '')
    $id = [long](Get-PropValue $Asset 'id' 0)
    if ($id -le 0) { throw (New-ReleaseModelError -Code 'invalid-release' -Message "Asset '$name' has no id.") }
    $path = Join-Path $Directory ($name -replace '[^A-Za-z0-9._-]', '_')
    $null = Save-GitHubReleaseAsset -Context $Context -AssetId $id -OutFile $path
    return [pscustomobject]@{
        Name   = $name
        Id     = $id
        Size   = [long](Get-PropValue $Asset 'size' 0)
        Digest = [string](Get-PropValue $Asset 'digest' '')
        Path   = $path
        Sha256 = (Get-FileSha256 -Path $path)
    }
}

function Assert-AssetHash {
    param(
        [Parameter(Mandatory)][object]$Asset,
        [Parameter(Mandatory)][string]$ExpectedSha256,
        [Parameter(Mandatory)][long]$ExpectedSize
    )
    if ($Asset.Size -ne $ExpectedSize) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Asset '$($Asset.Name)' size $($Asset.Size) does not match recorded size $ExpectedSize.")
    }
    if ($Asset.Sha256 -cne $ExpectedSha256) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Asset '$($Asset.Name)' hash does not match the recorded candidate manifest.")
    }
    if ($Asset.Digest -cmatch '^sha256:([0-9a-f]{64})$' -and $Matches[1] -cne $ExpectedSha256) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Asset '$($Asset.Name)' API digest does not match the downloaded bytes.")
    }
}

$exitCode = 1
$summary = [System.Collections.Generic.List[string]]::new()
$tempDir = $null
try {
    $ref = Get-RequiredEnv 'GITHUB_REF'
    if ($ref -cne 'refs/heads/main') {
        throw (New-ReleaseModelError -Code 'invalid-workflow-ref' -Message "Release Windows must run from main (refs/heads/main); got '$ref'.")
    }
    $eventName = Get-OptionalEnv 'GITHUB_EVENT_NAME' ''
    if ($eventName -and $eventName -cne 'workflow_dispatch') {
        throw (New-ReleaseModelError -Code 'invalid-workflow-ref' -Message "Release Windows must be workflow_dispatch only; got '$eventName'.")
    }
    $workspace = Get-OptionalEnv 'GITHUB_WORKSPACE' ''
    if (-not $workspace) { $workspace = Split-Path -Parent (Split-Path -Parent $scriptDir) }
    $runId = [long](Get-RequiredEnv 'GITHUB_RUN_ID')
    $runAttempt = [int](Get-RequiredEnv 'GITHUB_RUN_ATTEMPT')

    $inputs = Get-ReleaseEventInputs -EventPath $EventPath
    $expectedVersion = $inputs.ExpectedVersion
    $null = ConvertTo-ReleaseVersion $expectedVersion
    $formats = Get-ReleaseFormatsFromBooleans -MachineMsi $inputs.MachineMsi -UserExe $inputs.UserExe -PortableZip $inputs.PortableZip
    $releaseKind = $inputs.ReleaseKind
    $allowUnsignedPrerelease = $inputs.AllowUnsignedPrerelease
    $sourceKind = Resolve-SourceRefKind -SourceRef $inputs.SourceRef
    $tag = Get-ReleaseTag -Version $expectedVersion

    $head = Invoke-GitCommand -WorkingDirectory $workspace -Arguments @('rev-parse', '--verify', 'HEAD')
    if ($head.ExitCode -ne 0 -or $head.Output.Trim() -cnotmatch '^[0-9a-fA-F]{40}$') {
        throw (New-ReleaseModelError -Code 'source-unavailable' -Message 'Cannot resolve the trusted main checkout HEAD.')
    }
    $mainSha = $head.Output.Trim().ToLowerInvariant()

    $api = New-ReleaseApiContextFromEnv -Mode $Mode -ReadOnly
    $sourceSha = $null
    switch ($sourceKind.Kind) {
        'main' { $sourceSha = $mainSha }
        'sha' { $sourceSha = $sourceKind.Sha }
        'tag' {
            $tagRef = Get-GitHubTagRef -Context $api -Tag $sourceKind.Tag
            if (-not $tagRef) {
                throw (New-ReleaseModelError -Code 'source-unavailable' -Message "source_ref tag '$($sourceKind.Tag)' does not exist in this repository.")
            }
            $sourceSha = $tagRef
        }
    }
    if ($sourceSha -cnotmatch '^[0-9a-f]{40}$') {
        throw (New-ReleaseModelError -Code 'source-unavailable' -Message 'Source commit did not resolve to a full 40-hex SHA.')
    }
    $catFile = Invoke-GitCommand -WorkingDirectory $workspace -Arguments @('cat-file', '-e', "$sourceSha^{commit}")
    if ($catFile.ExitCode -ne 0) {
        throw (New-ReleaseModelError -Code 'source-unavailable' -Message "Source commit '$sourceSha' is not present in the trusted main checkout.")
    }
    $ancestor = Invoke-GitCommand -WorkingDirectory $workspace -Arguments @('merge-base', '--is-ancestor', $sourceSha, 'HEAD')
    if ($ancestor.ExitCode -ne 0) {
        throw (New-ReleaseModelError -Code 'source-not-on-main' -Message "Source commit '$sourceSha' is not in main history.")
    }
    if ($sourceKind.Kind -ceq 'tag' -and $sourceKind.TagVersion -cne $expectedVersion) {
        throw (New-ReleaseModelError -Code 'version-mismatch' -Message "source_ref tag version '$($sourceKind.TagVersion)' does not match expected_version '$expectedVersion'.")
    }

    $versionPropsText = (Invoke-GitCommand -WorkingDirectory $workspace -Arguments @('show', "${sourceSha}:platforms/windows/ClipBridgeShell_CS/Version.props"))
    if ($versionPropsText.ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($versionPropsText.Output)) {
        throw (New-ReleaseModelError -Code 'version-source-missing' -Message 'Cannot read Version.props from the fixed source commit.')
    }
    $xml = [System.Xml.XmlDocument]::new()
    try { $xml.LoadXml($versionPropsText.Output) } catch {
        throw (New-ReleaseModelError -Code 'invalid-version' -Message 'Version.props in the source commit is not valid XML.')
    }
    $versionNodes = @($xml.SelectNodes("/*[local-name()='Project']/*[local-name()='PropertyGroup']/*[local-name()='Version']"))
    if ($versionNodes.Count -ne 1) {
        throw (New-ReleaseModelError -Code 'invalid-version' -Message 'Version.props must contain exactly one Project/PropertyGroup/Version element.')
    }
    $versionPropsVersion = [string]$versionNodes[0].InnerText
    $null = ConvertTo-ReleaseVersion $versionPropsVersion
    if ($versionPropsVersion -cne $expectedVersion) {
        throw (New-ReleaseModelError -Code 'version-mismatch' -Message "expected_version '$expectedVersion' does not equal Version.props '$versionPropsVersion' at the fixed source commit.")
    }

    $tagSha = Get-GitHubTagRef -Context $api -Tag $tag
    if ($tagSha -and $tagSha -cne $sourceSha) {
        throw (New-ReleaseModelError -Code 'tag-mismatch' -Message "Existing tag '$tag' points to '$tagSha', not the fixed source SHA '$sourceSha'. Tags are never moved.")
    }

    $signing = Get-ReleaseSigningConfigurationFromEnv
    $signingStatus = if ($signing.Configured) { 'signed' } else { 'unsigned' }
    $trusted = [bool]$signing.Configured
    $timestamped = [bool]$signing.Configured
    # Apply the policy early for a new candidate. It also blocks stable before any build.
    $policy = Get-ReleaseSigningPolicy -ReleaseKind $releaseKind -SigningMode $signing.Mode -SigningStatus $signingStatus -Trusted $trusted -Timestamped $timestamped -AllowUnsignedPrerelease $allowUnsignedPrerelease

    $environmentPolicy = $null
    if ($releaseKind -ceq 'stable') {
        $environment = Get-GitHubEnvironment -Context $api -Name 'windows-release-stable'
        if ($null -eq $environment) {
            throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Required environment 'windows-release-stable' does not exist; stable promotion fails closed and no empty environment is created.")
        }
        $environmentPolicy = Get-EnvironmentReviewerPolicy -EnvironmentName 'windows-release-stable' -Environment $environment
        # Capability probe: the same token must be able to read this run's review history later.
        $null = Get-GitHubRunApprovals -Context $api -RunId $runId
    }

    $release = Get-ReleaseForTag -Context $api -Tag $tag
    $releaseState = 'none'
    $assetSet = $null
    $manifestParsed = $null
    $candidateDigest = $null
    $candidateSigned = $false
    $candidateAssets = @()
    $transition = $null

    if ($null -ne $release) {
        $releaseState = Assert-ReleaseIdentity -Release $release -Tag $tag -SourceSha $sourceSha -TagSha $tagSha
        $releaseId = [long](Get-PropValue $release 'id' 0)
        $assets = @(Get-GitHubReleaseAssets -Context $api -ReleaseId $releaseId)
        $assetMap = Get-ReleaseAssetMap -Assets $assets
        $allowMissing = ($releaseState -ceq 'draft')
        $assetSet = Assert-ReleaseAssetSet -ActualNames @($assetMap.Keys) -Version $expectedVersion -Formats $formats -AllowMissing:$allowMissing
        if (-not $assetMap.ContainsKey('release-manifest.json')) {
            throw (New-ReleaseModelError -Code 'candidate-record-missing' -Message 'Existing release has no release-manifest.json; source provenance cannot be verified and the candidate is not adopted.')
        }
        $tempDir = New-ReleaseTempDirectory -Prefix 'cb-preflight'
        $manifestAsset = Read-ReleaseAsset -Context $api -Asset $assetMap['release-manifest.json'] -Directory $tempDir
        $manifestText = Read-Utf8Text -Path $manifestAsset.Path
        $manifestParsed = Read-ReleaseManifestText -Text $manifestText
        $candidateDigest = $manifestParsed.Digest
        if ($manifestParsed.Manifest.version -cne $expectedVersion) {
            throw (New-ReleaseModelError -Code 'candidate-conflict' -Message "Existing candidate version '$($manifestParsed.Manifest.version)' does not match expected_version '$expectedVersion'.")
        }
        if ($manifestParsed.Manifest.commit -cne $sourceSha) {
            throw (New-ReleaseModelError -Code 'candidate-conflict' -Message "Existing candidate commit '$($manifestParsed.Manifest.commit)' does not match the fixed source SHA '$sourceSha'.")
        }
        if (($manifestParsed.Formats -join ',') -cne ($formats -join ',')) {
            throw (New-ReleaseModelError -Code 'candidate-conflict' -Message 'Existing candidate formats do not match the request; formats are never added or removed.')
        }
        $manifestSigning = Get-PropValue $manifestParsed.Manifest 'signing' $null
        $null = Get-ReleaseSigningPolicy -ReleaseKind $releaseKind -SigningMode ([string](Get-PropValue $manifestSigning 'mode' 'None')) -SigningStatus ([string](Get-PropValue $manifestSigning 'status' 'unsigned')) -Trusted ([bool](Get-PropValue $manifestSigning 'trusted' $false)) -Timestamped ([bool](Get-PropValue $manifestSigning 'timestamped' $false)) -AllowUnsignedPrerelease $allowUnsignedPrerelease
        $candidateSigned = ([string](Get-PropValue $manifestSigning 'mode' '') -ceq 'Certificate' -and [string](Get-PropValue $manifestSigning 'status' '') -ceq 'signed' -and [bool](Get-PropValue $manifestSigning 'trusted' $false) -and [bool](Get-PropValue $manifestSigning 'timestamped' $false))

        $manifestArtifacts = @(Get-PropValue $manifestParsed.Manifest 'artifacts' @())
        $manifestByName = @{}
        foreach ($artifact in $manifestArtifacts) {
            $name = [string](Get-PropValue $artifact 'name' '')
            $manifestByName[$name] = $artifact
        }
        $downloaded = [System.Collections.Generic.List[object]]::new()
        foreach ($name in (Get-ReleasePublicAssetNames -Version $expectedVersion -Formats $formats)) {
            if (-not $assetMap.ContainsKey($name)) { continue }
            $asset = Read-ReleaseAsset -Context $api -Asset $assetMap[$name] -Directory $tempDir
            if ($name -ceq 'release-manifest.json') {
                if ($asset.Sha256 -cne $candidateDigest) { throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message 'Downloaded release-manifest.json hash does not match its parsed digest.') }
            }
            elseif ($name -ceq 'SHA256SUMS.txt') {
                # Checked below against the downloaded package bytes.
            }
            else {
                if (-not $manifestByName.ContainsKey($name)) { throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest has no entry for release asset '$name'.") }
                $record = $manifestByName[$name]
                Assert-AssetHash -Asset $asset -ExpectedSha256 ([string](Get-PropValue $record 'sha256' '')) -ExpectedSize ([long](Get-PropValue $record 'size' 0))
            }
            $downloaded.Add($asset)
        }
        $checksumAsset = $downloaded | Where-Object { $_.Name -ceq 'SHA256SUMS.txt' } | Select-Object -First 1
        if ($checksumAsset) {
            $expectedEntries = [System.Collections.Generic.List[object]]::new()
            foreach ($artifact in $manifestArtifacts) {
                $expectedEntries.Add([pscustomobject]@{ name = [string](Get-PropValue $artifact 'name' ''); sha256 = [string](Get-PropValue $artifact 'sha256' '') })
            }
            $expectedEntries.Add([pscustomobject]@{ name = 'release-manifest.json'; sha256 = $candidateDigest })
            $null = Test-Sha256SumsText -Text (Read-Utf8Text -Path $checksumAsset.Path) -ExpectedEntries ([object[]]$expectedEntries.ToArray())
        }

        $transition = Get-ReleaseTransition -State $releaseState -ReleaseKind $releaseKind -Signed $candidateSigned -Complete ([bool]$assetSet.Complete) -ExistingFormats $manifestParsed.Formats -RequestedFormats $formats -AllowUnsignedPrerelease $allowUnsignedPrerelease
        if ($transition.Mode -ceq 'already-complete') {
            $expectedState = if ($releaseKind -ceq 'stable') { 'stable' } else { 'prerelease' }
            if ($releaseState -cne $expectedState) {
                throw (New-ReleaseModelError -Code 'invalid-release-state' -Message "already-complete requested but current state is '$releaseState', expected '$expectedState'.")
            }
            if (-not $assetSet.Complete) { throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message 'already-complete candidate is missing assets.') }
        }

        if ($transition.Mode -ceq 'reuse-candidate') {
            $sourceRun = Get-PropValue $manifestParsed.Manifest 'sourceRun' $null
            $sourceRunId = [long](Get-PropValue $sourceRun 'runId' 0)
            $sourceRunAttempt = [int](Get-PropValue $sourceRun 'runAttempt' 0)
            $artifactName = [string](Get-PropValue $sourceRun 'artifactName' '')
            $run = Get-GitHubWorkflowRun -Context $api -RunId $sourceRunId
            $null = Test-SourceRunRecord -Run $run -ExpectedRunId $sourceRunId -ExpectedRunAttempt $sourceRunAttempt
            $jobs = Get-GitHubWorkflowRunJobs -Context $api -RunId $sourceRunId -Attempt $sourceRunAttempt
            $null = Test-SourceRunJobs -Jobs $jobs -ExpectedRunId $sourceRunId -ExpectedRunAttempt $sourceRunAttempt
            $sourceArtifacts = @(Get-GitHubRunArtifacts -Context $api -RunId $sourceRunId | Where-Object { [string](Get-PropValue $_ 'name' '') -ceq $artifactName })
            if ($sourceArtifacts.Count -ne 1) {
                throw (New-ReleaseModelError -Code 'artifact-missing' -Message "Expected exactly one source candidate artifact '$artifactName'; found $($sourceArtifacts.Count).")
            }
            $artifactRecord = Test-CandidateArtifactRecord -Artifact $sourceArtifacts[0] -ExpectedId ([long](Get-PropValue $sourceArtifacts[0] 'id' 0)) -ExpectedName $artifactName -ExpectedRunId $sourceRunId
            $artifactRun = Get-PropValue $sourceArtifacts[0] 'workflow_run' $null
            if ($null -ne $artifactRun -and [long](Get-PropValue $artifactRun 'id' 0) -ne $sourceRunId) {
                throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message 'Candidate artifact belongs to a different workflow run.')
            }
            $candidateAssets = @($downloaded | Where-Object { $_.Name -cne 'release-manifest.json' -and $_.Name -cne 'SHA256SUMS.txt' } | ForEach-Object {
                    [pscustomobject]@{ name = $_.Name; id = $_.Id; size = $_.Size; sha256 = $_.Sha256 }
                })
            $candidate = [ordered]@{
                runId        = $sourceRunId
                runAttempt   = $sourceRunAttempt
                artifactName = $artifactName
                artifactId   = $artifactRecord.Id
                manifestSha256 = $candidateDigest
                assets       = [object[]]$candidateAssets
                sourceRun    = [ordered]@{
                    runId      = $sourceRunId
                    runAttempt = $sourceRunAttempt
                    artifactName = $artifactName
                }
            }
        }
    }
    else {
        $transition = Get-ReleaseTransition -State 'none' -ReleaseKind $releaseKind -Signed ([bool]$signing.Configured) -Complete $true -RequestedFormats $formats -AllowUnsignedPrerelease $allowUnsignedPrerelease
    }

    if ($null -eq $transition) { throw (New-ReleaseModelError -Code 'invalid-release-state' -Message 'Release transition was not determined.') }
    if ($transition.Mode -ceq 'build-new') {
        $candidate = $null
    }
    elseif ($transition.Mode -ceq 'already-complete') {
        $candidate = $null
    }
    elseif (-not (Get-Variable -Name candidate -ErrorAction SilentlyContinue)) {
        throw (New-ReleaseModelError -Code 'invalid-release-state' -Message 'Reuse candidate provenance was not assembled.')
    }

    $releaseInfo = $null
    if ($null -ne $release) {
        $releaseInfo = [ordered]@{
            id      = [long](Get-PropValue $release 'id' 0)
            state   = $releaseState
            draft   = [bool](Get-PropValue $release 'draft' $false)
            prerelease = [bool](Get-PropValue $release 'prerelease' $false)
            url     = [string](Get-PropValue $release 'html_url' '')
        }
    }
    $signingInfo = if ($releaseState -ne 'none' -and $null -ne $manifestParsed) {
        $ms = Get-PropValue $manifestParsed.Manifest 'signing' $null
        [ordered]@{ mode = [string](Get-PropValue $ms 'mode' ''); status = [string](Get-PropValue $ms 'status' ''); trusted = [bool](Get-PropValue $ms 'trusted' $false); timestamped = [bool](Get-PropValue $ms 'timestamped' $false); source = 'manifest' }
    }
    else {
        [ordered]@{ mode = $signing.Mode; status = $signingStatus; trusted = $trusted; timestamped = $timestamped; source = 'configured' }
    }
    $environmentInfo = $null
    if ($releaseKind -ceq 'stable' -and $null -ne $environmentPolicy) {
        $environmentInfo = [ordered]@{
            name      = $environmentPolicy.Name
            reviewers = [string[]]$environmentPolicy.RequiredReviewerLogins
        }
    }

    $plan = [ordered]@{
        schemaVersion     = 1
        mode              = $transition.Mode
        transitionReason  = $transition.Reason
        sourceRef         = $inputs.SourceRef
        sourceRefKind     = $sourceKind.Kind
        sourceSha         = $sourceSha
        version           = $expectedVersion
        tag               = $tag
        formats           = [string[]]$formats
        releaseKind       = $releaseKind
        allowUnsignedPrerelease = [bool]$allowUnsignedPrerelease
        signing           = $signingInfo
        environment       = $environmentInfo
        release           = $releaseInfo
        candidate         = $candidate
        stageRequired     = [bool]($transition.Mode -ne 'already-complete')
        publishRequired   = [bool]($transition.Mode -ne 'already-complete' -and $releaseKind -ne 'draft')
        stable            = [bool]($releaseKind -eq 'stable')
        currentRun        = [ordered]@{ runId = $runId; runAttempt = $runAttempt; ref = $ref }
    }

    $planJson = $plan | ConvertTo-Json -Depth 24 -Compress
    Write-GitHubOutput -Name 'plan_json' -Value $planJson
    Write-GitHubOutput -Name 'mode' -Value $plan.mode
    Write-GitHubOutput -Name 'stage_required' -Value ([string]$plan.stageRequired).ToLowerInvariant()
    Write-GitHubOutput -Name 'publish_required' -Value ([string]$plan.publishRequired).ToLowerInvariant()
    Write-GitHubOutput -Name 'stable' -Value ([string]$plan.stable).ToLowerInvariant()
    Write-GitHubOutput -Name 'source_sha' -Value $sourceSha
    Write-GitHubOutput -Name 'version' -Value $expectedVersion
    Write-GitHubOutput -Name 'tag' -Value $tag
    Write-GitHubOutput -Name 'formats' -Value ($formats -join ',')
    Write-GitHubOutput -Name 'release_kind' -Value $releaseKind
    Write-GitHubOutput -Name 'release_id' -Value $(if ($releaseInfo) { [string]$releaseInfo.id } else { '' })
    Write-GitHubOutput -Name 'candidate_digest' -Value $(if ($candidateDigest) { $candidateDigest } else { '' })
    Write-GitHubOutput -Name 'environment' -Value $(if ($environmentInfo) { 'windows-release-stable' } else { '' })

    $summary.Add("Mode: **$($plan.mode)** — $($transition.Reason)")
    $summary.Add('')
    $summary.Add("| field | value |")
    $summary.Add('|---|---|')
    $summary.Add("| source_ref | ``$($inputs.SourceRef)`` ($($sourceKind.Kind)) |")
    $summary.Add("| source SHA | ``$sourceSha`` |")
    $summary.Add("| version / tag | ``$expectedVersion`` / ``$tag`` |")
    $summary.Add("| formats | $($formats -join ', ') |")
    $summary.Add("| release kind | $releaseKind |")
    $summary.Add("| signing | $($signingInfo.mode)/$($signingInfo.status), trusted=$($signingInfo.trusted), timestamped=$($signingInfo.timestamped) ($($signingInfo.source)) |")
    if ($releaseInfo) { $summary.Add("| release | id $($releaseInfo.id), state $($releaseInfo.state) |") }
    if ($candidateDigest) { $summary.Add("| candidate manifest SHA256 | ``$candidateDigest`` |") }
    if ($environmentInfo) { $summary.Add("| stable environment | $($environmentInfo.name); required reviewers: $($environmentInfo.reviewers -join ', ') |") }
    $summary.Add('')
    if ($plan.mode -ceq 'already-complete') {
        $summary.Add('Existing published release already satisfies this request. No build, stage or write was performed.')
    }
    elseif ($plan.mode -ceq 'build-new') {
        $summary.Add('A new candidate will be built and verified. No release/tag is created in preflight.')
    }
    else {
        $summary.Add('Existing candidate will be verified and reused. The build job is intentionally skipped; stage validates the original run/attempt/artifact and every final asset hash.')
    }
    if ($releaseKind -ceq 'stable') {
        $summary.Add('')
        $summary.Add('Stable promotion is gated by `windows-release-stable` after stage. The approval comment must contain:')
        $summary.Add('```')
        $summary.Add("candidate=$candidateDigest")
        $summary.Add('result=pass')
        $summary.Add('record=<https URL of the isolated-environment acceptance record>')
        $summary.Add('```')
        if (-not $signing.Configured) {
            $summary.Add('**Blocked:** production signing is not configured. Stable cannot be published.')
        }
        $summary.Add('')
        $summary.Add('**Approval mechanism:** stage emits a fresh 256-bit nonce. The required reviewer must include nonce, candidate digest, run_id, run_attempt, result=pass and an https record in the approval comment. Publish verifies the exact attempt, gate start >= stage completion and the same nonce. The public API does not expose approval time; no approval timestamp is claimed.')
    }
    Write-ReleaseSummary -Heading 'Windows release preflight' -Lines ([string[]]$summary.ToArray())
    $exitCode = 0
}
catch {
    $errorText = Get-ReleaseErrorText -ErrorRecord $_
    Write-ReleaseSummary -Heading 'Windows release preflight failed' -Lines @('```', $errorText, '```')
    Write-Host "PREFLIGHT FAILED: $errorText" -ForegroundColor Red
    $exitCode = 1
}
finally {
    if ($tempDir -and (Test-Path -LiteralPath $tempDir)) {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
exit $exitCode
