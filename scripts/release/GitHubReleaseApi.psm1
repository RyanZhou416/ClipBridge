#Requires -Version 7.0
<#
.SYNOPSIS
    Narrow GitHub REST boundary for the manual Windows release.

.DESCRIPTION
    All network access and all release mutations pass through this module. The
    module has three explicit modes:

      - Mock   : a caller-supplied transport scriptblock receives every request.
                 Tests use this to assert exact paths/bodies without network.
      - DryRun : every request is refused before any transport/network call.
      - Live   : real GitHub REST calls with a token.

    The module exposes no tag-creation, tag-move, asset-delete or asset-clobber
    helper. Release updates are restricted to draft/prerelease/make_latest;
    tag_name and commit fields are rejected by Update-GitHubRelease.
#>

Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'ReleaseModel.psm1') -Force

function New-GitHubHttpError {
    param(
        [Parameter(Mandatory)][string]$Message,
        [int]$StatusCode = 0,
        [string]$Path = ''
    )
    $errorRecord = [System.InvalidOperationException]::new($Message)
    $errorRecord.Data['GitHubStatusCode'] = $StatusCode
    $errorRecord.Data['GitHubPath'] = $Path
    return $errorRecord
}

function New-GitHubUnavailableError {
    param([Parameter(Mandatory)][string]$Message)
    $errorRecord = [System.InvalidOperationException]::new($Message)
    $errorRecord.Data['GitHubUnavailable'] = $true
    return $errorRecord
}

function New-GitHubReleaseApiContext {
    param(
        [Parameter(Mandatory)][string]$Repository,
        [string]$Token = '',
        [ValidateSet('Live', 'Mock', 'DryRun')][string]$Mode = 'Mock',
        [scriptblock]$Transport,
        [switch]$ReadOnly,
        [string]$BaseUri = 'https://api.github.com',
        [string]$ApiVersion = '2022-11-28'
    )
    if ($Repository -cnotmatch '^[^/\s]+/[^/\s]+$') {
        throw (New-GitHubHttpError -Message "Repository must be owner/name; got '$Repository'.")
    }
    $parts = $Repository.Split('/')
    if ($Mode -ceq 'Live' -and [string]::IsNullOrWhiteSpace($Token)) {
        throw (New-GitHubHttpError -Message 'Live GitHub access requires a token.')
    }
    if ($Mode -ceq 'Mock' -and $null -eq $Transport) {
        throw (New-GitHubHttpError -Message 'Mock mode requires a transport scriptblock.')
    }
    return [pscustomobject]@{
        Repository = $Repository
        Owner      = $parts[0]
        Repo       = $parts[1]
        Token      = $Token
        Mode       = $Mode
        ReadOnly   = [bool]$ReadOnly
        Transport  = $Transport
        BaseUri    = $BaseUri.TrimEnd('/')
        ApiVersion = $ApiVersion
        Calls      = [System.Collections.Generic.List[object]]::new()
        Writes     = [System.Collections.Generic.List[object]]::new()
    }
}

function Assert-GitHubPath {
    param([Parameter(Mandatory)][string]$Path)
    if (-not $Path.StartsWith('/')) { throw (New-GitHubHttpError -Message "GitHub path must start with '/': $Path") }
    if ($Path -match '\.\.') { throw (New-GitHubHttpError -Message "GitHub path must not contain '..': $Path") }
    return $Path
}

function ConvertTo-GitHubJsonBody {
    param([object]$Body)
    if ($null -eq $Body) { return $null }
    if ($Body -is [string]) { return $Body }
    return ($Body | ConvertTo-Json -Depth 10 -Compress)
}

