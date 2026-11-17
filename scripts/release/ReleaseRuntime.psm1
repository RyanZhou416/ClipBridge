#Requires -Version 7.0
<#
.SYNOPSIS
    Runtime helpers shared by the release entrypoints: environment/event input,
    git plumbing, safe zip extraction, deterministic file hashing and GitHub
    Actions output/summary writing. These helpers perform I/O; they are kept out
    of ReleaseModel.psm1 so the model stays pure.
#>

Set-StrictMode -Version Latest
$script:Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Get-RequiredEnv {
    param([Parameter(Mandatory)][string]$Name)
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw (New-ReleaseModelError -Code 'missing-environment' -Message "Required environment variable '$Name' is not set.")
    }
    return $value
}

function Get-OptionalEnv {
    param([Parameter(Mandatory)][string]$Name, [string]$Default = '')
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ($null -eq $value) { return $Default }
    return $value
}

function Read-Utf8Text {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw (New-ReleaseModelError -Code 'missing-file' -Message "File not found: $Path")
    }
    return [System.IO.File]::ReadAllText($Path, $script:Utf8NoBom)
}

function Write-Utf8Text {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Text
    )
    $parent = Split-Path -Parent $Path
    if ($parent -and -not (Test-Path -LiteralPath $parent -PathType Container)) {
        $null = New-Item -ItemType Directory -Force -Path $parent
    }
    [System.IO.File]::WriteAllText($Path, $Text, $script:Utf8NoBom)
}

function Read-JsonFile {
    param([Parameter(Mandatory)][string]$Path)
    $text = Read-Utf8Text -Path $Path
    try { return ($text | ConvertFrom-Json) } catch {
        throw (New-ReleaseModelError -Code 'invalid-json' -Message "Invalid JSON in ${Path}: $($_.Exception.Message)")
    }
}

function Write-JsonFile {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][object]$Value
    )
    $json = $Value | ConvertTo-Json -Depth 24
    $json = ($json -replace "`r`n", "`n").TrimEnd("`n") + "`n"
    Write-Utf8Text -Path $Path -Text $json
}

function Get-FileSha256 {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw (New-ReleaseModelError -Code 'missing-file' -Message "File not found: $Path")
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-UtcNowString {
    return [DateTimeOffset]::UtcNow.ToString('o')
}

function Write-GitHubOutput {
    param(
        [Parameter(Mandatory)][string]$Name,
        [AllowEmptyString()][string]$Value = ''
    )
    $path = [Environment]::GetEnvironmentVariable('GITHUB_OUTPUT')
    if ([string]::IsNullOrWhiteSpace($path)) { return }
    if ($Value -match "`n" -or $Value -match "`r") {
        $delimiter = 'EOF_' + [guid]::NewGuid().ToString('N')
        $block = "$Name<<$delimiter`n$Value`n$delimiter`n"
        [System.IO.File]::AppendAllText($path, $block, $script:Utf8NoBom)
    }
    else {
        [System.IO.File]::AppendAllText($path, "$Name=$Value`n", $script:Utf8NoBom)
    }
}

function Write-ReleaseSummary {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string[]]$Lines,
        [string]$Heading = ''
    )
    $path = [Environment]::GetEnvironmentVariable('GITHUB_STEP_SUMMARY')
    if ([string]::IsNullOrWhiteSpace($path)) { return }
    $text = [System.Collections.Generic.List[string]]::new()
    if ($Heading) { $text.Add("## $Heading"); $text.Add('') }
    foreach ($line in $Lines) { $text.Add($line) }
    $text.Add('')
    [System.IO.File]::AppendAllText($path, (($text -join "`n") + "`n"), $script:Utf8NoBom)
}

function Invoke-GitCommand {
    param(
        [Parameter(Mandatory)][string]$WorkingDirectory,
        [Parameter(Mandatory)][string[]]$Arguments
    )
    $old = Get-Location
    Push-Location -LiteralPath $WorkingDirectory
    try {
        $output = & git @Arguments 2>&1
        $code = $LASTEXITCODE
    }
    finally { Pop-Location }
    return [pscustomobject]@{
        ExitCode = [int]$code
        Output   = (@($output) -join "`n")
    }
}

