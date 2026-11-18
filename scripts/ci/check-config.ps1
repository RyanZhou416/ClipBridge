#requires -version 7.0
<#
.SYNOPSIS
    ci-config 轻量检查：workflow/脚本语法、plan 契约、文档本地链接、工具 pin。

.DESCRIPTION
    - actionlint（固定 1.7.12）检查 .github/workflows/ci.yml 与 release-windows.yml；
      版本不符失败。
    - PowerShell AST 只解析 .ps1/.psm1/.psd1，不执行脚本。
    - Bash 用 `bash -n` 只做语法解析，不执行脚本。
    - 安装器项目 XML 与本次变更的 .wxs/.wxi/.wixproj/.props/.targets/.csproj
      使用 XmlReader 做语法解析（DtdProcessing=Prohibit、XmlResolver=null，不解析外部
      实体）；不执行 MSBuild/WiX，不下载工具。
    - 文档本地链接只检查本次新增/修改的文档（-DocFiles 可显式指定）；
      不扫描整个文档库，不请求外网站点。
    - 对 ci.yml 执行固定 job/条件/needs/pin/权限/并发/工具 pin 的结构契约检查。
    - diff 不可用时仍检查 workflow、分类脚本与结构契约；文档链接范围不明时
      在摘要中标注“未检查”，不启动全仓库文档巡检。

    注意：本脚本从不执行被检查的脚本，也不下载工具（actionlint 由工作流在
    独立步骤中固定版本 + SHA256 下载）。