function Invoke-GitHubRequest {
    <#
    .SYNOPSIS
        Executes one REST request through the configured mode. GET requests may
        retry transient failures; write requests are never retried.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][ValidateSet('GET', 'POST', 'PATCH')][string]$Method,
        [Parameter(Mandatory)][string]$Path,
        [object]$Body,
        [string]$InFile,
        [string]$OutFile,
        [string]$Accept = 'application/vnd.github+json',
        [switch]$AllowNotFound,
        [int]$MaxAttempts = 3
    )
    $null = Assert-GitHubPath -Path $Path
    $uri = $Context.BaseUri + $Path
    $jsonBody = ConvertTo-GitHubJsonBody -Body $Body
    $call = [pscustomobject]@{
        Method  = $Method
        Path    = $Path
        Uri     = $uri
        Body    = $Body
        InFile  = $InFile
        OutFile = $OutFile
        Accept  = $Accept
    }
    $Context.Calls.Add($call)
    if ($Method -cne 'GET') { $Context.Writes.Add($call) }
    if ($Context.ReadOnly -and $Method -cne 'GET') {
        throw (New-GitHubHttpError -Message "ReadOnly GitHub context forbids $Method $Path; use an explicit write-capable context for release mutations." -Path $Path)
    }

    if ($Context.Mode -ceq 'DryRun') {
        throw (New-GitHubHttpError -Message "DryRun forbids GitHub $Method $Path." -Path $Path)
    }

    if ($Context.Mode -ceq 'Mock') {
        $request = [pscustomobject]@{
            Method  = $Method
            Path    = $Path
            Uri     = $uri
            Body    = $Body
            JsonBody = $jsonBody
            InFile  = $InFile
            OutFile = $OutFile
            Accept  = $Accept
            Headers = @{
                Accept               = $Accept
                'X-GitHub-Api-Version' = $Context.ApiVersion
                'User-Agent'         = 'ClipBridge-release-worker'
            }
        }
        return ConvertFrom-GitHubMockResponse -Response (& $Context.Transport $request) -Accept $Accept -OutFile $OutFile -AllowNotFound:$AllowNotFound -Path $Path
    }

    $headers = @{
        Accept                  = $Accept
        Authorization           = "Bearer $($Context.Token)"
        'X-GitHub-Api-Version'  = $Context.ApiVersion
        'User-Agent'            = 'ClipBridge-release-worker'
    }
    $attempt = 0
    while ($true) {
        $attempt++
        try {
            if ($OutFile) {
                $null = Invoke-WebRequest -Uri $uri -Method $Method -Headers $headers -OutFile $OutFile -MaximumRedirection 10 -TimeoutSec 120 -ErrorAction Stop
                return $null
            }
            $params = @{
                Uri         = $uri
                Method      = $Method
                Headers     = $headers
                TimeoutSec  = 120
                ErrorAction = 'Stop'
            }
            if ($null -ne $jsonBody) { $params['Body'] = $jsonBody; $params['ContentType'] = 'application/json' }
            if ($InFile) {
                $params['InFile'] = $InFile
                $params['ContentType'] = 'application/octet-stream'
            }
            $response = Invoke-WebRequest @params
            if ([string]::IsNullOrWhiteSpace($response.Content)) { return $null }
            if ($Accept -match 'application/vnd\.github\+json' -or $Accept -match 'application/json') {
                return ($response.Content | ConvertFrom-Json)
            }
            return $response.Content
        }
        catch {
            $status = 0
            if ($_.Exception.PSObject.Properties['Response'] -and $null -ne $_.Exception.Response) {
                $status = [int]$_.Exception.Response.StatusCode
            }
            if ($status -eq 404 -and $AllowNotFound) { return $null }
            $transient = ($Method -ceq 'GET' -and ($status -eq 0 -or $status -eq 429 -or $status -ge 500))
            if ($transient -and $attempt -lt $MaxAttempts) {
                Start-Sleep -Milliseconds (250 * $attempt)
                continue
            }
            throw (New-GitHubHttpError -Message "GitHub $Method $Path failed (HTTP $status): $($_.Exception.Message)" -StatusCode $status -Path $Path)
        }
    }
}

function ConvertFrom-GitHubMockResponse {
    param(
        [object]$Response,
        [string]$Accept = 'application/vnd.github+json',
        [string]$OutFile = '',
        [switch]$AllowNotFound,
        [string]$Path = ''
    )
    if ($null -eq $Response) { return $null }
    $status = Get-PropValue $Response 'StatusCode' $null
    if ($null -eq $status) { return $Response }
    $statusCode = [int]$status
    if ($statusCode -eq 404 -and $AllowNotFound) { return $null }
    if ($statusCode -lt 200 -or $statusCode -ge 300) {
        throw (New-GitHubHttpError -Message "Mock GitHub response for $Path returned HTTP $statusCode." -StatusCode $statusCode -Path $Path)
    }
    if ($OutFile) {
        $bytes = Get-PropValue $Response 'Bytes' $null
        if ($null -ne $bytes) {
            [System.IO.File]::WriteAllBytes($OutFile, [byte[]]$bytes)
            return $null
        }
        $content = Get-PropValue $Response 'Content' $null
        if ($content -is [string] -and (Test-Path -LiteralPath $content -PathType Leaf)) {
            Copy-Item -LiteralPath $content -Destination $OutFile -Force
            return $null
        }
        if (Test-Path -LiteralPath $OutFile -PathType Leaf) { return $null }
        throw (New-GitHubHttpError -Message "Mock response for $Path did not provide bytes for $OutFile." -StatusCode $statusCode -Path $Path)
    }
    $content = Get-PropValue $Response 'Content' $null
    if ($null -eq $content) { return $null }
    if ($content -is [string]) {
        if ([string]::IsNullOrWhiteSpace($content)) { return $null }
        if ($Accept -match 'application/vnd\.github\+json' -or $Accept -match 'application/json') {
            return ($content | ConvertFrom-Json)
        }
        return $content
    }
    return $content
}