function Get-ReleaseEventInputs {
    <# Reads workflow_dispatch inputs from the event JSON without shell interpolation. #>
    param([Parameter(Mandatory)][string]$EventPath)
    if (-not (Test-Path -LiteralPath $EventPath -PathType Leaf)) {
        throw (New-ReleaseModelError -Code 'missing-event' -Message "GitHub event file not found: $EventPath")
    }
    $event = Read-JsonFile -Path $EventPath
    $inputs = Get-PropValue $event 'inputs' $null
    if ($null -eq $inputs) {
        throw (New-ReleaseModelError -Code 'invalid-input' -Message 'workflow_dispatch event has no inputs object.')
    }
    $allowed = @('source_ref', 'expected_version', 'machine_msi', 'user_exe', 'portable_zip', 'release_kind', 'allow_unsigned_prerelease')
    $keys = if ($inputs -is [System.Collections.IDictionary]) { @($inputs.Keys) } else { @($inputs.PSObject.Properties.Name) }
    foreach ($key in $keys) {
        if ($allowed -cnotcontains $key) {
            throw (New-ReleaseModelError -Code 'invalid-input' -Message "Unknown workflow_dispatch input '$key'.")
        }
    }
    $sourceRef = [string](Get-PropValue $inputs 'source_ref' 'main')
    if ([string]::IsNullOrWhiteSpace($sourceRef)) { $sourceRef = 'main' }
    $expectedVersion = [string](Get-PropValue $inputs 'expected_version' '')
    if ([string]::IsNullOrWhiteSpace($expectedVersion)) {
        throw (New-ReleaseModelError -Code 'invalid-input' -Message 'expected_version is required.')
    }
    $machine = Get-PropValue $inputs 'machine_msi' $true
    $userExe = Get-PropValue $inputs 'user_exe' $true
    $portable = Get-PropValue $inputs 'portable_zip' $true
    $releaseKind = [string](Get-PropValue $inputs 'release_kind' 'draft')
    if ($releaseKind -cnotin @('draft', 'prerelease', 'stable')) {
        throw (New-ReleaseModelError -Code 'invalid-input' -Message "release_kind must be draft/prerelease/stable; got '$releaseKind'.")
    }
    return [pscustomobject]@{
        SourceRef                = $sourceRef
        ExpectedVersion          = $expectedVersion
        MachineMsi               = (ConvertTo-ReleaseBoolean -Value $machine -Name 'machine_msi')
        UserExe                  = (ConvertTo-ReleaseBoolean -Value $userExe -Name 'user_exe')
        PortableZip              = (ConvertTo-ReleaseBoolean -Value $portable -Name 'portable_zip')
        ReleaseKind              = $releaseKind
        AllowUnsignedPrerelease  = (ConvertTo-ReleaseBoolean -Value (Get-PropValue $inputs 'allow_unsigned_prerelease' $false) -Name 'allow_unsigned_prerelease')
    }
}

function New-ReleaseApiContextFromEnv {
    param(
        [Parameter(Mandatory)][ValidateSet('Live', 'DryRun')][string]$Mode,
        [switch]$ReadOnly
    )
    $repository = Get-RequiredEnv 'GITHUB_REPOSITORY'
    $token = Get-RequiredEnv 'GITHUB_TOKEN'
    return New-GitHubReleaseApiContext -Repository $repository -Token $token -Mode $Mode -ReadOnly:$ReadOnly
}

function Read-ReleasePlanEnvironment {
    param([Parameter(Mandatory)][string]$Name = 'PLAN_JSON')
    $text = Get-RequiredEnv $Name
    try { return ($text | ConvertFrom-Json) } catch {
        throw (New-ReleaseModelError -Code 'invalid-plan' -Message "Release plan JSON is invalid: $($_.Exception.Message)")
    }
}

