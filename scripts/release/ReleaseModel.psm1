#Requires -Version 7.0
<#
.SYNOPSIS
    Pure release metadata/state/policy model for the manual Windows release.

.DESCRIPTION
    This module is deliberately side-effect free. It never reads files, the
    network, environment variables or secrets; every decision is a function of
    its parameters. The release workflow entrypoints (preflight/stage/publish)
    own all I/O and call these functions.

    Scope decisions encoded here:
      - one manual entry, fixed x64 Windows artifacts: machine MSI, user EXE,
        portable ZIP, plus SHA256SUMS.txt and release-manifest.json;
      - internal user MSI, PDB, logs, MSIX/StoreUpload/winget and arbitrary
        files are never public release assets;
      - a published release asset is immutable: no clobber, no add-a-format
        after publication, no state regression;
      - stable promotion requires a signed, trusted, timestamped candidate and
        a verifiable environment approval record bound to the candidate digest;
      - anything that cannot be verified fails closed.
#>

Set-StrictMode -Version Latest

$script:FormatOrder = @('machine-msi', 'user-exe', 'portable-zip')
$script:MetadataAssetNames = @('SHA256SUMS.txt', 'release-manifest.json')
$script:ReleaseWorkflowPath = '.github/workflows/release-windows.yml'
$script:ReleaseWorkflowRef = 'refs/heads/main'
$script:StableEnvironment = 'windows-release-stable'
$script:VersionPattern = '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
$script:Sha256Pattern = '^[0-9a-f]{64}$'
$script:CommitPattern = '^[0-9a-f]{40}$'
$script:TagPattern = '^clipbridge-windows-(?<version>(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))$'

function New-ReleaseModelError {
    <# Creates a typed-in-spirit error with a stable machine-readable code. #>
    param(
        [Parameter(Mandatory)][string]$Message,
        [string]$Code = 'invalid-release-state'
    )
    $errorRecord = [System.InvalidOperationException]::new($Message)
    $errorRecord.Data['ReleaseErrorCode'] = $Code
    return $errorRecord
}

function Get-PropValue {
    <# Null-safe property lookup for PSCustomObject / IDictionary / $null. #>
    param(
        [object]$Object,
        [Parameter(Mandatory)][string]$Name,
        [object]$Default = $null
    )
    if ($null -eq $Object) { return $Default }
    if ($Object -is [System.Collections.IDictionary]) {
        if ($Object.Contains($Name)) { return $Object[$Name] }
        return $Default
    }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $Default }
    return $property.Value
}

function ConvertTo-ReleaseBoolean {
    <# Accepts only real booleans or the workflow string literals true/false. #>
    param(
        [object]$Value,
        [Parameter(Mandatory)][string]$Name
    )
    if ($Value -is [bool]) { return [bool]$Value }
    if ($Value -is [string]) {
        switch ($Value.Trim().ToLowerInvariant()) {
            'true' { return $true }
            'false' { return $false }
        }
    }
    throw (New-ReleaseModelError -Code 'invalid-input' -Message "$Name must be a boolean (true/false); got '$Value'.")
}

function ConvertTo-ReleaseVersion {
    <# Parses and bounds the three-part installer version exactly like scripts/version.ps1. #>
    param([Parameter(Mandatory)][string]$Version)
    if ($Version -cnotmatch $script:VersionPattern) {
        throw (New-ReleaseModelError -Code 'invalid-version' -Message "Version must be three numeric segments without leading zeros (e.g. 0.1.0); got '$Version'.")
    }
    $major = [long]$Matches[1]
    $minor = [long]$Matches[2]
    $patch = [long]$Matches[3]
    if ($major -gt 255 -or $minor -gt 255 -or $patch -gt 65535) {
        throw (New-ReleaseModelError -Code 'invalid-version' -Message "Version $Version exceeds the MSI limits (major/minor <= 255, build <= 65535).")
    }
    return [pscustomobject]@{
        Version = $Version
        Major   = $major
        Minor   = $minor
        Patch   = $patch
    }
}

function Compare-ReleaseVersion {
    <# Returns -1, 0 or 1. Accepts already-validated three-part versions. #>
    param(
        [Parameter(Mandatory)][string]$Left,
        [Parameter(Mandatory)][string]$Right
    )
    $l = ConvertTo-ReleaseVersion $Left
    $r = ConvertTo-ReleaseVersion $Right
    foreach ($field in @('Major', 'Minor', 'Patch')) {
        if ($l.$field -lt $r.$field) { return -1 }
        if ($l.$field -gt $r.$field) { return 1 }
    }
    return 0
}

function Resolve-SourceRefKind {
    <#
    .SYNOPSIS
        Classifies the workflow source_ref. Only main, a full 40-hex commit or
        the exact clipbridge-windows-<version> tag are accepted.
    #>
    param([Parameter(Mandatory)][AllowEmptyString()][string]$SourceRef)
    if ([string]::IsNullOrWhiteSpace($SourceRef)) {
        throw (New-ReleaseModelError -Code 'invalid-source-ref' -Message 'source_ref must not be empty.')
    }
    if ($SourceRef -cne $SourceRef.Trim()) {
        throw (New-ReleaseModelError -Code 'invalid-source-ref' -Message 'source_ref must not contain leading/trailing whitespace.')
    }
    if ($SourceRef -ceq 'main') {
        return [pscustomobject]@{ Kind = 'main'; Sha = $null; Tag = $null; TagVersion = $null }
    }
    if ($SourceRef -cmatch '^[0-9a-fA-F]{40}$') {
        return [pscustomobject]@{ Kind = 'sha'; Sha = $SourceRef.ToLowerInvariant(); Tag = $null; TagVersion = $null }
    }
    if ($SourceRef -cmatch $script:TagPattern) {
        $tagVersion = $Matches['version']
        $null = ConvertTo-ReleaseVersion $tagVersion
        return [pscustomobject]@{ Kind = 'tag'; Sha = $null; Tag = $SourceRef; TagVersion = $tagVersion }
    }
    throw (New-ReleaseModelError -Code 'invalid-source-ref' -Message "source_ref must be 'main', a full 40-hex commit SHA, or a clipbridge-windows-<version> tag; got '$SourceRef'.")
}

function Get-ReleaseTag {
    param([Parameter(Mandatory)][string]$Version)
    $null = ConvertTo-ReleaseVersion $Version
    return "clipbridge-windows-$Version"
}

function Resolve-ReleaseFormatSet {
    <# Canonical, duplicate-free, non-empty public format selection. #>
    param([string[]]$Formats)
    $inputFormats = @($Formats | Where-Object { $null -ne $_ -and [string]$_ -ne '' })
    if ($inputFormats.Count -eq 0) {
        throw (New-ReleaseModelError -Code 'invalid-formats' -Message 'At least one of machine-msi, user-exe, portable-zip must be selected.')
    }
    $seen = @{}
    foreach ($format in $inputFormats) {
        if ($format -ceq 'user-msi') {
            throw (New-ReleaseModelError -Code 'invalid-formats' -Message 'user-msi is an internal dependency of user-exe and is not a selectable release format.')
        }
        if ($script:FormatOrder -cnotcontains $format) {
            throw (New-ReleaseModelError -Code 'invalid-formats' -Message "Unknown release format '$format'. Allowed: $($script:FormatOrder -join ', ').")
        }
        if ($seen.ContainsKey($format)) { continue }
        $seen[$format] = $true
    }
    return [string[]]@($script:FormatOrder | Where-Object { $seen.ContainsKey($_) })
}

function Get-ReleaseFormatsFromBooleans {
    param(
        [Parameter(Mandatory)][bool]$MachineMsi,
        [Parameter(Mandatory)][bool]$UserExe,
        [Parameter(Mandatory)][bool]$PortableZip
    )
    $formats = [System.Collections.Generic.List[string]]::new()
    if ($MachineMsi) { $formats.Add('machine-msi') }
    if ($UserExe) { $formats.Add('user-exe') }
    if ($PortableZip) { $formats.Add('portable-zip') }
    return Resolve-ReleaseFormatSet -Formats ([string[]]$formats.ToArray())
}

function Get-ReleaseAssetName {
    param(
        [Parameter(Mandatory)][string]$Format,
        [Parameter(Mandatory)][string]$Version
    )
    $null = ConvertTo-ReleaseVersion $Version
    switch ($Format) {
        'machine-msi' { return "ClipBridge-$Version-win-x64-machine.msi" }
        'user-exe' { return "ClipBridge-$Version-win-x64-user.exe" }
        'portable-zip' { return "ClipBridge-$Version-win-x64-portable.zip" }
        'user-msi' { return "ClipBridge-$Version-win-x64-user.msi" }
        default {
            throw (New-ReleaseModelError -Code 'invalid-formats' -Message "Unknown artifact format '$Format'.")
        }
    }
}

function Get-ReleasePackageAssetNames {
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string[]]$Formats
    )
    $set = Resolve-ReleaseFormatSet -Formats $Formats
    $names = [System.Collections.Generic.List[string]]::new()
    foreach ($format in $set) { $names.Add((Get-ReleaseAssetName -Format $format -Version $Version)) }
    return [string[]]$names.ToArray()
}

function Get-ReleasePublicAssetNames {
    <# Selected packages plus the two metadata files, in deterministic order. #>
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string[]]$Formats
    )
    $names = [System.Collections.Generic.List[string]]::new()
    foreach ($name in (Get-ReleasePackageAssetNames -Version $Version -Formats $Formats)) { $names.Add($name) }
    foreach ($name in $script:MetadataAssetNames) { $names.Add($name) }
    return [string[]]$names.ToArray()
}

function Get-ReleaseUploadOrder {
    <# Metadata first so a partially uploaded draft still carries its source record. #>
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string[]]$Formats
    )
    $names = [System.Collections.Generic.List[string]]::new()
    $names.Add('release-manifest.json')
    $names.Add('SHA256SUMS.txt')
    foreach ($name in (Get-ReleasePackageAssetNames -Version $Version -Formats $Formats)) { $names.Add($name) }
    return [string[]]$names.ToArray()
}

function Assert-ReleaseAssetSet {
    <#
    .SYNOPSIS
        Compares actual release asset names with the fixed whitelist. Extras,
        internal artifacts and path-like names always fail.
    #>
    param(
        [string[]]$ActualNames,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string[]]$Formats,
        [switch]$AllowMissing
    )
    $expected = Get-ReleasePublicAssetNames -Version $Version -Formats $Formats
    $actual = @($ActualNames | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
    $seen = @{}
    foreach ($name in $actual) {
        if ($name -cne $name.Trim() -or $name -match '[/\\]' -or $name -in @('.', '..')) {
            throw (New-ReleaseModelError -Code 'invalid-asset' -Message "Release asset name '$name' is not a plain file name.")
        }
        if ($seen.ContainsKey($name)) {
            throw (New-ReleaseModelError -Code 'invalid-asset' -Message "Duplicate release asset name '$name'.")
        }
        $seen[$name] = $true
        if ($name -match '(?i)\.pdb$' -or $name -match '(?i)\.log$' -or $name -match '(?i)\.msix(bundle)?$' -or $name -match '(?i)\.appx(bundle)?$' -or $name -match '(?i)\.pfx$' -or $name -match '(?i)\.cer$' -or $name -match '(?i)\.pem$') {
            throw (New-ReleaseModelError -Code 'forbidden-asset' -Message "Release asset '$name' is outside the public whitelist (diagnostics/identity/MSIX are forbidden).")
        }
        if ($name -match '(?i)-user\.msi$') {
            throw (New-ReleaseModelError -Code 'forbidden-asset' -Message "Internal user MSI '$name' must never be a public release asset.")
        }
        if ($expected -cnotcontains $name) {
            throw (New-ReleaseModelError -Code 'forbidden-asset' -Message "Release asset '$name' is not in the fixed public whitelist: $($expected -join ', ').")
        }
    }
    $missing = @($expected | Where-Object { -not $seen.ContainsKey($_) })
    if (-not $AllowMissing -and $missing.Count -gt 0) {
        throw (New-ReleaseModelError -Code 'missing-asset' -Message "Release is missing required assets: $($missing -join ', ').")
    }
    return [pscustomobject]@{
        Expected = [string[]]$expected
        Actual   = [string[]]$actual
        Missing  = [string[]]$missing
        Complete = [bool]($missing.Count -eq 0)
    }
}

