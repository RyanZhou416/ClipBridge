<#
.SYNOPSIS
    Mocked tests for the narrow GitHub release API boundary. No test performs a
    real network call. Every write path is asserted to produce the exact
    request, and DryRun is asserted to never reach a transport.
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Import-Module (Join-Path $repo 'scripts/release/GitHubReleaseApi.psm1') -Force
Import-Module (Join-Path $repo 'scripts/release/ReleaseRuntime.psm1') -Force
$script:FixtureTemp = New-ReleaseTempDirectory -Prefix 'cb-api-test'

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

function New-MockState {
    param([scriptblock]$Handler)
    $state = @{ Calls = [System.Collections.Generic.List[object]]::new(); Handler = $Handler }
    $transport = {
        param($request)
        $state.Calls.Add($request)
        & $state.Handler $request
    }.GetNewClosure()
    $context = New-GitHubReleaseApiContext -Repository 'owner/repo' -Mode Mock -Transport $transport
    return [pscustomobject]@{ Context = $context; State = $state }
}
function New-JsonResponse {
    param([object]$Value, [int]$StatusCode = 200)
    [pscustomobject]@{ StatusCode = $StatusCode; Content = ($Value | ConvertTo-Json -Depth 12 -Compress) }
}
function New-EmptyResponse { param([int]$StatusCode = 204) [pscustomobject]@{ StatusCode = $StatusCode; Content = '' } }

