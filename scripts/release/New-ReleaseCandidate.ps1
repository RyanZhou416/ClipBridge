#Requires -Version 7.0
<#
.SYNOPSIS
    Assemble the immutable public candidate from package-summary.json and the
    rust/windows verification summaries.

.DESCRIPTION
    The build job runs this after windows.ps1 and build-windows-installers.ps1.
    Only role=release artifacts are copied to the public candidate directory;
    the internal user MSI, logs and absolute paths never enter it. The manifest
    is generated here (build time) and never rewritten by stage/publish.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutDir,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$SourceCommit,
    [Parameter(Mandatory)][string[]]$Formats,
    [Parameter(Mandatory)][long]$SourceRunId,
    [Parameter(Mandatory)][int]$SourceRunAttempt,
    [Parameter(Mandatory)][string]$ArtifactName,
    [Parameter(Mandatory)][string]$PackageSummaryPath,
    [Parameter(Mandatory)][string]$RustSummaryPath,
    [Parameter(Mandatory)][string]$WindowsSummaryPath,
    [string]$GeneratedAtUtc = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$scriptDir = $PSScriptRoot
Import-Module (Join-Path $scriptDir 'ReleaseModel.psm1') -Force
Import-Module (Join-Path $scriptDir 'ReleaseRuntime.psm1') -Force

$null = ConvertTo-ReleaseVersion $Version
if ($SourceCommit -cnotmatch '^[0-9a-f]{40}$') { throw (New-ReleaseModelError -Code 'invalid-input' -Message 'SourceCommit must be a full 40-hex SHA.') }
$formatSet = Resolve-ReleaseFormatSet -Formats $Formats
if ($SourceRunId -le 0 -or $SourceRunAttempt -lt 1) { throw (New-ReleaseModelError -Code 'invalid-input' -Message 'Source run id/attempt is invalid.') }
if ($ArtifactName -cnotmatch '^windows-release-candidate-[0-9]+-[0-9]+$') { throw (New-ReleaseModelError -Code 'invalid-input' -Message "ArtifactName '$ArtifactName' is invalid.") }

$packageSummary = Read-JsonFile -Path $PackageSummaryPath
if (-not $GeneratedAtUtc) { $GeneratedAtUtc = [string](Get-PropValue $packageSummary 'generatedAtUtc' '') }
$package = Test-PackageSummary -Summary $packageSummary -Version $Version -Formats $formatSet
$rustEvidence = Test-RustSummary -Summary (Read-JsonFile -Path $RustSummaryPath) -ExpectedCommit $SourceCommit
$windowsEvidence = Test-WindowsSummary -Summary (Read-JsonFile -Path $WindowsSummaryPath) -ExpectedCommit $SourceCommit -Formats $formatSet
$nativeEvidence = Test-ReleaseNativeEvidence -PackageNativeEvidence (Get-PropValue $packageSummary 'nativeEvidence' $null) -ExpectedWindowsDllSha256 $windowsEvidence.nativeDllSha256

$packageDir = Split-Path -Parent $PackageSummaryPath
$releaseArtifacts = [System.Collections.Generic.List[object]]::new()
$sourceFiles = [System.Collections.Generic.List[object]]::new()
foreach ($record in $package.ReleaseArtifacts) {
    if (-not (Test-SafeRelativePath -Path $record.name)) {
        throw (New-ReleaseModelError -Code 'unsafe-path' -Message "package-summary artifact name '$($record.name)' is not a safe relative name.")
    }
    $source = Join-Path $packageDir $record.name
    $null = Assert-PathUnderRoot -Path $source -Root $packageDir
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw (New-ReleaseModelError -Code 'missing-file' -Message "package artifact '$($record.name)' is missing.")
    }
    $actualHash = Get-FileSha256 -Path $source
    $actualSize = [long](Get-Item -LiteralPath $source).Length
    if ($actualHash -cne $record.sha256 -or $actualSize -ne $record.size) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "package artifact '$($record.name)' does not match package-summary.")
    }
    $releaseArtifacts.Add([pscustomobject]@{
            name    = $record.name
            format  = $record.format
            role    = 'release'
            internal = $false
            size    = $actualSize
            sha256  = $actualHash
            signing = [pscustomobject]@{
                authenticode   = $(if ($record.format -eq 'portable-zip') { 'not-applicable' } else { if ($package.Signing.status -eq 'signed') { 'signed' } else { 'unsigned' } })
                timestamped    = [bool]$package.Signing.timestamped
                payloadsSigned = $(if ($record.format -eq 'portable-zip') { [bool]($package.Signing.status -eq 'signed') } else { $null })
            }
        })
    $sourceFiles.Add([pscustomobject]@{ Name = $record.name; Source = $source })
}

$toolchain = [ordered]@{}
$rustc = Get-Command rustc -ErrorAction SilentlyContinue
if ($rustc) {
    $rustVersion = (& rustc --version 2>$null)
    if ($LASTEXITCODE -eq 0 -and $rustVersion) { $toolchain['rustc'] = ([string]$rustVersion).Trim() }
}
$dotnet = Get-Command dotnet -ErrorAction SilentlyContinue
if ($dotnet) {
    $dotnetVersion = (& dotnet --version 2>$null)
    if ($LASTEXITCODE -eq 0 -and $dotnetVersion) { $toolchain['dotnet'] = ([string]$dotnetVersion).Trim() }
}
$toolchain['powershell'] = $PSVersionTable.PSVersion.ToString()
$toolchain['runnerOS'] = Get-OptionalEnv 'RUNNER_OS' 'Windows'
$toolchain['runnerImage'] = Get-OptionalEnv 'ImageOS' ''
if ($toolchain['runnerImage'] -eq '') { $toolchain.Remove('runnerImage') }