function New-Sha256SumsText {
    <#
    .SYNOPSIS
        Renders the deterministic SHA256SUMS.txt body. The entries must include
        release-manifest.json and the selected packages; the checksum file never
        hashes itself.
    #>
    param([Parameter(Mandatory)][object[]]$Entries)
    $byName = @{}
    foreach ($entry in $Entries) {
        $name = [string](Get-PropValue $entry 'name' '')
        $hash = ([string](Get-PropValue $entry 'sha256' '')).ToLowerInvariant()
        if ([string]::IsNullOrWhiteSpace($name) -or $name -cne $name.Trim() -or $name -match '[/\\]') {
            throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "SHA256SUMS entry has an invalid file name '$name'.")
        }
        if ($name -ceq 'SHA256SUMS.txt') {
            throw (New-ReleaseModelError -Code 'invalid-checksums' -Message 'SHA256SUMS.txt must not hash itself.')
        }
        if ($hash -cnotmatch $script:Sha256Pattern) {
            throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "SHA256SUMS entry '$name' has an invalid SHA256 '$hash'.")
        }
        if ($byName.ContainsKey($name)) {
            throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "SHA256SUMS entry '$name' is duplicated.")
        }
        $byName[$name] = $hash
    }
    if (-not $byName.ContainsKey('release-manifest.json')) {
        throw (New-ReleaseModelError -Code 'invalid-checksums' -Message 'SHA256SUMS.txt must cover release-manifest.json.')
    }
    $names = [string[]]@($byName.Keys)
    [Array]::Sort($names, [System.StringComparer]::Ordinal)
    $lines = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $names) { $lines.Add(('{0}  {1}' -f $byName[$name], $name)) }
    return (($lines -join "`n") + "`n")
}

function Test-Sha256SumsText {
    param(
        [Parameter(Mandatory)][string]$Text,
        [object[]]$ExpectedEntries,
        [switch]$RequireExactSet
    )
    if ([string]::IsNullOrWhiteSpace($Text)) {
        throw (New-ReleaseModelError -Code 'invalid-checksums' -Message 'SHA256SUMS.txt is empty.')
    }
    $byName = @{}
    $normalized = $Text -replace "`r`n", "`n"
    foreach ($line in $normalized.Split("`n")) {
        if ($line -ceq '') { continue }
        if ($line -cnotmatch '^(?<hash>[0-9a-f]{64})  (?<name>[^/\\]+)$') {
            throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "Malformed SHA256SUMS line '$line'.")
        }
        $name = $Matches['name']
        $hash = $Matches['hash']
        if ($name -ceq 'SHA256SUMS.txt') {
            throw (New-ReleaseModelError -Code 'invalid-checksums' -Message 'SHA256SUMS.txt must not hash itself.')
        }
        if ($byName.ContainsKey($name)) {
            throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "SHA256SUMS entry '$name' is duplicated.")
        }
        $byName[$name] = $hash
    }
    if (-not $byName.ContainsKey('release-manifest.json')) {
        throw (New-ReleaseModelError -Code 'invalid-checksums' -Message 'SHA256SUMS.txt does not cover release-manifest.json.')
    }
    if ($null -ne $ExpectedEntries) {
        $expected = @{}
        foreach ($entry in $ExpectedEntries) {
            $name = [string](Get-PropValue $entry 'name' '')
            $hash = ([string](Get-PropValue $entry 'sha256' '')).ToLowerInvariant()
            if ([string]::IsNullOrWhiteSpace($name) -or $hash -cnotmatch $script:Sha256Pattern) {
                throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "Expected checksum entry '$name' is invalid.")
            }
            $expected[$name] = $hash
        }
        foreach ($name in $expected.Keys) {
            if (-not $byName.ContainsKey($name)) {
                throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "SHA256SUMS.txt is missing '$name'.")
            }
            if ($byName[$name] -cne $expected[$name]) {
                throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "SHA256SUMS.txt hash mismatch for '$name'.")
            }
        }
        foreach ($name in $byName.Keys) {
            if (-not $expected.ContainsKey($name)) {
                throw (New-ReleaseModelError -Code 'invalid-checksums' -Message "SHA256SUMS.txt contains unexpected entry '$name'.")
            }
        }
    }
    elseif ($RequireExactSet) {
        throw (New-ReleaseModelError -Code 'invalid-checksums' -Message 'RequireExactSet needs ExpectedEntries.')
    }
    return [pscustomobject]@{
        Entries = @($byName.Keys | Sort-Object | ForEach-Object {
                [pscustomobject]@{ name = $_; sha256 = $byName[$_] }
            })
    }
}

function Get-ReleaseManifestDigest {
    <# SHA256 of the exact UTF-8 bytes of the manifest text (no BOM). #>
    param([Parameter(Mandatory)][string]$ManifestText)
    $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($ManifestText)
    $hash = [System.Security.Cryptography.SHA256]::HashData($bytes)
    return ([System.Convert]::ToHexString($hash)).ToLowerInvariant()
}

function Assert-NoUnsafeManifestContent {
    <# Rejects absolute/UNC paths and NUL bytes anywhere in a manifest-like object. #>
    param([object]$Value, [string]$Path = '$')
    if ($null -eq $Value) { return }
    if ($Value -is [string]) {
        $text = [string]$Value
        if ($text.Contains([char]0)) {
            throw (New-ReleaseModelError -Code 'unsafe-manifest' -Message "Manifest field $Path contains a NUL byte.")
        }
        if ($text -match '^[A-Za-z]:[\\/]' -or $text -match '^\\\\' -or $text -match '^/(?!/)') {
            throw (New-ReleaseModelError -Code 'unsafe-manifest' -Message "Manifest field $Path looks like an absolute path; only relative names/identifiers are allowed.")
        }
        return
    }
    if ($Value -is [System.Collections.IDictionary]) {
        foreach ($key in $Value.Keys) { Assert-NoUnsafeManifestContent -Value $Value[$key] -Path "$Path.$key" }
        return
    }
    if ($Value -is [System.Collections.IEnumerable] -and -not ($Value -is [string])) {
        $index = 0
        foreach ($item in $Value) {
            Assert-NoUnsafeManifestContent -Value $item -Path "$Path[$index]"
            $index++
        }
        return
    }
    foreach ($property in $Value.PSObject.Properties) {
        Assert-NoUnsafeManifestContent -Value $property.Value -Path "$Path.$($property.Name)"
    }
}