function Expand-ZipSafe {
    <#
    .SYNOPSIS
        Extracts a zip while rejecting absolute paths, traversal and backslash
        entries. Every output path must remain strictly under the destination.
    #>
    param(
        [Parameter(Mandatory)][string]$ZipPath,
        [Parameter(Mandatory)][string]$Destination
    )
    if (-not (Test-Path -LiteralPath $ZipPath -PathType Leaf)) {
        throw (New-ReleaseModelError -Code 'missing-file' -Message "Zip not found: $ZipPath")
    }
    $destFull = [System.IO.Path]::GetFullPath($Destination)
    $null = New-Item -ItemType Directory -Force -Path $destFull
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($ZipPath)
    try {
        foreach ($entry in $zip.Entries) {
            $entryName = $entry.FullName.Replace('\', '/')
            if ($entryName.EndsWith('/')) {
                if (-not (Test-SafeRelativePath -Path $entryName.TrimEnd('/'))) {
                    throw (New-ReleaseModelError -Code 'unsafe-zip' -Message "Unsafe zip directory entry '$entryName'.")
                }
                $null = Assert-PathUnderRoot -Path (Join-Path $destFull $entryName.TrimEnd('/')) -Root $destFull
                $null = New-Item -ItemType Directory -Force -Path (Join-Path $destFull ($entryName.TrimEnd('/').Replace('/', [System.IO.Path]::DirectorySeparatorChar)))
                continue
            }
            if (-not (Test-SafeRelativePath -Path $entryName)) {
                throw (New-ReleaseModelError -Code 'unsafe-zip' -Message "Unsafe zip entry '$entryName'.")
            }
            $target = Join-Path $destFull ($entryName.Replace('/', [System.IO.Path]::DirectorySeparatorChar))
            $null = Assert-PathUnderRoot -Path $target -Root $destFull
            $parent = Split-Path -Parent $target
            if ($parent) { $null = New-Item -ItemType Directory -Force -Path $parent }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $true)
        }
    }
    finally { $zip.Dispose() }
    return $destFull
}

