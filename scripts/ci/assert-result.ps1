#requires -version 7.0
<#
.SYNOPSIS
    CI 最终汇总检查（§4.5）：读取合法 plan 与 needs 实际结果，逐项核对。

.DESCRIPTION
    失败条件（全部直接非零退出，不退化为空集合）：
      - plan 缺失、非法 JSON、schema/字段/不变量错误
      - needs 集合与固定 job 集合不一致（多、少、漏 need）
      - changes 分类任务未成功
      - plan.required=true 的任务不是 success（failure/cancelled/skipped 都失败）
      - 非必跑任务实际执行后 failure/cancelled
      - plan.commit 与本次检出的 GITHUB_SHA 不一致
      - 工作流结构契约漂移（job 条件、needs、pin、超时、权限、并集/汇总）
    同时写 Markdown 摘要到 GITHUB_STEP_SUMMARY（或 -SummaryFile），
    摘要区分“必跑/跳过原因/实际结论”，不把 skipped 一律视为通过。

.EXAMPLE
    pwsh scripts/ci/assert-result.ps1 -PlanJson $env:CI_PLAN -NeedsJson $env:CI_NEEDS
#>
[CmdletBinding()]
param(
    [string]$PlanFile,
    [string]$PlanJson = $env:CI_PLAN,
    [string]$NeedsFile,
    [string]$NeedsJson = $env:CI_NEEDS,
    [string]$WorkflowPath,
    [string]$SummaryFile,
    [string]$ExpectedCommit = $env:GITHUB_SHA
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$errors = [System.Collections.Generic.List[string]]::new()
$summaryLines = [System.Collections.Generic.List[string]]::new()

function Get-EntryValue {
    param([AllowNull()]$Object, [string]$Name)
    if ($null -eq $Object) { return $null }
    if ($Object -is [System.Collections.IDictionary]) {
        if ($Object.Contains($Name)) { return $Object[$Name] }
        return $null
    }
    $prop = $Object.PSObject.Properties[$Name]
    if ($null -eq $prop) { return $null }
    return $prop.Value
}

function Format-Cell([AllowNull()][string]$Text) {
    $clean = Protect-CiText $Text
    if (-not $clean) { return '' }
    return $clean.Replace('|', '\|')
}

try {
    Import-Module (Join-Path $PSScriptRoot 'plan.psm1') -Force

    # ---- plan ----
    $plan = $null
    if ($PlanFile) {
        try { $plan = Read-CiPlan -Path $PlanFile }
        catch { $errors.Add("plan is invalid: $($_.Exception.Message)") }
    } elseif ($PlanJson) {
        try { $plan = ConvertFrom-CiPlanJson -Json $PlanJson }
        catch { $errors.Add("plan is invalid: $($_.Exception.Message)") }
    } else {
        $errors.Add('plan output is missing: the changes job failed or produced no valid plan')
    }

    # ---- needs ----
    $needs = $null
    if ($NeedsFile) {
        if (-not (Test-Path -LiteralPath $NeedsFile)) {
            $errors.Add("needs file not found: $NeedsFile")
        } else {
            try { $needs = [System.IO.File]::ReadAllText($NeedsFile) | ConvertFrom-Json -Depth 32 }
            catch { $errors.Add("needs is not valid JSON: $($_.Exception.Message)") }
        }
    } elseif ($NeedsJson) {
        try { $needs = $NeedsJson | ConvertFrom-Json -Depth 32 }
        catch { $errors.Add("needs is not valid JSON: $($_.Exception.Message)") }
    } else {
        $errors.Add('needs output is missing: ci-result must pass toJSON(needs)')
    }

    $tasks = @(Get-CiTaskNames)
    $expectedNeeds = @('changes') + $tasks
    $actualNeeds = @()
    if ($null -ne $needs) {
        $actualNeeds = @($needs.PSObject.Properties | ForEach-Object { $_.Name })
    }

    if ($null -ne $needs) {
        foreach ($name in $expectedNeeds) {
            if ($actualNeeds -notcontains $name) { $errors.Add("needs is missing job '$name'") }
        }
        foreach ($name in $actualNeeds) {
            if ($expectedNeeds -notcontains $name) { $errors.Add("needs has unexpected job '$name'; update the fixed task set, job conditions, needs and tests together") }
        }
        $changesResult = [string](Get-EntryValue (Get-EntryValue $needs 'changes') 'result')
        if ($changesResult -ne 'success') {
            $errors.Add("classification job 'changes' result is '$changesResult', not success")
        }
    }

    # ---- plan commit ----
    if ($plan) {
        if ($ExpectedCommit -and $plan.commit -ne $ExpectedCommit) {
            $errors.Add("plan commit '$($plan.commit)' does not match the checked-out commit '$ExpectedCommit'")
        }
    }

    # ---- per-task verification ----
    $rows = [System.Collections.Generic.List[object]]::new()
    if ($plan -and $null -ne $needs) {
        foreach ($task in $tasks) {
            $required = [bool](Get-EntryValue $plan.required $task)
            $entry = Get-EntryValue $needs $task
            $result = [string](Get-EntryValue $entry 'result')
            $validResults = @('success', 'failure', 'cancelled', 'skipped')
            if ($validResults -notcontains $result) {
                $errors.Add("job '$task' has unknown/missing result '$result'")
            }
            $verdict = 'PASS'
            if ($required) {
                if ($result -ne 'success') {
                    $errors.Add("required task '$task' result is '$result', not success")
                    $verdict = 'FAIL'
                }
            } else {
                if ($result -eq 'failure' -or $result -eq 'cancelled') {
                    $errors.Add("task '$task' was not required but finished '$result'")
                    $verdict = 'FAIL'
                }
            }
            $rows.Add([pscustomobject]@{ Job = $task; Required = $required; Result = $result; Verdict = $verdict })
        }
    }

    # ---- workflow structure drift ----
    if ([string]::IsNullOrWhiteSpace($WorkflowPath)) {
        $WorkflowPath = Join-Path $repoRoot '.github/workflows/ci.yml'
    }
    if (Test-Path -LiteralPath $WorkflowPath) {
        $content = [System.IO.File]::ReadAllText($WorkflowPath)
        $contract = Test-CiWorkflowContract -Content $content -Path $WorkflowPath
        foreach ($contractError in @($contract.Errors)) {
            $errors.Add("workflow contract: $contractError")
        }
    } else {
        $errors.Add("workflow file not found for contract verification: $WorkflowPath")
    }

    # ---- summary ----
    $overall = if ($errors.Count -eq 0) { 'PASS' } else { 'FAIL' }
    $summaryLines.Add("# CI result: $overall")
    $summaryLines.Add('')
    if ($plan) {
        $summaryLines.Add("- scope: ``$($plan.scope)``")
        $summaryLines.Add("- commit: ``$($plan.commit)``")
        $packagesText = if (@($plan.packages).Count -gt 0) { @($plan.packages) -join ', ' } else { '(none)' }
        $summaryLines.Add("- packages: $packagesText")
    } else {
        $summaryLines.Add('- plan: **missing or invalid**')
    }
    $summaryLines.Add('')
    $summaryLines.Add('| job | required | actual | verdict |')
    $summaryLines.Add('|---|---|---|---|')
    foreach ($row in $rows) {
        $requiredText = if ($row.Required) { 'yes' } else { 'no' }
        $summaryLines.Add("| $($row.Job) | $requiredText | $($row.Result) | $($row.Verdict) |")
    }
    if ($rows.Count -eq 0) { $summaryLines.Add('| (no per-task results available) | - | - | FAIL |') }
    $summaryLines.Add('')
    $summaryLines.Add('## Plan reasons')
    if ($plan) {
        foreach ($task in $tasks) {
            $reasons = @(Get-EntryValue $plan.reasons $task)
            $summaryLines.Add("- **$task**: " + (($reasons | ForEach-Object { Format-Cell ([string]$_) }) -join '; '))
        }
    } else {
        $summaryLines.Add('- (plan unavailable)')
    }
    if ($errors.Count -gt 0) {
        $summaryLines.Add('')
        $summaryLines.Add('## Failures')
        foreach ($errorMessage in $errors) { $summaryLines.Add("- $(Format-Cell $errorMessage)") }
    }

    $summaryText = ($summaryLines -join "`n") + "`n"
    if ($SummaryFile) {
        [System.IO.File]::WriteAllText($SummaryFile, $summaryText, [System.Text.UTF8Encoding]::new($false))
    } elseif ($env:GITHUB_STEP_SUMMARY) {
        [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, $summaryText, [System.Text.UTF8Encoding]::new($false))
    }
    Write-Host $summaryText

    if ($errors.Count -gt 0) {
        foreach ($errorMessage in $errors) { Write-Host "ERROR: $errorMessage" }
        exit 1
    }
    exit 0
} catch {
    Write-Error $_.Exception.Message
    exit 1
}