function New-ReleaseManifest {
    <#
    .SYNOPSIS
        Builds the immutable release-manifest.json object. Only role=release
        artifacts are recorded; internal user MSI/logs/absolute paths never enter.
    #>
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$SourceCommit,
        [Parameter(Mandatory)][string[]]$Formats,
        [Parameter(Mandatory)][string]$SigningMode,
        [Parameter(Mandatory)][string]$SigningStatus,
        [Parameter(Mandatory)][bool]$Trusted,
        [Parameter(Mandatory)][bool]$Timestamped,
        [Parameter(Mandatory)][long]$SourceRunId,
        [Parameter(Mandatory)][int]$SourceRunAttempt,
        [Parameter(Mandatory)][string]$CandidateArtifactName,
        [Parameter(Mandatory)][object[]]$Artifacts,
        [Parameter(Mandatory)][object]$RustEvidence,
        [Parameter(Mandatory)][object]$WindowsEvidence,
        [object]$Toolchain = @{},
        [string]$GeneratedAtUtc = ''
    )
    $formatSet = Resolve-ReleaseFormatSet -Formats $Formats
    $versionInfo = ConvertTo-ReleaseVersion $Version
    if ($SourceCommit -cnotmatch $script:CommitPattern) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "SourceCommit must be a full 40-hex SHA; got '$SourceCommit'.")
    }
    if ($SigningMode -cnotin @('None', 'Certificate')) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "SigningMode must be None or Certificate; got '$SigningMode'.")
    }
    if ($SigningStatus -cnotin @('unsigned', 'signed')) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "SigningStatus must be unsigned or signed; got '$SigningStatus'.")
    }
    if ($SigningStatus -ceq 'unsigned') {
        if ($Trusted -or $Timestamped) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'An unsigned manifest cannot be trusted or timestamped.')
        }
    }
    elseif (-not ($Trusted -and $Timestamped)) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'A signed manifest must be trusted and timestamped.')
    }
    if ($SourceRunId -le 0) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'SourceRunId must be positive.')
    }
    if ($SourceRunAttempt -lt 1) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'SourceRunAttempt must be >= 1.')
    }
    if ([string]::IsNullOrWhiteSpace($CandidateArtifactName) -or $CandidateArtifactName -match '[/\\]') {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "CandidateArtifactName must be a plain artifact name; got '$CandidateArtifactName'.")
    }

    $expectedPackageNames = Get-ReleasePackageAssetNames -Version $Version -Formats $formatSet
    $manifestArtifacts = [System.Collections.Generic.List[object]]::new()
    $seen = @{}
    foreach ($artifact in @($Artifacts)) {
        $name = [string](Get-PropValue $artifact 'name' '')
        $format = [string](Get-PropValue $artifact 'format' '')
        $role = [string](Get-PropValue $artifact 'role' '')
        $internal = [bool](Get-PropValue $artifact 'internal' $false)
        if ($role -cne 'release' -or $internal) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest may only contain release artifacts; '$name' is internal.")
        }
        if ($expectedPackageNames -cnotcontains $name) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Artifact '$name' is not a selected public package ($($expectedPackageNames -join ', ')).")
        }
        if ($format -cnotin $formatSet) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Artifact '$name' has an unselected format '$format'.")
        }
        if ($name -cne (Get-ReleaseAssetName -Format $format -Version $Version)) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Artifact '$name' does not match format '$format' and version '$Version'.")
        }
        if ($seen.ContainsKey($name)) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Artifact '$name' is duplicated.")
        }
        $seen[$name] = $true
        $size = [long](Get-PropValue $artifact 'size' 0)
        $sha = ([string](Get-PropValue $artifact 'sha256' '')).ToLowerInvariant()
        if ($size -le 0) { throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Artifact '$name' has a non-positive size.") }
        if ($sha -cnotmatch $script:Sha256Pattern) { throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Artifact '$name' has an invalid sha256.") }
        $signing = Get-PropValue $artifact 'signing' $null
        $authenticode = [string](Get-PropValue $signing 'authenticode' '')
        $artifactTimestamped = [bool](Get-PropValue $signing 'timestamped' $false)
        $payloadsSigned = Get-PropValue $signing 'payloadsSigned' $null
        $manifestArtifacts.Add([ordered]@{
                name   = $name
                format = $format
                size   = $size
                sha256 = $sha
                signing = [ordered]@{
                    authenticode   = $authenticode
                    timestamped    = $artifactTimestamped
                    payloadsSigned = $(if ($format -ceq 'portable-zip') { [bool]$payloadsSigned } else { $null })
                }
            })
    }
    foreach ($name in $expectedPackageNames) {
        if (-not $seen.ContainsKey($name)) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest is missing selected package '$name'.")
        }
    }

    $verification = [ordered]@{
        rust    = [ordered]@{
            status = [string](Get-PropValue $RustEvidence 'status' '')
            commit = [string](Get-PropValue $RustEvidence 'commit' '')
            tests  = [ordered]@{
                suites     = [long](Get-PropValue (Get-PropValue $RustEvidence 'tests' $null) 'suites' 0)
                discovered = [long](Get-PropValue (Get-PropValue $RustEvidence 'tests' $null) 'discovered' 0)
                executed   = [long](Get-PropValue (Get-PropValue $RustEvidence 'tests' $null) 'executed' 0)
                passed     = [long](Get-PropValue (Get-PropValue $RustEvidence 'tests' $null) 'passed' 0)
                failed     = [long](Get-PropValue (Get-PropValue $RustEvidence 'tests' $null) 'failed' 0)
                ignored    = [long](Get-PropValue (Get-PropValue $RustEvidence 'tests' $null) 'ignored' 0)
                filtered   = [long](Get-PropValue (Get-PropValue $RustEvidence 'tests' $null) 'filtered' 0)
            }
        }
        windows = [ordered]@{
            status                  = [string](Get-PropValue $WindowsEvidence 'status' '')
            commit                  = [string](Get-PropValue $WindowsEvidence 'commit' '')
            configuration           = [string](Get-PropValue $WindowsEvidence 'configuration' '')
            platform                = [string](Get-PropValue $WindowsEvidence 'platform' '')
            runtimeIdentifier       = [string](Get-PropValue $WindowsEvidence 'runtimeIdentifier' '')
            publishReadyToRun       = [bool](Get-PropValue $WindowsEvidence 'publishReadyToRun' $false)
            nativeDllSha256         = [string](Get-PropValue $WindowsEvidence 'nativeDllSha256' '')
            mandatorySuitesExecuted = [bool](Get-PropValue $WindowsEvidence 'mandatorySuitesExecuted' $false)
            tests                   = [ordered]@{
                discovered   = [long](Get-PropValue (Get-PropValue $WindowsEvidence 'tests' $null) 'discovered' 0)
                executed     = [long](Get-PropValue (Get-PropValue $WindowsEvidence 'tests' $null) 'executed' 0)
                passed       = [long](Get-PropValue (Get-PropValue $WindowsEvidence 'tests' $null) 'passed' 0)
                failed       = [long](Get-PropValue (Get-PropValue $WindowsEvidence 'tests' $null) 'failed' 0)
                skipped      = [long](Get-PropValue (Get-PropValue $WindowsEvidence 'tests' $null) 'skipped' 0)
                inconclusive = [long](Get-PropValue (Get-PropValue $WindowsEvidence 'tests' $null) 'inconclusive' 0)
            }
        }
    }
    if ($verification.rust.status -cne 'passed' -or $verification.windows.status -cne 'passed') {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Manifest verification evidence must be passed for both rust and windows.')
    }
    if ($verification.rust.commit -cne $SourceCommit -or $verification.windows.commit -cne $SourceCommit) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Manifest verification evidence commit does not match SourceCommit.')
    }
    if ($verification.rust.tests.executed -le 0 -or $verification.windows.tests.executed -le 0) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Manifest verification evidence reports zero executed tests.')
    }
    if ($verification.rust.tests.failed -gt 0 -or $verification.windows.tests.failed -gt 0) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Manifest verification evidence contains failed tests.')
    }

    $toolchainOrdered = [ordered]@{}
    if ($null -ne $Toolchain) {
        $toolchainKeys = @()
        if ($Toolchain -is [System.Collections.IDictionary]) { $toolchainKeys = @($Toolchain.Keys) }
        else { $toolchainKeys = @($Toolchain.PSObject.Properties.Name) }
        foreach ($key in ($toolchainKeys | Sort-Object)) {
            $value = [string](Get-PropValue $Toolchain $key '')
            if ($value -cne '') { $toolchainOrdered[[string]$key] = $value }
        }
    }

    $manifest = [ordered]@{
        schemaVersion = 1
        product       = 'ClipBridge'
        version       = $Version
        tag           = Get-ReleaseTag -Version $Version
        commit        = $SourceCommit
        architecture  = 'x64'
        platform      = 'windows'
        formats       = [string[]]$formatSet
        signing       = [ordered]@{
            mode        = $SigningMode
            status      = $SigningStatus
            trusted     = [bool]$Trusted
            timestamped = [bool]$Timestamped
        }
        sourceRun     = [ordered]@{
            runId        = [long]$SourceRunId
            runAttempt   = [int]$SourceRunAttempt
            workflow     = 'release-windows.yml'
            workflowPath = $script:ReleaseWorkflowPath
            workflowRef  = $script:ReleaseWorkflowRef
            event        = 'workflow_dispatch'
            artifactName = $CandidateArtifactName
        }
        generatedAtUtc = $(if ($GeneratedAtUtc) { [string]$GeneratedAtUtc } else { '' })
        toolchain      = $toolchainOrdered
        verification   = $verification
        artifacts      = [object[]]$manifestArtifacts.ToArray()
    }
    Assert-NoUnsafeManifestContent -Value $manifest -Path '$manifest'
    return $manifest
}

function ConvertTo-ReleaseManifestJson {
    param([Parameter(Mandatory)][object]$Manifest)
    $json = $Manifest | ConvertTo-Json -Depth 24
    $json = $json -replace "`r`n", "`n"
    return ($json.TrimEnd("`n") + "`n")
}

function Read-ReleaseManifestText {
    <# Parses and structurally validates a manifest; returns object + digest. #>
    param([Parameter(Mandatory)][string]$Text)
    if ([string]::IsNullOrWhiteSpace($Text)) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest.json is empty.')
    }
    try { $manifest = $Text | ConvertFrom-Json } catch {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "release-manifest.json is not valid JSON: $($_.Exception.Message)")
    }
    if ([int](Get-PropValue $manifest 'schemaVersion' 0) -ne 1) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Unsupported release-manifest schemaVersion '$((Get-PropValue $manifest 'schemaVersion' $null))'.")
    }
    if ([string](Get-PropValue $manifest 'product' '') -cne 'ClipBridge') {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest product must be ClipBridge.')
    }
    $version = [string](Get-PropValue $manifest 'version' '')
    $null = ConvertTo-ReleaseVersion $version
    if ([string](Get-PropValue $manifest 'tag' '') -cne (Get-ReleaseTag -Version $version)) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest tag does not match its version.')
    }
    if ([string](Get-PropValue $manifest 'commit' '') -cnotmatch $script:CommitPattern) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest commit must be a full 40-hex SHA.')
    }
    if ([string](Get-PropValue $manifest 'architecture' '') -cne 'x64' -or [string](Get-PropValue $manifest 'platform' '') -cne 'windows') {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest architecture/platform must be x64/windows.')
    }
    $formats = Resolve-ReleaseFormatSet -Formats ([string[]]@(Get-PropValue $manifest 'formats' @()))
    $signing = Get-PropValue $manifest 'signing' $null
    $mode = [string](Get-PropValue $signing 'mode' '')
    $status = [string](Get-PropValue $signing 'status' '')
    $trusted = [bool](Get-PropValue $signing 'trusted' $false)
    $timestamped = [bool](Get-PropValue $signing 'timestamped' $false)
    if ($mode -cnotin @('None', 'Certificate') -or $status -cnotin @('unsigned', 'signed')) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest signing mode/status is invalid.')
    }
    if ($status -ceq 'unsigned' -and ($trusted -or $timestamped)) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Unsigned manifest cannot be trusted/timestamped.')
    }
    if ($status -ceq 'signed' -and (-not ($trusted -and $timestamped))) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Signed manifest must be trusted and timestamped.')
    }
    $sourceRun = Get-PropValue $manifest 'sourceRun' $null
    if ([long](Get-PropValue $sourceRun 'runId' 0) -le 0 -or [int](Get-PropValue $sourceRun 'runAttempt' 0) -lt 1) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest sourceRun id/attempt is invalid.')
    }
    if ([string](Get-PropValue $sourceRun 'workflowPath' '') -cne $script:ReleaseWorkflowPath -or
        [string](Get-PropValue $sourceRun 'workflowRef' '') -cne $script:ReleaseWorkflowRef -or
        [string](Get-PropValue $sourceRun 'event' '') -cne 'workflow_dispatch') {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest sourceRun does not identify the manual main release workflow.')
    }
    if ([string](Get-PropValue $sourceRun 'artifactName' '') -notmatch '^windows-release-candidate-[0-9]+-[0-9]+$') {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'release-manifest sourceRun.artifactName is invalid.')
    }
    $artifacts = @(Get-PropValue $manifest 'artifacts' @())
    $expectedPackageNames = Get-ReleasePackageAssetNames -Version $version -Formats $formats
    $actualPackageNames = @($artifacts | ForEach-Object { [string](Get-PropValue $_ 'name' '') })
    if (@($actualPackageNames | Sort-Object -Unique).Count -ne $actualPackageNames.Count) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Manifest contains duplicate package names.')
    }
    foreach ($name in $actualPackageNames) {
        if ($expectedPackageNames -cnotcontains $name) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest contains unexpected package '$name'.")
        }
    }
    foreach ($name in $expectedPackageNames) {
        if ($actualPackageNames -cnotcontains $name) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest is missing selected package '$name'.")
        }
    }
    $artifactCheck = [pscustomobject]@{ Complete = ($actualPackageNames.Count -eq $expectedPackageNames.Count) }
    foreach ($artifact in $artifacts) {
        $name = [string](Get-PropValue $artifact 'name' '')
        $format = [string](Get-PropValue $artifact 'format' '')
        if ($format -cnotin $formats) { throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest artifact '$name' has an unselected format.") }
        if ($name -cne (Get-ReleaseAssetName -Format $format -Version $version)) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest artifact '$name' does not match format '$format'.")
        }
        if ([long](Get-PropValue $artifact 'size' 0) -le 0) { throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest artifact '$name' has no size.") }
        if (([string](Get-PropValue $artifact 'sha256' '')).ToLowerInvariant() -cnotmatch $script:Sha256Pattern) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Manifest artifact '$name' has an invalid sha256.")
        }
        $artifactSigning = Get-PropValue $artifact 'signing' $null
        $authenticode = [string](Get-PropValue $artifactSigning 'authenticode' '')
        if ($format -ceq 'portable-zip') {
            if ($authenticode -cne 'not-applicable') { throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Portable ZIP authenticode must be not-applicable.') }
        }
        elseif ($authenticode -cnotin @('signed', 'unsigned')) {
            throw (New-ReleaseModelError -Code 'invalid-manifest' -Message "Artifact '$name' has an invalid authenticode state.")
        }
    }
    $verification = Get-PropValue $manifest 'verification' $null
    $rust = Get-PropValue $verification 'rust' $null
    $windows = Get-PropValue $verification 'windows' $null
    if ([string](Get-PropValue $rust 'status' '') -cne 'passed' -or [string](Get-PropValue $windows 'status' '') -cne 'passed') {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Manifest verification status is not passed.')
    }
    if ([string](Get-PropValue $rust 'commit' '') -cne [string](Get-PropValue $manifest 'commit' '') -or
        [string](Get-PropValue $windows 'commit' '') -cne [string](Get-PropValue $manifest 'commit' '')) {
        throw (New-ReleaseModelError -Code 'invalid-manifest' -Message 'Manifest verification commit does not match the source commit.')
    }
    Assert-NoUnsafeManifestContent -Value $manifest -Path '$manifest'
    return [pscustomobject]@{
        Manifest = $manifest
        Digest   = Get-ReleaseManifestDigest -ManifestText $Text
        Complete = [bool]$artifactCheck.Complete
        Formats  = [string[]]$formats
    }
}

