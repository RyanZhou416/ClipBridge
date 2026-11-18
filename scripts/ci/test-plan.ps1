#requires -version 7.0
<#
.SYNOPSIS
    plan / 分类 / 汇总 / 工作流契约的表驱动回归测试（无新框架，纯 PowerShell）。

.DESCRIPTION
    覆盖 schemaVersion=2 的日常 CI 契约：
      - 分类矩阵：共享 Rust/ABI、单平台 FFI/头文件、工具链、平台生产/测试/资源、
        打包与发布输入、CI 公共脚本、文档、本地渠道、未知路径与回退
      - plan schema 严格校验（无 packages 字段；拒绝 legacy schemaVersion=1、
        旧 packages 字段和旧 windows-packages 任务名）
      - needs 实际结果核对：必跑失败/取消/未运行，额外任务与缺失 needs
      - 固定 7 job 工作流契约，以及防止日常 CI 重新引入安装器构建/发布路径的变异用例
      - actionlint/WiX 工具 pin、第三方 Action SHA pin、文档本地链接与安装器 XML 语法
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
$script:fixedTasks = @('ci-config', 'rust', 'windows', 'android', 'macos')
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

function Get-PlanTopLevelKeys {
    param($Plan)
    if ($Plan -is [System.Collections.IDictionary]) { return @($Plan.Keys | ForEach-Object { [string]$_ }) }
    return @($Plan.PSObject.Properties | ForEach-Object { $_.Name })
}

function Assert-CleanSchema2Plan {
    param($Plan, [string]$Label)
    $keys = @(Get-PlanTopLevelKeys $Plan)
    Assert-True ($keys -notcontains 'packages') "$Label must not contain the removed packages field"
    Assert-Equal @('commit', 'reasons', 'required', 'schemaVersion', 'scope') ($keys | Sort-Object) "$Label top-level fields"
    Assert-Equal 2 $Plan.schemaVersion "$Label schemaVersion"
    $requiredKeys = @(if ($Plan.required -is [System.Collections.IDictionary]) { $Plan.required.Keys } else { $Plan.required.PSObject.Properties | ForEach-Object { $_.Name } })
    Assert-Equal ($script:fixedTasks | Sort-Object) ($requiredKeys | Sort-Object) "$Label required keys"
}

function New-TestPlanHashtable {
    $required = [ordered]@{}
    $reasons = [ordered]@{}
    foreach ($task in $script:fixedTasks) {
        $required[$task] = ($task -eq 'ci-config')
        $reasons[$task] = @("test reason for $task")
    }
    return @{
        schemaVersion = 2
        scope         = 'affected'
        commit        = $script:testCommit
        required      = $required
        reasons       = $reasons
    }
}

function Invoke-CiScript {
    param([string]$Script, [string[]]$Arguments)
    & $script:pwsh -NoProfile -File $Script @Arguments *> $null
    return $LASTEXITCODE
}

function Invoke-CiScriptCaptured {
    param([string]$Script, [string[]]$Arguments)
    $raw = & $script:pwsh -NoProfile -File $Script @Arguments 2>&1
    $code = $LASTEXITCODE
    $text = ($raw | ForEach-Object { [string]$_ }) -join "`n"
    return [pscustomobject]@{ ExitCode = $code; Text = $text }
}

$cfg = @('ci-config')
$cfgRust = @('ci-config', 'rust')
$cfgWin = @('ci-config', 'windows')
$cfgAndroid = @('ci-config', 'android')
$cfgMac = @('ci-config', 'macos')
$cfgRustWin = @('ci-config', 'rust', 'windows')
$cfgRustAndroid = @('ci-config', 'rust', 'android')
$cfgRustMac = @('ci-config', 'rust', 'macos')
$cfgRustAndroidMac = @('ci-config', 'rust', 'android', 'macos')
$cfgWinAndroid = @('ci-config', 'windows', 'android')
$allTasks = @('ci-config', 'rust', 'windows', 'android', 'macos')