Test-Case 'context rejects invalid repository' {
    Assert-Throws { New-GitHubReleaseApiContext -Repository 'not-a-repo' -Mode Mock -Transport { } } 'owner/name'
}
Test-Case 'Live requires a token' {
    Assert-Throws { New-GitHubReleaseApiContext -Repository 'o/r' -Mode Live } 'requires a token'
}
Test-Case 'Mock requires a transport' {
    Assert-Throws { New-GitHubReleaseApiContext -Repository 'o/r' -Mode Mock } 'requires a transport'
}
Test-Case 'GET release by tag and 404 -> null' {
    $m = New-MockState { param($r) if ($r.Path -like '*/releases/tags/clipbridge-windows-0.1.0') { New-JsonResponse @{ id = 7; tag_name = 'clipbridge-windows-0.1.0'; draft = $true } } else { [pscustomobject]@{ StatusCode = 404; Content = '{}' } } }
    $release = Get-GitHubReleaseByTag -Context $m.Context -Tag 'clipbridge-windows-0.1.0'
    Assert-Equal 7 $release.id
    Assert-Equal 'GET' $m.State.Calls[0].Method
    $missing = Get-GitHubReleaseByTag -Context $m.Context -Tag 'clipbridge-windows-9.9.9'
    Assert-True ($null -eq $missing)
}
Test-Case 'release assets paginate with per_page/page' {
    $page1 = @(1..100 | ForEach-Object { [pscustomobject]@{ id = $_; name = "a$_.msi" } })
    $page2 = @([pscustomobject]@{ id = 101; name = 'b.msi' })
    $m = New-MockState { param($r) if ($r.Path -match 'page=1$') { New-JsonResponse $page1 } elseif ($r.Path -match 'page=2$') { New-JsonResponse $page2 } else { [pscustomobject]@{ StatusCode = 404; Content = '{}' } } }
    $assets = Get-GitHubReleaseAssets -Context $m.Context -ReleaseId 7
    Assert-Equal 101 $assets.Count
    Assert-Equal 2 $m.State.Calls.Count
    Assert-True ($m.State.Calls[0].Path -match 'per_page=100&page=1$')
    Assert-True ($m.State.Calls[1].Path -match 'per_page=100&page=2$')
}
Test-Case 'annotated tag is peeled to its commit' {
    $m = New-MockState {
        param($r)
        if ($r.Path -like '*/git/ref/tags/v1') { New-JsonResponse @{ object = @{ type = 'tag'; sha = ('a' * 40) } } }
        elseif ($r.Path -like "*/git/tags/$('a' * 40)") { New-JsonResponse @{ object = @{ type = 'commit'; sha = ('b' * 40) } } }
        else { [pscustomobject]@{ StatusCode = 404; Content = '{}' } }
    }
    $sha = Get-GitHubTagRef -Context $m.Context -Tag 'v1'
    Assert-Equal ('b' * 40) $sha
    Assert-Equal 2 $m.State.Calls.Count
}
Test-Case 'jobs are fetched from the exact attempt, not latest' {
    $m = New-MockState { param($r) New-JsonResponse @([pscustomobject]@{ id = 1; name = 'verify'; run_attempt = 3 }) }
    $jobs = Get-GitHubWorkflowRunJobs -Context $m.Context -RunId 100 -Attempt 3
    Assert-Equal 1 $jobs.Count
    Assert-True ($m.State.Calls[0].Path -match '/actions/runs/100/attempts/3/jobs')
    Assert-True ($m.State.Calls[0].Path -notmatch '/latest')
}
Test-Case 'artifact record is fetched by exact id' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 9; name = 'windows-release-candidate-100-1'; workflow_run = @{ id = 100 } } }
    $a = Get-GitHubArtifact -Context $m.Context -ArtifactId 9
    Assert-Equal 9 $a.id
    Assert-True ($m.State.Calls[0].Path -match '/actions/artifacts/9$')
}
Test-Case 'artifact zip download writes bytes' {
    $bytes = [byte[]](1, 2, 3, 4)
    $m = New-MockState { param($r) [pscustomobject]@{ StatusCode = 200; Bytes = $bytes } }
    $out = Join-Path $script:FixtureTemp ('cb-api-' + [guid]::NewGuid().ToString('N') + '.zip')
    try {
        $null = Save-GitHubArtifactZip -Context $m.Context -ArtifactId 9 -OutFile $out
        Assert-True (Test-Path $out)
        Assert-Equal 4 (Get-Item $out).Length
    }
    finally { if (Test-Path $out) { Remove-Item $out -Force } }
}
Test-Case 'environment read and 403 unavailable' {
    $ok = New-MockState { param($r) New-JsonResponse @{ name = 'windows-release-stable'; protection_rules = @() } }
    $env = Get-GitHubEnvironment -Context $ok.Context -Name 'windows-release-stable'
    Assert-Equal 'windows-release-stable' $env.name
    $denied = New-MockState { param($r) [pscustomobject]@{ StatusCode = 403; Content = '{"message":"Forbidden"}' } }
    Assert-Throws { Get-GitHubEnvironment -Context $denied.Context -Name 'windows-release-stable' } 'Cannot verify environment'
}
Test-Case 'environment 404 returns null (caller fails closed)' {
    $m = New-MockState { param($r) [pscustomobject]@{ StatusCode = 404; Content = '{}' } }
    Assert-True ($null -eq (Get-GitHubEnvironment -Context $m.Context -Name 'missing'))
}
Test-Case 'approvals read and 403 unavailable' {
    $ok = New-MockState { param($r) New-JsonResponse @([pscustomobject]@{ state = 'approved'; comment = 'x'; environments = @(@{ name = 'windows-release-stable' }); user = @{ login = 'alice' } }) }
    $reviews = Get-GitHubRunApprovals -Context $ok.Context -RunId 100
    Assert-Equal 1 $reviews.Count
    $denied = New-MockState { param($r) [pscustomobject]@{ StatusCode = 403; Content = '{"message":"Forbidden"}' } }
    Assert-Throws { Get-GitHubRunApprovals -Context $denied.Context -RunId 100 } 'Cannot read workflow run approvals'
}
Test-Case 'latest release 404 -> null' {
    $m = New-MockState { param($r) [pscustomobject]@{ StatusCode = 404; Content = '{}' } }
    Assert-True ($null -eq (Get-GitHubLatestRelease -Context $m.Context))
}
Test-Case 'draft creation body is fixed and status-only' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 7; draft = $true; tag_name = 'clipbridge-windows-0.1.0' } }
    $release = New-GitHubDraftRelease -Context $m.Context -TagName 'clipbridge-windows-0.1.0' -TargetCommitish ('a' * 40) -Name 'ClipBridge 0.1.0' -Body 'body'
    Assert-Equal 7 $release.id
    $call = $m.State.Calls[0]
    Assert-Equal 'POST' $call.Method
    Assert-True ($call.Path -match '/releases$')
    Assert-Equal 'clipbridge-windows-0.1.0' $call.Body.tag_name
    Assert-Equal ('a' * 40) $call.Body.target_commitish
    Assert-True ([bool]$call.Body.draft)
    Assert-True (-not [bool]$call.Body.prerelease)
    Assert-True (-not [bool]$call.Body.generate_release_notes)
    Assert-True (-not ($call.Body.PSObject.Properties.Name -contains 'make_latest'))
}
Test-Case 'draft creation rejects non-SHA target' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 7 } }
    Assert-Throws { New-GitHubDraftRelease -Context $m.Context -TagName 't' -TargetCommitish 'main' -Name 'n' -Body 'b' } '40-hex'
    Assert-Equal 0 $m.State.Calls.Count
}
Test-Case 'release update is status-only and exact' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 7; draft = $false; prerelease = $true } }
    $null = Update-GitHubRelease -Context $m.Context -ReleaseId 7 -Update ([ordered]@{ draft = $false; prerelease = $true; make_latest = 'false' })
    $call = $m.State.Calls[0]
    Assert-Equal 'PATCH' $call.Method
    Assert-True ($call.Path -match '/releases/7$')
    Assert-Equal 'false' $call.Body.make_latest
    Assert-True (-not ($call.Body.PSObject.Properties.Name -contains 'tag_name'))
    Assert-True (-not ($call.Body.PSObject.Properties.Name -contains 'target_commitish'))
}
Test-Case 'release update rejects tag_name and content fields' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 7 } }
    foreach ($bad in @('tag_name', 'target_commitish', 'body', 'name', 'assets')) {
        Assert-Throws { Update-GitHubRelease -Context $m.Context -ReleaseId 7 -Update ([ordered]@{ $bad = 'x' }) } 'forbidden'
    }
    Assert-Equal 0 $m.State.Calls.Count
}
Test-Case 'release update rejects invalid make_latest' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 7 } }
    Assert-Throws { Update-GitHubRelease -Context $m.Context -ReleaseId 7 -Update ([ordered]@{ make_latest = 'yes' }) } 'make_latest'
}
Test-Case 'asset upload uses exact name query and InFile' {
    $tmp = Join-Path $script:FixtureTemp ('cb-asset-' + [guid]::NewGuid().ToString('N'))
    Set-Content -Path $tmp -Value 'payload' -NoNewline
    try {
        $m = New-MockState { param($r) New-JsonResponse @{ id = 11; name = 'ClipBridge-0.1.0-win-x64-machine.msi'; size = 7; digest = ('sha256:' + ('a' * 64)) } }
        $asset = Publish-GitHubReleaseAsset -Context $m.Context -ReleaseId 7 -Name 'ClipBridge-0.1.0-win-x64-machine.msi' -InFile $tmp
        Assert-Equal 11 $asset.id
        $call = $m.State.Calls[0]
        Assert-Equal 'POST' $call.Method
        Assert-True ($call.Path -match '/releases/7/assets\?name=ClipBridge-0\.1\.0-win-x64-machine\.msi$')
        Assert-Equal $tmp $call.InFile
    }
    finally { if (Test-Path $tmp) { Remove-Item $tmp -Force } }
}
Test-Case 'asset upload rejects unsafe names and missing files' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 11 } }
    Assert-Throws { Publish-GitHubReleaseAsset -Context $m.Context -ReleaseId 7 -Name '..\evil.exe' -InFile (Join-Path $script:FixtureTemp 'nope') } 'invalid'
    Assert-Throws { Publish-GitHubReleaseAsset -Context $m.Context -ReleaseId 7 -Name 'ok.msi' -InFile (Join-Path $script:FixtureTemp 'nope') } 'does not exist'
}
Test-Case 'release asset download writes bytes' {
    $bytes = [byte[]](9, 8, 7)
    $m = New-MockState { param($r) [pscustomobject]@{ StatusCode = 200; Bytes = $bytes } }
    $out = Join-Path $script:FixtureTemp ('cb-download-' + [guid]::NewGuid().ToString('N'))
    try {
        $null = Save-GitHubReleaseAsset -Context $m.Context -AssetId 11 -OutFile $out
        Assert-Equal 3 (Get-Item $out).Length
    }
    finally { if (Test-Path $out) { Remove-Item $out -Force } }
}
Test-Case 'DryRun refuses every request without a transport call' {
    $state = @{ Calls = 0 }
    $transport = { param($r) $state.Calls++; New-JsonResponse @{} }
    $ctx = New-GitHubReleaseApiContext -Repository 'o/r' -Mode DryRun -Transport $transport
    Assert-Throws { Invoke-GitHubRequest -Context $ctx -Method GET -Path '/x' } 'DryRun forbids'
    Assert-Throws { Invoke-GitHubRequest -Context $ctx -Method PATCH -Path '/x' -Body @{ draft = $false } } 'DryRun forbids'
    Assert-Equal 0 $state.Calls
}
Test-Case 'GET 5xx is an error in mock mode' {
    $m = New-MockState { param($r) [pscustomobject]@{ StatusCode = 500; Content = '{}' } }
    Assert-Throws { Invoke-GitHubRequest -Context $m.Context -Method GET -Path '/x' } 'HTTP 500'
}
Test-Case 'module exposes no delete/tag/clobber helper' {
    $module = Get-Module GitHubReleaseApi
    $names = @($module.ExportedFunctions.Keys)
    foreach ($name in $names) {
        Assert-True ($name -notmatch '(?i)delete|clobber|create.*tag|set.*ref') "unexpected destructive helper: $name"
    }
}
Test-Case 'update release rejects empty body' {
    $m = New-MockState { param($r) New-JsonResponse @{} }
    Assert-Throws { Update-GitHubRelease -Context $m.Context -ReleaseId 7 -Update @{} } 'empty'
}
Test-Case 'tag ref 404 -> null' {
    $m = New-MockState { param($r) [pscustomobject]@{ StatusCode = 404; Content = '{}' } }
    Assert-True ($null -eq (Get-GitHubTagRef -Context $m.Context -Tag 'missing'))
}
Test-Case 'workflow run and artifact paths' {
    $m = New-MockState { param($r) New-JsonResponse @{ id = 100; run_attempt = 2 } }
    $run = Get-GitHubWorkflowRun -Context $m.Context -RunId 100
    Assert-Equal 100 $run.id
    Assert-True ($m.State.Calls[0].Path -match '/actions/runs/100$')
}