function New-ReleaseTempDirectory {
    param([string]$Prefix = 'clipbridge-release')
    $base = Get-OptionalEnv 'RUNNER_TEMP' ([System.IO.Path]::GetTempPath())
    $path = Join-Path $base ($Prefix + '-' + [guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Force -Path $path
    return [System.IO.Path]::GetFullPath($path)
}

function Get-ReleaseSigningConfigurationFromEnv {
    <#
    .SYNOPSIS
        Reads explicit signing configuration. No provider is invented here: the
        only supported mode is None or an existing Windows certificate store
        certificate (Certificate). Missing parameters fail fast.
    #>
    param()
    $mode = Get-OptionalEnv 'RELEASE_SIGNING_MODE' 'None'
    if ([string]::IsNullOrWhiteSpace($mode)) { $mode = 'None' }
    if ($mode -cnotin @('None', 'Certificate')) {
        throw (New-ReleaseModelError -Code 'invalid-signing' -Message "RELEASE_SIGNING_MODE must be None or Certificate; got '$mode'.")
    }
    $thumbprint = Get-OptionalEnv 'RELEASE_SIGNING_CERT_THUMBPRINT' ''
    $timestampUrl = Get-OptionalEnv 'RELEASE_SIGNING_TIMESTAMP_URL' ''
    $signtool = Get-OptionalEnv 'RELEASE_SIGNING_SIGNTOOL_PATH' ''
    if ($mode -eq 'Certificate') {
        if ($thumbprint -cnotmatch '^[0-9a-fA-F]{40}$') {
            throw (New-ReleaseModelError -Code 'invalid-signing' -Message 'Certificate signing requires a 40-hex RELEASE_SIGNING_CERT_THUMBPRINT.')
        }
        if ($timestampUrl -notmatch '^https://') {
            throw (New-ReleaseModelError -Code 'invalid-signing' -Message 'Certificate signing requires an https RELEASE_SIGNING_TIMESTAMP_URL.')
        }
    }
    return [pscustomobject]@{
        Mode            = $mode
        Configured      = [bool]($mode -eq 'Certificate')
        Thumbprint      = $thumbprint
        TimestampUrl    = $timestampUrl
        SigntoolPath    = $signtool
    }
}

function New-ApprovalNonce {
    <# 256-bit cryptographically random approval nonce, lowercase hex. #>
    $bytes = [System.Security.Cryptography.RandomNumberGenerator]::GetBytes(32)
    return ([System.Convert]::ToHexString($bytes)).ToLowerInvariant()
}
function Get-ReleaseErrorText {
    param([Parameter(Mandatory)][object]$ErrorRecord)
    $message = $ErrorRecord.Exception.Message
    $code = $ErrorRecord.Exception.Data['ReleaseErrorCode']
    if ($code) { return "[$code] $message" }
    return $message
}

function Get-AuthenticodeState {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw (New-ReleaseModelError -Code 'missing-file' -Message "Signature check target is missing: $Path")
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    return [pscustomobject]@{
        Path         = $Path
        Status       = [string]$signature.Status
        Signed       = ($null -ne $signature.SignerCertificate)
        Timestamped  = ($null -ne $signature.TimeStamperCertificate)
    }
}

function Assert-ReleaseArtifactSignatures {
    <#
    .SYNOPSIS
        Re-verifies the final artifacts (not just the pre-sign stage). A signed
        candidate must be Valid and timestamped; an unsigned candidate must not
        silently carry a signature that the manifest does not record.
    #>
    param(
        [Parameter(Mandatory)][string]$Directory,
        [Parameter(Mandatory)][object[]]$Artifacts,
        [Parameter(Mandatory)][ValidateSet('unsigned', 'signed')][string]$SigningStatus
    )
    $root = [System.IO.Path]::GetFullPath($Directory)
    foreach ($artifact in $Artifacts) {
        $name = [string](Get-PropValue $artifact 'name' '')
        $format = [string](Get-PropValue $artifact 'format' '')
        $path = Join-Path $root $name
        $null = Assert-PathUnderRoot -Path $path -Root $root
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw (New-ReleaseModelError -Code 'missing-asset' -Message "Final artifact '$name' is missing from the candidate directory.")
        }
        if ($format -ceq 'portable-zip') {
            $zipDir = New-ReleaseTempDirectory -Prefix 'cb-zip-sign'
            try {
                $null = Expand-ZipSafe -ZipPath $path -Destination $zipDir
                foreach ($payload in @('ClipBridgeShell_CS.exe', 'core_ffi_windows.dll')) {
                    $found = @(Get-ChildItem -LiteralPath $zipDir -Recurse -File -Filter $payload)
                    if ($found.Count -ne 1) {
                        throw (New-ReleaseModelError -Code 'verification-failed' -Message "Portable ZIP '$name' must contain exactly one '$payload' (found $($found.Count)).")
                    }
                    $state = Get-AuthenticodeState -Path $found[0].FullName
                    if ($SigningStatus -ceq 'signed') {
                        if (-not $state.Signed -or $state.Status -cne 'Valid' -or -not $state.Timestamped) {
                            throw (New-ReleaseModelError -Code 'signing-required' -Message "Portable ZIP payload '$payload' is not validly signed and timestamped.")
                        }
                    }
                    elseif ($state.Signed) {
                        throw (New-ReleaseModelError -Code 'invalid-signing' -Message "Unsigned candidate ZIP payload '$payload' unexpectedly carries a signature.")
                    }
                }
            }
            finally {
                if (Test-Path -LiteralPath $zipDir) { Remove-Item -LiteralPath $zipDir -Recurse -Force -ErrorAction SilentlyContinue }
            }
            continue
        }
        $state = Get-AuthenticodeState -Path $path
        if ($SigningStatus -ceq 'signed') {
            if (-not $state.Signed -or $state.Status -cne 'Valid' -or -not $state.Timestamped) {
                throw (New-ReleaseModelError -Code 'signing-required' -Message "Artifact '$name' is not validly signed and timestamped (status=$($state.Status), signed=$($state.Signed), timestamped=$($state.Timestamped)).")
            }
        }
        elseif ($state.Signed) {
            throw (New-ReleaseModelError -Code 'invalid-signing' -Message "Unsigned candidate artifact '$name' unexpectedly carries a signature.")
        }
    }
    return $true
}

Export-ModuleMember -Function @(
    'Get-RequiredEnv',
    'Get-OptionalEnv',
    'Read-Utf8Text',
    'Write-Utf8Text',
    'Read-JsonFile',
    'Write-JsonFile',
    'Get-FileSha256',
    'Get-UtcNowString',
    'Write-GitHubOutput',
    'Write-ReleaseSummary',
    'Invoke-GitCommand',
    'Get-ReleaseEventInputs',
    'New-ReleaseApiContextFromEnv',
    'Read-ReleasePlanEnvironment',
    'Expand-ZipSafe',
    'New-ReleaseTempDirectory',
    'Get-ReleaseSigningConfigurationFromEnv',
    'Get-ReleaseErrorText',
    'New-ApprovalNonce',
    'Get-AuthenticodeState',
    'Assert-ReleaseArtifactSignatures'
)