$manifest = New-ReleaseManifest -Version $Version -SourceCommit $SourceCommit -Formats $formatSet `
    -SigningMode $package.Signing.mode -SigningStatus $package.Signing.status -Trusted $package.Signing.trusted -Timestamped $package.Signing.timestamped `
    -SourceRunId $SourceRunId -SourceRunAttempt $SourceRunAttempt -CandidateArtifactName $ArtifactName `
    -Artifacts ([object[]]$releaseArtifacts.ToArray()) -RustEvidence $rustEvidence -WindowsEvidence $windowsEvidence `
    -Toolchain $toolchain -GeneratedAtUtc $GeneratedAtUtc
$manifestText = ConvertTo-ReleaseManifestJson -Manifest $manifest
$manifestDigest = Get-ReleaseManifestDigest -ManifestText $manifestText

$outRoot = [System.IO.Path]::GetFullPath($(if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path (Split-Path -Parent (Split-Path -Parent $scriptDir)) $OutDir }))
if (Test-Path -LiteralPath $outRoot) {
    # The build workflow supplies a fresh directory; refuse to mix old files into a candidate.
    $existing = @(Get-ChildItem -LiteralPath $outRoot -Force)
    if ($existing.Count -gt 0) {
        throw (New-ReleaseModelError -Code 'unsafe-output' -Message "Candidate output directory must be empty: $outRoot")
    }
}
$null = New-Item -ItemType Directory -Force -Path $outRoot
$evidenceDir = Join-Path $outRoot 'evidence'
$null = New-Item -ItemType Directory -Force -Path $evidenceDir

foreach ($file in $sourceFiles) {
    $destination = Join-Path $outRoot $file.Name
    $null = Assert-PathUnderRoot -Path $destination -Root $outRoot
    Copy-Item -LiteralPath $file.Source -Destination $destination -Force
    if ((Get-FileSha256 -Path $destination) -cne ($releaseArtifacts | Where-Object { $_.name -ceq $file.Name }).sha256) {
        throw (New-ReleaseModelError -Code 'asset-hash-mismatch' -Message "Copied candidate file '$($file.Name)' changed hash.")
    }
}
Write-Utf8Text -Path (Join-Path $outRoot 'release-manifest.json') -Text $manifestText
Copy-Item -LiteralPath $PackageSummaryPath -Destination (Join-Path $evidenceDir 'package-summary.json') -Force
Copy-Item -LiteralPath $RustSummaryPath -Destination (Join-Path $evidenceDir 'rust-summary.json') -Force
Copy-Item -LiteralPath $WindowsSummaryPath -Destination (Join-Path $evidenceDir 'windows-summary.json') -Force

$checksumEntries = [System.Collections.Generic.List[object]]::new()
foreach ($artifact in $releaseArtifacts) {
    $checksumEntries.Add([pscustomobject]@{ name = $artifact.name; sha256 = $artifact.sha256 })
}
$checksumEntries.Add([pscustomobject]@{ name = 'release-manifest.json'; sha256 = $manifestDigest })
$checksumText = New-Sha256SumsText -Entries ([object[]]$checksumEntries.ToArray())
Write-Utf8Text -Path (Join-Path $outRoot 'SHA256SUMS.txt') -Text $checksumText
$null = Test-Sha256SumsText -Text $checksumText -ExpectedEntries ([object[]]$checksumEntries.ToArray())

# Re-open the written manifest exactly as stage will; catches serialization drift here.
$written = Read-ReleaseManifestText -Text (Read-Utf8Text -Path (Join-Path $outRoot 'release-manifest.json'))
if ($written.Digest -cne $manifestDigest) {
    throw (New-ReleaseModelError -Code 'candidate-digest-mismatch' -Message 'Written manifest digest changed during serialization.')
}
if (@(Get-ChildItem -LiteralPath $outRoot -Recurse -File -Filter '*-user.msi').Count -gt 0) {
    throw (New-ReleaseModelError -Code 'forbidden-asset' -Message 'Internal user MSI must not be copied into the public candidate.')
}

Write-GitHubOutput -Name 'candidate_digest' -Value $manifestDigest
Write-GitHubOutput -Name 'candidate_artifact_name' -Value $ArtifactName
Write-ReleaseSummary -Heading 'Windows release candidate' -Lines @(
    "Candidate manifest SHA256: ``$manifestDigest``",
    "Native source reuse: input=stage=$($nativeEvidence.StageDllSha256)",
    '',
    "Artifacts: $(@($releaseArtifacts | ForEach-Object { $_.name }) -join ', ')",
    '',
    'The internal user MSI and raw logs are kept out of this public candidate.'
)
Write-Host "Candidate assembled at $outRoot (manifest SHA256 $manifestDigest)"