function Get-CandidateIdentity {
    param([Parameter(Mandatory)][object]$Manifest)
    $formats = @(Get-PropValue $Manifest 'formats' @())
    $signing = Get-PropValue $Manifest 'signing' $null
    return [pscustomobject]@{
        version       = [string](Get-PropValue $Manifest 'version' '')
        commit        = [string](Get-PropValue $Manifest 'commit' '')
        architecture  = [string](Get-PropValue $Manifest 'architecture' '')
        formats       = [string[]]@($script:FormatOrder | Where-Object { $formats -ccontains $_ })
        signingPolicy = ('{0}/{1}/trusted={2}/timestamped={3}' -f
            [string](Get-PropValue $signing 'mode' ''),
            [string](Get-PropValue $signing 'status' ''),
            [bool](Get-PropValue $signing 'trusted' $false),
            [bool](Get-PropValue $signing 'timestamped' $false))
    }
}

function Test-CandidateIdentity {
    param(
        [Parameter(Mandatory)][object]$Expected,
        [Parameter(Mandatory)][object]$Actual,
        [string]$ExpectedDigest,
        [string]$ActualDigest
    )
    $failures = [System.Collections.Generic.List[string]]::new()
    foreach ($field in @('version', 'commit', 'architecture', 'signingPolicy')) {
        if ([string](Get-PropValue $Expected $field '') -cne [string](Get-PropValue $Actual $field '')) {
            $failures.Add("candidate $field mismatch")
        }
    }
    $expectedFormats = @(Get-PropValue $Expected 'formats' @())
    $actualFormats = @(Get-PropValue $Actual 'formats' @())
    if (($expectedFormats -join ',') -cne ($actualFormats -join ',')) { $failures.Add('candidate formats mismatch') }
    if ($PSBoundParameters.ContainsKey('ExpectedDigest') -or $PSBoundParameters.ContainsKey('ActualDigest')) {
        if ($ExpectedDigest -cnotmatch $script:Sha256Pattern -or $ActualDigest -cnotmatch $script:Sha256Pattern) {
            $failures.Add('candidate digest is not a valid SHA256')
        }
        elseif ($ExpectedDigest -cne $ActualDigest) {
            $failures.Add('candidate digest mismatch')
        }
    }
    return [pscustomobject]@{ Ok = ($failures.Count -eq 0); Failures = [string[]]$failures.ToArray() }
}

function Get-ReleaseState {
    param([object]$Release)
    if ($null -eq $Release) { return 'none' }
    if ([bool](Get-PropValue $Release 'draft' $false)) { return 'draft' }
    if ([bool](Get-PropValue $Release 'prerelease' $false)) { return 'prerelease' }
    return 'stable'
}

function Get-ReleaseSigningPolicy {
    <#
    .SYNOPSIS
        Applies the signing policy matrix for draft/prerelease/stable. A
        configured Certificate build that ends unsigned is a hard failure, not
        a silent downgrade.
    #>
    param(
        [Parameter(Mandatory)][ValidateSet('draft', 'prerelease', 'stable')][string]$ReleaseKind,
        [Parameter(Mandatory)][ValidateSet('None', 'Certificate')][string]$SigningMode,
        [Parameter(Mandatory)][ValidateSet('unsigned', 'signed')][string]$SigningStatus,
        [bool]$Trusted = $false,
        [bool]$Timestamped = $false,
        [bool]$AllowUnsignedPrerelease = $false
    )
    if ($SigningMode -ceq 'None') {
        if ($SigningStatus -cne 'unsigned') {
            throw (New-ReleaseModelError -Code 'invalid-signing' -Message 'SigningMode None must report unsigned.')
        }
        if ($Trusted -or $Timestamped) {
            throw (New-ReleaseModelError -Code 'invalid-signing' -Message 'Unsigned candidate cannot be trusted/timestamped.')
        }
        switch ($ReleaseKind) {
            'draft' { return [pscustomobject]@{ Signed = $false; Allowed = $true; UnsignedPublic = $false; Reason = 'draft may be unsigned' } }
            'prerelease' {
                if (-not $AllowUnsignedPrerelease) {
                    throw (New-ReleaseModelError -Code 'signing-required' -Message 'Unsigned prerelease requires allow_unsigned_prerelease=true.')
                }
                return [pscustomobject]@{ Signed = $false; Allowed = $true; UnsignedPublic = $true; Reason = 'unsigned prerelease explicitly allowed' }
            }
            'stable' {
                throw (New-ReleaseModelError -Code 'signing-required' -Message 'Stable release requires a configured production signature; no unsigned fallback is allowed.')
            }
        }
    }

    if ($SigningStatus -cne 'signed' -or -not $Trusted -or -not $Timestamped) {
        throw (New-ReleaseModelError -Code 'signing-required' -Message 'Certificate signing was configured but the candidate is not signed/trusted/timestamped; refusing a silent downgrade.')
    }
    return [pscustomobject]@{ Signed = $true; Allowed = $true; UnsignedPublic = $false; Reason = 'candidate is signed, trusted and timestamped' }
}

function Get-ReleaseTransition {
    <#
    .SYNOPSIS
        Implements the documented state transition table. Returns the preflight
        mode and the publish action, or throws for forbidden transitions.
    #>
    param(
        [Parameter(Mandatory)][ValidateSet('none', 'draft', 'prerelease', 'stable')][string]$State,
        [Parameter(Mandatory)][ValidateSet('draft', 'prerelease', 'stable')][string]$ReleaseKind,
        [Parameter(Mandatory)][bool]$Signed,
        [Parameter(Mandatory)][bool]$Complete,
        [string[]]$ExistingFormats = @(),
        [Parameter(Mandatory)][string[]]$RequestedFormats,
        [bool]$AllowUnsignedPrerelease = $false
    )
    $requested = Resolve-ReleaseFormatSet -Formats $RequestedFormats
    if ($State -cne 'none') {
        $existing = Resolve-ReleaseFormatSet -Formats $ExistingFormats
        if (($existing -join ',') -cne ($requested -join ',')) {
            throw (New-ReleaseModelError -Code 'candidate-conflict' -Message "Existing candidate formats ($($existing -join ', ')) do not match the request ($($requested -join ', ')); formats cannot be added or removed after publication and a draft candidate cannot be repurposed.")
        }
        if ($State -cne 'draft' -and -not $Complete) {
            throw (New-ReleaseModelError -Code 'candidate-incomplete' -Message 'A published release with missing assets can never be modified or reused automatically.')
        }
    }

    if ($State -ceq 'none') {
        switch ($ReleaseKind) {
            'draft' { return [pscustomobject]@{ Mode = 'build-new'; Publish = 'none'; RequiresApproval = $false; Reason = 'no release; build new draft' } }
            'prerelease' {
                if (-not $Signed -and -not $AllowUnsignedPrerelease) {
                    throw (New-ReleaseModelError -Code 'signing-required' -Message 'No release and unsigned prerelease requested without allow_unsigned_prerelease=true.')
                }
                return [pscustomobject]@{ Mode = 'build-new'; Publish = 'prerelease'; RequiresApproval = $false; Reason = 'no release; build and publish prerelease' }
            }
            'stable' {
                if (-not $Signed) { throw (New-ReleaseModelError -Code 'signing-required' -Message 'Stable release requires a production-signed candidate.') }
                return [pscustomobject]@{ Mode = 'build-new'; Publish = 'stable'; RequiresApproval = $true; Reason = 'no release; build signed candidate and request stable approval' }
            }
        }
    }

    if ($State -ceq 'draft') {
        switch ($ReleaseKind) {
            'draft' { return [pscustomobject]@{ Mode = 'reuse-candidate'; Publish = 'none'; RequiresApproval = $false; Reason = 'complete/partial draft retained; candidate verified' } }
            'prerelease' {
                if (-not $Signed -and -not $AllowUnsignedPrerelease) {
                    throw (New-ReleaseModelError -Code 'signing-required' -Message 'Unsigned draft cannot be published as prerelease without allow_unsigned_prerelease=true.')
                }
                return [pscustomobject]@{ Mode = 'reuse-candidate'; Publish = 'prerelease'; RequiresApproval = $false; Reason = 'draft verified; publish prerelease' }
            }
            'stable' {
                if (-not $Signed) { throw (New-ReleaseModelError -Code 'signing-required' -Message 'Unsigned draft cannot be promoted to stable; create a signed candidate under a new version.') }
                return [pscustomobject]@{ Mode = 'reuse-candidate'; Publish = 'stable'; RequiresApproval = $true; Reason = 'signed draft verified; stable promotion requires environment approval' }
            }
        }
    }

    if ($State -ceq 'prerelease') {
        switch ($ReleaseKind) {
            'draft' { throw (New-ReleaseModelError -Code 'state-regression' -Message 'Refusing to hide a published prerelease by requesting draft.') }
            'prerelease' {
                return [pscustomobject]@{ Mode = 'already-complete'; Publish = 'none'; RequiresApproval = $false; Reason = 'published prerelease already satisfies the request' }
            }
            'stable' {
                if (-not $Signed) { throw (New-ReleaseModelError -Code 'signing-required' -Message 'Unsigned published prerelease cannot be promoted to stable; use a new version with a signed candidate.') }
                return [pscustomobject]@{ Mode = 'reuse-candidate'; Publish = 'stable'; RequiresApproval = $true; Reason = 'signed prerelease verified; stable promotion requires fresh environment approval' }
            }
        }
    }

    if ($State -ceq 'stable') {
        switch ($ReleaseKind) {
            'draft' { throw (New-ReleaseModelError -Code 'state-regression' -Message 'Refusing to hide a stable release by requesting draft.') }
            'prerelease' { throw (New-ReleaseModelError -Code 'state-regression' -Message 'Refusing to downgrade a stable release to prerelease.') }
            'stable' { return [pscustomobject]@{ Mode = 'already-complete'; Publish = 'none'; RequiresApproval = $false; Reason = 'published stable release already satisfies the request' } }
        }
    }

    throw (New-ReleaseModelError -Code 'invalid-release-state' -Message "Unhandled release state '$State'.")
}