try {
    # ------------------------------------------------------------------
    # 1. 分类矩阵
    # ------------------------------------------------------------------
    $classificationCases = @(
        # ---- CI 自身与公共契约 ----
        @{ Name = 'daily workflow'; Paths = @('.github/workflows/ci.yml'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'release workflow orchestration'; Paths = @('.github/workflows/release-windows.yml'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'unknown workflow'; Paths = @('.github/workflows/other.yml'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'unknown github metadata'; Paths = @('.github/dependabot.yml'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'classifier module'; Paths = @('scripts/ci/plan.psm1'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'get-plan cli'; Paths = @('scripts/ci/get-plan.ps1'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'assert-result cli'; Paths = @('scripts/ci/assert-result.ps1'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'check-config cli'; Paths = @('scripts/ci/check-config.ps1'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'public plan tests'; Paths = @('scripts/ci/test-plan.ps1'); Tasks = $allTasks; Scope = 'full' }
        # ---- 平台 CI 入口 ----
        @{ Name = 'rust ci entry'; Paths = @('scripts/ci/rust.ps1'); Tasks = $cfgRust; Scope = 'affected' }
        @{ Name = 'rust report module'; Paths = @('scripts/ci/rust-report.psm1'); Tasks = $cfgRust; Scope = 'affected' }
        @{ Name = 'ffi surface gate'; Paths = @('scripts/verify-ffi-surface.ps1'); Tasks = $cfgRust; Scope = 'affected' }
        @{ Name = 'windows ci entry'; Paths = @('scripts/ci/windows.ps1'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows report module'; Paths = @('scripts/ci/windows-report.psm1'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'android ci entry'; Paths = @('scripts/ci/android.sh'); Tasks = $cfgAndroid; Scope = 'affected' }
        @{ Name = 'android apk verifier'; Paths = @('scripts/ci/android/verify_apk.py'); Tasks = $cfgAndroid; Scope = 'affected' }
        @{ Name = 'android policy verifier'; Paths = @('scripts/ci/android/osv_policy.py'); Tasks = $cfgAndroid; Scope = 'affected' }
        @{ Name = 'macos ci entry'; Paths = @('scripts/ci/macos.sh'); Tasks = $cfgMac; Scope = 'affected' }
        @{ Name = 'macos probe'; Paths = @('scripts/ci/macos/probe_ffi.c'); Tasks = $cfgMac; Scope = 'affected' }
        @{ Name = 'macos xcode list validator'; Paths = @('scripts/ci/macos/validate_xcodebuild_list.py'); Tasks = $cfgMac; Scope = 'affected' }
        @{ Name = 'macos dylib verifier'; Paths = @('scripts/ci/macos/verify_embedded_dylib.sh'); Tasks = $cfgMac; Scope = 'affected' }
        # ---- 平台契约夹具 / 报告夹具：只跑恒跑的 ci-config ----
        @{ Name = 'android contract fixture'; Paths = @('scripts/ci/android/tests/run-tests.sh'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'android policy fixture'; Paths = @('scripts/ci/android/tests/test_policy_and_apk.py'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'macos contract fixture'; Paths = @('scripts/ci/macos/tests/run-tests.sh'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'macos xcodebuild-list fixture'; Paths = @('scripts/ci/macos/tests/fixtures/xcodebuild-list.json'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'rust report fixture'; Paths = @('scripts/ci/test-rust-report.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'windows report fixture'; Paths = @('scripts/ci/test-windows-report.ps1'); Tasks = $cfg; Scope = 'affected' }
        # ---- 纯打包 / 发布输入：日常 CI 不构建安装器 ----
        @{ Name = 'wix bootstrap script'; Paths = @('scripts/ci/setup-wix.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'signing script'; Paths = @('scripts/ci/sign-windows.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'packaging module'; Paths = @('scripts/ci/ClipBridge.Packaging.psm1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'packaging contract tests'; Paths = @('scripts/ci/test-packaging.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'release contract tests'; Paths = @('scripts/ci/test-release-model.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'release orchestration module'; Paths = @('scripts/release/ReleaseModel.psm1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'windows installer entry'; Paths = @('scripts/build-windows-installers.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'windows all wrapper'; Paths = @('scripts/build-windows-all.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'machine msi wrapper'; Paths = @('scripts/build-windows-machine-msi.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'generic msi wrapper'; Paths = @('scripts/build-windows-msi.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'user exe wrapper'; Paths = @('scripts/build-windows-user-exe.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'portable zip wrapper'; Paths = @('scripts/build-windows-portable-zip.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'installer product wxs'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'installer wixproj'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/ClipBridgeShell_CS.Installer.wixproj'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'installer license resource'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/License.rtf'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'bundle wxs'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/Bundle.wxs'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'bundle wixproj'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/ClipBridgeShell_CS.Bundle.wixproj'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'scripts readme'; Paths = @('scripts/README.md'); Tasks = $cfg; Scope = 'affected' }
        # ---- 共享 Rust / ABI：rust + 三个平台消费者 ----
        @{ Name = 'rust toolchain'; Paths = @('rust-toolchain.toml'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'root cargo manifest'; Paths = @('Cargo.toml'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'root cargo lock'; Paths = @('Cargo.lock'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'cargo config'; Paths = @('.cargo/config.toml'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'shared rust core'; Paths = @('cb_core/src/lib.rs'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'cb_core manifest'; Paths = @('cb_core/Cargo.toml'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'shared c abi implementation'; Paths = @('crates/cb_core_ffi/src/lib.rs'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'shared c abi manifest'; Paths = @('crates/cb_core_ffi/Cargo.toml'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'shared rust crate'; Paths = @('crates/cb_util/src/lib.rs'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'rust source manifest'; Paths = @('crates/cb_util/Cargo.lock'); Tasks = $allTasks; Scope = 'affected' }
        # ---- 单平台 FFI / 头文件 / 嵌套 manifest：rust + 对应消费者 ----
        @{ Name = 'windows ffi source'; Paths = @('platforms/windows/core-ffi/src/lib.rs'); Tasks = $cfgRustWin; Scope = 'affected' }
        @{ Name = 'windows ffi manifest'; Paths = @('platforms/windows/core-ffi/Cargo.toml'); Tasks = $cfgRustWin; Scope = 'affected' }
        @{ Name = 'android ffi source'; Paths = @('platforms/android/core-ffi/src/lib.rs'); Tasks = $cfgRustAndroid; Scope = 'affected' }
        @{ Name = 'android ffi manifest'; Paths = @('platforms/android/core-ffi/Cargo.lock'); Tasks = $cfgRustAndroid; Scope = 'affected' }
        @{ Name = 'macos ffi source'; Paths = @('platforms/macos/core-ffi/src/lib.rs'); Tasks = $cfgRustMac; Scope = 'affected' }
        @{ Name = 'macos ffi manifest'; Paths = @('platforms/macos/core-ffi/Cargo.toml'); Tasks = $cfgRustMac; Scope = 'affected' }
        @{ Name = 'windows nested manifest'; Paths = @('platforms/windows/extra/Cargo.toml'); Tasks = $cfgRustWin; Scope = 'affected' }
        @{ Name = 'windows abi header'; Paths = @('platforms/windows/include/clipbridge_core.h'); Tasks = $cfgRustWin; Scope = 'affected' }
        @{ Name = 'android abi header'; Paths = @('platforms/android/include/clipbridge_core.h'); Tasks = $cfgRustAndroid; Scope = 'affected' }
        @{ Name = 'macos abi header'; Paths = @('platforms/macos/include/clipbridge_core.h'); Tasks = $cfgRustMac; Scope = 'affected' }
        # ---- Rust 策略 / 工具链 ----
        @{ Name = 'rustfmt policy'; Paths = @('rustfmt.toml'); Tasks = $cfgRust; Scope = 'affected' }
        @{ Name = 'cargo deny policy'; Paths = @('deny.toml'); Tasks = $cfgRust; Scope = 'affected' }
        @{ Name = 'dotnet toolchain pin'; Paths = @('global.json'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'editorconfig'; Paths = @('.editorconfig'); Tasks = $cfgWin; Scope = 'affected' }
        # ---- Windows 生产 / 测试 / 资源输入 ----
        @{ Name = 'windows production code'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/App.xaml.cs'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows core project'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Core/Services/CoreConfigBuilder.cs'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows shared csproj'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Core/ClipBridgeShell_CS.Core.csproj'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows mstest source'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/CoreHostSmokeTests.cs'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows mstest csproj'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/ClipBridgeShell_CS.Tests.MSTest.csproj'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows app asset'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Assets/WindowIcon.ico'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows app asset readme'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Assets/README.md'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows publish profile'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Properties/PublishProfiles/win10-x64.pubxml'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows solution'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.sln'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows version props'; Paths = @('platforms/windows/ClipBridgeShell_CS/Version.props'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows shared build props'; Paths = @('platforms/windows/ClipBridgeShell_CS/Directory.Build.props'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows readme'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/README.md'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'msix-named platform file'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/msix/notes.txt'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'windows version script'; Paths = @('scripts/version.ps1'); Tasks = $cfgWin; Scope = 'affected' }
        # ---- Android 生产输入 ----
        @{ Name = 'android shell'; Paths = @('platforms/android/ClipBridgeShellAndroid/app/build.gradle.kts'); Tasks = $cfgAndroid; Scope = 'affected' }
        @{ Name = 'android readme'; Paths = @('platforms/android/ClipBridgeShellAndroid/README.md'); Tasks = $cfgAndroid; Scope = 'affected' }
        @{ Name = 'android contract script'; Paths = @('scripts/check-android-shell-contracts.sh'); Tasks = $cfgAndroid; Scope = 'affected' }
        @{ Name = 'android ndk setup'; Paths = @('scripts/setup-ndk.ps1'); Tasks = $cfgAndroid; Scope = 'affected' }
        # ---- macOS 生产输入 ----
        @{ Name = 'macos shell'; Paths = @('platforms/macos/ClipBridgeShell_macOS/ClipBridge/ContentView.swift'); Tasks = $cfgMac; Scope = 'affected' }
        @{ Name = 'macos ffi build script'; Paths = @('scripts/build-macos-ffi.sh'); Tasks = $cfgMac; Scope = 'affected' }
        @{ Name = 'macos shell env script'; Paths = @('scripts/check-macos-shell-env.sh'); Tasks = $cfgMac; Scope = 'affected' }
        # ---- 本地渠道（精确路径）----
        @{ Name = 'msix local script'; Paths = @('scripts/build-windows-msix.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'winget local script'; Paths = @('scripts/build-windows-winget.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'winget manifest script'; Paths = @('scripts/build-winget-manifest.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'store identity'; Paths = @('store/identity.example.json'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'msix manifest'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Package.appxmanifest'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'msix appinstaller'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Package.appinstaller'); Tasks = $cfg; Scope = 'affected' }
        # ---- 文档 ----
        @{ Name = 'documentation'; Paths = @('docs/ci-architecture.md'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'documentation readme'; Paths = @('docs/README.md'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'root readme'; Paths = @('README.md'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'root agents doc'; Paths = @('AGENTS.md'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'root chinese doc'; Paths = @('ClipBridge项目文档.md'); Tasks = $cfg; Scope = 'affected' }
        # ---- 未知路径保守全跑 ----
        @{ Name = 'unknown path'; Paths = @('some/unknown.bin'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'unknown readme'; Paths = @('some/README.md'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'gitattributes'; Paths = @('.gitattributes'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'gitignore'; Paths = @('.gitignore'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'license'; Paths = @('LICENSE'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'repository asset'; Paths = @('Assets/Picture/img.png'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'tools script'; Paths = @('tools/cargo-build.cmd'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'unrecognized config'; Paths = @('unrecognized-config.json'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'trailing space path'; Paths = @('Cargo.toml '); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'leading space path'; Paths = @(' Cargo.toml'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'local script with trailing space'; Paths = @('scripts/build-windows-msix.ps1 '); Tasks = $allTasks; Scope = 'full' }
        # ---- 空变更与混合范围（按路径取并集）----
        @{ Name = 'empty change set'; Paths = @(); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'mixed core + docs'; Paths = @('cb_core/src/lib.rs', 'docs/windows-build.md'); Tasks = $allTasks; Scope = 'affected' }
        @{ Name = 'mixed windows code + installer'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/App.xaml.cs', 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs'); Tasks = $cfgWin; Scope = 'affected' }
        @{ Name = 'mixed windows + android production'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/App.xaml.cs', 'platforms/android/ClipBridgeShellAndroid/app/build.gradle.kts'); Tasks = $cfgWinAndroid; Scope = 'affected' }
        @{ Name = 'mixed android ffi + macos header'; Paths = @('platforms/android/core-ffi/src/lib.rs', 'platforms/macos/include/clipbridge_core.h'); Tasks = $cfgRustAndroidMac; Scope = 'affected' }
        @{ Name = 'mixed docs + packaging'; Paths = @('docs/windows-release.md', 'scripts/ci/setup-wix.ps1'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'mixed docs + unknown'; Paths = @('docs/windows-release.md', 'mystery/file.bin'); Tasks = $allTasks; Scope = 'full' }
        @{ Name = 'mixed release workflow + docs'; Paths = @('.github/workflows/release-windows.yml', 'docs/ci-architecture.md'); Tasks = $cfg; Scope = 'affected' }
        @{ Name = 'mixed installer + shared core'; Paths = @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/Bundle.wxs', 'cb_core/src/lib.rs'); Tasks = $allTasks; Scope = 'affected' }
    )

    foreach ($case in $classificationCases) {
        Test-Case "classification: $($case.Name)" {
            $plan = Get-CiPlanFromChangedPaths -Paths $case.Paths -Commit $script:testCommit
            $errors = @(Get-CiPlanErrors -Plan $plan)
            Assert-Equal 0 $errors.Count "plan must be valid: $($errors -join '; ')"
            Assert-CleanSchema2Plan $plan $case.Name
            Assert-Equal $case.Scope $plan.scope 'scope mismatch'
            Assert-Equal $case.Tasks (Get-PlanTaskSet $plan) 'required task set mismatch'
            foreach ($task in $script:fixedTasks) {
                Assert-True (@($plan.reasons.$task).Count -gt 0) "reason missing for $task"
            }
        }
    }

    Test-Case 'classification: tighter rules are not widened by broader unions' {
        $plan = Get-CiPlanFromChangedPaths -Paths @('platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs') -Commit $script:testCommit
        Assert-Equal $cfg (Get-PlanTaskSet $plan) 'installer-only change must not require windows'
        Assert-True (($plan.reasons.windows -join ' ') -notmatch '^$') 'skipped task reason must be present'
        $setup = Get-CiPlanFromChangedPaths -Paths @('scripts/ci/setup-wix.ps1') -Commit $script:testCommit
        Assert-Equal $cfg (Get-PlanTaskSet $setup) 'packaging script change must stay config-only'
        $release = Get-CiPlanFromChangedPaths -Paths @('.github/workflows/release-windows.yml') -Commit $script:testCommit
        Assert-Equal 'affected' $release.scope 'release orchestration change must stay affected/config-only'
    }

    Test-Case 'classification: reasons are sanitized and cannot inject control characters' {
        $weird = "cb_core/src/evil`npath.rs"
        $plan = Get-CiPlanFromChangedPaths -Paths @($weird, "mystery/evil`r`npath.bin") -Commit $script:testCommit
        Assert-Equal $allTasks (Get-PlanTaskSet $plan) 'unknown path forces full'
        $json = ConvertTo-CiPlanJson -Plan $plan
        $parsed = ConvertFrom-CiPlanJson -Json $json
        Assert-Equal 0 @(Get-CiPlanErrors -Plan $parsed).Count 'sanitized plan must round-trip'
        foreach ($task in $script:fixedTasks) {
            foreach ($reason in @($parsed.reasons.$task)) {
                Assert-True ($reason -notmatch '[\x00-\x1F]') "reason for $task must not contain control characters"
            }
        }
    }

    Test-Case 'mixed build input + docs does not mislabel false tasks as doc-only' {
        $plan = Get-CiPlanFromChangedPaths -Paths @('docs/windows-build.md', 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/CoreHostSmokeTests.cs') -Commit $script:testCommit
        Assert-Equal $cfgWin (Get-PlanTaskSet $plan) 'windows test + docs task set'
        Assert-True (($plan.reasons.android -join ' ') -notmatch 'documentation/non-build') 'a real build input must not produce doc-only reasons'
    }

    # 删除 / 重命名按旧路径 + 新路径共同计算影响。
    Test-Case 'rename parser keeps old and new paths' {
        $entries = @(ConvertFrom-GitDiffNameStatusZ -Text "M`0docs/a.md`0R100`0scripts/build-windows-portable-zip.ps1`0scripts/check-android-shell-contracts.sh`0D`0platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/Old.cs`0")
        Assert-Equal 3 $entries.Count 'entry count'
        Assert-Equal 'R100' $entries[1].Status 'rename status'
        Assert-Equal 'scripts/build-windows-portable-zip.ps1' $entries[1].OldPath 'rename old path'
        Assert-Equal 'scripts/check-android-shell-contracts.sh' $entries[1].Path 'rename new path'
        $paths = @($entries | ForEach-Object { if ($_.OldPath) { $_.OldPath }; if ($_.Path) { $_.Path } })
        $plan = Get-CiPlanFromChangedPaths -Paths $paths -Commit $script:testCommit
        Assert-Equal $cfgWinAndroid (Get-PlanTaskSet $plan) 'rename union must add the new Android script while deleted Windows test input keeps windows'
        Assert-Equal 'affected' $plan.scope 'packaging old path must not widen the rename plan'
    }

    Test-Case 'rename from packaging-only to platform script does not widen' {
        $entries = @(ConvertFrom-GitDiffNameStatusZ -Text "R100`0scripts/build-windows-portable-zip.ps1`0scripts/check-android-shell-contracts.sh`0")
        $paths = @($entries | ForEach-Object { if ($_.OldPath) { $_.OldPath }; if ($_.Path) { $_.Path } })
        $plan = Get-CiPlanFromChangedPaths -Paths $paths -Commit $script:testCommit
        Assert-Equal $cfgAndroid (Get-PlanTaskSet $plan) 'retired packaging path must not add windows'
        Assert-Equal 'affected' $plan.scope 'rename must stay affected'
    }

    Test-Case 'rename union across platforms keeps both consumers' {
        $entries = @(ConvertFrom-GitDiffNameStatusZ -Text "R100`0platforms/windows/core-ffi/src/lib.rs`0platforms/android/core-ffi/src/lib.rs`0")
        $paths = @($entries | ForEach-Object { if ($_.OldPath) { $_.OldPath }; if ($_.Path) { $_.Path } })
        $plan = Get-CiPlanFromChangedPaths -Paths $paths -Commit $script:testCommit
        Assert-Equal @('ci-config', 'rust', 'windows', 'android') (Get-PlanTaskSet $plan) 'rename union must include both platform consumers'
    }

    Test-Case 'deleted inputs are still classified' {
        $installer = @(ConvertFrom-GitDiffNameStatusZ -Text "D`0platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs`0")
        $plan = Get-CiPlanFromChangedPaths -Paths @($installer[0].Path) -Commit $script:testCommit
        Assert-Equal $cfg (Get-PlanTaskSet $plan) 'deleted installer must stay config-only'
        $header = @(ConvertFrom-GitDiffNameStatusZ -Text "D`0platforms/windows/include/clipbridge_core.h`0")
        $headerPlan = Get-CiPlanFromChangedPaths -Paths @($header[0].Path) -Commit $script:testCommit
        Assert-Equal $cfgRustWin (Get-PlanTaskSet $headerPlan) 'deleted header must still be checked'
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
    # 2. plan schema / 不变量负路径（schemaVersion=2，无 packages）
    # ------------------------------------------------------------------
    $planMutations = @(
        @{ Name = 'valid baseline'; Mutate = { param($p) }; ShouldFail = $false; Pattern = '' }
        @{ Name = 'missing required'; Mutate = { param($p) $p.Remove('required') }; ShouldFail = $true; Pattern = 'missing top-level field' }
        @{ Name = 'missing reasons'; Mutate = { param($p) $p.Remove('reasons') }; ShouldFail = $true; Pattern = 'missing top-level field' }
        @{ Name = 'extra top-level field'; Mutate = { param($p) $p['publish'] = @() }; ShouldFail = $true; Pattern = 'unknown top-level field' }
        @{ Name = 'obsolete packages field'; Mutate = { param($p) $p['packages'] = @('machine-msi') }; ShouldFail = $true; Pattern = "obsolete top-level field 'packages'" }
        @{ Name = 'legacy schemaVersion 1'; Mutate = { param($p) $p.schemaVersion = 1 }; ShouldFail = $true; Pattern = 'unsupported schemaVersion' }
        @{ Name = 'future schemaVersion 3'; Mutate = { param($p) $p.schemaVersion = 3 }; ShouldFail = $true; Pattern = 'unsupported schemaVersion' }
        @{ Name = 'string schemaVersion'; Mutate = { param($p) $p.schemaVersion = '2' }; ShouldFail = $true; Pattern = 'schemaVersion must be an integer' }
        @{ Name = 'unknown scope'; Mutate = { param($p) $p.scope = 'partial' }; ShouldFail = $true; Pattern = 'scope must be' }
        @{ Name = 'short commit'; Mutate = { param($p) $p.commit = 'abc123' }; ShouldFail = $true; Pattern = 'full lowercase commit SHA' }
        @{ Name = 'uppercase commit'; Mutate = { param($p) $p.commit = ('A' * 40) }; ShouldFail = $true; Pattern = 'full lowercase commit SHA' }
        @{ Name = 'required missing fixed task'; Mutate = { param($p) $p.required.Remove('rust') }; ShouldFail = $true; Pattern = "missing fixed task 'rust'" }
        @{ Name = 'required unknown task'; Mutate = { param($p) $p.required['publish'] = $true }; ShouldFail = $true; Pattern = "unknown task 'publish'" }
        @{ Name = 'required obsolete windows-packages task'; Mutate = { param($p) $p.required['windows-packages'] = $true }; ShouldFail = $true; Pattern = "unknown task 'windows-packages'" }
        @{ Name = 'required non-boolean'; Mutate = { param($p) $p.required.rust = 'true' }; ShouldFail = $true; Pattern = 'must be a boolean' }
        @{ Name = 'ci-config false'; Mutate = { param($p) $p.required.'ci-config' = $false }; ShouldFail = $true; Pattern = 'must always be true' }
        @{ Name = 'reasons missing task'; Mutate = { param($p) $p.reasons.Remove('macos') }; ShouldFail = $true; Pattern = "missing fixed task 'macos'" }
        @{ Name = 'reasons unknown task'; Mutate = { param($p) $p.reasons['publish'] = @('x') }; ShouldFail = $true; Pattern = "unknown task 'publish'" }
        @{ Name = 'reasons obsolete windows-packages task'; Mutate = { param($p) $p.reasons['windows-packages'] = @('x') }; ShouldFail = $true; Pattern = "unknown task 'windows-packages'" }
        @{ Name = 'reasons empty array'; Mutate = { param($p) $p.reasons.rust = @() }; ShouldFail = $true; Pattern = 'must not be empty' }
        @{ Name = 'reasons empty string'; Mutate = { param($p) $p.reasons.rust = @('') }; ShouldFail = $true; Pattern = 'empty string' }
        @{ Name = 'reasons non-string'; Mutate = { param($p) $p.reasons.rust = @(1) }; ShouldFail = $true; Pattern = 'non-string' }
        @{ Name = 'reasons control character'; Mutate = { param($p) $p.reasons.rust = @("bad`nvalue") }; ShouldFail = $true; Pattern = 'control characters' }
        @{ Name = 'reasons not an array'; Mutate = { param($p) $p.reasons.rust = 'one reason' }; ShouldFail = $true; Pattern = 'must be an array' }
        @{ Name = 'full scope with false task'; Mutate = { param($p) $p.scope = 'full'; foreach ($t in $script:fixedTasks) { $p.required[$t] = $true }; $p.required['macos'] = $false }; ShouldFail = $true; Pattern = "requires required\['macos'\]" }
        @{ Name = 'full scope all true'; Mutate = { param($p) $p.scope = 'full'; foreach ($t in $script:fixedTasks) { $p.required[$t] = $true } }; ShouldFail = $false; Pattern = '' }
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

    Test-Case 'plan JSON: schemaVersion 2 round-trip without packages' {
        $plan = Get-CiPlanFromChangedPaths -Paths @('Cargo.lock') -Commit $script:testCommit
        $json = ConvertTo-CiPlanJson -Plan $plan
        Assert-True ($json -notmatch '"packages"') 'serialized plan must not contain packages'
        $parsed = ConvertFrom-CiPlanJson -Json $json
        Assert-Equal $plan.commit $parsed.commit 'commit round-trip'
        Assert-Equal $allTasks (Get-PlanTaskSet $parsed) 'task set round-trip'
        Assert-CleanSchema2Plan $parsed 'parsed plan'
    }

    Test-Case 'plan JSON: manual full and unknown plans carry no package fields' {
        foreach ($plan in @(
                (New-CiFullPlan -Commit $script:testCommit -Reason 'manual full'),
                (Get-CiPlanFromChangedPaths -Paths @('some/unknown.bin') -Commit $script:testCommit)
            )) {
            $json = ConvertTo-CiPlanJson -Plan $plan
            Assert-True ($json -notmatch '"packages"') 'JSON must not contain packages'
            Assert-True ($json -notmatch 'windows-packages') 'JSON must not contain the retired task name'
            $parsed = ConvertFrom-CiPlanJson -Json $json
            Assert-CleanSchema2Plan $parsed 'serialized plan'
            Assert-Equal $allTasks (Get-PlanTaskSet $parsed) 'full/unknown plans require all five tasks'
        }
    }

    Test-Case 'plan JSON: legacy schema and retired fields are rejected' {
        $legacy = New-TestPlanHashtable
        $legacy.schemaVersion = 1
        $legacy['packages'] = @('machine-msi')
        $legacy.required['windows-packages'] = $true
        $legacy.reasons['windows-packages'] = @('legacy reason')
        $legacyJson = $legacy | ConvertTo-Json -Depth 10
        Assert-ThrowsLike { ConvertFrom-CiPlanJson -Json $legacyJson } 'unsupported schemaVersion' 'legacy schemaVersion=1 must be rejected'

        $oldField = New-TestPlanHashtable
        $oldField['packages'] = @()
        $oldFieldJson = $oldField | ConvertTo-Json -Depth 10
        Assert-ThrowsLike { ConvertFrom-CiPlanJson -Json $oldFieldJson } "obsolete top-level field 'packages'" 'old packages field must be rejected'

        $retiredTask = New-TestPlanHashtable
        $retiredTask.required['windows-packages'] = $true
        $retiredTaskJson = $retiredTask | ConvertTo-Json -Depth 10
        Assert-ThrowsLike { ConvertFrom-CiPlanJson -Json $retiredTaskJson } "unknown task 'windows-packages'" 'retired windows-packages task must be rejected'
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
        Assert-Equal $cfgWin (Get-PlanTaskSet $plan) 'test-only plan task set'
        Assert-CleanSchema2Plan $plan 'get-plan output'
    }

    Test-Case 'get-plan: full scope' {
        $out = Join-Path $script:tempRoot 'get-plan-full.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-Scope', 'full', '-OutFile', $out)
        Assert-Equal 0 $code 'get-plan full exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal 'full' $plan.scope 'full scope'
        Assert-Equal $allTasks (Get-PlanTaskSet $plan) 'full tasks'
        Assert-CleanSchema2Plan $plan 'full plan'
        Assert-True ([System.IO.File]::ReadAllText($out) -notmatch '"packages"') 'full plan file must not contain packages'
    }

    Test-Case 'get-plan: release workflow stays config-only' {
        $out = Join-Path $script:tempRoot 'get-plan-release.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-ChangedPaths', '.github/workflows/release-windows.yml', '-OutFile', $out)
        Assert-Equal 0 $code 'release workflow plan exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal $cfg (Get-PlanTaskSet $plan) 'release orchestration only needs ci-config'
        Assert-Equal 'affected' $plan.scope 'release orchestration stays affected'
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

    Test-Case 'get-plan: unknown path falls back to full without packages' {
        $out = Join-Path $script:tempRoot 'get-plan-unknown.json'
        $code = Invoke-CiScript (Join-Path $PSScriptRoot 'get-plan.ps1') @('-Commit', $script:testCommit, '-ChangedPaths', 'mystery/file.bin', '-OutFile', $out)
        Assert-Equal 0 $code 'unknown path plan exit code'
        $plan = Read-CiPlan -Path $out
        Assert-Equal 'full' $plan.scope 'unknown path scope'
        Assert-Equal $allTasks (Get-PlanTaskSet $plan) 'unknown path requires all five tasks'
        Assert-CleanSchema2Plan $plan 'unknown path plan'
        Assert-True ([System.IO.File]::ReadAllText($out) -notmatch '"packages"') 'unknown plan file must not contain packages'
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
        Assert-Equal $allTasks (Get-PlanTaskSet $plan) 'shared core + header must require all consumers'
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
        param([hashtable]$Results)
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
        $arguments = @('-PlanFile', $PlanPath, '-NeedsFile', $needsPath, '-ExpectedCommit', $ExpectedCommit, '-SummaryFile', $SummaryPath)
        return Invoke-CiScript (Join-Path $PSScriptRoot 'assert-result.ps1') $arguments
    }

    Test-Case 'assert-result: documentation-only plan allows skipped optional jobs' {
        $summary = Join-Path $script:tempRoot 'summary-pass.md'
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson (New-NeedsJson @{ 'ci-config' = 'success' }) -SummaryPath $summary
        Assert-Equal 0 $code 'doc-only run must pass'
        $text = [System.IO.File]::ReadAllText($summary)
        Assert-Match $text 'CI result: PASS' 'summary must state PASS'
        Assert-True ($text -notmatch 'packages') 'summary must not report a package scope'
    }

    Test-Case 'assert-result: required task skipped fails' {
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

    Test-Case 'assert-result: missing result value fails' {
        $needs = New-NeedsJson @{ 'ci-config' = 'success' }
        $obj = $needs | ConvertFrom-Json
        $obj.macos.PSObject.Properties.Remove('result')
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson ($obj | ConvertTo-Json -Depth 10) -SummaryPath $null
        Assert-True ($code -ne 0) 'missing result must fail'
    }

    Test-Case 'assert-result: extra needs job fails' {
        $needs = New-NeedsJson @{ 'ci-config' = 'success' }
        $obj = $needs | ConvertFrom-Json
        $obj | Add-Member -NotePropertyName 'legacy-job' -NotePropertyValue ([pscustomobject]@{ result = 'success' })
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson ($obj | ConvertTo-Json -Depth 10) -SummaryPath $null
        Assert-True ($code -ne 0) 'extra needs job must fail'
    }

    Test-Case 'assert-result: retired windows-packages needs entry fails' {
        $needs = New-NeedsJson @{ 'ci-config' = 'success' }
        $obj = $needs | ConvertFrom-Json
        $obj | Add-Member -NotePropertyName 'windows-packages' -NotePropertyValue ([pscustomobject]@{ result = 'success' })
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson ($obj | ConvertTo-Json -Depth 10) -SummaryPath $null
        Assert-True ($code -ne 0) 'retired package job in needs must fail'
    }

    Test-Case 'assert-result: invalid plan JSON fails' {
        $badPlan = Join-Path $script:tempRoot 'bad-plan.json'
        [System.IO.File]::WriteAllText($badPlan, '{ not json ')
        $code = Invoke-AssertResult -PlanPath $badPlan -NeedsJson (New-NeedsJson @{ 'ci-config' = 'success' }) -SummaryPath $null
        Assert-True ($code -ne 0) 'invalid plan must fail'
    }

    Test-Case 'assert-result: legacy schema plan fails' {
        $legacy = New-TestPlanHashtable
        $legacy.schemaVersion = 1
        $legacy['packages'] = @()
        $legacyPath = Join-Path $script:tempRoot 'legacy-plan.json'
        [System.IO.File]::WriteAllText($legacyPath, ($legacy | ConvertTo-Json -Depth 10))
        $code = Invoke-AssertResult -PlanPath $legacyPath -NeedsJson (New-NeedsJson @{ 'ci-config' = 'success' }) -SummaryPath $null
        Assert-True ($code -ne 0) 'legacy schema plan must fail'
    }

    Test-Case 'assert-result: commit mismatch fails' {
        $code = Invoke-AssertResult -PlanPath $docPlanPath -NeedsJson (New-NeedsJson @{ 'ci-config' = 'success' }) -SummaryPath $null -ExpectedCommit ('c' * 40)
        Assert-True ($code -ne 0) 'plan commit must match checked out SHA'
    }

    Test-Case 'assert-result: full plan all success passes' {
        $results = @{}
        foreach ($task in $script:fixedTasks) { $results[$task] = 'success' }
        $summary = Join-Path $script:tempRoot 'summary-full.md'
        $code = Invoke-AssertResult -PlanPath $fullPlanPath -NeedsJson (New-NeedsJson $results) -SummaryPath $summary
        Assert-Equal 0 $code 'full green plan must pass'
        Assert-True ([System.IO.File]::ReadAllText($summary) -notmatch 'packages') 'full summary must not report packages'
    }

    # ------------------------------------------------------------------
    # 5. workflow 结构契约（7 个 job，无日常安装器构建/发布路径）
    # ------------------------------------------------------------------
    $workflowPath = Join-Path $repoRoot '.github/workflows/ci.yml'
    $workflowContent = [System.IO.File]::ReadAllText($workflowPath)

    Test-Case 'workflow contract: fixed job and task names' {
        Assert-Equal $script:fixedTasks @(Get-CiTaskNames) 'fixed plan task names'
        Assert-Equal @('changes', 'ci-config', 'rust', 'windows', 'android', 'macos', 'ci-result') @(Get-CiWorkflowJobNames) 'fixed workflow job names'
    }

    Test-Case 'workflow contract: real ci.yml passes' {
        $result = Test-CiWorkflowContract -Content $workflowContent -Path '.github/workflows/ci.yml'
        Assert-Equal 0 @($result.Errors).Count ("workflow contract errors: " + (@($result.Errors) -join '; '))
        $expectedJobs = @('changes', 'ci-config', 'rust', 'windows', 'android', 'macos', 'ci-result') | Sort-Object
        $actualJobs = @($result.Jobs) | Sort-Object
        Assert-Equal $expectedJobs $actualJobs 'workflow job set'
    }

    Test-Case 'workflow inventory: only ci.yml and release-windows.yml exist' {
        $files = @(Get-ChildItem -LiteralPath (Join-Path $repoRoot '.github/workflows') -File | ForEach-Object { $_.Name } | Sort-Object)
        Assert-Equal @('ci.yml', 'release-windows.yml') $files 'unexpected workflow file; classification falls back to full and the inventory test rejects extras'
    }

    Test-Case 'workflow contract: daily ci.yml contains no installer or release path' {
        Assert-True ($workflowContent -notmatch 'windows-packages') 'ci.yml must not mention the retired packages job'
        Assert-True ($workflowContent -notmatch 'build-windows-installers|setup-wix|sign-windows|ClipBridge\.Packaging') 'ci.yml must not call packaging/signing scripts'
        Assert-True ($workflowContent -notmatch '(?i)\b(?:candle|light|heat)(?:\.exe)?\b') 'ci.yml must not run WiX tools'
        Assert-True ($workflowContent -notmatch 'gh\s+release|softprops/action-gh-release|release-windows\.yml') 'ci.yml must not require the release path'
    }

    Test-Case 'workflow embedded scripts: extraction and PowerShell AST' {
        $blocks = @(Get-CiWorkflowRunBlocks -Content $workflowContent)
        $pwshBlocks = @($blocks | Where-Object { $_.Shell -eq 'pwsh' })
        $bashBlocks = @($blocks | Where-Object { $_.Shell -eq 'bash' })
        Assert-True ($pwshBlocks.Count -ge 3) 'expected embedded pwsh blocks (changes/check-config/result)'
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

    $packagingStep = "      - name: Download WiX`n        shell: pwsh`n        run: ./scripts/ci/setup-wix.ps1 -DestDir `$env:RUNNER_TEMP/wix`n`n      - name: Windows native integration and MSTest"
    $workflowMutations = @(
        @{ Name = 'unpinned action'; Old = 'uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0'; New = 'uses: actions/checkout@v4'; Pattern = 'pinned to a full commit SHA' }
        @{ Name = 'wrong pinned sha'; Old = 'uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0'; New = 'uses: actions/checkout@' + ('0' * 40) + ' # v4.4.0'; Pattern = 'must use SHA' }
        @{ Name = 'rust condition bypass'; Old = "required['rust']"; New = 'true'; Pattern = "plan.required\['rust'\]" }
        @{ Name = 'missing timeout'; Old = '    timeout-minutes: 30'; New = '    # timeout removed'; Pattern = 'missing timeout-minutes' }
        @{ Name = 'extra job'; Old = '  ci-result:'; New = "  legacy-job:`n    runs-on: ubuntu-24.04`n    timeout-minutes: 10`n  ci-result:"; Pattern = "unknown job 'legacy-job'" }
        @{ Name = 'reintroduced windows-packages job'; Old = '  ci-result:'; New = "  windows-packages:`n    needs: [ changes, windows ]`n    if: github.event_name == 'workflow_dispatch'`n    runs-on: windows-2022`n    timeout-minutes: 10`n    steps:`n      - run: echo packaging`n`n  ci-result:"; Pattern = "unknown job 'windows-packages'" }
        @{ Name = 'missing ci-result need'; Old = 'needs: [ changes, ci-config, rust, windows, android, macos ]'; New = 'needs: [ changes, ci-config, rust, windows, android ]'; Pattern = "ci-result needs must include 'macos'" }
        @{ Name = 'retired package need'; Old = 'needs: [ changes, ci-config, rust, windows, android, macos ]'; New = 'needs: [ changes, ci-config, rust, windows, android, macos, windows-packages ]'; Pattern = "ci-result needs has unexpected job 'windows-packages'" }
        @{ Name = 'continue-on-error'; Old = '    timeout-minutes: 30'; New = "    timeout-minutes: 30`n    continue-on-error: true"; Pattern = 'continue-on-error' }
        @{ Name = 'cancel main'; Old = "cancel-in-progress: `${{ github.event_name == 'pull_request' }}"; New = 'cancel-in-progress: true'; Pattern = 'concurrency must cancel' }
        @{ Name = 'no workflow_dispatch'; Old = "  workflow_dispatch:`n"; New = ''; Pattern = 'workflow_dispatch' }
        @{ Name = 'secrets reference'; Old = '      shell: pwsh'; New = "      shell: pwsh`n      env:`n        TOKEN: `${{ secrets.TOKEN }}"; Pattern = 'secrets' }
        @{ Name = 'winget command'; Old = 'run: ./scripts/ci/test-plan.ps1'; New = 'run: winget validate dist/winget'; Pattern = 'forbidden local-channel command' }
        @{ Name = 'missing plan contract suite'; Old = 'run: ./scripts/ci/test-plan.ps1'; New = 'run: echo tests-removed'; Pattern = 'public plan/classification contract tests' }
        @{ Name = 'missing Android SDK install'; Old = '          sdkmanager --install \'; New = '          echo skipped-sdk-install'; Pattern = 'Android job must install' }
        @{ Name = 'setup-android implicit legacy default'; Old = "        with:`n          packages: ''"; New = '        # packages omitted'; Pattern = 'setup-android must explicitly use packages' }
        @{ Name = 'setup-android removed tools package'; Old = "          packages: ''"; New = "          packages: 'tools platform-tools'"; Pattern = 'setup-android must explicitly use packages' }
        @{ Name = 'missing windows report suite'; Old = 'run: ./scripts/ci/test-windows-report.ps1'; New = 'run: ./scripts/ci/test-windows-report-missing.ps1'; Pattern = 'windows report contract tests' }
        @{ Name = 'missing fixed release suite'; Old = "'test-release-candidate.ps1'"; New = "'test-release-candidate-missing.ps1'"; Pattern = "fixed release contract suite 'test-release-candidate\.ps1'" }
        @{ Name = 'missing release fail-closed guard'; Old = 'missing required contract suite'; New = 'missing suite'; Pattern = 'fail when a required suite is missing' }
        @{ Name = 'reintroduced setup-wix step'; Old = '      - name: Windows native integration and MSTest'; New = $packagingStep; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced installer build'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: Build installers`n        shell: pwsh`n        run: ./scripts/build-windows-installers.ps1 -Formats portable-zip`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced plan.packages consumer in changes job'; Old = '      - name: Compute structured plan'; New = "      - name: packages`n        shell: pwsh`n        run: echo `$plan.packages`n`n      - name: Compute structured plan"; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced formats flag'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: formats`n        shell: pwsh`n        run: echo '-Formats portable-zip'`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced signing'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: Sign installers`n        shell: pwsh`n        run: ./scripts/ci/sign-windows.ps1`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced candle'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: candle`n        shell: pwsh`n        run: candle.exe Product.wxs`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced light'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: light`n        shell: pwsh`n        run: light.exe -o out.msi`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced heat'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: heat`n        shell: pwsh`n        run: heat.exe dir -o HeatGenerated.wxs`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not build installers' }
        @{ Name = 'reintroduced release command'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: publish`n        if: github.event_name == 'workflow_dispatch'`n        shell: pwsh`n        run: gh release create v1 --notes test`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not require or run the release path' }
        @{ Name = 'release workflow reference'; Old = '      - name: Windows native integration and MSTest'; New = "      - name: release`n        shell: pwsh`n        run: echo release-windows.yml`n`n      - name: Windows native integration and MSTest"; Pattern = 'must not require or run the release path' }
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
    # 7. 工具 pin、setup-wix 与 check-config（含 XML 语法）
    # ------------------------------------------------------------------
    Test-Case 'tool pins: actionlint and WiX digests stay pinned for the release bootstrap' {
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

    Test-Case 'legacy package helper is no longer exported' {
        Assert-True ($null -eq (Get-Command Get-CiPackageFormats -ErrorAction SilentlyContinue)) 'Get-CiPackageFormats must be removed'
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

    function New-CheckConfigRoot {
        $root = Join-Path $script:tempRoot ('cfg-root-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Force -Path (Join-Path $root '.github/workflows') | Out-Null
        Copy-Item -LiteralPath (Join-Path $repoRoot '.github/workflows/ci.yml') -Destination (Join-Path $root '.github/workflows/ci.yml')
        Copy-Item -LiteralPath (Join-Path $repoRoot '.github/workflows/release-windows.yml') -Destination (Join-Path $root '.github/workflows/release-windows.yml')
        $pins = Get-CiToolPins
        $globalJson = [ordered]@{ sdk = [ordered]@{ version = $pins.DotnetSdkVersion; rollForward = 'disable' } } | ConvertTo-Json -Depth 5
        [System.IO.File]::WriteAllText((Join-Path $root 'global.json'), $globalJson)
        [System.IO.File]::WriteAllText((Join-Path $root 'rust-toolchain.toml'), "[toolchain]`nchannel = `"$($pins.RustToolchainVersion)`"`n")
        return $root
    }

    Test-Case 'check-config: invalid project XML is rejected' {
        $root = New-CheckConfigRoot
        [System.IO.File]::WriteAllText((Join-Path $root 'bad.wxs'), '<Wix><Fragment></Wix>')
        $result = Invoke-CiScriptCaptured (Join-Path $PSScriptRoot 'check-config.ps1') @('-RepoRoot', $root, '-Files', 'bad.wxs', '-SkipActionlint', '-AllowMissingBash')
        Assert-True ($result.ExitCode -ne 0) 'invalid XML must fail check-config'
        Assert-Match $result.Text 'XML syntax' 'invalid XML error must be reported, not swallowed'
        Assert-Match $result.Text 'XML files: 0' 'failed XML must not be counted as checked'
    }

    Test-Case 'check-config: XML with a DTD is rejected without entity resolution' {
        $root = New-CheckConfigRoot
        $dtd = '<!DOCTYPE Wix [<!ENTITY external SYSTEM "file:///etc/hostname">]><Wix>&external;</Wix>'
        [System.IO.File]::WriteAllText((Join-Path $root 'dtd.wxs'), $dtd)
        $result = Invoke-CiScriptCaptured (Join-Path $PSScriptRoot 'check-config.ps1') @('-RepoRoot', $root, '-Files', 'dtd.wxs', '-SkipActionlint', '-AllowMissingBash')
        Assert-True ($result.ExitCode -ne 0) 'DTD-bearing XML must be rejected'
        Assert-Match $result.Text 'XML syntax' 'DTD rejection must be reported as an XML error'
        Assert-Match $result.Text 'XML files: 0' 'rejected DTD XML must not be counted as checked'
    }

    Test-Case 'check-config: valid project XML passes' {
        $root = New-CheckConfigRoot
        [System.IO.File]::WriteAllText((Join-Path $root 'ok.wxs'), '<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi"><Fragment /></Wix>')
        $result = Invoke-CiScriptCaptured (Join-Path $PSScriptRoot 'check-config.ps1') @('-RepoRoot', $root, '-Files', 'ok.wxs', '-SkipActionlint', '-AllowMissingBash')
        Assert-Equal 0 $result.ExitCode ('valid XML must pass: ' + (Protect-CiText $result.Text))
        Assert-Match $result.Text 'XML files: 1' 'valid XML must be counted'
    }

    Test-Case 'check-config: actionlint receives both workflows' {
        $root = New-CheckConfigRoot
        $log = Join-Path $root 'actionlint-args.txt'
        $fake = @'
if ($args -contains '-version') { Write-Output '1.7.12'; exit 0 }
[System.IO.File]::AppendAllText('__LOG__', (($args -join ' ') + "`n"))
exit 0
'@.Replace('__LOG__', $log)
        [System.IO.File]::WriteAllText((Join-Path $root 'actionlint.ps1'), $fake)
        $result = Invoke-CiScriptCaptured (Join-Path $PSScriptRoot 'check-config.ps1') @('-RepoRoot', $root, '-ActionlintPath', (Join-Path $root 'actionlint.ps1'), '-AllowMissingBash')
        Assert-Equal 0 $result.ExitCode ('actionlint fixture run must pass: ' + (Protect-CiText $result.Text))
        Assert-True (Test-Path -LiteralPath $log -PathType Leaf) 'fake actionlint was not invoked'
        $argsText = [System.IO.File]::ReadAllText($log)
        Assert-Match $argsText '\.github/workflows/ci\.yml' 'actionlint must lint ci.yml'
        Assert-Match $argsText '\.github/workflows/release-windows\.yml' 'actionlint must lint release-windows.yml'
    }

    Test-Case 'check-config: full scope parses declared installer XML' {
        $root = New-CheckConfigRoot
        Copy-Item -LiteralPath (Join-Path $repoRoot 'scripts') -Destination (Join-Path $root 'scripts') -Recurse
        $installerDir = Join-Path $root 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer'
        $bundleDir = Join-Path $root 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle'
        foreach ($dir in @($installerDir, $bundleDir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        Copy-Item -LiteralPath (Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/Product.wxs') -Destination $installerDir
        Copy-Item -LiteralPath (Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/ClipBridgeShell_CS.Installer.wixproj') -Destination $installerDir
        Copy-Item -LiteralPath (Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/Bundle.wxs') -Destination $bundleDir
        Copy-Item -LiteralPath (Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle/ClipBridgeShell_CS.Bundle.wixproj') -Destination $bundleDir
        $result = Invoke-CiScriptCaptured (Join-Path $PSScriptRoot 'check-config.ps1') @('-RepoRoot', $root, '-Scope', 'full', '-SkipActionlint', '-AllowMissingBash')
        Assert-Equal 0 $result.ExitCode ('full scope must pass: ' + (Protect-CiText $result.Text))
        Assert-Match $result.Text 'XML files: 4' 'declared installer XML files must be parsed'
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