function Get-GitHubOwnerRepo {
    param([Parameter(Mandatory)][object]$Context)
    return "/repos/$($Context.Owner)/$($Context.Repo)"
}

function Get-GitHubPagedList {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][string]$BasePath,
        [string]$Accept = 'application/vnd.github+json'
    )
    $items = [System.Collections.Generic.List[object]]::new()
    $page = 1
    while ($true) {
        $separator = if ($BasePath.Contains('?')) { '&' } else { '?' }
        $path = "$BasePath${separator}per_page=100&page=$page"
        $response = Invoke-GitHubRequest -Context $Context -Method GET -Path $path -Accept $Accept
        if ($null -eq $response) { break }
        $pageItems = @($response)
        foreach ($item in $pageItems) { $items.Add($item) }
        if ($pageItems.Count -lt 100) { break }
        $page++
        if ($page -gt 50) { throw (New-GitHubHttpError -Message "Pagination exceeded 50 pages for $BasePath.") }
    }
    return [object[]]$items.ToArray()
}

function Get-GitHubReleaseByTag {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][string]$Tag
    )
    $path = "$(Get-GitHubOwnerRepo $Context)/releases/tags/$([Uri]::EscapeDataString($Tag))"
    return Invoke-GitHubRequest -Context $Context -Method GET -Path $path -AllowNotFound
}

function Get-GitHubReleases {
    <# Lists releases (including drafts visible to the token); used as a fallback for draft releases. #>
    param([Parameter(Mandatory)][object]$Context)
    return Get-GitHubPagedList -Context $Context -BasePath "$(Get-GitHubOwnerRepo $Context)/releases"
}

function Get-GitHubRelease {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$ReleaseId
    )
    $path = "$(Get-GitHubOwnerRepo $Context)/releases/$ReleaseId"
    return Invoke-GitHubRequest -Context $Context -Method GET -Path $path -AllowNotFound
}

function Get-GitHubReleaseAssets {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$ReleaseId
    )
    return Get-GitHubPagedList -Context $Context -BasePath "$(Get-GitHubOwnerRepo $Context)/releases/$ReleaseId/assets"
}

function Get-GitHubReleaseAsset {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$AssetId
    )
    return Invoke-GitHubRequest -Context $Context -Method GET -Path "$(Get-GitHubOwnerRepo $Context)/releases/assets/$AssetId" -AllowNotFound
}

function Get-GitHubTagRef {
    <# Returns the commit SHA a tag ultimately points to, or $null when absent. #>
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][string]$Tag
    )
    $path = "$(Get-GitHubOwnerRepo $Context)/git/ref/tags/$([Uri]::EscapeDataString($Tag))"
    $ref = Invoke-GitHubRequest -Context $Context -Method GET -Path $path -AllowNotFound
    if ($null -eq $ref) { return $null }
    $object = Get-PropValue $ref 'object' $null
    $type = [string](Get-PropValue $object 'type' '')
    $sha = [string](Get-PropValue $object 'sha' '')
    if ($sha -cnotmatch '^[0-9a-f]{40}$') { throw (New-GitHubHttpError -Message "Tag '$Tag' returned an invalid object SHA.") }
    if ($type -ceq 'commit') { return $sha }
    if ($type -ceq 'tag') {
        $tagObject = Invoke-GitHubRequest -Context $Context -Method GET -Path "$(Get-GitHubOwnerRepo $Context)/git/tags/$sha"
        $target = Get-PropValue $tagObject 'object' $null
        $targetType = [string](Get-PropValue $target 'type' '')
        $targetSha = [string](Get-PropValue $target 'sha' '')
        if ($targetType -cne 'commit' -or $targetSha -cnotmatch '^[0-9a-f]{40}$') {
            throw (New-GitHubHttpError -Message "Tag '$Tag' does not resolve to a commit.")
        }
        return $targetSha
    }
    throw (New-GitHubHttpError -Message "Tag '$Tag' points to unsupported object type '$type'.")
}