function Get-EnvironmentReviewerPolicy {
    <#
    .SYNOPSIS
        Reads the required-reviewer protection rule from a GitHub environment
        response. Missing/unreadable protection or team-only reviewers (whose
        membership this token cannot verify) fail closed.
    #>
    param(
        [string]$EnvironmentName = $script:StableEnvironment,
        [object]$Environment
    )
    if ($null -eq $Environment) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Environment '$EnvironmentName' could not be read; stable promotion fails closed.")
    }
    if ([string](Get-PropValue $Environment 'name' '') -cne $EnvironmentName) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Environment response name does not match '$EnvironmentName'.")
    }
    $rules = @(Get-PropValue $Environment 'protection_rules' @())
    $rule = $rules | Where-Object { [string](Get-PropValue $_ 'type' '') -ceq 'required_reviewers' } | Select-Object -First 1
    if ($null -eq $rule) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Environment '$EnvironmentName' has no required_reviewers protection rule; stable promotion fails closed.")
    }
    $reviewers = @(Get-PropValue $rule 'reviewers' @())
    $userLogins = [System.Collections.Generic.List[string]]::new()
    $teamSlugs = [System.Collections.Generic.List[string]]::new()
    foreach ($reviewer in $reviewers) {
        $type = [string](Get-PropValue $reviewer 'type' '')
        $target = Get-PropValue $reviewer 'reviewer' $null
        if ($type -ceq 'User') {
            $login = [string](Get-PropValue $target 'login' '')
            if ($login -cne '') { $userLogins.Add($login) }
        }
        elseif ($type -ceq 'Team') {
            $slug = [string](Get-PropValue $target 'slug' '')
            if ($slug -cne '') { $teamSlugs.Add($slug) }
        }
    }
    if ($userLogins.Count -eq 0 -and $teamSlugs.Count -eq 0) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Environment '$EnvironmentName' required_reviewers is empty; stable promotion fails closed.")
    }
    if ($userLogins.Count -eq 0) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Environment '$EnvironmentName' only configures team reviewers; team membership cannot be verified with this token, so stable promotion fails closed.")
    }
    return [pscustomobject]@{
        Name                   = $EnvironmentName
        RequiredReviewerLogins = [string[]]$userLogins.ToArray()
        TeamSlugs              = [string[]]$teamSlugs.ToArray()
        Verifiable             = $true
    }
}

function Get-ApprovalGateTiming {
    <#
    .SYNOPSIS
        Derives the approval ordering from the same GitHub job records.

    .DESCRIPTION
        The local runner clock is diagnostic only. The authoritative ordering is
        the current attempt's `stage` job completed_at <= `approve-stable` job
        started_at. Both jobs must exist exactly once, belong to the current
        run/attempt and have succeeded. Equal timestamps are allowed (GitHub
        timestamps have second resolution). Actual approval time is not exposed
        by the public API and is not claimed.
    #>
    param(
        [object[]]$Jobs,
        [Parameter(Mandatory)][long]$ExpectedRunId,
        [Parameter(Mandatory)][int]$ExpectedRunAttempt
    )
    $list = @($Jobs)
    $stageJobs = @($list | Where-Object { [string](Get-PropValue $_ 'name' '') -ceq 'stage' })
    $gateJobs = @($list | Where-Object { [string](Get-PropValue $_ 'name' '') -ceq 'approve-stable' })
    if ($stageJobs.Count -ne 1) { throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Expected exactly one 'stage' job in the current attempt; found $($stageJobs.Count).") }
    if ($gateJobs.Count -ne 1) { throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Expected exactly one 'approve-stable' job in the current attempt; found $($gateJobs.Count).") }
    foreach ($job in @($stageJobs[0], $gateJobs[0])) {
        if ([long](Get-PropValue $job 'run_id' 0) -ne $ExpectedRunId) {
            throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Approval gate job does not belong to the current workflow run.')
        }
        if ([int](Get-PropValue $job 'run_attempt' 0) -ne $ExpectedRunAttempt) {
            throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Approval gate job does not belong to the current run attempt.')
        }
        if ([string](Get-PropValue $job 'conclusion' '') -cne 'success') {
            throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "Job '$((Get-PropValue $job 'name' $null))' did not complete successfully; stable promotion fails closed.")
        }
    }
    $stageCompletedRaw = [string](Get-PropValue $stageJobs[0] 'completed_at' '')
    $gateStartedRaw = [string](Get-PropValue $gateJobs[0] 'started_at' '')
    $stageCompleted = [DateTimeOffset]::MinValue
    $gateStarted = [DateTimeOffset]::MinValue
    if ([string]::IsNullOrWhiteSpace($stageCompletedRaw) -or -not [DateTimeOffset]::TryParse($stageCompletedRaw, [System.Globalization.CultureInfo]::InvariantCulture, [System.Globalization.DateTimeStyles]::AssumeUniversal, [ref]$stageCompleted)) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'The current attempt stage job has no parseable completed_at.')
    }
    if ([string]::IsNullOrWhiteSpace($gateStartedRaw) -or -not [DateTimeOffset]::TryParse($gateStartedRaw, [System.Globalization.CultureInfo]::InvariantCulture, [System.Globalization.DateTimeStyles]::AssumeUniversal, [ref]$gateStarted)) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'The current attempt approve-stable job has no parseable started_at.')
    }
    if ($gateStarted -lt $stageCompleted) {
        throw (New-ReleaseModelError -Code 'approval-rejected' -Message 'GitHub job records show approve-stable started before stage completed; the approval is not causally bound to this candidate.')
    }
    return [pscustomobject]@{
        StageJobId         = [long](Get-PropValue $stageJobs[0] 'id' 0)
        StageCompletedAt   = $stageCompletedRaw
        StageJobConclusion = 'success'
        GateJobId          = [long](Get-PropValue $gateJobs[0] 'id' 0)
        GateStartedAt      = $gateStartedRaw
        GateJobConclusion  = 'success'
        SameSecond         = [bool]($gateStarted -eq $stageCompleted)
    }
}
function Test-ApprovalRecords {
    <#
    .SYNOPSIS
        Validates at least one current-fresh-nonce environment approval.

    .DESCRIPTION
        GitHub's review history keeps old attempts. A stage rerun generates a
        new nonce, so old approved records must not authorize this stage and
        must not permanently block a legitimate newer approval either. This
        function therefore:
          - rejects any rejected review that carries the CURRENT nonce;
          - ignores approved/rejected records with missing or old nonces
            (they are not authorization evidence);
          - requires at least one approved record with the current nonce,
            the exact candidate digest, run_id, run_attempt, result=pass and
            an https record, from a configured required reviewer;
          - fails if an approved record carries the current nonce but is
            unauthorized/malformed (same-nonce misuse is explicit).
        The public API exposes no review timestamp; the gate job start is only
        an ordering bound, not an approval time.
    #>
    param(
        [object[]]$Reviews,
        [string]$EnvironmentName = $script:StableEnvironment,
        [Parameter(Mandatory)][string[]]$RequiredReviewerLogins,
        [Parameter(Mandatory)][string]$CandidateDigest,
        [Parameter(Mandatory)][string]$ApprovalNonce,
        [Parameter(Mandatory)][long]$RunId,
        [Parameter(Mandatory)][int]$RunAttempt,
        [Parameter(Mandatory)][string]$StageCompletedAt,
        [Parameter(Mandatory)][string]$GateJobStartedAt
    )
    if ($CandidateDigest -cnotmatch $script:Sha256Pattern) {
        throw (New-ReleaseModelError -Code 'approval-rejected' -Message 'Candidate digest is not a valid SHA256.')
    }
    if ($ApprovalNonce -cnotmatch '^[0-9a-f]{32,128}$') {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Approval nonce must be at least 128 bits (32 lowercase hex chars).')
    }
    if ($RunId -le 0 -or $RunAttempt -lt 1) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Run id/attempt is invalid; approval cannot be bound.')
    }
    if ($null -eq $Reviews -or @($Reviews).Count -eq 0) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Environment review records are missing; stable promotion fails closed.')
    }
    $stageTime = [DateTimeOffset]::MinValue
    $gateTime = [DateTimeOffset]::MinValue
    if (-not [DateTimeOffset]::TryParse($StageCompletedAt, [System.Globalization.CultureInfo]::InvariantCulture, [System.Globalization.DateTimeStyles]::AssumeUniversal, [ref]$stageTime)) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Stage completion time is missing/unparseable; gate causality cannot be verified.')
    }
    if (-not [DateTimeOffset]::TryParse($GateJobStartedAt, [System.Globalization.CultureInfo]::InvariantCulture, [System.Globalization.DateTimeStyles]::AssumeUniversal, [ref]$gateTime)) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Approval gate job start is missing/unparseable; gate causality cannot be verified.')
    }
    if ($gateTime -lt $stageTime) {
        throw (New-ReleaseModelError -Code 'approval-rejected' -Message 'Approval gate started before the candidate was staged; the approval is not causally bound to this candidate.')
    }

    $environmentReviews = @($Reviews | Where-Object {
            $envs = @(Get-PropValue $_ 'environments' @())
            @($envs | Where-Object { [string](Get-PropValue $_ 'name' '') -ceq $EnvironmentName }).Count -gt 0
        })
    if ($environmentReviews.Count -eq 0) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "No environment review for '$EnvironmentName' exists in this run.")
    }

    $required = @($RequiredReviewerLogins | ForEach-Object { [string]$_ })
    $evidence = [System.Collections.Generic.List[object]]::new()
    foreach ($review in $environmentReviews) {
        $state = [string](Get-PropValue $review 'state' '')
        $comment = [string](Get-PropValue $review 'comment' '')
        $nonceMatch = [regex]::Match($comment, '(?i)\bnonce\s*=\s*([0-9a-f]{32,128})\b')
        $isCurrentNonce = $nonceMatch.Success -and ($nonceMatch.Groups[1].Value.ToLowerInvariant() -ceq $ApprovalNonce)

        if ($state -ceq 'rejected') {
            # A rejection of THIS fresh nonce blocks. A rejection from an older
            # attempt (no/old nonce) is not evidence and must not block retries.
            if ($isCurrentNonce) {
                throw (New-ReleaseModelError -Code 'approval-rejected' -Message "A reviewer rejected the current approval nonce for '$EnvironmentName'; stable promotion fails closed.")
            }
            continue
        }
        if ($state -cne 'approved') {
            # pending/unknown records are not authorization evidence.
            continue
        }
        if (-not $isCurrentNonce) {
            # Old nonce / no nonce: retained history, not current authorization.
            # Do not let it block a legitimate newer approval.
            continue
        }

        # Current nonce + approved: every field is now mandatory and explicit.
        $user = Get-PropValue $review 'user' $null
        $login = [string](Get-PropValue $user 'login' '')
        if ([string]::IsNullOrWhiteSpace($login) -or -not ($required | Where-Object { $_ -ieq $login })) {
            throw (New-ReleaseModelError -Code 'approval-rejected' -Message "Approval with the current nonce by '$login' is not from a configured required reviewer.")
        }
        $digestMatch = [regex]::Match($comment, '(?i)\bcandidate\s*=\s*([0-9a-f]{64})\b')
        if (-not $digestMatch.Success -or $digestMatch.Groups[1].Value.ToLowerInvariant() -cne $CandidateDigest) {
            throw (New-ReleaseModelError -Code 'approval-stale' -Message "Approval by '$login' with the current nonce is bound to a different candidate digest.")
        }
        $runIdMatch = [regex]::Match($comment, '(?i)\brun_id\s*=\s*(\d+)\b')
        if (-not $runIdMatch.Success -or [long]$runIdMatch.Groups[1].Value -ne $RunId) {
            throw (New-ReleaseModelError -Code 'approval-stale' -Message "Approval by '$login' with the current nonce does not contain run_id=$RunId.")
        }
        $attemptMatch = [regex]::Match($comment, '(?i)\brun_attempt\s*=\s*(\d+)\b')
        if (-not $attemptMatch.Success -or [int]$attemptMatch.Groups[1].Value -ne $RunAttempt) {
            throw (New-ReleaseModelError -Code 'approval-stale' -Message "Approval by '$login' with the current nonce does not contain run_attempt=$RunAttempt.")
        }
        if (-not [regex]::IsMatch($comment, '(?i)\bresult\s*=\s*(pass|passed)\b')) {
            throw (New-ReleaseModelError -Code 'approval-rejected' -Message "Approval by '$login' with the current nonce does not state result=pass.")
        }
        $recordMatch = [regex]::Match($comment, '(?i)\brecord\s*=\s*(https://[^\s]+)')
        if (-not $recordMatch.Success) {
            throw (New-ReleaseModelError -Code 'approval-rejected' -Message "Approval by '$login' with the current nonce does not contain an https record=<url> acceptance location.")
        }
        $recordUrl = $recordMatch.Groups[1].Value
        if (-not [Uri]::IsWellFormedUriString($recordUrl, [UriKind]::Absolute)) {
            throw (New-ReleaseModelError -Code 'approval-rejected' -Message "Approval by '$login' has a malformed record URL.")
        }
        $evidence.Add([pscustomobject]@{
                reviewer    = $login
                record      = $recordUrl
                candidate   = $CandidateDigest
                nonce       = $ApprovalNonce
                runId       = $RunId
                runAttempt  = $RunAttempt
                comment     = $comment
            })
    }

    if ($evidence.Count -eq 0) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message "No approved review contains the current fresh nonce/candidate/run/attempt for '$EnvironmentName'. Old-nonce records are retained history, not authorization evidence.")
    }

    return [pscustomobject]@{
        Ok                     = $true
        Environment            = $EnvironmentName
        RequiredReviewerLogins = [string[]]$required
        Approvals              = [object[]]$evidence.ToArray()
        Nonce                  = $ApprovalNonce
        RunId                  = $RunId
        RunAttempt             = $RunAttempt
        StageCompletedAt       = $StageCompletedAt
        GateJobStartedAt       = $GateJobStartedAt
    }
}
function Assert-StableReleaseGate {
    <# Stable requires signed+trusted+timestamped AND verifiable current approval. #>
    param(
        [Parameter(Mandatory)][string]$SigningMode,
        [Parameter(Mandatory)][string]$SigningStatus,
        [Parameter(Mandatory)][bool]$Trusted,
        [Parameter(Mandatory)][bool]$Timestamped,
        [object]$EnvironmentPolicy,
        [object]$ApprovalEvidence
    )
    if ($SigningMode -cne 'Certificate' -or $SigningStatus -cne 'signed' -or -not $Trusted -or -not $Timestamped) {
        throw (New-ReleaseModelError -Code 'signing-required' -Message 'Stable release requires Certificate-signed, trusted, timestamped artifacts.')
    }
    if ($null -eq $EnvironmentPolicy -or -not [bool](Get-PropValue $EnvironmentPolicy 'Verifiable' $false)) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Stable release requires a verifiable required-reviewer environment policy.')
    }
    if ($null -eq $ApprovalEvidence -or -not [bool](Get-PropValue $ApprovalEvidence 'Ok' $false)) {
        throw (New-ReleaseModelError -Code 'approval-unverifiable' -Message 'Stable release requires a verified current-run environment approval.')
    }
    return [pscustomobject]@{
        Signed      = $true
        Environment = [string](Get-PropValue $EnvironmentPolicy 'Name' '')
        Approvals   = @(Get-PropValue $ApprovalEvidence 'Approvals' @())
    }
}