Test-Case 'ReadOnly context allows GET and refuses writes before transport' {
    $state = @{ Calls = 0 }
    $transport = { param($r) $state.Calls++; New-JsonResponse @{ id = 1 } }
    $ctx = New-GitHubReleaseApiContext -Repository 'o/r' -Mode Mock -Transport $transport -ReadOnly
    Assert-True $ctx.ReadOnly
    $release = Get-GitHubRelease -Context $ctx -ReleaseId 1
    Assert-Equal 1 $release.id
    Assert-Equal 1 $state.Calls
    Assert-Throws { Invoke-GitHubRequest -Context $ctx -Method POST -Path '/releases' -Body @{ tag_name = 'x' } } 'ReadOnly'
    Assert-Throws { Invoke-GitHubRequest -Context $ctx -Method PATCH -Path '/releases/1' -Body @{ draft = $false } } 'ReadOnly'
    Assert-Equal 1 $state.Calls
}
Test-Case 'ReadOnly context refuses upload and draft creation helpers' {
    $state = @{ Calls = 0 }
    $transport = { param($r) $state.Calls++; New-JsonResponse @{} }
    $ctx = New-GitHubReleaseApiContext -Repository 'o/r' -Mode Mock -Transport $transport -ReadOnly
    Assert-Throws { New-GitHubDraftRelease -Context $ctx -TagName 't' -TargetCommitish ('a' * 40) -Name 'n' -Body 'b' } 'ReadOnly'
    Assert-Throws { Update-GitHubRelease -Context $ctx -ReleaseId 1 -Update @{ draft = $false } } 'ReadOnly'
    Assert-Equal 0 $state.Calls
}
if (Test-Path -LiteralPath $script:FixtureTemp) { Remove-Item -LiteralPath $script:FixtureTemp -Recurse -Force -ErrorAction SilentlyContinue }
Write-Host ''
Write-Host "$($script:Total - $script:Failed)/$($script:Total) GitHub API mock tests passed."
if ($script:Failed -gt 0) {
    $script:Failures | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}
exit 0