function Get-GitHubWorkflowRun {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$RunId
    )
    return Invoke-GitHubRequest -Context $Context -Method GET -Path "$(Get-GitHubOwnerRepo $Context)/actions/runs/$RunId" -AllowNotFound
}

function Get-GitHubWorkflowRunJobs {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$RunId,
        [Parameter(Mandatory)][int]$Attempt
    )
    return Get-GitHubPagedList -Context $Context -BasePath "$(Get-GitHubOwnerRepo $Context)/actions/runs/$RunId/attempts/$Attempt/jobs"
}

function Get-GitHubRunArtifacts {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$RunId
    )
    return Get-GitHubPagedList -Context $Context -BasePath "$(Get-GitHubOwnerRepo $Context)/actions/runs/$RunId/artifacts"
}

function Get-GitHubArtifact {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$ArtifactId
    )
    return Invoke-GitHubRequest -Context $Context -Method GET -Path "$(Get-GitHubOwnerRepo $Context)/actions/artifacts/$ArtifactId" -AllowNotFound
}

function Save-GitHubArtifactZip {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$ArtifactId,
        [Parameter(Mandatory)][string]$OutFile
    )
    $path = "$(Get-GitHubOwnerRepo $Context)/actions/artifacts/$ArtifactId/zip"
    $null = Invoke-GitHubRequest -Context $Context -Method GET -Path $path -Accept 'application/vnd.github+json' -OutFile $OutFile
    if (-not (Test-Path -LiteralPath $OutFile -PathType Leaf)) {
        throw (New-GitHubHttpError -Message "Artifact $ArtifactId download did not produce $OutFile.")
    }
    return (Get-Item -LiteralPath $OutFile).FullName
}

function Get-GitHubEnvironment {
    <#
    .SYNOPSIS
        Reads an environment. 403/404 are surfaced as unavailable so callers can
        fail closed instead of assuming "no protection".
    #>
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][string]$Name
    )
    $path = "$(Get-GitHubOwnerRepo $Context)/environments/$([Uri]::EscapeDataString($Name))"
    try {
        return Invoke-GitHubRequest -Context $Context -Method GET -Path $path -AllowNotFound
    }
    catch {
        $status = [int](Get-PropValue $_.Exception.Data 'GitHubStatusCode' 0)
        if ($status -in @(403, 404)) {
            throw (New-GitHubUnavailableError -Message "Cannot verify environment '$Name' (HTTP $status); stable promotion fails closed.")
        }
        throw
    }
}

function Get-GitHubRunApprovals {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$RunId
    )
    $path = "$(Get-GitHubOwnerRepo $Context)/actions/runs/$RunId/approvals"
    try {
        return Invoke-GitHubRequest -Context $Context -Method GET -Path $path -AllowNotFound
    }
    catch {
        $status = [int](Get-PropValue $_.Exception.Data 'GitHubStatusCode' 0)
        if ($status -in @(403, 404)) {
            throw (New-GitHubUnavailableError -Message "Cannot read workflow run approvals (HTTP $status); stable promotion fails closed.")
        }
        throw
    }
}

function Get-GitHubLatestRelease {
    param([Parameter(Mandatory)][object]$Context)
    return Invoke-GitHubRequest -Context $Context -Method GET -Path "$(Get-GitHubOwnerRepo $Context)/releases/latest" -AllowNotFound
}

function New-GitHubDraftRelease {
    <# Creates a draft only; public/empty releases are never created. #>
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][string]$TagName,
        [Parameter(Mandatory)][string]$TargetCommitish,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Body
    )
    if ($TargetCommitish -cnotmatch '^[0-9a-f]{40}$') {
        throw (New-GitHubHttpError -Message 'Draft release target_commitish must be the fixed 40-hex source SHA.')
    }
    $payload = [ordered]@{
        tag_name               = $TagName
        target_commitish       = $TargetCommitish
        name                   = $Name
        body                   = $Body
        draft                  = $true
        prerelease             = $false
        generate_release_notes = $false
    }
    return Invoke-GitHubRequest -Context $Context -Method POST -Path "$(Get-GitHubOwnerRepo $Context)/releases" -Body $payload
}

