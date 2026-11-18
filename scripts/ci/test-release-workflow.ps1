<#
.SYNOPSIS
    Static contract tests for the manual-only release workflow and the
    two-workflow CI surface. The YAML structure is parsed when PyYAML is
    available; text invariants run regardless.

.DESCRIPTION
    Regression contract for the removal of release-please/version-PR
    automation:
      - exactly two workflows: ci.yml and release-windows.yml
      - no release-please workflow, config, manifest or action reference
      - no workflow may request pull-requests: write or run commands that
        create branches/PRs or bypass the release entry with direct git mutations
      - normal CI stays contents: read and cannot publish
      - only the manual workflow may publish, and only via workflow_dispatch
      - the Windows version remains the single Version.props XML truth
    The manual release approval/nonce/hash/pinning safety contracts are kept.
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Import-Module (Join-Path $repo 'scripts/release/ReleaseRuntime.psm1') -Force
$script:FixtureTemp = New-ReleaseTempDirectory -Prefix 'cb-workflow-test'
$workflowDir = Join-Path $repo '.github/workflows'
$ciWorkflow = Join-Path $workflowDir 'ci.yml'
$releaseWorkflow = Join-Path $workflowDir 'release-windows.yml'
$releasePleaseWorkflow = Join-Path $workflowDir 'release-please.yml'
$releasePleaseConfig = Join-Path $repo 'release-please-config.json'
$releasePleaseManifest = Join-Path $repo 'release-please-manifest.json'

$script:Total = 0
$script:Failed = 0
$script:Skipped = 0
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

function Get-YamlDocument {
    param([string]$Path)
    $python = Get-Command python -ErrorAction SilentlyContinue
    if (-not $python) { return $null }
    $script = @'
import json, sys
try:
    import yaml
except Exception:
    sys.exit(3)
with open(sys.argv[1], encoding='utf-8') as f:
    d = yaml.safe_load(f)
on = d.get('on', d.get(True))
if True in d:
    d.pop(True)
d['on'] = on
print(json.dumps(d, ensure_ascii=False))
'@
    $py = Join-Path $script:FixtureTemp ('cb-yaml-' + [guid]::NewGuid().ToString('N') + '.py')
    try {
        Set-Content -Path $py -Value $script -Encoding utf8
        $json = & python $py $Path 2>$null
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace(($json -join ''))) { return $null }
        return (($json -join "`n") | ConvertFrom-Json)
    }
    finally { if (Test-Path $py) { Remove-Item $py -Force } }
}

$workflowFiles = @(
    Get-ChildItem -LiteralPath $workflowDir -File |
        Where-Object { $_.Extension -in @('.yml', '.yaml') } |
        Sort-Object Name
)
$workflowTexts = @{}
foreach ($file in $workflowFiles) { $workflowTexts[$file.Name] = Get-Content -Raw -LiteralPath $file.FullName }

$releaseDoc = Get-YamlDocument -Path $releaseWorkflow
$ciDoc = Get-YamlDocument -Path $ciWorkflow
$releaseText = Get-Content -Raw $releaseWorkflow
$ciText = Get-Content -Raw $ciWorkflow

