#requires -version 7.0
<#
.SYNOPSIS
    plan / 分类 / 汇总 / 工作流契约的表驱动回归测试（无新框架，纯 PowerShell）。

.DESCRIPTION
    覆盖 §4.2-§4.5 与 §9.2 的核心契约：
      - 公共核心、头文件、锁文件、工具链、工作流、脚本、删除/重命名、纯文档、未知路径
      - plan schema 严格校验与全部不变量（负路径）
      - needs 实际结果核对：必跑失败/取消/意外 skipped，额外任务失败，缺失 needs
      - actionlint/WiX 工具 pin 与第三方 Action SHA pin 的工作流结构契约
      - 文档本地链接只检查指定文档
    测试只读仓库、写独占临时目录，不执行 dotnet/cargo/安装器，不联网。
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$script:Passed = 0
$script:Failed = 0
$script:Failures = [System.Collections.Generic.List[string]]::new()
$script:tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('clipbridge-ci-tests-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $script:tempRoot | Out-Null
$script:pwsh = (Get-Command pwsh).Source
$script:fixedTasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages')
$script:fixedPackages = @('machine-msi', 'user-exe', 'portable-zip')
$script:testCommit = 'a' * 40

Import-Module (Join-Path $PSScriptRoot 'plan.psm1') -Force

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-Equal {
    param($Expected, $Actual, [string]$Message)
    $expectedText = if ($Expected -is [System.Array]) { ($Expected -join ',') } else { [string]$Expected }
    $actualText = if ($Actual -is [System.Array]) { ($Actual -join ',') } else { [string]$Actual }
    if ($expectedText -ne $actualText) {
        throw "$Message`n  expected: $expectedText`n  actual:   $actualText"
    }
}

function Assert-Match {
    param([string]$Text, [string]$Pattern, [string]$Message)
    if ($Text -notmatch $Pattern) { throw "$Message (pattern /$Pattern/ not found in: $(Protect-CiText $Text))" }
}

function Assert-ThrowsLike {
    param([scriptblock]$Block, [string]$Pattern, [string]$Message)
    $threw = $false
    try {
        & $Block | Out-Null
    } catch {
        $threw = $true
        if ($_.Exception.Message -notmatch $Pattern) {
            throw "$Message (expected /$Pattern/, got: $($_.Exception.Message))"
        }
    }
    if (-not $threw) { throw "$Message (expected an error, but the call succeeded)" }
}

function Test-Case {
    param([string]$Name, [scriptblock]$Body)
    try {
        & $Body
        $script:Passed++
    } catch {
        $script:Failed++
        $script:Failures.Add("$Name -> $($_.Exception.Message)")
        Write-Host "FAIL $Name"
        Write-Host "     $($_.Exception.Message)"
    }
}

function Get-PlanTaskSet {
    param($Plan)
    $required = $Plan.required
    if ($required -is [System.Collections.IDictionary]) {
        return @($required.Keys | Where-Object { [bool]$required[$_] } | ForEach-Object { [string]$_ })
    }
    return @($required.PSObject.Properties | Where-Object { $_.Value } | ForEach-Object { $_.Name })
}

function Get-PlanPackageSet {
    param($Plan)
    return @($Plan.packages)
}

function New-TestPlanHashtable {
    $required = [ordered]@{}
    $reasons = [ordered]@{}
    foreach ($task in $script:fixedTasks) {
        $required[$task] = ($task -eq 'ci-config')
        $reasons[$task] = @("test reason for $task")
    }
    return @{
        schemaVersion = 1
        scope         = 'affected'
        commit        = $script:testCommit
        required      = $required
        packages      = @()
        reasons       = $reasons
    }
}

function Invoke-CiScript {
    param([string]$Script, [string[]]$Arguments)
    & $script:pwsh -NoProfile -File $Script @Arguments *> $null
    return $LASTEXITCODE
}

try {
    # ------------------------------------------------------------------
    # 1. 分类矩阵
    # ------------------------------------------------------------------
    $classificationCases = @(
        @{ Name = 'shared core'; Paths = @('cb_core/src/lib.rs'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos'); Packages = @(); Scope = 'affected' }
        @{ Name = 'shared crates'; Paths = @('crates/cb_core_ffi/src/lib.rs'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'abi header'; Paths = @('platforms/android/include/clipbridge_core.h'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'Cargo.toml'; Paths = @('Cargo.toml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'Cargo.lock'; Paths = @('Cargo.lock'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'cargo config'; Paths = @('.cargo/config.toml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'rustfmt policy'; Paths = @('rustfmt.toml'); Tasks = @('ci-config', 'rust'); Packages = @(); Scope = 'affected' }
        @{ Name = 'cargo deny policy'; Paths = @('deny.toml'); Tasks = @('ci-config', 'rust'); Packages = @(); Scope = 'affected' }
        @{ Name = 'editorconfig'; Paths = @('.editorconfig'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'rust toolchain pin'; Paths = @('rust-toolchain.toml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'dotnet toolchain pin'; Paths = @('global.json'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'workflow'; Paths = @('.github/workflows/ci.yml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'ci classification script'; Paths = @('scripts/ci/plan.psm1'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'ffi surface gate'; Paths = @('scripts/verify-ffi-surface.ps1'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'windows production code'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/App.xaml.cs'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'windows core library code'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Core/Services/CoreConfigBuilder.cs'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'windows test-only'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/CoreHostSmokeTests.cs'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'windows test csproj'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/ClipBridgeShell_CS.Tests.MSTest.csproj'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'windows shared project file'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Core/ClipBridgeShell_CS.Core.csproj'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows app resources'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Assets/WindowIcon.ico'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows publish profile'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Properties/PublishProfiles/win10-x64.pubxml'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows solution'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.sln'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows installer project'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe'); Scope = 'affected' }
        @{ Name = 'windows bundle project'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/Bundle.wxs'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('user-exe'); Scope = 'affected' }
        @{ Name = 'windows version props'; Paths = @('platforms/windows/ClipBridgeShell_CS/Version.props'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows installer entry'; Paths = @('scripts/build-windows-installers.ps1'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows all wrapper'; Paths = @('scripts/build-windows-all.ps1'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows version script'; Paths = @('scripts/version.ps1'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'machine msi wrapper'; Paths = @('scripts/build-windows-machine-msi.ps1'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi'); Scope = 'affected' }
        @{ Name = 'user exe wrapper'; Paths = @('scripts/build-windows-user-exe.ps1'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('user-exe'); Scope = 'affected' }
        @{ Name = 'portable wrapper'; Paths = @('scripts/build-windows-portable-zip.ps1'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('portable-zip'); Scope = 'affected' }
        @{ Name = 'windows ffi crate'; Paths = @('platforms/windows/core-ffi/src/lib.rs'); Tasks = @('ci-config', 'rust', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'android ffi crate'; Paths = @('platforms/android/core-ffi/src/lib.rs'); Tasks = @('ci-config', 'rust', 'android'); Packages = @(); Scope = 'affected' }
        @{ Name = 'macos ffi crate'; Paths = @('platforms/macos/core-ffi/src/lib.rs'); Tasks = @('ci-config', 'rust', 'macos'); Packages = @(); Scope = 'affected' }
        @{ Name = 'android shell'; Paths = @('platforms/android/ClipBridgeShellAndroid/app/build.gradle.kts'); Tasks = @('ci-config', 'android'); Packages = @(); Scope = 'affected' }
        @{ Name = 'macos shell'; Paths = @('platforms/macos/ClipBridgeShell_macOS/ClipBridge/ContentView.swift'); Tasks = @('ci-config', 'macos'); Packages = @(); Scope = 'affected' }
        @{ Name = 'android build script'; Paths = @('scripts/check-android-shell-contracts.sh'); Tasks = @('ci-config', 'android'); Packages = @(); Scope = 'affected' }
        @{ Name = 'macos build script'; Paths = @('scripts/build-macos-ffi.sh'); Tasks = @('ci-config', 'macos'); Packages = @(); Scope = 'affected' }
        @{ Name = 'msix local script'; Paths = @('scripts/build-windows-msix.ps1'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        @{ Name = 'winget local script'; Paths = @('scripts/build-windows-winget.ps1'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        @{ Name = 'store identity'; Paths = @('store/identity.example.json'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        # 专用本地渠道文件（精确路径）显式识别；不增加 MSIX job。
        @{ Name = 'msix manifest'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Package.appxmanifest'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        @{ Name = 'msix appinstaller'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Package.appinstaller'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        # 带路径的 README 先走构建消费者规则，不能被 .md 全局免检。
        @{ Name = 'windows app asset readme'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Assets/README.md'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'windows app readme'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/README.md'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'windows test readme'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/README.md'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'android readme'; Paths = @('platforms/android/ClipBridgeShellAndroid/README.md'); Tasks = @('ci-config', 'android'); Packages = @(); Scope = 'affected' }
        # 未证明非构建的路径必须保守全跑，而不是被 .md / assets / tools 白名单免检。
        @{ Name = 'scripts readme unknown'; Paths = @('scripts/README.md'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'unknown readme'; Paths = @('some/README.md'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'gitattributes'; Paths = @('.gitattributes'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'gitignore'; Paths = @('.gitignore'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'repository asset'; Paths = @('Assets/Picture/img.png'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'tools script'; Paths = @('tools/cargo-build.cmd'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'license'; Paths = @('LICENSE'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'release please config'; Paths = @('release-please-config.json'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        # 嵌套 manifest 触发依赖解析与打包。
        @{ Name = 'nested cb_core Cargo.toml'; Paths = @('cb_core/Cargo.toml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'nested crate Cargo.toml'; Paths = @('crates/cb_core_ffi/Cargo.toml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        @{ Name = 'nested platform Cargo.toml'; Paths = @('platforms/windows/core-ffi/Cargo.toml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'affected' }
        # 名字里带 msix/winget 的真实源码不能被本地渠道规则吞掉。
        @{ Name = 'msix-named shared source'; Paths = @('crates/foo/msix/evil.rs'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos'); Packages = @(); Scope = 'affected' }
        @{ Name = 'winget-named shared source'; Paths = @('crates/foo/winget/evil.rs'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos'); Packages = @(); Scope = 'affected' }
        @{ Name = 'msix-named platform file'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/msix/notes.txt'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        # 路径首尾空格是身份的一部分：不能 Trim 成已知路径（精确规则不应命中）。
        @{ Name = 'trailing space path'; Paths = @('Cargo.toml '); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'leading space path'; Paths = @(' Cargo.toml'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'local script with trailing space'; Paths = @('scripts/build-windows-msix.ps1 '); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'documentation'; Paths = @('docs/ci-architecture.md'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        @{ Name = 'documentation readme'; Paths = @('docs/README.md'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        @{ Name = 'root readme'; Paths = @('README.md'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        @{ Name = 'root chinese doc'; Paths = @('ClipBridge项目文档.md'); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
        @{ Name = 'unknown path'; Paths = @('some/unknown.bin'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages'); Packages = @('machine-msi', 'user-exe', 'portable-zip'); Scope = 'full' }
        @{ Name = 'mixed core + docs'; Paths = @('cb_core/src/lib.rs', 'docs/windows-build.md'); Tasks = @('ci-config', 'rust', 'windows', 'android', 'macos'); Packages = @(); Scope = 'affected' }
        @{ Name = 'mixed windows code + installer'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/App.xaml.cs', 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs'); Tasks = @('ci-config', 'windows', 'windows-packages'); Packages = @('machine-msi', 'user-exe'); Scope = 'affected' }
        @{ Name = 'windows test + docs'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/CoreHostSmokeTests.cs', 'docs/windows-build.md'); Tasks = @('ci-config', 'windows'); Packages = @(); Scope = 'affected' }
        @{ Name = 'empty change set'; Paths = @(); Tasks = @('ci-config'); Packages = @(); Scope = 'affected' }
    )

    foreach ($case in $classificationCases) {
        Test-Case "classification: $($case.Name)" {
            $plan = Get-CiPlanFromChangedPaths -Paths $case.Paths -Commit $script:testCommit
            $errors = @(Get-CiPlanErrors -Plan $plan)
            Assert-Equal 0 $errors.Count "plan must be valid: $($errors -join '; ')"
            Assert-Equal $case.Scope $plan.scope "scope mismatch"
            Assert-Equal $case.Tasks (Get-PlanTaskSet $plan) "required task set mismatch"
            Assert-Equal $case.Packages (Get-PlanPackageSet $plan) "package set mismatch"
            foreach ($task in $script:fixedTasks) {
                Assert-True (@($plan.reasons.$task).Count -gt 0) "reason missing for $task"
            }
        }
    }

    Test-Case 'mixed build input + docs does not mislabel false tasks as doc-only' {
        $plan = Get-CiPlanFromChangedPaths -Paths @('docs/windows-build.md', 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/CoreHostSmokeTests.cs') -Commit $script:testCommit
        Assert-Equal @('ci-config', 'windows') (Get-PlanTaskSet $plan) 'windows test + docs task set'
        Assert-True (($plan.reasons.android -join ' ') -notmatch 'documentation/non-build') 'a real build input must not produce doc-only reasons'
    }

    # 删除 / 重命名按旧路径 + 新路径共同计算影响。
    Test-Case 'rename parser keeps old and new paths' {
        $entries = @(ConvertFrom-GitDiffNameStatusZ -Text "M`0docs/a.md`0R100`0scripts/build-windows-portable-zip.ps1`0scripts/build-windows-user-exe.ps1`0D`0platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/Old.cs`0")
        Assert-Equal 3 $entries.Count 'entry count'
        Assert-Equal 'R100' $entries[1].Status 'rename status'
        Assert-Equal 'scripts/build-windows-portable-zip.ps1' $entries[1].OldPath 'rename old path'
        Assert-Equal 'scripts/build-windows-user-exe.ps1' $entries[1].Path 'rename new path'
        $paths = @($entries | ForEach-Object { if ($_.OldPath) { $_.OldPath }; if ($_.Path) { $_.Path } })
        $plan = Get-CiPlanFromChangedPaths -Paths $paths -Commit $script:testCommit
        Assert-Equal @('ci-config', 'windows', 'windows-packages') (Get-PlanTaskSet $plan) 'rename impact must include both paths'
        Assert-Equal @('user-exe', 'portable-zip') (Get-PlanPackageSet $plan) 'rename must union old and new package formats'
    }

    Test-Case 'deleted path still classifies' {
        $entries = @(ConvertFrom-GitDiffNameStatusZ -Text "D`0platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs`0")
        $plan = Get-CiPlanFromChangedPaths -Paths @($entries[0].Path) -Commit $script:testCommit
        Assert-Equal @('ci-config', 'windows', 'windows-packages') (Get-PlanTaskSet $plan) 'deleted installer must still require packages'
        Assert-Equal @('machine-msi', 'user-exe') (Get-PlanPackageSet $plan) 'deleted installer package formats'
    }

    Test-Case 'malformed diff status fails' {
        Assert-ThrowsLike { ConvertFrom-GitDiffNameStatusZ -Text "??`0some/path`0" } '无法解析' 'malformed status must fail'
    }

    Test-Case 'plan reasons are bounded for GitHub output size' {
        $paths = @(1..40 | ForEach-Object { "cb_core/src/file$_.rs" })
        $plan = Get-CiPlanFromChangedPaths -Paths $paths -Commit $script:testCommit
        Assert-True (@($plan.reasons.rust).Count -le 21) 'reasons must be capped'
        Assert-Match (@($plan.reasons.rust) -join ' ') 'more matching input reason' 'capped reasons must state the truncation'
        Assert-Equal 0 @(Get-CiPlanErrors -Plan $plan).Count 'capped plan must remain valid'
    }

    # ------------------------------------------------------------------
    # 2. plan schema / 不变量负路径
    # ------------------------------------------------------------------
    $planMutations = @(
        @{ Name = 'valid baseline'; Mutate = { param($p) }; ShouldFail = $false; Pattern = '' }
        @{ Name = 'missing reasons'; Mutate = { param($p) $p.Remove('reasons') }; ShouldFail = $true; Pattern = 'missing top-level field' }
        @{ Name = 'missing packages'; Mutate = { param($p) $p.Remove('packages') }; ShouldFail = $true; Pattern = 'missing top-level field' }
        @{ Name = 'extra top-level field'; Mutate = { param($p) $p['publish'] = @() }; ShouldFail = $true; Pattern = 'unknown top-level field' }
        @{ Name = 'unsupported schemaVersion'; Mutate = { param($p) $p.schemaVersion = 2 }; ShouldFail = $true; Pattern = 'unsupported schemaVersion' }
        @{ Name = 'string schemaVersion'; Mutate = { param($p) $p.schemaVersion = '1' }; ShouldFail = $true; Pattern = 'schemaVersion must be an integer' }
        @{ Name = 'unknown scope'; Mutate = { param($p) $p.scope = 'partial' }; ShouldFail = $true; Pattern = 'scope must be' }
        @{ Name = 'short commit'; Mutate = { param($p) $p.commit = 'abc123' }; ShouldFail = $true; Pattern = 'full lowercase commit SHA' }
        @{ Name = 'uppercase commit'; Mutate = { param($p) $p.commit = ('A' * 40) }; ShouldFail = $true; Pattern = 'full lowercase commit SHA' }
        @{ Name = 'required missing fixed task'; Mutate = { param($p) $p.required.Remove('rust') }; ShouldFail = $true; Pattern = "missing fixed task 'rust'" }
        @{ Name = 'required unknown task'; Mutate = { param($p) $p.required['publish'] = $true }; ShouldFail = $true; Pattern = "unknown task 'publish'" }
        @{ Name = 'required non-boolean'; Mutate = { param($p) $p.required.rust = 'true' }; ShouldFail = $true; Pattern = 'must be a boolean' }
        @{ Name = 'ci-config false'; Mutate = { param($p) $p.required.'ci-config' = $false }; ShouldFail = $true; Pattern = 'must always be true' }
        @{ Name = 'packages set without windows-packages'; Mutate = { param($p) $p.packages = @('machine-msi') }; ShouldFail = $true; Pattern = 'if and only if' }
        @{ Name = 'windows-packages without packages'; Mutate = { param($p) $p.required.'windows-packages' = $true }; ShouldFail = $true; Pattern = 'if and only if' }
        @{ Name = 'packages without windows'; Mutate = { param($p) $p.packages = @('machine-msi'); $p.required.'windows-packages' = $true; $p.required.windows = $false }; ShouldFail = $true; Pattern = 'requires required' }
        @{ Name = 'duplicate package'; Mutate = { param($p) $p.packages = @('machine-msi', 'machine-msi'); $p.required.'windows-packages' = $true; $p.required.windows = $true }; ShouldFail = $true; Pattern = 'duplicated' }
        @{ Name = 'msix package'; Mutate = { param($p) $p.packages = @('msix'); $p.required.'windows-packages' = $true; $p.required.windows = $true }; ShouldFail = $true; Pattern = 'outside the supported GitHub scope' }
        @{ Name = 'storeupload package'; Mutate = { param($p) $p.packages = @('storeupload'); $p.required.'windows-packages' = $true; $p.required.windows = $true }; ShouldFail = $true; Pattern = 'outside the supported GitHub scope' }
        @{ Name = 'winget package'; Mutate = { param($p) $p.packages = @('winget'); $p.required.'windows-packages' = $true; $p.required.windows = $true }; ShouldFail = $true; Pattern = 'outside the supported GitHub scope' }
        @{ Name = 'reasons missing task'; Mutate = { param($p) $p.reasons.Remove('macos') }; ShouldFail = $true; Pattern = "missing fixed task 'macos'" }
        @{ Name = 'reasons empty array'; Mutate = { param($p) $p.reasons.rust = @() }; ShouldFail = $true; Pattern = 'must not be empty' }
        @{ Name = 'reasons empty string'; Mutate = { param($p) $p.reasons.rust = @('') }; ShouldFail = $true; Pattern = 'empty string' }
        @{ Name = 'reasons control character'; Mutate = { param($p) $p.reasons.rust = @("bad`nvalue") }; ShouldFail = $true; Pattern = 'control characters' }
        @{ Name = 'full scope with false task'; Mutate = { param($p) $p.scope = 'full'; foreach ($t in @('ci-config','rust','windows','android','macos','windows-packages')) { $p.required[$t] = $true }; $p.packages = @('machine-msi','user-exe','portable-zip'); $p.required.'windows-packages' = $true; $p.required.'macos' = $false }; ShouldFail = $true; Pattern = "requires required\['macos'\]" }
        @{ Name = 'full scope with partial packages'; Mutate = { param($p) $p.scope = 'full'; foreach ($t in $script:fixedTasks) { $p.required[$t] = $true }; $p.packages = @('machine-msi','user-exe') }; ShouldFail = $true; Pattern = "requires packages to include 'portable-zip'" }
    )

    foreach ($mutation in $planMutations) {
        Test-Case "plan validation: $($mutation.Name)" {
            $plan = New-TestPlanHashtable
            & $mutation.Mutate $plan
            if ($mutation.ShouldFail) {
                Assert-ThrowsLike { Assert-CiPlan -Plan $plan } $mutation.Pattern 'expected invalid plan'
            } else {
                $errors = @(Get-CiPlanErrors -Plan $plan)
                Assert-Equal 0 $errors.Count "valid plan rejected: $($errors -join '; ')"
            }
        }
    }

    Test-Case 'plan JSON: valid round-trip' {
        $plan = Get-CiPlanFromChangedPaths -Paths @('Cargo.lock') -Commit $script:testCommit
        $json = ConvertTo-CiPlanJson -Plan $plan
        $parsed = ConvertFrom-CiPlanJson -Json $json
        Assert-Equal $plan.commit $parsed.commit 'commit round-trip'
        Assert-Equal @('machine-msi', 'user-exe', 'portable-zip') @($parsed.packages) 'packages round-trip'
    }

    foreach ($jsonCase in @(
            @{ Name = 'plain text'; Json = 'not json'; Pattern = 'invalid plan JSON' },
            @{ Name = 'json array'; Json = '[]'; Pattern = 'plan must be a JSON object' },
            @{ Name = 'empty object'; Json = '{}'; Pattern = 'plan must be a JSON object|missing top-level field' },
            @{ Name = 'empty string'; Json = ''; Pattern = 'is empty' }
        )) {
        Test-Case "plan JSON negative: $($jsonCase.Name)" {
            Assert-ThrowsLike { ConvertFrom-CiPlanJson -Json $jsonCase.Json } $jsonCase.Pattern 'invalid JSON must fail'
        }
    }

    # ------------------------------------------------------------------
    # 3. get-plan 脚本（真实进程、真实 git）
    # ------------------------------------------------------------------
    Test-Case 'get-plan: changed paths output file is valid' {
        $out = Join-Path $script:tempRoot 'get-plan-changed.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-ChangedPaths', 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/CoreHostSmokeTests.cs', '-OutFile', $out)
        Assert-Equal 0 $code 'get-plan exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal @('ci-config', 'windows') (Get-PlanTaskSet $plan) 'test-only plan task set'
        Assert-Equal 0 @($plan.packages).Count 'test-only plan must not package'
    }

    Test-Case 'get-plan: full scope' {
        $out = Join-Path $script:tempRoot 'get-plan-full.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-Scope', 'full', '-OutFile', $out)
        Assert-Equal 0 $code 'get-plan full exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal 'full' $plan.scope 'full scope'
        Assert-Equal @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages') (Get-PlanTaskSet $plan) 'full tasks'
        Assert-Equal @('machine-msi', 'user-exe', 'portable-zip') @($plan.packages) 'full packages'
    }

    Test-Case 'get-plan: missing change input fails' {
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit)
        Assert-True ($code -ne 0) 'missing input must not produce a green plan'
    }

    Test-Case 'get-plan: invalid commit fails' {
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', 'not-a-sha', '-ChangedPaths', 'cb_core/src/lib.rs')
        Assert-True ($code -ne 0) 'invalid commit must fail'
    }

    Test-Case 'get-plan: zero diff base falls back to full' {
        $out = Join-Path $script:tempRoot 'get-plan-zerobase.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-DiffBase', ('0' * 40), '-DiffHead', $script:testCommit, '-OutFile', $out)
        Assert-Equal 0 $code 'diff fallback must still produce a valid plan'
        $plan = Read-CiPlan -Path $out
        Assert-Equal 'full' $plan.scope 'zero base must fall back to full'
    }

    Test-Case 'get-plan: invalid diff base falls back to full' {
        $out = Join-Path $script:tempRoot 'get-plan-badbase.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-DiffBase', ('b' * 40), '-DiffHead', $script:testCommit, '-OutFile', $out)
        Assert-Equal 0 $code 'bad base must still produce a valid plan'
        $plan = Read-CiPlan -Path $out
        Assert-Equal 'full' $plan.scope 'bad base must fall back to full'
        Assert-Match ($plan.reasons.windows -join ' ') 'diff unavailable' 'reason must explain the diff fallback'
    }

    Test-Case 'get-plan: Unknown path falls back to full' {
        $out = Join-Path $script:tempRoot 'get-plan-unknown.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-ChangedPaths', 'mystery/file.bin', '-OutFile', $out)
        Assert-Equal 0 $code 'unknown path plan exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal 'full' $plan.scope 'unknown path scope'
    }

    Test-Case 'get-plan: VerifyHead rejects forged commit' {
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', ('b' * 40), '-ChangedPaths', 'cb_core/src/lib.rs', '-VerifyHead')
        Assert-True ($code -ne 0) 'VerifyHead must reject a commit that is not HEAD'
    }

    Test-Case 'get-plan: changed files file input' {
        $file = Join-Path $script:tempRoot 'changed-files.txt'
        [System.IO.File]::WriteAllText($file, "cb_core/src/lib.rs`nplatforms/android/include/clipbridge_core.h`n")
        $out = Join-Path $script:tempRoot 'get-plan-file.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-ChangedFilesFile', $file, '-OutFile', $out)
        Assert-Equal 0 $code 'changed-files file exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages') (Get-PlanTaskSet $plan) 'changed-files task set'
        Assert-Equal @('machine-msi', 'user-exe', 'portable-zip') (Get-PlanPackageSet $plan) 'shared header triggers full package verification'
    }

    Test-Case 'get-plan: changed-files file does not trim path identity' {
        $file = Join-Path $script:tempRoot 'changed-files-space.txt'
        [System.IO.File]::WriteAllText($file, "Cargo.toml `n")
        $out = Join-Path $script:tempRoot 'get-plan-space.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-ChangedFilesFile', $file, '-OutFile', $out)
        Assert-Equal 0 $code 'trailing-space plan exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal 'full' $plan.scope 'trailing-space path must stay unclassified and force full'
    }

    Test-Case 'git diff parser integration (HEAD~1..HEAD)' {
        $parent = (& git -C $repoRoot rev-parse HEAD~1 2>$null)
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($parent)) { return } # 单提交仓库：合法跳过
        $head = (& git -C $repoRoot rev-parse HEAD).Trim()
        $paths = @(Get-CiChangedPaths -BaseSha $parent.Trim() -HeadSha $head -RepoRoot $repoRoot)
        Assert-True ($paths.Count -ge 1) 'rename/name-status parser should see at least one path'
        foreach ($path in $paths) {
            Assert-True ($path -notmatch "`0") 'parsed paths must not contain NUL'
        }
        $plan = Get-CiPlanFromChangedPaths -Paths $paths -Commit $head
        Assert-Equal 0 @(Get-CiPlanErrors -Plan $plan).Count 'real diff plan must be valid'
    }

    # ------------------------------------------------------------------
    # 4. assert-result（真实进程）
    # ------------------------------------------------------------------
    $docPlanPath = Join-Path $script:tempRoot 'assert-doc-plan.json'
    $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-ChangedPaths', 'docs/ci-architecture.md', '-OutFile', $docPlanPath)
    Assert-Equal 0 $code 'doc plan setup'

    $fullPlanPath = Join-Path $script:tempRoot 'assert-full-plan.json'
    $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-Scope', 'full', '-OutFile', $fullPlanPath)
    Assert-Equal 0 $code 'full plan setup'

    function New-NeedsJson {
        param([hashtable]$Results, [hashtable]$Outputs = @{})
        $needs = [ordered]@{ changes = [ordered]@{ result = 'success'; outputs = @{ plan = 'irrelevant' } } }
        foreach ($task in $script:fixedTasks) {
            $result = if ($Results.ContainsKey($task)) { $Results[$task] } else { 'skipped' }
            $needs[$task] = [ordered]@{ result = $result }
        }
        return ($needs | ConvertTo-Json -Depth 10)
    }

    function Invoke-AssertResult {
        param([string]$PlanPath, [string]$NeedsJson, [string]$SummaryPath, [string]$ExpectedCommit = $script:testCommit)
        $needsPath = Join-Path $script:tempRoot ('needs-' + [guid]::NewGuid().ToString('N') + '.json')
        [System.IO.File]::WriteAllText($needsPath, $NeedsJson)
        if (-not $SummaryPath) { $SummaryPath = Join-Path $script:tempRoot ('summary-' + [guid]::NewGuid().ToString('N') + '.md') }
        $args = @('-PlanFile', $PlanPath, '-NeedsFile', $needsPath, '-ExpectedCommit', $ExpectedCommit, '-SummaryFile', $SummaryPath)
        return Invoke-CiScript (Join-Path $PSScriptRoot 'assert-result.ps1') $args
    }

    Test-Case 'assert-result: documentation-only plan allows skipped optional jobs' {
        $summary = Join-Path $script:tempRoot 'summary-pass.md'
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson (New-NeedsJson @{ 'ci-config' = 'success' }) -SummaryPath $summary
        Assert-Equal 0 $code 'doc-only run must pass'
        $text = [System.IO.File]::ReadAllText($summary)
        Assert-Match $text 'CI result: PASS' 'summary must state PASS'
    }

    Test-Case 'assert-result: required task skipped fails' {
        $plan = Read-CiPlan -Path $fullPlanPath
        $results = @{}
        foreach ($task in $script:fixedTasks) { $results[$task] = 'success' }
        $results['windows'] = 'skipped'
        $code = Invoke-AssertResult -PlanPath $fullPlanPath -NeedsJson (New-NeedsJson $results) -SummaryPath $null
        Assert-True ($code -ne 0) 'required skipped must fail'
    }

    Test-Case 'assert-result: required task failure fails' {
        $results = @{}
        foreach ($task in $script:fixedTasks) { $results[$task] = 'success' }
        $results['rust'] = 'failure'
        $code = Invoke-AssertResult -PlanPath $fullPlanPath -NeedsJson (New-NeedsJson $results) -SummaryPath $null
        Assert-True ($code -ne 0) 'required failure must fail'
    }

    Test-Case 'assert-result: required task cancelled fails' {
        $results = @{}
        foreach ($task in $script:fixedTasks) { $results[$task] = 'success' }
        $results['android'] = 'cancelled'
        $code = Invoke-AssertResult -PlanPath $fullPlanPath -NeedsJson (New-NeedsJson $results) -SummaryPath $null
        Assert-True ($code -ne 0) 'required cancelled must fail'
    }

    Test-Case 'assert-result: optional task failure fails' {
        $results = @{ 'ci-config' = 'success'; 'windows' = 'failure' }
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson (New-NeedsJson $results) -SummaryPath $null
        Assert-True ($code -ne 0) 'unexpectedly executed failed task must fail'
    }

    Test-Case 'assert-result: changes failure fails' {
        $needs = New-NeedsJson @{ 'ci-config' = 'skipped' }
        $needs = $needs.Replace('"result": "success"', '"result": "failure"')
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson $needs -SummaryPath $null
        Assert-True ($code -ne 0) 'classification failure must fail'
    }

    Test-Case 'assert-result: missing needs job fails' {
        $needs = New-NeedsJson @{ 'ci-config' = 'success' }
        $obj = $needs | ConvertFrom-Json
        $obj.PSObject.Properties.Remove('macos')
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson ($obj | ConvertTo-Json -Depth 10) -SummaryPath $null
        Assert-True ($code -ne 0) 'missing needs entry must fail'
    }

    Test-Case 'assert-result: extra needs job fails' {
        $needs = New-NeedsJson @{ 'ci-config' = 'success' }
        $obj = $needs | ConvertFrom-Json
        $obj | Add-Member -NotePropertyName 'legacy-job' -NotePropertyValue ([pscustomobject]@{ result = 'success' })
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson ($obj | ConvertTo-Json -Depth 10) -SummaryPath $null
        Assert-True ($code -ne 0) 'extra needs job must fail'
    }

    Test-Case 'assert-result: invalid plan JSON fails' {
        $badPlan = Join-Path $script:tempRoot 'bad-plan.json'
        [System.IO.File]::WriteAllText($badPlan, '{ not json ')
        $code = Invoke-AssertResult -PlanPath $badPlan -NeedsJson (New-NeedsJson @{ 'ci-config' = 'success' }) -SummaryPath $null
        Assert-True ($code -ne 0) 'invalid plan must fail'
    }

    Test-Case 'assert-result: commit mismatch fails' {
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson (New-NeedsJson @{ 'ci-config' = 'success' }) -SummaryPath $null -ExpectedCommit ('c' * 40)
        Assert-True ($code -ne 0) 'plan commit must match checked out SHA'
    }

    Test-Case 'assert-result: full plan all success passes' {
        $results = @{}
        foreach ($task in $script:fixedTasks) { $results[$task] = 'success' }
        $code = Invoke-AssertResult -PlanPath $fullPlanPath -NeedsJson (New-NeedsJson $results) -SummaryPath (Join-Path $script:tempRoot 'summary-full.md')
        Assert-Equal 0 $code 'full green plan must pass'
    }

    Test-Case 'assert-result: full plan with skipped package job fails' {
        $results = @{}
        foreach ($task in $script:fixedTasks) { $results[$task] = 'success' }
        $results['windows-packages'] = 'skipped'
        $code = Invoke-AssertResult -PlanPath $fullPlanPath -NeedsJson (New-NeedsJson $results) -SummaryPath $null
        Assert-True ($code -ne 0) 'skipped package job must fail a full plan'
    }

    # ------------------------------------------------------------------
    # 5. workflow 结构契约
    # ------------------------------------------------------------------
    $workflowPath = Join-Path $repoRoot '.github/workflows/ci.yml'
    $workflowContent = [System.IO.File]::ReadAllText($workflowPath)

    Test-Case 'workflow contract: real ci.yml passes' {
        $result = Test-CiWorkflowContract -Content $workflowContent -Path '.github/workflows/ci.yml'
        Assert-Equal 0 @($result.Errors).Count ("workflow contract errors: " + (@($result.Errors) -join '; '))
        $expectedJobs = @('changes', 'ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages', 'ci-result') | Sort-Object
        $actualJobs = @($result.Jobs) | Sort-Object
        Assert-Equal $expectedJobs $actualJobs 'workflow job set'
    }

    Test-Case 'workflow embedded scripts: extraction and PowerShell AST' {
        $blocks = @(Get-CiWorkflowRunBlocks -Content $workflowContent)
        $pwshBlocks = @($blocks | Where-Object { $_.Shell -eq 'pwsh' })
        $bashBlocks = @($blocks | Where-Object { $_.Shell -eq 'bash' })
        Assert-True ($pwshBlocks.Count -ge 3) 'expected embedded pwsh blocks (changes/check-config/package/result)'
        Assert-True ($bashBlocks.Count -ge 4) 'expected embedded bash blocks (range/actionlint/android/macos)'
        foreach ($block in $pwshBlocks) {
            $tokens = $null
            $parseErrors = $null
            [System.Management.Automation.Language.Parser]::ParseInput([string]$block.Text, "ci.yml:$($block.Line)", [ref]$tokens, [ref]$parseErrors) | Out-Null
            Assert-Equal 0 @($parseErrors).Count ("embedded pwsh syntax at line $($block.Line): " + (@($parseErrors | ForEach-Object { $_.Message }) -join '; '))
        }
    }

    Test-Case 'workflow embedded scripts: malformed PowerShell is detectable' {
        $fixture = @(
            'jobs:',
            '  a:',
            '    runs-on: ubuntu-24.04',
            '    steps:',
            '      - name: broken',
            '        shell: pwsh',
            '        run: |',
            '          if (',
            '      - name: bash',
            '        shell: bash',
            '        run: |',
            '          set -euo pipefail',
            '          echo ok'
        ) -join "`n"
        $blocks = @(Get-CiWorkflowRunBlocks -Content $fixture)
        Assert-Equal 2 $blocks.Count 'two embedded blocks expected'
        Assert-Equal 'pwsh' $blocks[0].Shell 'first block shell'
        Assert-Equal 'bash' $blocks[1].Shell 'second block shell'
        $tokens = $null
        $parseErrors = $null
        [System.Management.Automation.Language.Parser]::ParseInput([string]$blocks[0].Text, 'fixture', [ref]$tokens, [ref]$parseErrors) | Out-Null
        Assert-True (@($parseErrors).Count -gt 0) 'broken embedded PowerShell must be rejected'
    }

    $workflowMutations = @(
        @{ Name = 'unpinned action'; Old = 'uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0'; New = 'uses: actions/checkout@v4'; Pattern = 'pinned to a full commit SHA' }
        @{ Name = 'wrong pinned sha'; Old = 'uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0'; New = 'uses: actions/checkout@' + ('0' * 40) + ' # v4.4.0'; Pattern = 'must use SHA' }
        @{ Name = 'rust condition bypass'; Old = "required['rust']"; New = 'true'; Pattern = "plan.required\['rust'\]" }
        @{ Name = 'missing timeout'; Old = '    timeout-minutes: 30'; New = '    # timeout removed'; Pattern = 'missing timeout-minutes' }
        @{ Name = 'extra job'; Old = '  ci-result:'; New = "  legacy-job:`n    runs-on: ubuntu-24.04`n    timeout-minutes: 10`n  ci-result:"; Pattern = "unknown job 'legacy-job'" }
        @{ Name = 'missing ci-result need'; Old = 'needs: [ changes, ci-config, rust, windows, android, macos, windows-packages ]'; New = 'needs: [ changes, ci-config, rust, windows, android, macos ]'; Pattern = "ci-result needs must include 'windows-packages'" }
        @{ Name = 'continue-on-error'; Old = '    timeout-minutes: 30'; New = "    timeout-minutes: 30`n    continue-on-error: true"; Pattern = 'continue-on-error' }
        @{ Name = 'cancel main'; Old = "cancel-in-progress: `${{ github.event_name == 'pull_request' }}"; New = 'cancel-in-progress: true'; Pattern = 'concurrency must cancel' }
        @{ Name = 'no workflow_dispatch'; Old = "  workflow_dispatch:`n"; New = ''; Pattern = 'workflow_dispatch' }
        @{ Name = 'secrets reference'; Old = '      shell: pwsh'; New = "      shell: pwsh`n      env:`n        TOKEN: `${{ secrets.TOKEN }}"; Pattern = 'secrets' }
        @{ Name = 'winget command'; Old = 'run: ./scripts/ci/test-plan.ps1'; New = 'run: winget validate dist/winget'; Pattern = 'forbidden local-channel command' }
        @{ Name = 'missing Android SDK install'; Old = '          sdkmanager --install \'; New = '          echo skipped-sdk-install'; Pattern = 'Android job must install' }
        @{ Name = 'setup-android implicit legacy default'; Old = "        with:`n          packages: ''"; New = '        # packages omitted'; Pattern = 'setup-android must explicitly use packages' }
        @{ Name = 'setup-android removed tools package'; Old = "          packages: ''"; New = "          packages: 'tools platform-tools'"; Pattern = 'setup-android must explicitly use packages' }
        @{ Name = 'missing windows report suite'; Old = 'run: ./scripts/ci/test-windows-report.ps1'; New = 'run: ./scripts/ci/test-windows-report-missing.ps1'; Pattern = 'windows report contract tests' }
        @{ Name = 'missing fixed release suite'; Old = "'test-release-candidate.ps1'"; New = "'test-release-candidate-missing.ps1'"; Pattern = "fixed release contract suite 'test-release-candidate\.ps1'" }
        @{ Name = 'missing release fail-closed guard'; Old = 'missing required contract suite'; New = 'missing suite'; Pattern = 'fail when a required suite is missing' }
    )

    foreach ($mutation in $workflowMutations) {
        Test-Case "workflow contract: $($mutation.Name)" {
            Assert-True ($workflowContent.Contains($mutation.Old)) "fixture text not found for mutation: $($mutation.Name)"
            $mutated = $workflowContent.Replace($mutation.Old, $mutation.New)
            $result = Test-CiWorkflowContract -Content $mutated -Path '.github/workflows/ci.yml'
            $messages = @($result.Errors) -join '; '
            Assert-Match $messages $mutation.Pattern 'expected contract failure'
        }
    }

    # ------------------------------------------------------------------
    # 6. 文档本地链接
    # ------------------------------------------------------------------
    Test-Case 'doc links: only local targets of supplied docs are checked' {
        $root = Join-Path $script:tempRoot 'docs-root'
        New-Item -ItemType Directory -Force -Path (Join-Path $root 'docs/sub') | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $root 'docs/b.md'), "# b`nline2`nline3`n")
        [System.IO.File]::WriteAllText((Join-Path $root 'docs/img.png'), 'x')
        [System.IO.File]::WriteAllText((Join-Path $root 'outside.md'), 'outside')
        $doc = @(
            '# doc',
            '[ok](b.md)',
            '[angle](<b.md>)',
            '[line](b.md#L2-L3)',
            '[bad-line](b.md#L99)',
            '[missing](nope.md)',
            '[escape](../../outside.md)',
            '[dir](sub/)',
            '[external](https://example.com/x)',
            '[anchor](#section)',
            '[image](img.png)',
            '```',
            '[inside-code](missing-in-code.md)',
            '```'
        ) -join "`n"
        [System.IO.File]::WriteAllText((Join-Path $root 'docs/a.md'), $doc)
        $result = Test-CiDocLinks -RepoRoot $root -DocPaths @('docs/a.md')
        $errors = @($result.Errors) -join '; '
        Assert-Match $errors 'nope\.md' 'missing local file must fail'
        Assert-Match $errors 'L99' 'out-of-range line target must fail'
        Assert-Match $errors 'escapes the repository' 'repo escape must fail'
        Assert-True ($errors -notmatch 'missing-in-code') 'fenced code links must be ignored'
        Assert-True ($errors -notmatch 'example\.com') 'external URLs must be ignored'
        Assert-True ($result.CheckedFiles -eq 1) 'one doc checked'
        Assert-True ($result.CheckedLinks -ge 4) 'valid links counted'
    }

    Test-Case 'doc links: clean doc has no errors' {
        $root = Join-Path $script:tempRoot 'docs-clean'
        New-Item -ItemType Directory -Force -Path (Join-Path $root 'docs') | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $root 'docs/x.md'), "# x`n[y](y.md#L1)`n")
        [System.IO.File]::WriteAllText((Join-Path $root 'docs/y.md'), "# y`n")
        $result = Test-CiDocLinks -RepoRoot $root -DocPaths @('docs/x.md')
        Assert-Equal 0 @($result.Errors).Count ('unexpected doc errors: ' + (@($result.Errors) -join '; '))
    }

    # ------------------------------------------------------------------
    # 7. 工具 pin 与 setup-wix
    # ------------------------------------------------------------------
    Test-Case 'tool pins: actionlint 1.7.12 and WiX 3.14.1 digests are pinned' {
        $pins = Get-CiToolPins
        Assert-Equal '1.7.12' $pins.ActionlintVersion 'actionlint version'
        Assert-Equal '8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8' $pins.ActionlintLinuxAmd64Sha256 'actionlint linux sha'
        Assert-Equal '6e7241b51e6817ea6a047693d8e6fed13b31819c9a0dd6c5a726e1592d22f6e9' $pins.ActionlintWindowsAmd64Sha256 'actionlint windows sha'
        Assert-Equal '3.14.1' $pins.WixVersion 'WiX version'
        Assert-Equal '6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31' $pins.WixZipSha256 'WiX sha'
    }

    Test-Case 'tool pins: repository toolchain files match the single source' {
        $pins = Get-CiToolPins
        $global = [System.IO.File]::ReadAllText((Join-Path $repoRoot 'global.json')) | ConvertFrom-Json -Depth 8
        Assert-Equal $pins.DotnetSdkVersion ([string]$global.sdk.version) '.NET SDK pin'
        Assert-Equal 'disable' ([string]$global.sdk.rollForward) '.NET SDK rollForward must be exact'
        $rustToolchain = [System.IO.File]::ReadAllText((Join-Path $repoRoot 'rust-toolchain.toml'))
        Assert-Match $rustToolchain ('channel\s*=\s*"' + [regex]::Escape($pins.RustToolchainVersion) + '"') 'Rust toolchain pin'
    }

    Test-Case 'tool pins: every action sha is a unique 40-hex commit' {
        $pins = Get-CiActionPins
        $seen = @{}
        foreach ($repo in $pins.Keys) {
            $sha = [string]$pins[$repo].Sha
            Assert-True ($sha -match '^[0-9a-f]{40}$') "invalid SHA for $repo"
            Assert-True (-not $seen.ContainsKey($sha)) "duplicate SHA for $repo"
            $seen[$sha] = $true
            Assert-True ([string]$pins[$repo].Version -match '^v[0-9]+\.[0-9]+\.[0-9]+$') "invalid version comment for $repo"
        }
    }

    Test-Case 'setup-wix: wrong zip checksum fails' {
        $bad = Join-Path $script:tempRoot 'not-wix.zip'
        [System.IO.File]::WriteAllText($bad, 'this is not a WiX zip')
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'setup-wix.ps1') @('-VerifyZipPath', $bad)
        Assert-True ($code -ne 0) 'wrong checksum must fail'
    }

    Test-Case 'setup-wix: missing zip fails' {
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'setup-wix.ps1') @('-VerifyZipPath', (Join-Path $script:tempRoot 'does-not-exist.zip'))
        Assert-True ($code -ne 0) 'missing zip must fail'
    }

    Test-Case 'setup-wix: complete existing directory is reused without download' {
        $dest = Join-Path $script:tempRoot 'wix-reuse'
        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        foreach ($name in @('candle.exe', 'light.exe', 'heat.exe', 'WixTasks.dll', 'wix.targets')) {
            [System.IO.File]::WriteAllText((Join-Path $dest $name), '')
        }
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'setup-wix.ps1') @('-DestDir', $dest)
        Assert-Equal 0 $code 'existing tool directory should be reused'
    }
} finally {
    Remove-Item -LiteralPath $script:tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}

$total = $script:Passed + $script:Failed
Write-Host ''
Write-Host "plan contract tests: $($script:Passed)/$total passed, $($script:Failed) failed"
if ($script:Failed -gt 0) {
    Write-Host ''
    Write-Host 'Failures:'
    foreach ($failure in $script:Failures) { Write-Host " - $failure" }
    exit 1
}
exit 0