function Test-SourceRunRecord {
    <# Validates the original candidate run identity (workflow_dispatch on main). #>
    param(
        [object]$Run,
        [Parameter(Mandatory)][long]$ExpectedRunId,
        [Parameter(Mandatory)][int]$ExpectedRunAttempt
    )
    if ($null -eq $Run) {
        throw (New-ReleaseModelError -Code 'source-run-missing' -Message "Source run $ExpectedRunId is missing.")
    }
    if ([long](Get-PropValue $Run 'id' 0) -ne $ExpectedRunId) {
        throw (New-ReleaseModelError -Code 'source-run-mismatch' -Message 'Source run id does not match the recorded candidate provenance.')
    }
    if ([string](Get-PropValue $Run 'event' '') -cne 'workflow_dispatch') {
        throw (New-ReleaseModelError -Code 'source-run-mismatch' -Message 'Source run was not triggered by workflow_dispatch.')
    }
    if ([string](Get-PropValue $Run 'path' '') -cne $script:ReleaseWorkflowPath) {
        throw (New-ReleaseModelError -Code 'source-run-mismatch' -Message "Source run is not $script:ReleaseWorkflowPath.")
    }
    if ([string](Get-PropValue $Run 'head_branch' '') -cne 'main') {
        throw (New-ReleaseModelError -Code 'source-run-mismatch' -Message 'Source run did not run from main.')
    }
    if ([string](Get-PropValue $Run 'status' '') -cne 'completed') {
        throw (New-ReleaseModelError -Code 'source-run-incomplete' -Message 'Source run is not completed.')
    }
    if ([string](Get-PropValue $Run 'conclusion' '') -cnotin @('success', 'failure')) {
        throw (New-ReleaseModelError -Code 'source-run-incomplete' -Message "Source run conclusion '$((Get-PropValue $Run 'conclusion' $null))' cannot be used as candidate evidence.")
    }
    $attempt = [int](Get-PropValue $Run 'run_attempt' $ExpectedRunAttempt)
    if ($attempt -lt $ExpectedRunAttempt) {
        throw (New-ReleaseModelError -Code 'source-run-mismatch' -Message 'Source run attempt is older than the recorded candidate attempt.')
    }
    return [pscustomobject]@{ Ok = $true; RunId = $ExpectedRunId; RunAttempt = $ExpectedRunAttempt; Conclusion = [string](Get-PropValue $Run 'conclusion' '') }
}

function Test-SourceRunJobs {
    <# Requires the original verify and build jobs from the exact recorded attempt. #>
    param(
        [object[]]$Jobs,
        [Parameter(Mandatory)][long]$ExpectedRunId,
        [Parameter(Mandatory)][int]$ExpectedRunAttempt
    )
    $list = @($Jobs)
    $verify = @($list | Where-Object { [string](Get-PropValue $_ 'name' '') -ceq 'verify' })
    $build = @($list | Where-Object { [string](Get-PropValue $_ 'name' '') -ceq 'build' })
    if ($verify.Count -ne 1) { throw (New-ReleaseModelError -Code 'source-run-missing' -Message "Expected exactly one 'verify' job in the source run attempt; found $($verify.Count).") }
    if ($build.Count -ne 1) { throw (New-ReleaseModelError -Code 'source-run-missing' -Message "Expected exactly one 'build' job in the source run attempt; found $($build.Count).") }
    foreach ($job in @($verify[0], $build[0])) {
        $jobRunId = Get-PropValue $job 'run_id' $null
        if ($null -eq $jobRunId -or [long]$jobRunId -ne $ExpectedRunId) {
            throw (New-ReleaseModelError -Code 'source-run-mismatch' -Message 'Source job does not belong to the recorded workflow run.')
        }
        $jobAttempt = Get-PropValue $job 'run_attempt' $null
        if ($null -eq $jobAttempt -or [int]$jobAttempt -ne $ExpectedRunAttempt) {
            throw (New-ReleaseModelError -Code 'source-run-mismatch' -Message "Source job belongs to run attempt '$jobAttempt', not the recorded attempt '$ExpectedRunAttempt'.")
        }
    }
    if ([string](Get-PropValue $verify[0] 'conclusion' '') -cne 'success') {
        throw (New-ReleaseModelError -Code 'source-run-verification-failed' -Message 'Original verify job did not succeed; candidate cannot be reused.')
    }
    if ([string](Get-PropValue $build[0] 'conclusion' '') -cne 'success') {
        throw (New-ReleaseModelError -Code 'source-run-build-failed' -Message 'Original build job did not succeed; candidate cannot be reused.')
    }
    return [pscustomobject]@{
        Ok           = $true
        VerifyJobId  = [long](Get-PropValue $verify[0] 'id' 0)
        BuildJobId   = [long](Get-PropValue $build[0] 'id' 0)
        VerifyResult = 'success'
        BuildResult  = 'success'
    }
}

function Test-CandidateArtifactRecord {
    param(
        [object]$Artifact,
        [Parameter(Mandatory)][long]$ExpectedId,
        [Parameter(Mandatory)][string]$ExpectedName,
        [Parameter(Mandatory)][long]$ExpectedRunId
    )
    if ($null -eq $Artifact) {
        throw (New-ReleaseModelError -Code 'artifact-missing' -Message "Candidate artifact $ExpectedId is missing.")
    }
    if ([long](Get-PropValue $Artifact 'id' 0) -ne $ExpectedId) {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message 'Candidate artifact id does not match the recorded provenance.')
    }
    if ([string](Get-PropValue $Artifact 'name' '') -cne $ExpectedName) {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message "Candidate artifact name '$((Get-PropValue $Artifact 'name' $null))' does not match '$ExpectedName'.")
    }
    $workflowRun = Get-PropValue $Artifact 'workflow_run' $null
    if ($null -eq $workflowRun) {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message 'Candidate artifact has no workflow_run association; provenance cannot be verified.')
    }
    if ([long](Get-PropValue $workflowRun 'id' 0) -ne $ExpectedRunId) {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message "Candidate artifact belongs to workflow run '$((Get-PropValue $workflowRun 'id' $null))', not the recorded run '$ExpectedRunId'.")
    }
    $headBranch = [string](Get-PropValue $workflowRun 'head_branch' '')
    if ($headBranch -ne '' -and $headBranch -cne 'main') {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message "Candidate artifact workflow_run head_branch is '$headBranch', not main.")
    }
    if ([bool](Get-PropValue $Artifact 'expired' $false)) {
        throw (New-ReleaseModelError -Code 'artifact-expired' -Message "Candidate artifact '$ExpectedName' has expired; automatic reuse is refused.")
    }
    if ([long](Get-PropValue $Artifact 'size_in_bytes' 0) -le 0) {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message "Candidate artifact '$ExpectedName' is empty.")
    }
    $digest = [string](Get-PropValue $Artifact 'digest' '')
    if ($digest -cne '' -and $digest -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw (New-ReleaseModelError -Code 'artifact-mismatch' -Message "Candidate artifact '$ExpectedName' has an invalid digest.")
    }
    return [pscustomobject]@{ Ok = $true; Id = $ExpectedId; Name = $ExpectedName; Digest = $digest }
}

function Get-PublishLatestMode {
    <# GitHub make_latest: only a stable release at/above the current stable latest updates latest. Unknown/invalid comparisons conservatively do not update latest. #>
    param(
        [object]$CurrentStableLatestVersion,
        [Parameter(Mandatory)][string]$NewVersion
    )
    $null = ConvertTo-ReleaseVersion $NewVersion
    if ($null -eq $CurrentStableLatestVersion -or [string]$CurrentStableLatestVersion -ceq '') { return 'true' }
    try { $null = ConvertTo-ReleaseVersion ([string]$CurrentStableLatestVersion) } catch { return 'false' }
    if ((Compare-ReleaseVersion -Left $NewVersion -Right ([string]$CurrentStableLatestVersion)) -ge 0) { return 'true' }
    return 'false'
}

function Test-SafeRelativePath {
    <# Rejects absolute/UNC/traversal/control-character relative paths. #>
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $false }
    if ($Path -match '[\x00-\x1f]') { return $false }
    if ($Path -match '\\') { return $false }
    if ($Path -match '^[A-Za-z]:' -or $Path.StartsWith('/') -or $Path.StartsWith('~')) { return $false }
    $segments = $Path.Split('/')
    foreach ($segment in $segments) {
        if ($segment -ceq '' -or $segment -ceq '.' -or $segment -ceq '..') { return $false }
    }
    return $true
}

function Assert-PathUnderRoot {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Root
    )
    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $pathFull = [System.IO.Path]::GetFullPath($Path)
    $prefix = $rootFull + [System.IO.Path]::DirectorySeparatorChar
    if (-not $pathFull.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw (New-ReleaseModelError -Code 'unsafe-path' -Message "Path '$pathFull' escapes the allowed root '$rootFull'.")
    }
    return $pathFull
}