Test-Case 'workflow inventory is exactly ci.yml and release-windows.yml' {
    $names = @($workflowFiles | ForEach-Object { $_.Name })
    Assert-Equal 'ci.yml,release-windows.yml' ($names -join ',')
    foreach ($legacy in @('publish.yml', 'publish.yaml', 'release-please.yml', 'release-please.yaml', 'release.yml')) {
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $workflowDir $legacy))) "obsolete workflow still present: $legacy"
    }
}
Test-Case 'release-please automation and configuration are removed' {
    foreach ($artifact in @($releasePleaseWorkflow, $releasePleaseConfig, $releasePleaseManifest)) {
        Assert-True (-not (Test-Path -LiteralPath $artifact)) "release-please artifact must be removed: $artifact"
    }
    Assert-Equal 0 @(Get-ChildItem -LiteralPath $repo -File -Filter 'release-please*').Count 'root release-please files must be removed'
    foreach ($entry in $workflowTexts.GetEnumerator()) {
        Assert-True ($entry.Value -notmatch '(?i)release-please-(?:action|config|manifest)') "$($entry.Key) still references release-please automation"
    }
    Assert-True ($releaseText -notmatch '(?i)googleapis/release-please-action') 'release workflow must not invoke release-please'
}
Test-Case 'no workflow can request PR writes or create branches/PRs' {
    $forbidden = [ordered]@{
        'pull-requests: write'       = '(?im)^\s*pull-requests\s*:\s*write\s*$'
        'pull_request_target'        = '(?i)pull_request_target'
        'gh pr command'              = '(?i)\bgh\s+pr\b'
        'create-pull-request action' = '(?i)create-pull-request'
        'release-please action'      = '(?i)release-please-action'
        'softprops action-gh-release' = '(?i)softprops/action-gh-release|action-gh-release'
        'git branch/tag/push command' = '(?i)\bgit\s+(?:push|tag|branch|commit|update-ref|checkout\s+-b|switch\s+(?:-c|--create))\b'
        'GitHub CreateRef'           = '(?i)\bCreateRef\b'
        'git refs API'               = '(?i)git/refs\b'
        'pulls API'                  = '(?i)(?:/pulls\b|"pulls"|merge_pull_request)'
        'write-all permissions'      = '(?i)\bwrite-all\b'
    }
    foreach ($entry in $workflowTexts.GetEnumerator()) {
        foreach ($rule in $forbidden.GetEnumerator()) {
            Assert-True ($entry.Value -notmatch $rule.Value) "$($entry.Key) contains forbidden $($rule.Key)"
        }
    }
}
Test-Case 'normal CI is read-only and cannot publish' {
    Assert-True ($null -ne $ciDoc)
    Assert-Equal 'read' $ciDoc.permissions.contents 'ci.yml workflow permissions'
    Assert-True (-not ($ciDoc.permissions.PSObject.Properties.Name -contains 'pull-requests')) 'ci.yml must not request pull-requests permission'
    foreach ($job in $ciDoc.jobs.PSObject.Properties) {
        $permissionsProperty = $job.Value.PSObject.Properties['permissions']
        if ($null -eq $permissionsProperty) { continue }
        foreach ($permission in $permissionsProperty.Value.PSObject.Properties) {
            Assert-Equal 'read' $permission.Value "ci.yml job $($job.Name) permission $($permission.Name) must stay read-only"
        }
    }
    $ciTriggers = @($ciDoc.on.PSObject.Properties.Name | Sort-Object)
    Assert-Equal 'pull_request,push,workflow_dispatch' ($ciTriggers -join ',') 'ci.yml trigger set'
    Assert-Equal 'main' (@($ciDoc.on.push.branches) -join ',') 'ci.yml push must only target main'
    Assert-True (-not ($ciText -match '(?im)^\s*contents\s*:\s*write')) 'ci.yml must not request contents: write'
    Assert-True (-not ($ciText -match '(?i)secrets\.')) 'normal CI must not consume release secrets'
}
Test-Case 'daily CI never builds Windows distribution packages, including manual full validation' {
    Assert-True ($null -ne $ciDoc)
    $jobs = @($ciDoc.jobs.PSObject.Properties.Name | Sort-Object)
    Assert-Equal 'android,changes,ci-config,ci-result,macos,rust,windows' ($jobs -join ',') 'daily CI is validation-only, with no packaging job'
    $ciCode = (($ciText -split "`r?`n") | Where-Object { $_ -notmatch '^\s*#' }) -join "`n"
    foreach ($entry in @(
            'build-windows-(?:installers|all|machine-msi|msi|user-exe|portable-zip)\.ps1',
            'setup-wix\.ps1',
            'sign-windows\.ps1',
            'scripts/release/Invoke-(?:Preflight|Stage|Publish)\.ps1',
            'windows-packages(?:-diagnostic)?-x64-'
        )) {
        Assert-True ($ciCode -notmatch $entry) "daily CI contains a distribution build/release entry: $entry"
    }
    $packageSteps = @($releaseDoc.jobs.build.steps | Where-Object { $_.PSObject.Properties['name'] -and $_.name -eq 'Build selected packages' })
    Assert-Equal 1 $packageSteps.Count 'manual release retains the single selected-package build entry'
    Assert-True (($packageSteps[0].run -join "`n") -match 'build-windows-installers\.ps1') 'real installer builds belong to the manual release workflow'
}
Test-Case 'manual dispatch is the only publishing path' {
    Assert-True ($null -ne $releaseDoc)
    Assert-Equal 'workflow_dispatch' (@($releaseDoc.on.PSObject.Properties.Name) -join ',') 'release trigger must stay manual-only'
    $writeWorkflows = @($workflowFiles | Where-Object { $workflowTexts[$_.Name] -match '(?im)^\s*contents\s*:\s*write' } | ForEach-Object { $_.Name })
    Assert-Equal 'release-windows.yml' ($writeWorkflows -join ',') 'only the manual release workflow may write contents'
    foreach ($entry in $workflowTexts.GetEnumerator()) {
        $doc = Get-YamlDocument -Path (Join-Path $workflowDir $entry.Key)
        if ($null -eq $doc) { continue }
        $triggers = @($doc.on.PSObject.Properties.Name)
        foreach ($prohibited in @('release', 'workflow_run', 'schedule')) {
            Assert-True ($triggers -cnotcontains $prohibited) "$($entry.Key) must not trigger on $prohibited"
        }
        if ($triggers -ccontains 'push' -and $null -ne $doc.on.push) {
            Assert-True (-not ($doc.on.push.PSObject.Properties.Name -contains 'tags')) "$($entry.Key) must not trigger on tags"
        }
    }
}
Test-Case 'release workflow is workflow_dispatch only' {
    if ($null -ne $releaseDoc) {
        $on = $releaseDoc.on
        $keys = @($on.PSObject.Properties.Name)
        Assert-Equal 'workflow_dispatch' ($keys -join ',')
        foreach ($forbidden in @('push', 'pull_request', 'schedule', 'release', 'workflow_run')) {
            Assert-True ($keys -cnotcontains $forbidden) "forbidden trigger: $forbidden"
        }
    }
    else {
        Assert-True ($releaseText -match '(?m)^on:\s*\r?\n\s+workflow_dispatch:')
        Assert-True ($releaseText -notmatch '(?m)^  (push|pull_request|schedule|release|workflow_run):')
    }
}
Test-Case 'release inputs are the fixed contract' {
    if ($null -ne $releaseDoc) {
        $inputs = $releaseDoc.on.workflow_dispatch.inputs
        Assert-Equal 'source_ref,expected_version,machine_msi,user_exe,portable_zip,release_kind,allow_unsigned_prerelease' (@($inputs.PSObject.Properties.Name) -join ',')
        Assert-Equal 'main' $inputs.source_ref.default
        Assert-Equal 'string' $inputs.expected_version.type
        Assert-True ([bool]$inputs.machine_msi.default)
        Assert-True ([bool]$inputs.user_exe.default)
        Assert-True ([bool]$inputs.portable_zip.default)
        Assert-Equal 'draft' $inputs.release_kind.default
        Assert-Equal 'draft,prerelease,stable' (@($inputs.release_kind.options) -join ',')
        Assert-True (-not [bool]$inputs.allow_unsigned_prerelease.default)
    }
    else {
        foreach ($name in @('source_ref', 'expected_version', 'machine_msi', 'user_exe', 'portable_zip', 'release_kind', 'allow_unsigned_prerelease')) {
            Assert-True ($releaseText -match "(?m)^\s+$name\s*:") "missing input $name"
        }
        Assert-True ($releaseText -match 'options:\s*\r?\n\s+- draft\s*\r?\n\s+- prerelease\s*\r?\n\s+- stable')
    }
}
Test-Case 'release jobs are fixed and time-bounded' {
    if ($null -ne $releaseDoc) {
        Assert-Equal 'preflight,verify,build,stage,approve-stable,publish' (@($releaseDoc.jobs.PSObject.Properties.Name) -join ',')
        foreach ($job in $releaseDoc.jobs.PSObject.Properties) {
            Assert-True ($null -ne $job.Value.'timeout-minutes') "job $($job.Name) has no timeout"
            Assert-True ($null -ne $job.Value.'runs-on') "job $($job.Name) has no runner"
        }
        Assert-Equal 'windows-release-stable' $releaseDoc.jobs.'approve-stable'.environment
        Assert-True ($releaseDoc.jobs.stage.if -match "build-new" -and $releaseDoc.jobs.stage.if -match "build.result == 'skipped'" -and $releaseDoc.jobs.stage.if -match "verify.result == 'success'")
        Assert-True ($releaseDoc.jobs.publish.if -match "publish_required == 'true'" -and $releaseDoc.jobs.publish.if -match "approve-stable.result == 'success'")
        Assert-Equal 'windows-release' $releaseDoc.concurrency.group
        Assert-True (-not [bool]$releaseDoc.concurrency.'cancel-in-progress')
    }
    else {
        foreach ($job in @('preflight', 'verify', 'build', 'stage', 'approve-stable', 'publish')) {
            Assert-True ($releaseText -match "(?m)^  ${job}:") "missing job $job"
        }
        Assert-True ($releaseText -match 'environment:\s*windows-release-stable')
        Assert-True ($releaseText -match "build\.result == 'skipped'")
        Assert-True ($releaseText -match "approve-stable\.result == 'success'")
        Assert-True ($releaseText -match 'group:\s*windows-release')
        Assert-True ($releaseText -match 'cancel-in-progress:\s*false')
    }
}
Test-Case 'verify runs Rust host checks on ubuntu and build owns Windows' {
    if ($null -ne $releaseDoc) {
        Assert-Equal 'ubuntu-24.04' $releaseDoc.jobs.verify.'runs-on'
        Assert-Equal 'windows-2022' $releaseDoc.jobs.build.'runs-on'
    }
    else {
        Assert-True ($releaseText -match '(?ms)verify:.*?runs-on:\s*ubuntu-24\.04')
    }
}
Test-Case 'build uses the exact SDK from source global.json and conditional WiX' {
    Assert-True ($null -ne $releaseDoc)
    $buildSteps = @($releaseDoc.jobs.build.steps)
    $resolve = @($buildSteps | Where-Object { $_.PSObject.Properties['id'] -and $_.id -eq 'dotnet-sdk' })
    Assert-Equal 1 $resolve.Count
    $setup = @($buildSteps | Where-Object { $_.PSObject.Properties['uses'] -and $_.uses -match 'actions/setup-dotnet' })
    Assert-Equal 1 $setup.Count
    Assert-Equal '${{ steps.dotnet-sdk.outputs.dotnet-version }}' $setup[0].with.'dotnet-version'
    Assert-True (-not ($setup[0].with.'dotnet-version' -match '10\.0\.x'))
    $wix = @($buildSteps | Where-Object { $_.name -match 'WiX' })
    Assert-Equal 1 $wix.Count
    Assert-True ($wix[0].if -match "machine-msi" -and $wix[0].if -match "user-exe")
    Assert-True (($wix[0].run -join "`n") -match 'setup-wix\.ps1')
    Assert-True (-not ($releaseText -match '6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31'))
}
Test-Case 'job permissions are least-privilege' {
    if ($null -ne $releaseDoc) {
        $workflowPerms = $releaseDoc.permissions
        Assert-Equal 'read' $workflowPerms.contents
        Assert-Equal 'write' $releaseDoc.jobs.preflight.permissions.contents
        Assert-Equal 'read' $releaseDoc.jobs.preflight.permissions.actions
        Assert-Equal 'read' $releaseDoc.jobs.verify.permissions.contents
        Assert-Equal 'read' $releaseDoc.jobs.build.permissions.contents
        Assert-Equal 'write' $releaseDoc.jobs.stage.permissions.contents
        Assert-Equal 'read' $releaseDoc.jobs.stage.permissions.actions
        Assert-Equal 'write' $releaseDoc.jobs.publish.permissions.contents
        Assert-Equal 'read' $releaseDoc.jobs.publish.permissions.actions
        $approvalPerms = $releaseDoc.jobs.'approve-stable'.permissions
        if ($null -ne $approvalPerms) { Assert-Equal 'read' $approvalPerms.contents }
    }
    else {
        Assert-True ($releaseText -match 'permissions:\s*\r?\n\s+contents:\s*read')
        Assert-True ($releaseText -match 'contents:\s*write')
    }
}
Test-Case 'all uses are pinned to full commit SHAs' {
    $uses = [regex]::Matches($releaseText, '(?m)^\s*-?\s*uses:\s*(\S+)') | ForEach-Object { $_.Groups[1].Value }
    Assert-True ($uses.Count -ge 4)
    foreach ($use in $uses) {
        Assert-True ($use -match '@[0-9a-f]{40}$') "unpinned action: $use"
    }
    Assert-True (-not ($releaseText -match 'softprops|action-gh-release'))
    Assert-True (-not ($releaseText -match '(?i)--clobber|git\s+tag|git\s+push|git\s+reset|CreateRef|delete'))
    Assert-True (-not ($releaseText -match 'pull_request_target|workflow_run'))
    Assert-True (-not ($releaseText -match 'RELEASE_PLEASE_TOKEN'))
    $tokenRefs = [regex]::Matches($releaseText, 'secrets\.([A-Za-z0-9_]+)') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
    Assert-Equal 'GITHUB_TOKEN' (($tokenRefs) -join ',')
}
Test-Case 'old automatic tag publish entry is removed' {
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $workflowDir 'publish.yml')))
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $workflowDir 'publish.yaml')))
}
Test-Case 'Windows Version.props is the single XML version truth' {
    $versionProps = Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/Version.props'
    Assert-True (Test-Path -LiteralPath $versionProps)
    [xml]$xml = Get-Content -Raw -LiteralPath $versionProps
    $nodes = @($xml.SelectNodes('//Project/PropertyGroup/Version'))
    Assert-Equal 1 $nodes.Count 'Version.props must define exactly one PropertyGroup Version'
    Assert-True ($nodes[0].InnerText -match '^\d+\.\d+\.\d+$') 'Version.props Version must be three-part'
    Assert-Equal 1 @($xml.SelectNodes('//Version')).Count 'Version.props must not define competing Version elements'
    $directoryProps = Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/Directory.Build.props'
    Assert-True (Test-Path -LiteralPath $directoryProps)
    $directoryPropsText = Get-Content -Raw -LiteralPath $directoryProps
    Assert-True ($directoryPropsText -match 'Version\.props') 'Directory.Build.props must import Version.props'
    Assert-True (-not ($directoryPropsText -match '<Version>')) 'Directory.Build.props must not duplicate the version'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/version.txt')))
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $repo 'version.txt')))
}
Test-Case 'nonce gate is wired from stage through approve-stable to publish' {
    Assert-True ($null -ne $releaseDoc)
    Assert-Equal '${{ steps.stage.outputs.approval_nonce }}' $releaseDoc.jobs.stage.outputs.approval_nonce
    Assert-Equal '${{ steps.gate.outputs.approval_nonce }}' $releaseDoc.jobs.'approve-stable'.outputs.approval_nonce
    Assert-Equal '${{ steps.gate.outputs.run_attempt }}' $releaseDoc.jobs.'approve-stable'.outputs.run_attempt
    $gate = @($releaseDoc.jobs.'approve-stable'.steps | Where-Object { $_.PSObject.Properties['id'] -and $_.id -eq 'gate' })
    Assert-Equal 1 $gate.Count
    Assert-Equal '${{ needs.stage.outputs.approval_nonce }}' $gate[0].env.STAGE_NONCE
    $publishEnv = $releaseDoc.jobs.publish.steps[-1].env
    Assert-Equal '${{ needs.stage.outputs.approval_nonce }}' $publishEnv.STAGE_APPROVAL_NONCE
    Assert-Equal '${{ needs.approve-stable.outputs.approval_nonce }}' $publishEnv.GATE_APPROVAL_NONCE
    Assert-Equal '${{ needs.approve-stable.outputs.run_attempt }}' $publishEnv.GATE_RUN_ATTEMPT
}
Test-Case 'preflight is GET-only at the API context boundary' {
    $preflight = Get-Content -Raw (Join-Path $repo 'scripts/release/Invoke-Preflight.ps1')
    Assert-True ($preflight -match 'New-ReleaseApiContextFromEnv -Mode \$Mode -ReadOnly')
    $api = Get-Content -Raw (Join-Path $repo 'scripts/release/GitHubReleaseApi.psm1')
    Assert-True ($api -match 'ReadOnly.*forbids|forbids.*ReadOnly')
}
Test-Case 'packaging reuses the Windows-tested DLL and expected hash' {
    Assert-True ($null -ne $releaseDoc)
    $buildStep = @($releaseDoc.jobs.build.steps | Where-Object { $_.PSObject.Properties['name'] -and $_.name -eq 'Build selected packages' })
    Assert-Equal 1 $buildStep.Count
    $run = ($buildStep[0].run -join "`n")
    Assert-True ($run -match 'windows-summary\.json')
    Assert-True ($run -match 'nativeEvidence\.dllSha256')
    Assert-True ($run -match 'SkipRust\s*=\s*\$true')
    Assert-True ($run -match 'ExpectedCoreDllSha256')
}
Test-Case 'PowerShell release files parse without syntax errors' {
    $files = @(Get-ChildItem -LiteralPath (Join-Path $repo 'scripts/release') -File -Include '*.ps1', '*.psm1' -Recurse) + @(Get-ChildItem -LiteralPath $PSScriptRoot -File -Filter 'test-release*.ps1')
    Assert-True ($files.Count -ge 5)
    foreach ($file in $files) {
        $tokens = $null; $errors = $null
        $null = [System.Management.Automation.Language.Parser]::ParseFile($file.FullName, [ref]$tokens, [ref]$errors)
        Assert-Equal 0 @($errors).Count "parse errors in $($file.Name): $((@($errors) | ForEach-Object { $_.Message }) -join '; ')"
    }
}
Test-Case 'release workflow does not interpolate dispatch inputs into shell source' {
    # Inputs must flow through the event JSON / environment, not ${{ inputs.* }} in run:.
    Assert-True (-not ($releaseText -match '\$\{\{\s*inputs\.'))
    Assert-True (-not ($releaseText -match '\$\{\{\s*github\.event\.inputs'))
}
if (Test-Path -LiteralPath $script:FixtureTemp) { Remove-Item -LiteralPath $script:FixtureTemp -Recurse -Force -ErrorAction SilentlyContinue }

Write-Host ''
if ($null -eq $releaseDoc) { Write-Host 'note: PyYAML not available; structural YAML checks used text invariants only.' }
Write-Host "$($script:Total - $script:Failed)/$($script:Total) workflow/config tests passed."
if ($script:Failed -gt 0) {
    $script:Failures | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}
exit 0