function Update-GitHubRelease {
    <#
    .SYNOPSIS
        Status-only release update. tag_name/target_commitish/asset fields are
        rejected so a workflow can never move a tag or rewrite release content.
    #>
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$ReleaseId,
        [Parameter(Mandatory)][object]$Update
    )
    $allowed = @('draft', 'prerelease', 'make_latest')
    $keys = @()
    if ($Update -is [System.Collections.IDictionary]) { $keys = @($Update.Keys) }
    else { $keys = @($Update.PSObject.Properties.Name) }
    if ($keys.Count -eq 0) { throw (New-GitHubHttpError -Message 'Release update body is empty.') }
    foreach ($key in $keys) {
        if ($allowed -cnotcontains $key) {
            throw (New-GitHubHttpError -Message "Release update key '$key' is forbidden; only $($allowed -join ', ') may change.")
        }
    }
    if ($keys -ccontains 'make_latest') {
        $makeLatest = [string](Get-PropValue $Update 'make_latest' '')
        if ($makeLatest -cnotin @('true', 'false', 'legacy')) {
            throw (New-GitHubHttpError -Message "make_latest must be true/false/legacy; got '$makeLatest'.")
        }
    }
    $payload = [ordered]@{}
    foreach ($key in $allowed) {
        if ($keys -ccontains $key) { $payload[$key] = Get-PropValue $Update $key $null }
    }
    return Invoke-GitHubRequest -Context $Context -Method PATCH -Path "$(Get-GitHubOwnerRepo $Context)/releases/$ReleaseId" -Body $payload
}

function Publish-GitHubReleaseAsset {
    <# Uploads a missing asset by exact name. Callers must pre-check for clobber. #>
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$ReleaseId,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$InFile
    )
    if ([string]::IsNullOrWhiteSpace($Name) -or $Name -match '[/\\]') {
        throw (New-GitHubHttpError -Message "Asset name '$Name' is invalid.")
    }
    if (-not (Test-Path -LiteralPath $InFile -PathType Leaf)) {
        throw (New-GitHubHttpError -Message "Asset input file does not exist: $InFile")
    }
    $path = "$(Get-GitHubOwnerRepo $Context)/releases/$ReleaseId/assets?name=$([Uri]::EscapeDataString($Name))"
    $response = Invoke-GitHubRequest -Context $Context -Method POST -Path $path -InFile $InFile -Accept 'application/vnd.github+json'
    if ($null -eq $response) { throw (New-GitHubHttpError -Message "Upload of '$Name' returned no asset metadata.") }
    return $response
}

function Save-GitHubReleaseAsset {
    param(
        [Parameter(Mandatory)][object]$Context,
        [Parameter(Mandatory)][long]$AssetId,
        [Parameter(Mandatory)][string]$OutFile
    )
    $path = "$(Get-GitHubOwnerRepo $Context)/releases/assets/$AssetId"
    $null = Invoke-GitHubRequest -Context $Context -Method GET -Path $path -Accept 'application/octet-stream' -OutFile $OutFile
    if (-not (Test-Path -LiteralPath $OutFile -PathType Leaf)) {
        throw (New-GitHubHttpError -Message "Asset $AssetId download did not produce $OutFile.")
    }
    return (Get-Item -LiteralPath $OutFile).FullName
}

Export-ModuleMember -Function @(
    'New-GitHubHttpError',
    'New-GitHubUnavailableError',
    'New-GitHubReleaseApiContext',
    'Invoke-GitHubRequest',
    'Get-GitHubReleaseByTag',
    'Get-GitHubReleases',
    'Get-GitHubRelease',
    'Get-GitHubReleaseAssets',
    'Get-GitHubReleaseAsset',
    'Get-GitHubTagRef',
    'Get-GitHubWorkflowRun',
    'Get-GitHubWorkflowRunJobs',
    'Get-GitHubRunArtifacts',
    'Get-GitHubArtifact',
    'Save-GitHubArtifactZip',
    'Get-GitHubEnvironment',
    'Get-GitHubRunApprovals',
    'Get-GitHubLatestRelease',
    'New-GitHubDraftRelease',
    'Update-GitHubRelease',
    'Publish-GitHubReleaseAsset',
    'Save-GitHubReleaseAsset'
)
