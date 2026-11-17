#requires -version 7.0
<#
.SYNOPSIS
    计算本次 CI 的结构化 plan（§4.4 唯一输出）。

.DESCRIPTION
    输入优先级：
      -Scope full                          → 全量计划（手动全量或 diff 不可用）
      -ChangedPaths / -ChangedFilesFile    → 直接分类给定路径
      -DiffBase + -DiffHead                → 执行 git diff --name-status -z 后分类
    都不提供时失败（执行错误，不退回“没有任务”）。git diff 本身失败时输出合法
    full 计划并在 reasons 中说明，供工作流记录“保守全跑”；分类/序列化错误直接
    非零退出，绝不生成绿色空计划。

    输出：默认 stdout 为严格校验后的 plan JSON；-OutFile 时写入 UTF-8(no BOM) 文件。

.PARAMETER Commit
    当前实际检出的完整提交 SHA。默认取 GITHUB_SHA；PR 必须是 merge result SHA。

.PARAMETER VerifyHead
    校验 Commit 等于仓库 HEAD，防止把 PR head SHA 冒充被测合并提交。

.EXAMPLE
    pwsh scripts/ci/get-plan.ps1 -Scope full -OutFile ci-plan.json

.EXAMPLE
    pwsh scripts/ci/get-plan.ps1 -Commit $env:GITHUB_SHA -DiffBase $base -DiffHead $head -VerifyHead
#>
[CmdletBinding()]
param(
    [string]$Commit = $env:GITHUB_SHA,
    [ValidateSet('affected', 'full')]
    [string]$Scope = 'affected',
    [string]$DiffBase,
    [string]$DiffHead,
    [string]$ChangedFilesFile,
    [string[]]$ChangedPaths,
    [string]$FullReason,
    [string]$OutFile,
    [switch]$VerifyHead
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

try {
    Import-Module (Join-Path $PSScriptRoot 'plan.psm1') -Force

    if ([string]::IsNullOrWhiteSpace($Commit)) { Fail 'missing -Commit (and GITHUB_SHA is empty)' }
    if (-not (Test-CiCommitSha $Commit)) { Fail "commit must be a full lowercase SHA: $Commit" }

    if ($VerifyHead) {
        $head = (& git -C $repoRoot rev-parse HEAD 2>$null)
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($head)) {
            Fail 'cannot verify commit: git rev-parse HEAD failed'
        }
        if ($head.Trim() -ne $Commit) {
            Fail "commit '$Commit' is not the checked-out HEAD '$($head.Trim())'; PR runs must use the merge-result SHA"
        }
    }

    $plan = $null
    if ($Scope -eq 'full') {
        $reason = if ($FullReason) { $FullReason } else { 'manual full validation or diff unavailable' }
        $plan = New-CiFullPlan -Commit $Commit -Reason $reason
    } else {
        $paths = @()
        if ($ChangedPaths -and @($ChangedPaths).Count -gt 0) {
            $paths = @($ChangedPaths)
        } elseif ($ChangedFilesFile) {
            if (-not (Test-Path -LiteralPath $ChangedFilesFile)) { Fail "changed-files file not found: $ChangedFilesFile" }
            $paths = @(
                [System.IO.File]::ReadAllLines($ChangedFilesFile) |
                    Where-Object { $_ -ne '' }
            )
        } elseif ($DiffBase -or $DiffHead) {
            if (-not $DiffBase -or -not $DiffHead) { Fail 'both -DiffBase and -DiffHead are required for a diff classification' }
            try {
                $paths = @(Get-CiChangedPaths -BaseSha $DiffBase -HeadSha $DiffHead -RepoRoot $repoRoot)
            } catch {
                $why = Protect-CiText ("diff unavailable: " + $_.Exception.Message)
                $plan = New-CiFullPlan -Commit $Commit -Reason $why
            }
        } else {
            Fail 'no change input: pass -Scope full, -ChangedPaths, -ChangedFilesFile, or -DiffBase/-DiffHead'
        }
        if ($null -eq $plan) { $plan = Get-CiPlanFromChangedPaths -Paths $paths -Commit $Commit }
    }

    $null = Assert-CiPlan -Plan $plan
    $json = ConvertTo-CiPlanJson -Plan $plan

    if ($OutFile) {
        $outFull = if ([System.IO.Path]::IsPathRooted($OutFile)) { $OutFile } else { Join-Path (Get-Location).Path $OutFile }
        [System.IO.File]::WriteAllText($outFull, $json + "`n", [System.Text.UTF8Encoding]::new($false))
        Write-Host "plan written: $outFull"
    } else {
        Write-Output $json
    }
    exit 0
} catch {
    Write-Error $_.Exception.Message
    exit 1
}