#>
[CmdletBinding()]
param(
    [string]$RepoRoot = (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)),
    [string]$PlanJson = $env:CI_PLAN,
    [ValidateSet('affected', 'full')]
    [string]$Scope = 'affected',
    [string]$BaseSha,
    [string]$HeadSha,
    [string]$DiffOk = 'false',
    [string[]]$Files,
    [string]$ChangedFilesFile,
    [string[]]$DocFiles,
    [string]$ActionlintPath = 'actionlint',
    [string]$BashPath,
    [switch]$AllowMissingActionlint,
    [switch]$AllowMissingBash,
    [switch]$SkipActionlint
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$errors = [System.Collections.Generic.List[string]]::new()
$notes = [System.Collections.Generic.List[string]]::new()
$root = [System.IO.Path]::GetFullPath($RepoRoot).TrimEnd('\', '/')

function Add-Error([string]$Message) {
    $errors.Add($Message)
    Write-Host "ERROR: $Message"
}

try {
    Import-Module (Join-Path $PSScriptRoot 'plan.psm1') -Force
    $toolPins = Get-CiToolPins

    # ---- 工具链 pin 与工作流/本地配置一致 ----
    $globalJsonPath = Join-Path $root 'global.json'
    if (-not (Test-Path -LiteralPath $globalJsonPath -PathType Leaf)) {
        Add-Error 'global.json is missing (the .NET SDK must be pinned)'
    } else {
        try {
            $globalJson = [System.IO.File]::ReadAllText($globalJsonPath) | ConvertFrom-Json -Depth 8
            $sdkVersion = [string]$globalJson.sdk.version
            $rollForward = [string]$globalJson.sdk.rollForward
            if ($sdkVersion -ne $toolPins.DotnetSdkVersion) {
                Add-Error "global.json SDK version must be $($toolPins.DotnetSdkVersion), got '$sdkVersion'"
            }
            if ($rollForward -ne 'disable') {
                Add-Error "global.json must pin the SDK exactly (rollForward=disable), got '$rollForward'"
            }
        } catch {
            Add-Error "global.json is invalid: $($_.Exception.Message)"
        }
    }
    $rustToolchainPath = Join-Path $root 'rust-toolchain.toml'
    if (-not (Test-Path -LiteralPath $rustToolchainPath -PathType Leaf)) {
        Add-Error 'rust-toolchain.toml is missing (the Rust toolchain must be pinned)'
    } else {
        $rustToolchainText = [System.IO.File]::ReadAllText($rustToolchainPath)
        if ($rustToolchainText -notmatch '(?m)^\s*channel\s*=\s*"([^"]+)"\s*$') {
            Add-Error 'rust-toolchain.toml must contain a channel pin'
        } elseif ($Matches[1] -ne $toolPins.RustToolchainVersion) {
            Add-Error "rust-toolchain.toml channel must be $($toolPins.RustToolchainVersion), got '$($Matches[1])'"
        }
    }

    # ---- plan（如果提供了就严格校验，并采用 plan.scope 单源） ----
    $plan = $null
    if ($PlanJson) {
        try {
            $plan = ConvertFrom-CiPlanJson -Json $PlanJson
            $Scope = [string]$plan.scope
            Write-Host "plan: schemaVersion=$($plan.schemaVersion) scope=$Scope commit=$($plan.commit)"
        } catch {
            Add-Error "plan is invalid: $($_.Exception.Message)"
        }
    }

    # ---- 变更文件集合 ----
    $changed = [System.Collections.Generic.List[string]]::new()
    $diffAvailable = $false
    if ($Files -and @($Files).Count -gt 0) {
        foreach ($file in @($Files)) { $changed.Add((ConvertTo-CiPath $file)) }
        $diffAvailable = $true
    } elseif ($ChangedFilesFile) {
        if (-not (Test-Path -LiteralPath $ChangedFilesFile)) {
            Add-Error "changed-files file not found: $ChangedFilesFile"
        } else {
            foreach ($line in [System.IO.File]::ReadAllLines($ChangedFilesFile)) {
                if ($line -ne '') { $changed.Add((ConvertTo-CiPath $line)) }
            }
            $diffAvailable = $true
        }
    } elseif ($DiffOk -eq 'true' -and $BaseSha -and $HeadSha) {
        try {
            foreach ($path in @(Get-CiChangedPaths -BaseSha $BaseSha -HeadSha $HeadSha -RepoRoot $root)) {
                $changed.Add((ConvertTo-CiPath $path))
            }
            $diffAvailable = $true
        } catch {
            $notes.Add("diff unavailable: $(Protect-CiText $_.Exception.Message)")
        }
    } else {
        $notes.Add('diff unavailable: changed-file scope unknown; workflow and classification checks still run')
    }

    # ---- 工作流结构契约（ci.yml）与 actionlint 目标（ci.yml + release-windows.yml）----
    $workflowRel = '.github/workflows/ci.yml'
    $releaseWorkflowRel = '.github/workflows/release-windows.yml'
    $workflowFull = Join-Path $root $workflowRel
    $workflowContent = ''
    if (-not (Test-Path -LiteralPath $workflowFull)) {
        Add-Error "workflow file not found: $workflowRel"
    } else {
        $workflowContent = [System.IO.File]::ReadAllText($workflowFull)
        $contract = Test-CiWorkflowContract -Content $workflowContent -Path $workflowRel
        foreach ($contractError in @($contract.Errors)) { Add-Error "workflow contract: $contractError" }
        Write-Host "workflow contract: $($contract.Jobs.Count) jobs checked"
    }
    $workflowLintFiles = [System.Collections.Generic.List[string]]::new()
    foreach ($rel in @($workflowRel, $releaseWorkflowRel)) {
        $full = Join-Path $root $rel
        if (Test-Path -LiteralPath $full -PathType Leaf) {
            # actionlint 按进程 cwd 解析相对路径；显式传入 root 下的绝对路径，
            # 保证 -RepoRoot 与调用目录不一致时仍检查同一组工作流。
            $workflowLintFiles.Add($full.Replace('\', '/'))
        } else {
            Add-Error "workflow file not found for actionlint: $rel"
        }
    }

    $bash = $null
    if ($BashPath) {
        if (Test-Path -LiteralPath $BashPath -PathType Leaf) { $bash = Get-Item -LiteralPath $BashPath } else { $bash = Get-Command $BashPath -ErrorAction SilentlyContinue }
    } else {
        $bash = Get-Command bash -ErrorAction SilentlyContinue
    }
    $bashAvailable = $false
    $bashExe = $null
    if ($null -ne $bash) {
        $bashExe = if ($bash -is [System.IO.FileInfo]) { $bash.FullName } else { $bash.Source }
        $null = & $bashExe -c 'exit 0' 2>&1
        $bashAvailable = ($LASTEXITCODE -eq 0)
    }
    if (-not $bashAvailable -and -not $IsWindows -and -not $AllowMissingBash) {
        Add-Error 'bash is required to syntax-check shell scripts but was not usable'
    }
    $embeddedPs = 0
    $embeddedBash = 0
    if ($workflowContent) {
        foreach ($block in @(Get-CiWorkflowRunBlocks -Content $workflowContent)) {
            if ($block.Shell -eq 'pwsh') {
                $tokens = $null
                $parseErrors = $null
                [System.Management.Automation.Language.Parser]::ParseInput(
                    [string]$block.Text,
                    "${workflowRel}:$($block.Line)",
                    [ref]$tokens,
                    [ref]$parseErrors) | Out-Null
                if (@($parseErrors).Count -gt 0) {
                    foreach ($parseError in @($parseErrors)) {
                        Add-Error "workflow PowerShell ${workflowRel}:$($block.Line): $($parseError.Message)"
                    }
                }
                $embeddedPs++
            } elseif ($block.Shell -eq 'bash') {
                if ($bashAvailable) {
                    $tempScript = Join-Path ([System.IO.Path]::GetTempPath()) ('clipbridge-ci-bash-' + [guid]::NewGuid().ToString('N') + '.sh')
                    try {
                        [System.IO.File]::WriteAllText($tempScript, [string]$block.Text + "`n", [System.Text.UTF8Encoding]::new($false))
                        $output = & $bashExe -n $tempScript 2>&1
                        if ($LASTEXITCODE -ne 0) {
                            Add-Error "workflow bash ${workflowRel}:$($block.Line): $((($output | ForEach-Object { [string]$_ }) -join '; '))"
                        }
                        $embeddedBash++
                    } finally {
                        Remove-Item -LiteralPath $tempScript -Force -ErrorAction SilentlyContinue
                    }
                } else {
                    $notes.Add("bash not available on this host; skipped workflow bash block at line $($block.Line)")
                }
            }
        }
    }

    # ---- 选择需要做语法检查的脚本 ----
    $declared = @(
        '.github/workflows/ci.yml',
        'scripts/ci/plan.psm1',
        'scripts/ci/get-plan.ps1',
        'scripts/ci/test-plan.ps1',
        'scripts/ci/assert-result.ps1',
        'scripts/ci/check-config.ps1',
        'scripts/ci/setup-wix.ps1',
        'scripts/ci/rust.ps1',
        'scripts/ci/rust-report.psm1',
        'scripts/ci/test-rust-report.ps1',
        'scripts/ci/test-windows-report.ps1',
        'scripts/ci/test-packaging.ps1',
        'scripts/ci/test-release-model.ps1',
        'scripts/ci/test-release-github-api.ps1',
        'scripts/ci/test-release-candidate.ps1',
        'scripts/ci/test-release-workflow.ps1',
        'scripts/ci/windows.ps1',
        'scripts/ci/sign-windows.ps1',
        'scripts/ci/android.sh',
        'scripts/ci/macos.sh',
        'scripts/ci/android/tests/run-tests.sh',
        'scripts/ci/macos/tests/run-tests.sh',
        'scripts/build-windows-installers.ps1',
        'scripts/build-windows-all.ps1',
        'scripts/build-windows-machine-msi.ps1',
        'scripts/build-windows-msi.ps1',
        'scripts/build-windows-user-exe.ps1',
        'scripts/build-windows-portable-zip.ps1',
        'scripts/version.ps1',
        'scripts/verify-ffi-surface.ps1',
        'scripts/setup-ndk.ps1',
        'scripts/check-android-shell-contracts.sh',
        'scripts/build-macos-ffi.sh',
        'scripts/check-macos-shell-env.sh',
        'scripts/ci/android/verify_apk.py',
        'scripts/ci/android/osv_policy.py'
    )

    $scriptCandidates = [System.Collections.Generic.List[string]]::new()
    if ($Scope -eq 'full') {
        foreach ($rel in $declared) {
            if (Test-Path -LiteralPath (Join-Path $root $rel) -PathType Leaf) { $scriptCandidates.Add($rel) }
        }
        foreach ($file in Get-ChildItem -LiteralPath (Join-Path $root 'scripts/ci') -Recurse -File -ErrorAction SilentlyContinue) {
            $rel = [System.IO.Path]::GetRelativePath($root, $file.FullName).Replace('\', '/')
            if ($file.Extension -in @('.ps1', '.psm1', '.psd1', '.sh') -and -not $scriptCandidates.Contains($rel)) {
                $scriptCandidates.Add($rel)
            }
        }
    } elseif ($diffAvailable) {
        foreach ($rel in $changed) {
            $ext = [System.IO.Path]::GetExtension($rel).ToLowerInvariant()
            if ($ext -in @('.ps1', '.psm1', '.psd1', '.sh') -and $rel -match '^(scripts|\.github)/') {
                if (-not $scriptCandidates.Contains($rel)) { $scriptCandidates.Add($rel) }
            }
        }
        # 分类/汇总实现是 plan 单源；即使 diff 只改文档也必须验证自身结构。
        foreach ($rel in @('scripts/ci/plan.psm1', 'scripts/ci/get-plan.ps1', 'scripts/ci/assert-result.ps1')) {
            if ((Test-Path -LiteralPath (Join-Path $root $rel) -PathType Leaf) -and -not $scriptCandidates.Contains($rel)) {
                $scriptCandidates.Add($rel)
            }
        }
    } else {
        foreach ($rel in @('scripts/ci/plan.psm1', 'scripts/ci/get-plan.ps1', 'scripts/ci/assert-result.ps1')) {
            if (Test-Path -LiteralPath (Join-Path $root $rel) -PathType Leaf) { $scriptCandidates.Add($rel) }
        }
        $notes.Add('script scope fallback: only the classification/aggregation implementation was syntax checked')
    }

    # ---- PowerShell AST ----
    $psChecked = 0
    foreach ($rel in $scriptCandidates) {
        $full = Join-Path $root $rel
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
            if ($Scope -eq 'full') {
                Add-Error "declared script is missing: $rel"
            } else {
                $notes.Add("changed/deleted script not present, skipped: $rel")
            }
            continue
        }
        if ([System.IO.Path]::GetExtension($rel) -notin @('.ps1', '.psm1', '.psd1')) { continue }
        $tokens = $null
        $parseErrors = $null
        [System.Management.Automation.Language.Parser]::ParseFile($full, [ref]$tokens, [ref]$parseErrors) | Out-Null
        if (@($parseErrors).Count -gt 0) {
            foreach ($parseError in @($parseErrors)) {
                Add-Error "PowerShell syntax ${rel}:$($parseError.Extent.StartLineNumber): $($parseError.Message)"
            }
        }
        $psChecked++
    }

    # ---- Bash -n ----
    $bashChecked = 0
    $bashFiles = @($scriptCandidates | Where-Object { [System.IO.Path]::GetExtension($_) -eq '.sh' })
    if ($bashFiles.Count -gt 0) {
        if (-not $bashAvailable) {
            if ($IsWindows -or $AllowMissingBash) {
                $notes.Add("bash not available on this host; skipped bash -n for $($bashFiles.Count) file(s)")
            } else {
                Add-Error 'bash is required to syntax-check shell scripts but was not found'
            }
        } else {
            foreach ($rel in $bashFiles) {
                $output = & $bashExe -n (Join-Path $root $rel) 2>&1
                if ($LASTEXITCODE -ne 0) {
                    Add-Error "bash syntax ${rel}: $((($output | ForEach-Object { [string]$_ }) -join '; '))"
                }
                $bashChecked++
            }
        }
    }

    # ---- actionlint ----
    $actionlintVersion = 'not checked'
    if ($SkipActionlint) {
        $notes.Add('actionlint skipped by -SkipActionlint')
    } else {
        $tool = $null
        if ($ActionlintPath -and (Test-Path -LiteralPath $ActionlintPath -PathType Leaf)) {
            $tool = (Resolve-Path -LiteralPath $ActionlintPath).Path
        } else {
            $command = Get-Command $ActionlintPath -ErrorAction SilentlyContinue
            if ($null -ne $command) { $tool = $command.Source }
        }
        if ($null -eq $tool) {
            if ($AllowMissingActionlint) {
                $notes.Add('actionlint not found; skipped (-AllowMissingActionlint)')
            } else {
                Add-Error "actionlint not found (expected $($toolPins.ActionlintVersion) at '$ActionlintPath'); ci-config must install the pinned tool"
            }
        } else {
            $versionOutput = (& $tool -version 2>&1 | ForEach-Object { [string]$_ }) -join ' '
            if ($versionOutput -notmatch [regex]::Escape($toolPins.ActionlintVersion)) {
                Add-Error "actionlint version mismatch: expected $($toolPins.ActionlintVersion), got '$versionOutput'"
            }
            $actionlintVersion = $toolPins.ActionlintVersion
            foreach ($lintFile in @($workflowLintFiles)) {
                $lintOutput = & $tool -no-color $lintFile 2>&1
                if ($LASTEXITCODE -ne 0) {
                    foreach ($line in $lintOutput) { Add-Error "actionlint ${lintFile}: $([string]$line)" }
                }
            }
        }
    }

    # ---- 安装器/项目 XML 语法（不执行 MSBuild/WiX，不解析外部实体） ----
    $declaredXml = @(
        'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs',
        'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/ClipBridgeShell_CS.Installer.wixproj',
        'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/Bundle.wxs',
        'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/ClipBridgeShell_CS.Bundle.wixproj'
    )
    $xmlExtensions = @('.wxs', '.wxi', '.wixproj', '.props', '.targets', '.csproj')
    $xmlCandidates = [System.Collections.Generic.List[string]]::new()
    $addXmlCandidate = {
        param([string]$Rel)
        if ($Rel -and -not $xmlCandidates.Contains($Rel)) { $xmlCandidates.Add($Rel) }
    }
    if ($Scope -eq 'full') {
        foreach ($rel in $declaredXml) {
            if (Test-Path -LiteralPath (Join-Path $root $rel) -PathType Leaf) {
                & $addXmlCandidate $rel
            } else {
                Add-Error "declared installer XML is missing: $rel"
            }
        }
        if ($diffAvailable) {
            foreach ($rel in $changed) {
                if ($xmlExtensions -contains [System.IO.Path]::GetExtension($rel).ToLowerInvariant()) { & $addXmlCandidate $rel }
            }
        }
    } elseif ($diffAvailable) {
        foreach ($rel in $changed) {
            if ($xmlExtensions -contains [System.IO.Path]::GetExtension($rel).ToLowerInvariant()) { & $addXmlCandidate $rel }
        }
    } else {
        $notes.Add('XML syntax scope unknown (diff unavailable); no changed project XML was checked')
    }

    $xmlChecked = 0
    foreach ($rel in $xmlCandidates) {
        $full = Join-Path $root $rel
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
            $notes.Add("deleted/changed XML not present, skipped: $rel")
            continue
        }
        $settings = [System.Xml.XmlReaderSettings]::new()
        $settings.DtdProcessing = [System.Xml.DtdProcessing]::Prohibit
        $settings.XmlResolver = $null
        $stream = $null
        $reader = $null
        try {
            $stream = [System.IO.File]::OpenRead($full)
            $reader = [System.Xml.XmlReader]::Create($stream, $settings)
            while ($reader.Read()) { }
            $xmlChecked++
        } catch [System.Xml.XmlException] {
            Add-Error "XML syntax ${rel}:$($_.Exception.LineNumber): $($_.Exception.Message)"
        } catch {
            Add-Error "XML parse ${rel}: $($_.Exception.Message)"
        } finally {
            if ($null -ne $reader) { $reader.Dispose() }
            if ($null -ne $stream) { $stream.Dispose() }
        }
    }

    # ---- 文档本地链接（只检查本次修改/新增文档） ----
    $docChecked = 0
    $docLinks = 0
    $unverifiedAnchors = 0
    $docsToCheck = [System.Collections.Generic.List[string]]::new()
    if ($DocFiles -and @($DocFiles).Count -gt 0) {
        foreach ($doc in @($DocFiles)) { $docsToCheck.Add((ConvertTo-CiPath $doc)) }
    } elseif ($diffAvailable) {
        foreach ($rel in $changed) {
            if ([System.IO.Path]::GetExtension($rel).ToLowerInvariant() -eq '.md') {
                if (Test-Path -LiteralPath (Join-Path $root $rel) -PathType Leaf) {
                    if (-not $docsToCheck.Contains($rel)) { $docsToCheck.Add($rel) }
                } else {
                    $notes.Add("deleted document not link-checked: $rel")
                }
            }
        }
    } else {
        $notes.Add('documentation link scope unknown (diff unavailable); local links were NOT checked')
    }
    if ($docsToCheck.Count -gt 0) {
        $docResult = Test-CiDocLinks -RepoRoot $root -DocPaths @($docsToCheck)
        foreach ($docError in @($docResult.Errors)) { Add-Error "doc link: $docError" }
        $docChecked = [int]$docResult.CheckedFiles
        $docLinks = [int]$docResult.CheckedLinks
        $unverifiedAnchors = [int]$docResult.UnverifiedAnchors
        if ($unverifiedAnchors -gt 0) {
            $notes.Add("$unverifiedAnchors section/anchor target(s) not verified (line targets are checked)")
        }
    }

    # ---- 摘要 ----
    $status = if ($errors.Count -eq 0) { 'PASS' } else { 'FAIL' }
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add("# ci-config: $status")
    $lines.Add('')
    $lines.Add("- scope: ``$Scope``")
    $lines.Add("- actionlint: $actionlintVersion")
    $lines.Add("- PowerShell AST files: $psChecked")
    $lines.Add("- embedded workflow scripts: pwsh=$embeddedPs bash=$embeddedBash")
    $lines.Add("- bash -n files: $bashChecked")
    $lines.Add("- XML files: $xmlChecked")
    $lines.Add("- documentation files: $docChecked ($docLinks local links checked)")
    foreach ($note in $notes) { $lines.Add("- note: $(Protect-CiText $note)") }
    if ($errors.Count -gt 0) {
        $lines.Add('')
        $lines.Add('## Failures')
        foreach ($errorMessage in $errors) { $lines.Add("- $(Protect-CiText $errorMessage)") }
    }
    $summary = ($lines -join "`n") + "`n"
    if ($env:GITHUB_STEP_SUMMARY) {
        [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, $summary, [System.Text.UTF8Encoding]::new($false))
    }
    Write-Host $summary

    if ($errors.Count -gt 0) { exit 1 }
    exit 0
} catch {
    Write-Error $_.Exception.Message
    exit 1
}