function ConvertTo-ReleaseSha256 {
    <# Normalizes a raw/`sha256:`-prefixed SHA256 to lowercase 64-hex. #>
    param([AllowEmptyString()][string]$Value)
    $text = [string]$Value
    $text = $text.Trim()
    if ($text -match '^sha256:(?<hash>[0-9a-fA-F]{64})$') { $text = $Matches['hash'] }
    if ($text -cnotmatch '^[0-9a-fA-F]{64}$') {
        throw (New-ReleaseModelError -Code 'invalid-native-evidence' -Message "Expected a 64-hex SHA256; got '$Value'.")
    }
    return $text.ToLowerInvariant()
}

function Test-ReleaseNativeEvidence {
    <#
    .SYNOPSIS
        Release-only check that packaging reused the Windows-tested native DLL.

    .DESCRIPTION
        Duplicate cargo builds are not byte-reproducible in this PE/link
        environment. The release build therefore runs windows.ps1, then asks
        packaging for `-SkipRust -ExpectedCoreDllSha256 <windows.dllSha256>`.
        package-summary must prove both the input target DLL and the staged
        pre-sign DLL match the Windows-tested hash, and that source reuse was
        explicitly verified. This is not required for ordinary CI.
    #>
    param(
        [object]$PackageNativeEvidence,
        [Parameter(Mandatory)][string]$ExpectedWindowsDllSha256
    )
    $expected = ConvertTo-ReleaseSha256 -Value $ExpectedWindowsDllSha256
    if ($null -eq $PackageNativeEvidence) {
        throw (New-ReleaseModelError -Code 'native-evidence-missing' -Message 'package-summary has no nativeEvidence for the release candidate.')
    }
    $inputRaw = [string](Get-PropValue $PackageNativeEvidence 'inputDllSha256' '')
    $stageRaw = [string](Get-PropValue $PackageNativeEvidence 'stageDllSha256' '')
    $reused = Get-PropValue $PackageNativeEvidence 'sourceReusedVerified' $null
    if ($reused -isnot [bool]) {
        throw (New-ReleaseModelError -Code 'native-evidence-unverified' -Message 'package-summary nativeEvidence.sourceReusedVerified must be a JSON boolean, not a truthy string or number.')
    }
    if ([string]::IsNullOrWhiteSpace($inputRaw) -or [string]::IsNullOrWhiteSpace($stageRaw)) {
        throw (New-ReleaseModelError -Code 'native-evidence-missing' -Message 'package-summary nativeEvidence is missing inputDllSha256/stageDllSha256.')
    }
    $inputHash = ConvertTo-ReleaseSha256 -Value $inputRaw
    $stageHash = ConvertTo-ReleaseSha256 -Value $stageRaw
    if ($inputHash -cne $expected) {
        throw (New-ReleaseModelError -Code 'native-evidence-mismatch' -Message "package-summary nativeEvidence.inputDllSha256 '$inputHash' does not match windows-summary '$expected'.")
    }
    if ($stageHash -cne $expected) {
        throw (New-ReleaseModelError -Code 'native-evidence-mismatch' -Message "package-summary nativeEvidence.stageDllSha256 '$stageHash' does not match windows-summary '$expected'.")
    }
    if (-not $reused) {
        throw (New-ReleaseModelError -Code 'native-evidence-unverified' -Message 'package-summary nativeEvidence.sourceReusedVerified is not true; the release did not prove it reused the Windows-tested DLL.')
    }
    return [pscustomobject]@{
        InputDllSha256      = $inputHash
        StageDllSha256      = $stageHash
        SourceReusedVerified = $true
        ExpectedWindowsDllSha256 = $expected
    }
}
function Test-PackageSummary {
    <#
    .SYNOPSIS
        Validates package-summary.json (schema fixed by the packaging worker)
        and returns the release/internal artifact split plus signing state.
    #>
    param(
        [object]$Summary,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string[]]$Formats
    )
    $null = ConvertTo-ReleaseVersion $Version
    $formatSet = Resolve-ReleaseFormatSet -Formats $Formats
    if ($null -eq $Summary) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'package-summary.json is missing.') }
    if ([int](Get-PropValue $Summary 'schemaVersion' 0) -ne 1) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'Unsupported package-summary schemaVersion.') }
    if ([string](Get-PropValue $Summary 'product' '') -cne 'ClipBridge') { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'package-summary product must be ClipBridge.') }
    if ([string](Get-PropValue $Summary 'version' '') -cne $Version) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'package-summary version does not match the planned version.') }
    if ([string](Get-PropValue $Summary 'platform' '') -cne 'x64') { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'package-summary platform must be x64.') }
    $summaryFormats = Resolve-ReleaseFormatSet -Formats ([string[]]@(Get-PropValue $Summary 'formats' @()))
    if (($summaryFormats -join ',') -cne ($formatSet -join ',')) {
        throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'package-summary formats do not match the requested selection.')
    }
    $signing = Get-PropValue $Summary 'signing' $null
    $mode = [string](Get-PropValue $signing 'mode' '')
    $status = [string](Get-PropValue $signing 'status' '')
    $trusted = [bool](Get-PropValue $signing 'trusted' $false)
    $timestamped = [bool](Get-PropValue $signing 'timestamped' $false)
    if ($mode -cnotin @('None', 'Certificate')) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "Unknown package signing mode '$mode'.") }
    if ($status -cnotin @('unsigned', 'signed')) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "Unknown package signing status '$status'.") }
    if ($status -ceq 'unsigned' -and ($trusted -or $timestamped)) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'Unsigned package summary cannot be trusted/timestamped.') }
    if ($mode -ceq 'Certificate' -and ($status -cne 'signed' -or -not $trusted -or -not $timestamped)) {
        throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'Certificate package summary must be signed/trusted/timestamped.')
    }

    $artifacts = @(Get-PropValue $Summary 'artifacts' @())
    $release = [System.Collections.Generic.List[object]]::new()
    $internal = [System.Collections.Generic.List[object]]::new()
    $seen = @{}
    foreach ($artifact in $artifacts) {
        $name = [string](Get-PropValue $artifact 'name' '')
        $format = [string](Get-PropValue $artifact 'format' '')
        $role = [string](Get-PropValue $artifact 'role' '')
        $isInternal = [bool](Get-PropValue $artifact 'internal' $false)
        if ($name -ceq '' -or $name -match '[/\\]') { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'package-summary contains an invalid artifact name.') }
        if ($seen.ContainsKey($name)) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary duplicates artifact '$name'.") }
        $seen[$name] = $true
        if (([string](Get-PropValue $artifact 'sha256' '')).ToLowerInvariant() -cnotmatch $script:Sha256Pattern) {
            throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary artifact '$name' has an invalid sha256.")
        }
        if ([long](Get-PropValue $artifact 'size' 0) -le 0) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary artifact '$name' has no size.") }
        $artifactSigning = Get-PropValue $artifact 'signing' $null
        $authenticode = [string](Get-PropValue $artifactSigning 'authenticode' '')
        $artifactTimestamped = [bool](Get-PropValue $artifactSigning 'timestamped' $false)
        $payloadsSigned = Get-PropValue $artifactSigning 'payloadsSigned' $null
        if ($format -ceq 'portable-zip') {
            if ($authenticode -cne 'not-applicable') { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary ZIP '$name' must be not-applicable.") }
        }
        elseif ($authenticode -cnotin @('signed', 'unsigned')) {
            throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary artifact '$name' has an invalid authenticode state.")
        }
        if ($mode -ceq 'Certificate') {
            if ($format -ceq 'portable-zip') {
                if (-not [bool]$payloadsSigned) { throw (New-ReleaseModelError -Code 'signing-required' -Message "Certificate mode requires signed payloads inside '$name'.") }
            }
            elseif ($authenticode -cne 'signed' -or -not $artifactTimestamped) {
                throw (New-ReleaseModelError -Code 'signing-required' -Message "Certificate mode requires '$name' to be signed and timestamped.")
            }
        }
        elseif ($authenticode -cne 'unsigned' -and $format -cne 'portable-zip') {
            throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "None mode expected unsigned '$name'.")
        }
        $record = [pscustomobject]@{
            name   = $name
            format = $format
            role   = $role
            internal = $isInternal
            size   = [long](Get-PropValue $artifact 'size' 0)
            sha256 = ([string](Get-PropValue $artifact 'sha256' '')).ToLowerInvariant()
        }
        if ($role -ceq 'release' -and -not $isInternal) { $release.Add($record) }
        elseif ($role -ceq 'internal' -and $isInternal) { $internal.Add($record) }
        else { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary artifact '$name' mixes role/internal flags.") }
    }

    $expectedPublic = Get-ReleasePackageAssetNames -Version $Version -Formats $formatSet
    $releaseNames = @($release | ForEach-Object { $_.name })
    foreach ($name in $expectedPublic) {
        if ($releaseNames -cnotcontains $name) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary is missing selected release artifact '$name'.") }
    }
    foreach ($record in $release) {
        if ($expectedPublic -cnotcontains $record.name) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary has an unselected release artifact '$($record.name)'.") }
        if ($record.format -cnotin $formatSet) { throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary artifact '$($record.name)' has an unselected format.") }
        if ($record.name -cne (Get-ReleaseAssetName -Format $record.format -Version $Version)) {
            throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message "package-summary artifact '$($record.name)' does not match its format.")
        }
    }
    if ($formatSet -ccontains 'user-exe') {
        if ($internal.Count -ne 1 -or $internal[0].format -cne 'user-msi') {
            throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'user-exe requires exactly one internal user-msi artifact.')
        }
    }
    elseif ($internal.Count -ne 0) {
        throw (New-ReleaseModelError -Code 'invalid-package-summary' -Message 'Internal user-msi is present but user-exe was not selected.')
    }

    return [pscustomobject]@{
        ReleaseArtifacts  = [object[]]$release.ToArray()
        InternalArtifacts = [object[]]$internal.ToArray()
        Signing           = [pscustomobject]@{ mode = $mode; status = $status; trusted = $trusted; timestamped = $timestamped }
    }
}

function Test-RustSummary {
    param(
        [object]$Summary,
        [Parameter(Mandatory)][string]$ExpectedCommit
    )
    if ($null -eq $Summary) { throw (New-ReleaseModelError -Code 'invalid-verification' -Message 'rust-summary.json is missing.') }
    if ([int](Get-PropValue $Summary 'schemaVersion' 0) -ne 1) { throw (New-ReleaseModelError -Code 'invalid-verification' -Message 'Unsupported rust-summary schemaVersion.') }
    if ([string](Get-PropValue $Summary 'status' '') -cne 'passed') { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'rust-summary status is not passed.') }
    if ([string](Get-PropValue $Summary 'commit' '') -cne $ExpectedCommit) { throw (New-ReleaseModelError -Code 'verification-mismatch' -Message 'rust-summary commit does not match the source commit.') }
    $checks = @(Get-PropValue $Summary 'checks' @())
    if ($checks.Count -eq 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'rust-summary has no checks.') }
    foreach ($check in $checks) {
        if ([int](Get-PropValue $check 'exitCode' 1) -ne 0) {
            throw (New-ReleaseModelError -Code 'verification-failed' -Message "rust-summary check '$((Get-PropValue $check 'name' $null))' failed.")
        }
    }
    $tests = Get-PropValue $Summary 'tests' $null
    if ($null -eq $tests) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'rust-summary has no test evidence.') }
    if ([long](Get-PropValue $tests 'executed' 0) -le 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'rust-summary reports zero executed tests.') }
    if ([long](Get-PropValue $tests 'failed' 0) -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'rust-summary reports failed tests.') }
    return [pscustomobject]@{
        status = 'passed'
        commit = $ExpectedCommit
        tests  = [pscustomobject]@{
            suites     = [long](Get-PropValue $tests 'suites' 0)
            discovered = [long](Get-PropValue $tests 'discovered' 0)
            executed   = [long](Get-PropValue $tests 'executed' 0)
            passed     = [long](Get-PropValue $tests 'passed' 0)
            failed     = [long](Get-PropValue $tests 'failed' 0)
            ignored    = [long](Get-PropValue $tests 'ignored' 0)
            filtered   = [long](Get-PropValue $tests 'filtered' 0)
        }
    }
}

