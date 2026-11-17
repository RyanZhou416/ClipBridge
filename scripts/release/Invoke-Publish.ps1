#Requires -Version 7.0
<#
.SYNOPSIS
    Publish or promote a staged candidate after re-verifying the immutable
    candidate and (for stable) the current run's environment approval.

.DESCRIPTION
    Only Release status fields are changed. The tag is never moved, assets are
    never replaced/deleted and a published release is never hidden or
    downgraded. Any post-publish verification failure is reported as
    "published but verification failed"; the workflow does not pretend the
    release is unpublished.
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

function Get-WindowsReleaseVersionFromTag {
    param([string]$Tag)
    if ($Tag -cmatch '^clipbridge-windows-((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))$') { return $Matches[1] }
    return $null
}

function Get-LatestWindowsStableVersion {
    param([Parameter(Mandatory)][object]$Context)
    $releases = @(Get-GitHubReleases -Context $Context)
    $best = $null
    foreach ($release in $releases) {
        if ([bool](Get-PropValue $release 'draft' $false) -or [bool](Get-PropValue $release 'prerelease' $false)) { continue }
        $version = Get-WindowsReleaseVersionFromTag -Tag ([string](Get-PropValue $release 'tag_name' ''))
        if (-not $version) { continue }
        if ($null -eq $best -or (Compare-ReleaseVersion -Left $version -Right $best) -gt 0) { $best = $version }
    }
    return $best
}

function Get-ArtifactFormatFromName {
    param([string]$Name)
    if ($Name -match '-machine\.msi$') { return 'machine-msi' }
    if ($Name -match '-user\.exe$') { return 'user-exe' }
    if ($Name -match '-portable\.zip$') { return 'portable-zip' }
    return $null
}

function Assert-StageAssetsUnchanged {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$ReleaseId,
        [Parameter(Mandatory)][object[]]$StageAssets,
        [Parameter(Mandatory)][string]$Directory
    )
    $assets = @(Get-GitHubReleaseAssets -Context $Context -ReleaseId $ReleaseId)
    $map = @{}
    foreach ($asset in $assets) {
        $name = [string](Get-PropValue $asset 'name' '')
        if ($map.ContainsKey($name)) { throw (New-ReleaseModelError -Code 'invalid-release' -Message "Duplicate release asset '$name'.") }
        $map[$name] = $asset
    }
    $expectedNames = @($StageAssets | ForEach-Object { [string](Get-PropValue $_ 'name' '') })
    $actualNames = @($map.Keys)
    foreach ($name in $actualNames) {
        if ($expectedNames -cnotcontains $name) { throw (New-ReleaseModelError -Code 'candidate-changed' -Message "Unexpected release asset '$name' appeared after stage.") }
    }
    foreach ($name in $expectedNames) {
        if ($actualNames -cnotcontains $name) { throw (New-ReleaseModelError -Code 'candidate-changed' -Message "Release asset '$name' disappeared after stage.") }
    }
    $verified = [System.Collections.Generic.List[object]]::new()
    foreach ($record in $StageAssets) {
        $name = [string](Get-PropValue $record 'name' '')
        $expectedId = [long](Get-PropValue $record 'id' 0)
        $expectedSize = [long](Get-PropValue $record 'size' 0)
        $expectedSha = [string](Get-PropValue $record 'sha256' '')
        $asset = $map[$name]
        if ([long](Get-PropValue $asset 'id' 0) -ne $expectedId) {
            throw (New-ReleaseModelError -Code 'candidate-changed' -Message "Release asset '$name' id changed after stage.")
        }
        $path = Join-Path $Directory ($name -replace '[^A-Za-z0-9._-]', '_')
        $null = Save-GitHubReleaseAsset -Context $Context -AssetId $expectedId -OutFile $path
        $actualSize = [long](Get-Item -LiteralPath $path).Length
        $actualSha = Get-FileSha256 -Path $path
        if ($actualSize -ne $expectedSize -or $actualSha -cne $expectedSha) {
            throw (New-ReleaseModelError -Code 'candidate-changed' -Message "Release asset '$name' hash changed after stage.")
        }
        $verified.Add([pscustomobject]@{ name = $name; path = $path; size = $actualSize; sha256 = $actualSha })
    }
    return [object[]]$verified.ToArray()
}

