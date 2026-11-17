<#
.SYNOPSIS
    Static contract tests for the manual release workflow, release-please config
    and the removed old tag-triggered publish workflow. The YAML structure is
    parsed when PyYAML is available; text invariants run regardless.
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Import-Module (Join-Path $repo 'scripts/release/ReleaseRuntime.psm1') -Force
$script:FixtureTemp = New-ReleaseTempDirectory -Prefix 'cb-workflow-test'
$releaseWorkflow = Join-Path $repo '.github/workflows/release-windows.yml'
$releasePleaseWorkflow = Join-Path $repo '.github/workflows/release-please.yml'
$releasePleaseConfig = Join-Path $repo 'release-please-config.json'

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

$releaseDoc = Get-YamlDocument -Path $releaseWorkflow
$releasePleaseDoc = Get-YamlDocument -Path $releasePleaseWorkflow
$releaseText = Get-Content -Raw $releaseWorkflow
$releasePleaseText = Get-Content -Raw $releasePleaseWorkflow
$config = Get-Content -Raw $releasePleaseConfig | ConvertFrom-Json

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
    Assert-True (-not (Test-Path (Join-Path $repo '.github/workflows/publish.yml')))
    Assert-True (-not (Test-Path (Join-Path $repo '.github/workflows/publish.yml')))
}
Test-Case 'release-please is optional manual preparation on main, not a push-triggered PR' {
    if ($null -ne $releasePleaseDoc) {
        $keys = @($releasePleaseDoc.on.PSObject.Properties.Name)
        Assert-Equal 'workflow_dispatch' ($keys -join ',')
        Assert-Equal "github.ref == 'refs/heads/main'" $releasePleaseDoc.jobs.'release-please'.if
    }
    else {
        Assert-True ($releasePleaseText -match '(?m)^on:\s*\r?\n\s+workflow_dispatch:')
        Assert-True ($releasePleaseText -match "(?m)^\s+if:\s*github\.ref == 'refs/heads/main'\s*$")
    }
    Assert-True ($releasePleaseText -notmatch '(?m)^  (push|pull_request|pull_request_target|schedule|release|workflow_run|workflow_call):') 'normal pushes must not create version PRs'
}
Test-Case 'release-please cannot create releases or tags' {
    Assert-True ($config.'skip-github-release' -eq $true)
    if ($null -ne $releasePleaseDoc) {
        $step = $releasePleaseDoc.jobs.'release-please'.steps[0]
        Assert-True ($step.uses -match '@[0-9a-f]{40}$')
        Assert-Equal 'true' ([string]$step.with.'skip-github-release').ToLowerInvariant()
        Assert-Equal 'release-please-config.json' $step.with.'config-file'
        Assert-Equal 'release-please-manifest.json' $step.with.'manifest-file'
    }
    else {
        Assert-True ($releasePleaseText -match 'skip-github-release:\s*true')
        Assert-True ($releasePleaseText -match 'release-please-config\.json')
        Assert-True ($releasePleaseText -notmatch 'token:\s*\$\{\{\s*secrets\.')
    }
    Assert-True ($releasePleaseText -notmatch 'tag_name|createRelease|softprops|action-gh-release')
}
Test-Case 'release-please Windows XML extra-file path is package-relative and preserves one version truth' {
    $package = $config.packages.'platforms/windows/ClipBridgeShell_CS'
    Assert-True ($null -ne $package)
    Assert-Equal 'simple' $package.'release-type'
    Assert-Equal 'Version.props' $package.'extra-files'[0].path
    Assert-Equal 'xml' $package.'extra-files'[0].type
    Assert-True (-not ($package.PSObject.Properties.Name -contains 'version-file'))
    Assert-True (-not (Test-Path (Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/version.txt')))
    Assert-True (-not (Test-Path (Join-Path $repo 'version.txt')))
    $manifest = Get-Content -Raw (Join-Path $repo 'release-please-manifest.json') | ConvertFrom-Json
    Assert-True ($null -ne $manifest.'platforms/windows/ClipBridgeShell_CS')
}
Test-Case 'release-please XML xpath targets the single Version.props truth' {
    $versionProps = Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/Version.props'
    Assert-True (Test-Path $versionProps)
    [xml]$xml = Get-Content -Raw $versionProps
    $nodes = @($xml.SelectNodes('//Project/PropertyGroup/Version'))
    Assert-Equal 1 $nodes.Count
    Assert-True ($nodes[0].InnerText -match '^\d+\.\d+\.\d+$')
    Assert-True (-not (Test-Path (Join-Path $repo 'platforms/windows/ClipBridgeShell_CS/version.txt')))
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