function Test-WindowsSummary {
    param(
        [object]$Summary,
        [Parameter(Mandatory)][string]$ExpectedCommit,
        [Parameter(Mandatory)][string[]]$Formats
    )
    if ($null -eq $Summary) { throw (New-ReleaseModelError -Code 'invalid-verification' -Message 'windows-summary.json is missing.') }
    if ([int](Get-PropValue $Summary 'schemaVersion' 0) -ne 1) { throw (New-ReleaseModelError -Code 'invalid-verification' -Message 'Unsupported windows-summary schemaVersion.') }
    if ([string](Get-PropValue $Summary 'status' '') -cne 'passed') { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary status is not passed.') }
    if ([int](Get-PropValue $Summary 'exitCode' 1) -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary exitCode is not zero.') }
    if ([string](Get-PropValue $Summary 'commit' '') -cne $ExpectedCommit) { throw (New-ReleaseModelError -Code 'verification-mismatch' -Message 'windows-summary commit does not match the source commit.') }
    if ([bool](Get-PropValue $Summary 'dirty' $true)) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports a dirty worktree.') }
    if ([string](Get-PropValue $Summary 'configuration' '') -cne 'Release') { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary configuration must be Release.') }
    if ([string](Get-PropValue $Summary 'platform' '') -cne 'x64') { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary platform must be x64.') }
    if ([string](Get-PropValue $Summary 'runtimeIdentifier' '') -cne 'win-x64') { throw (New-ReleaseModelError -Code 'verification-failed -MessagePlaceholder') }
    if (-not [bool](Get-PropValue $Summary 'publishReadyToRun' $false)) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary must use PublishReadyToRun.') }
    if ($null -ne (Get-PropValue $Summary 'failure' $null)) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports a failure record.') }
    if ($null -ne (Get-PropValue $Summary 'error' $null)) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports an error record.') }

    $native = Get-PropValue $Summary 'nativeEvidence' $null
    if (-not [bool](Get-PropValue $native 'locked' $false)) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary native evidence is not locked.') }
    if (([string](Get-PropValue $native 'dllSha256' '')).ToLowerInvariant() -cnotmatch $script:Sha256Pattern) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary native DLL sha256 is missing.') }
    if ([long](Get-PropValue $native 'dllSizeBytes' 0) -le 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary native DLL size is missing.') }
    if ([string](Get-PropValue $native 'machine' '') -ine '0x8664') { throw (New-ReleaseModelError -Code 'verification-failed' -Message "windows-summary native DLL machine '$((Get-PropValue $native 'machine' $null))' is not x64 (0x8664).") }
    if ([int](Get-PropValue $native 'cargoExitCode' 1) -ne 0 -or [int](Get-PropValue $native 'buildExitCode' 1) -ne 0) {
        throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary native build failed.')
    }
    $formatEvidence = Get-PropValue $Summary 'formatEvidence' $null
    if (-not [bool](Get-PropValue $formatEvidence 'verified' $false)) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary format evidence is not verified.') }
    $formatProjects = @(Get-PropValue $formatEvidence 'projects' @())
    if ($formatProjects.Count -eq 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary has no format projects.') }
    foreach ($project in $formatProjects) {
        if ([int](Get-PropValue $project 'exitCode' 1) -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary format check failed for a project.') }
    }
    $nuget = Get-PropValue $Summary 'nugetEvidence' $null
    $queryExitCodes = @(Get-PropValue $nuget 'queryExitCodes' @())
    if ($queryExitCodes.Count -eq 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary has no NuGet query evidence.') }
    foreach ($code in $queryExitCodes) { if ([int]$code -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary NuGet query failed.') } }
    if ([long](Get-PropValue $nuget 'totalPackagesScanned' 0) -le 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary NuGet scan found no packages.') }
    if ([long](Get-PropValue $nuget 'vulnerablePackageCount' 0) -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports vulnerable NuGet packages.') }
    if (@(Get-PropValue $nuget 'violations' @()).Count -gt 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports NuGet policy violations.') }
    if (@(Get-PropValue $nuget 'problems' @()).Count -gt 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports NuGet problems.') }
    $tests = Get-PropValue $Summary 'testsEvidence' $null
    if ($null -eq $tests) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary has no test evidence.') }
    if ([int](Get-PropValue $tests 'testCommandExitCode' 1) -ne 0 -or [int](Get-PropValue $tests 'buildExitCode' 1) -ne 0) {
        throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary test/build command failed.')
    }
    if ([long](Get-PropValue $tests 'discovered' 0) -le 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary discovered zero tests.') }
    if ([long](Get-PropValue $tests 'executed' 0) -le 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports zero executed tests.') }
    if ([long](Get-PropValue $tests 'failed' 0) -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports failed tests.') }
    if (@(Get-PropValue $tests 'problems' @()).Count -gt 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary reports test problems.') }
    if (-not [bool](Get-PropValue $tests 'allMandatoryExecuted' $false)) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary did not execute all mandatory suites.') }
    $mandatory = @(Get-PropValue $tests 'mandatorySuites' @())
    if ($mandatory.Count -eq 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary has no mandatory suite evidence.') }
    foreach ($suite in $mandatory) {
        if ([long](Get-PropValue $suite 'executed' 0) -le 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message "windows-summary mandatory suite '$((Get-PropValue $suite 'name' $null))' executed zero tests.") }
        if ([long](Get-PropValue $suite 'failed' 0) -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message "windows-summary mandatory suite '$((Get-PropValue $suite 'name' $null))' had failures.") }
    }
    $checks = @(Get-PropValue $Summary 'checks' @())
    if ($checks.Count -eq 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message 'windows-summary has no check evidence.') }
    foreach ($check in $checks) {
        if ([int](Get-PropValue $check 'exitCode' 1) -ne 0) { throw (New-ReleaseModelError -Code 'verification-failed' -Message "windows-summary check '$((Get-PropValue $check 'name' $null))' failed.") }
    }
    return [pscustomobject]@{
        status                  = 'passed'
        commit                  = $ExpectedCommit
        configuration           = 'Release'
        platform                = 'x64'
        runtimeIdentifier       = 'win-x64'
        publishReadyToRun       = $true
        nativeDllSha256         = ([string](Get-PropValue $native 'dllSha256' '')).ToLowerInvariant()
        mandatorySuitesExecuted = $true
        tests                   = [pscustomobject]@{
            discovered   = [long](Get-PropValue $tests 'discovered' 0)
            executed     = [long](Get-PropValue $tests 'executed' 0)
            passed       = [long](Get-PropValue $tests 'passed' 0)
            failed       = [long](Get-PropValue $tests 'failed' 0)
            skipped      = [long](Get-PropValue $tests 'skipped' 0)
            inconclusive = [long](Get-PropValue $tests 'inconclusive' 0)
        }
    }
}
function Get-ReleaseBody {
    <# Deterministic, path-free release body. #>
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$SourceCommit,
        [Parameter(Mandatory)][string[]]$Formats,
        [Parameter(Mandatory)][string]$SigningStatus,
        [Parameter(Mandatory)][string]$CandidateDigest,
        [Parameter(Mandatory)][ValidateSet('draft', 'prerelease', 'stable')][string]$ReleaseKind
    )
    $null = ConvertTo-ReleaseVersion $Version
    if ($SourceCommit -cnotmatch $script:CommitPattern) { throw (New-ReleaseModelError -Code 'invalid-input' -Message 'Release body source commit is invalid.') }
    if ($CandidateDigest -cnotmatch $script:Sha256Pattern) { throw (New-ReleaseModelError -Code 'invalid-input' -Message 'Release body candidate digest is invalid.') }
    $formatList = (Resolve-ReleaseFormatSet -Formats $Formats) -join ', '
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add("ClipBridge $Version (Windows x64)")
    $lines.Add('')
    $lines.Add("Source commit: $SourceCommit")
    $lines.Add("Formats: $formatList")
    $lines.Add("Candidate manifest SHA256: $CandidateDigest")
    if ($SigningStatus -ceq 'signed') {
        $lines.Add('Signing: Authenticode signed, trust chain and timestamp verified by the build job.')
    }
    else {
        $lines.Add('Signing: UNSIGNED candidate. Do not describe this as production-signed.')
        if ($ReleaseKind -eq 'prerelease') {
            $lines.Add('WARNING: unsigned public prerelease. Windows SmartScreen may warn; stable promotion is blocked.')
        }
    }
    if ($ReleaseKind -eq 'stable') {
        $lines.Add('')
        $lines.Add('Stable promotion requires the windows-release-stable environment approval. The approval comment must include:')
        $lines.Add("  candidate=$CandidateDigest")
        $lines.Add('  result=pass')
        $lines.Add('  record=<https URL of the isolated-environment acceptance record>')
    }
    return (($lines -join "`n") + "`n")
}

function Get-ReleaseName {
    param([Parameter(Mandatory)][string]$Version)
    $null = ConvertTo-ReleaseVersion $Version
    return "ClipBridge $Version (Windows x64)"
}

Export-ModuleMember -Function @(
    'New-ReleaseModelError',
    'Get-PropValue',
    'ConvertTo-ReleaseBoolean',
    'ConvertTo-ReleaseVersion',
    'Compare-ReleaseVersion',
    'Resolve-SourceRefKind',
    'Get-ReleaseTag',
    'Resolve-ReleaseFormatSet',
    'Get-ReleaseFormatsFromBooleans',
    'Get-ReleaseAssetName',
    'Get-ReleasePackageAssetNames',
    'Get-ReleasePublicAssetNames',
    'Get-ReleaseUploadOrder',
    'Assert-ReleaseAssetSet',
    'New-Sha256SumsText',
    'Test-Sha256SumsText',
    'Get-ReleaseManifestDigest',
    'Assert-NoUnsafeManifestContent',
    'New-ReleaseManifest',
    'ConvertTo-ReleaseManifestJson',
    'Read-ReleaseManifestText',
    'Get-CandidateIdentity',
    'Test-CandidateIdentity',
    'Get-ReleaseState',
    'Get-ReleaseSigningPolicy',
    'Get-ReleaseTransition',
    'Get-EnvironmentReviewerPolicy',
    'Get-ApprovalGateTiming',
    'Test-ApprovalRecords',
    'Assert-StableReleaseGate',
    'Test-SourceRunRecord',
    'Test-SourceRunJobs',
    'Test-CandidateArtifactRecord',
    'Get-PublishLatestMode',
    'Test-SafeRelativePath',
    'Assert-PathUnderRoot',
    'ConvertTo-ReleaseSha256',
    'Test-ReleaseNativeEvidence',
    'Test-PackageSummary',
    'Test-RustSummary',
    'Test-WindowsSummary',
    'Get-ReleaseBody',
    'Get-ReleaseName'
)