$exitCode = 1
$summary = [System.Collections.Generic.List[string]]::new()
$tempDir = $null
$patched = $false
try {
    $ref = Get-RequiredEnv 'GITHUB_REF'
    if ($ref -cne 'refs/heads/main') { throw (New-ReleaseModelError -Code 'invalid-workflow-ref' -Message "publish must run from refs/heads/main; got '$ref'.") }
    $plan = Read-ReleasePlanEnvironment
    $stage = Read-ReleasePlanEnvironment -Name 'STAGE_JSON'
    if (-not [bool](Get-PropValue $plan 'publishRequired' $false)) {
        throw (New-ReleaseModelError -Code 'invalid-plan' -Message 'publish was invoked for a plan that does not require publication.')
    }
    $releaseKind = [string](Get-PropValue $plan 'releaseKind' '')
    if ($releaseKind -cnotin @('prerelease', 'stable')) {
        throw (New-ReleaseModelError -Code 'invalid-plan' -Message "publish only handles prerelease/stable; got '$releaseKind'.")
    }
    $runId = [long](Get-RequiredEnv 'GITHUB_RUN_ID')
    $runAttempt = [int](Get-RequiredEnv 'GITHUB_RUN_ATTEMPT')
    $releaseId = [long](Get-PropValue $stage 'releaseId' 0)
    $sourceSha = [string](Get-PropValue $stage 'sourceSha' '')
    $version = [string](Get-PropValue $stage 'version' '')
    $tag = [string](Get-PropValue $stage 'tag' '')
    $candidateDigest = [string](Get-PropValue $stage 'candidateDigest' '')
    $stageCompletedAt = [string](Get-PropValue $stage 'stageCompletedAt' '')
    $formats = Resolve-ReleaseFormatSet -Formats ([string[]]@(Get-PropValue $stage 'formats' @()))
    $stageNonce = [string](Get-PropValue $stage 'approvalNonce' '')
    if ($stageNonce -cnotmatch '^[0-9a-f]{64}$') {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Stage summary has no valid 256-bit approval nonce.')
    }
    if ((Get-OptionalEnv 'STAGE_APPROVAL_NONCE' '') -cne $stageNonce) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Stage approval nonce output does not match the stage summary.')
    }
    if ($releaseId -le 0 -or $sourceSha -cnotmatch '^[0-9a-f]{40}$' -or $candidateDigest -cnotmatch '^[0-9a-f]{64}$') {
        throw (New-ReleaseModelError -Code 'invalid-stage' -Message 'Stage summary is missing release/source/candidate identity.')
    }
    if ($tag -cne (Get-ReleaseTag -Version $version)) { throw (New-ReleaseModelError -Code 'invalid-stage' -Message 'Stage tag/version mismatch.') }
    $allowUnsigned = [bool](Get-PropValue $plan 'allowUnsignedPrerelease' $false)
    $stageSigning = Get-PropValue $stage 'signing' $null
    $signingMode = [string](Get-PropValue $stageSigning 'mode' '')
    $signingStatus = [string](Get-PropValue $stageSigning 'status' '')
    $signingTrusted = [bool](Get-PropValue $stageSigning 'trusted' $false)
    $signingTimestamped = [bool](Get-PropValue $stageSigning 'timestamped' $false)
    $null = Get-ReleaseSigningPolicy -ReleaseKind $releaseKind -SigningMode $signingMode -SigningStatus $signingStatus -Trusted $signingTrusted -Timestamped $signingTimestamped -AllowUnsignedPrerelease $allowUnsigned

    $api = New-ReleaseApiContextFromEnv -Mode $Mode
    $tempDir = New-ReleaseTempDirectory -Prefix 'cb-publish'
    $release = Get-GitHubRelease -Context $api -ReleaseId $releaseId
    if ($null -eq $release) { throw (New-ReleaseModelError -Code 'release-disappeared' -Message "Staged release $releaseId no longer exists.") }
    if ([string](Get-PropValue $release 'tag_name' '') -cne $tag) { throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Staged release tag changed.') }
    $state = Get-ReleaseState -Release $release
    if ($state -ceq 'stable' -and $releaseKind -ceq 'prerelease') {
        throw (New-ReleaseModelError -Code 'state-regression' -Message 'Refusing to downgrade a stable release to prerelease.')
    }
    if ($state -ceq 'stable' -and $releaseKind -ceq 'stable') {
        $summary.Add('Release is already stable; verified assets and tag. No status change was needed.')
        $null = Assert-StageAssetsUnchanged -Context $api -ReleaseId $releaseId -StageAssets ([object[]]@(Get-PropValue $stage 'assets' @())) -Directory $tempDir
        $tagSha = Get-GitHubTagRef -Context $api -Tag $tag
        if ($tagSha -cne $sourceSha) { throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Stable release tag does not point to the staged source SHA.') }
        Write-GitHubOutput -Name 'published' -Value 'already-complete'
        Write-GitHubOutput -Name 'release_url' -Value ([string](Get-PropValue $release 'html_url' ''))
        Write-ReleaseSummary -Heading 'Windows release publish' -Lines ([string[]]$summary.ToArray())
        $exitCode = 0
        exit $exitCode
    }
    if ($state -ceq 'prerelease' -and $releaseKind -ceq 'prerelease') {
        $summary.Add('Release is already a prerelease; verified assets and tag. No status change was needed.')
        $null = Assert-StageAssetsUnchanged -Context $api -ReleaseId $releaseId -StageAssets ([object[]]@(Get-PropValue $stage 'assets' @())) -Directory $tempDir
        $tagSha = Get-GitHubTagRef -Context $api -Tag $tag
        if ($tagSha -cne $sourceSha) { throw (New-ReleaseModelError -Code 'tag-mismatch' -Message 'Prerelease tag does not point to the staged source SHA.') }
        Write-GitHubOutput -Name 'published' -Value 'already-complete'
        Write-GitHubOutput -Name 'release_url' -Value ([string](Get-PropValue $release 'html_url' ''))
        Write-ReleaseSummary -Heading 'Windows release publish' -Lines ([string[]]$summary.ToArray())
        $exitCode = 0
        exit $exitCode
    }
    if ($state -cnotin @('draft', 'prerelease')) {
        throw (New-ReleaseModelError -Code 'invalid-release-state' -Message "Cannot publish from state '$state'.")
    }

    $verifiedAssets = Assert-StageAssetsUnchanged -Context $api -ReleaseId $releaseId -StageAssets ([object[]]@(Get-PropValue $stage 'assets' @())) -Directory $tempDir
    $signatureArtifacts = @($verifiedAssets | Where-Object { Get-ArtifactFormatFromName -Name $_.name } | ForEach-Object {
            [pscustomobject]@{ name = $_.name; format = (Get-ArtifactFormatFromName -Name $_.name) }
        })
    if ($signingStatus -ceq 'signed') {
        $null = Assert-ReleaseArtifactSignatures -Directory $tempDir -Artifacts $signatureArtifacts -SigningStatus 'signed'
    }
    elseif ($releaseKind -ceq 'stable') {
        throw (New-ReleaseModelError -Code 'signing-required' -Message 'Stable publication requires signed/trusted/timestamped assets.')
    }

    if ($releaseKind -ceq 'stable') {
        $environment = Get-GitHubEnvironment -Context $api -Name 'windows-release-stable'
        if ($null -eq $environment) {
            throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Required environment 'windows-release-stable' does not exist; stable publication fails closed.")
        }
        $environmentPolicy = Get-EnvironmentReviewerPolicy -EnvironmentName 'windows-release-stable' -Environment $environment
        $approvals = Get-GitHubRunApprovals -Context $api -RunId $runId
        $jobs = @(Get-GitHubWorkflowRunJobs -Context $api -RunId $runId -Attempt $runAttempt)
        $timing = Get-ApprovalGateTiming -Jobs $jobs -ExpectedRunId $runId -ExpectedRunAttempt $runAttempt
        $gateNonce = Get-OptionalEnv 'GATE_APPROVAL_NONCE' ''
        if ($gateNonce -cne $stageNonce) {
            throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Approve-stable did not return the current stage nonce; stable publication fails closed.')
        }
        $gateAttempt = 0
        if (-not [int]::TryParse((Get-OptionalEnv 'GATE_RUN_ATTEMPT' ''), [ref]$gateAttempt) -or $gateAttempt -ne $runAttempt) {
            throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Approve-stable did not return the current run attempt; stable publication fails closed.')
        }
        $approvalEvidence = Test-ApprovalRecords -Reviews $approvals -EnvironmentName 'windows-release-stable' -RequiredReviewerLogins $environmentPolicy.RequiredReviewerLogins -CandidateDigest $candidateDigest -ApprovalNonce $stageNonce -RunId $runId -RunAttempt $runAttempt -StageCompletedAt $timing.StageCompletedAt -GateJobStartedAt $timing.GateStartedAt
        $null = Assert-StableReleaseGate -SigningMode $signingMode -SigningStatus $signingStatus -Trusted $signingTrusted -Timestamped $signingTimestamped -EnvironmentPolicy $environmentPolicy -ApprovalEvidence $approvalEvidence
        $summary.Add("Stable approval verified: $($approvalEvidence.Approvals.Count) approval(s). nonce=$stageNonce; GitHub stage job completed_at=$($timing.StageCompletedAt); GitHub approve-stable job started_at=$($timing.GateStartedAt); sameSecond=$($timing.SameSecond); local stage timestamp (diagnostic, not authority)=$stageCompletedAt. The public API does not expose the actual approval time; none is claimed.")
    }

    $targetPrerelease = ($releaseKind -ceq 'prerelease')
    $latestVersion = Get-LatestWindowsStableVersion -Context $api
    $makeLatest = if ($targetPrerelease) { 'false' } else { Get-PublishLatestMode -CurrentStableLatestVersion $latestVersion -NewVersion $version }
    $needsPatch = (([bool](Get-PropValue $release 'draft' $false)) -or (([bool](Get-PropValue $release 'prerelease' $false)) -ne $targetPrerelease))
    if ($needsPatch) {
        $update = [ordered]@{ draft = $false; prerelease = $targetPrerelease }
        if ($releaseKind -ceq 'stable') { $update['make_latest'] = $makeLatest }
        $null = Update-GitHubRelease -Context $api -ReleaseId $releaseId -Update $update
        $patched = $true
    }

    $after = Get-GitHubRelease -Context $api -ReleaseId $releaseId
    if ($null -eq $after) { throw (New-ReleaseModelError -Code 'post-publish-failed' -Message 'Release disappeared after status update.') }
    if ([bool](Get-PropValue $after 'draft' $true)) {
        throw (New-ReleaseModelError -Code 'post-publish-failed' -Message 'Published but the release is still a draft.')
    }
    if ([bool](Get-PropValue $after 'prerelease' $false) -ne $targetPrerelease) {
        throw (New-ReleaseModelError -Code 'post-publish-failed' -Message 'Published but the prerelease flag is wrong.')
    }
    if ([string](Get-PropValue $after 'tag_name' '') -cne $tag) {
        throw (New-ReleaseModelError -Code 'post-publish-failed' -Message 'Published but the release tag changed.')
    }
    $tagSha = $null
    for ($attempt = 1; $attempt -le 6; $attempt++) {
        $tagSha = Get-GitHubTagRef -Context $api -Tag $tag
        if ($tagSha -ceq $sourceSha) { break }
        if ($attempt -lt 6) { Start-Sleep -Seconds 2 }
    }
    if ($tagSha -cne $sourceSha) {
        throw (New-ReleaseModelError -Code 'post-publish-failed' -Message "Published but tag '$tag' does not point to the staged source SHA; no tag move is attempted.")
    }
    $null = Assert-StageAssetsUnchanged -Context $api -ReleaseId $releaseId -StageAssets ([object[]]@(Get-PropValue $stage 'assets' @())) -Directory $tempDir
    if ($releaseKind -ceq 'stable') {
        $latestAfter = $null
        for ($attempt = 1; $attempt -le 6; $attempt++) {
            $latestAfter = Get-LatestWindowsStableVersion -Context $api
            if ($makeLatest -ceq 'true' -and $latestAfter -ceq $version) { break }
            if ($makeLatest -ceq 'false' -and $latestAfter -cne $version) { break }
            if ($attempt -lt 6) { Start-Sleep -Seconds 2 }
        }
        if ($makeLatest -ceq 'true' -and $latestAfter -cne $version) {
            throw (New-ReleaseModelError -Code 'post-publish-failed' -Message "Published but latest Windows stable is '$latestAfter', expected '$version'.")
        }
        if ($makeLatest -ceq 'false' -and $latestAfter -ceq $version) {
            throw (New-ReleaseModelError -Code 'post-publish-failed' -Message 'Published a lower stable version but it unexpectedly became latest.')
        }
    }

    Write-GitHubOutput -Name 'published' -Value 'published'
    Write-GitHubOutput -Name 'release_url' -Value ([string](Get-PropValue $after 'html_url' ''))
    $summary.Insert(0, "Published **$version** as $releaseKind.")
    $summary.Add('')
    $summary.Add("| field | value |")
    $summary.Add('|---|---|')
    $summary.Add("| release | id $releaseId, $releaseKind |")
    $summary.Add("| tag | ``$tag`` -> ``$sourceSha`` |")
    $summary.Add("| candidate manifest SHA256 | ``$candidateDigest`` |")
    $summary.Add("| make_latest | $makeLatest |")
    $summary.Add('')
    $summary.Add('Post-publish verification passed: tag, asset IDs/sizes/hashes and release flags were re-checked.')
    Write-ReleaseSummary -Heading 'Windows release publish' -Lines ([string[]]$summary.ToArray())
    $exitCode = 0
}
catch {
    $errorText = Get-ReleaseErrorText -ErrorRecord $_
    if ($patched) {
        Write-ReleaseSummary -Heading 'Windows release published but verification failed' -Lines @('The release status change was already applied; assets and the tag were not modified.', '', '```', $errorText, '```')
        Write-Host "PUBLISHED BUT VERIFICATION FAILED: $errorText" -ForegroundColor Red
    }
    else {
        Write-ReleaseSummary -Heading 'Windows release publish failed' -Lines @('```', $errorText, '```')
        Write-Host "PUBLISH FAILED: $errorText" -ForegroundColor Red
    }
    $exitCode = 1
}
finally {
    if ($tempDir -and (Test-Path -LiteralPath $tempDir)) {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
exit $exitCode
