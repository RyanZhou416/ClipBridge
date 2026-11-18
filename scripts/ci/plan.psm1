#requires -version 7.0
<#
    ClipBridge CI 变更分类 / 计划契约 / 汇总契约的单一实现。

    本模块是 §4.2/§4.4 的唯一分类来源：只在这里把“变更路径 + 事件”映射成固定
    任务集合，工作流 job 条件与 ci-result 汇总都读取同一份 plan JSON。

    契约（严格，schemaVersion=2）：
      schemaVersion = 2
      scope         = affected | full
      commit        = 当前实际检出的完整小写提交 SHA（40 或 64 个十六进制字符）
      required      = 固定 5 键布尔 map（ci-config, rust, windows, android, macos），
                      ci-config 恒为 true
      reasons       = 与 required 同键的非空字符串数组
    不变量：
      scope=full 时 5 个固定任务全部 true；full 表示五个验证任务，不包含安装器构建。
      日常 CI 不构建 MSI / Burn EXE / 便携 ZIP，因此 plan 不再包含 packages 字段；
      旧 schemaVersion=1、旧 packages 字段与旧 windows-packages 任务名一律拒绝。
      MSIX / StoreUpload / winget 是本地渠道，不允许出现在固定任务集合中。

    注意：本文件不读取 GitHub 事件，也不调用网络；工作流负责提供 base/head/commit。
#>

Set-StrictMode -Version Latest

$script:CiSchemaVersion = 2
$script:CiTasks = @('ci-config', 'rust', 'windows', 'android', 'macos')
$script:CiWorkflowJobs = @('changes') + $script:CiTasks + @('ci-result')

# 经官方 GitHub API 核实（2026-11-17）的完整提交 SHA：
#   GET https://api.github.com/repos/<owner>/<repo>/tags?per_page=20
# 再按 tag 名取 commit.sha（annotated tag 需 peel 到 commit）。这里不使用浮动 @vN。
# 工作流必须使用这里的 SHA；check-config 会核对，避免 @v4 之类的浮动引用。
$script:CiActionPins = [ordered]@{
    'actions/checkout'            = @{ Sha = '11d5960a326750d5838078e36cf38b85af677262'; Version = 'v4.4.0' }
    'actions/upload-artifact'     = @{ Sha = 'ea165f8d65b6e75b540449e92b4886f43607fa02'; Version = 'v4.6.2' }
    'actions/setup-dotnet'        = @{ Sha = '67a3573c9a986a3f9c594539f4ab511d57bb3ce9'; Version = 'v4.3.1' }
    'actions/setup-java'          = @{ Sha = 'cf277c60eb25467037889841efdb72551f06f6c3'; Version = 'v4.9.1' }
    'android-actions/setup-android' = @{ Sha = '9fc6c4e9069bf8d3d10b2204b1fb8f6ef7065407'; Version = 'v3.2.2' }
    'Swatinem/rust-cache'         = @{ Sha = 'd1527c8f67ef9af357290475d4af4b0fab8f12e0'; Version = 'v2.9.3' }
}

# 下载工具的固定版本与官方发布摘要（校验失败直接失败，不静默换版本）。
# actionlint 摘要来源：https://api.github.com/repos/rhysd/actionlint/releases/latest 的
# asset digest（linux_amd64.tar.gz / windows_amd64.zip）。
# WiX 摘要来源：官方 release asset
# https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip
# 下载后本地 Get-FileHash 复算一致（2026-11-17）。
$script:CiToolPins = @{
    DotnetSdkVersion            = '10.0.403'
    RustToolchainVersion        = '1.99.1'
    ActionlintVersion           = '1.7.12'
    ActionlintLinuxAmd64Sha256  = '8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8'
    ActionlintWindowsAmd64Sha256 = '6e7241b51e6817ea6a047693d8e6fed13b31819c9a0dd6c5a726e1592d22f6e9'
    WixVersion                  = '3.14.1'
    WixZipSha256                = '6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31'
    WixDownloadUrl              = 'https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip'
}

# 本地渠道：只在本机执行，不进入 GitHub CI。仅使用“专用脚本/存储目录”精确路径；
# 生产规则优先，任何位于 crates/ 或 platforms/ 下的同形目录（例如 msix/ 子目录里的
# 真实源码）先按正常构建输入分类，不会被这里吞掉。
$script:CiLocalOnlyPatterns = @(
    '^store/',
    '^scripts/build-windows-msix\.ps1$',
    '^scripts/build-windows-winget\.ps1$',
    '^scripts/build-winget-manifest\.ps1$',
    '^platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Package\.appxmanifest$',
    '^platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Package\.appinstaller$'
)

# 纯说明文档：只认明确的文档目录与仓库根目录 Markdown（根 README/AGENTS/STATUS/
# 大型中文文档）。scripts/README.md 由显式规则单独识别；其它带路径的 .md 先走构建
# 规则，没有构建规则时就按未知路径保守全跑。
$script:CiDocOnlyPatterns = @(
    '^docs/',
    '^[^/]+\.md$'
)

# 规则按“具体 -> 宽泛”排序，每条路径只在首个命中的规则处生效；宽泛规则不能用
# 并集把更精确的收紧结论（例如纯打包项目只跑 ci-config）重新扩大。
# Tasks 里的任务名不得出现固定集合之外的名字；Tasks=@() 表示该输入只需要恒跑的
# ci-config；Full 表示保守全量（5 个任务全跑）；Context 用于未命中任务的原因说明。
$script:CiRules = @(
    # ---- CI 自身与公共契约 ----
    @{ Name = 'ci-workflow'; Pattern = '^\.github/workflows/ci\.yml$'; Full = $true; Reason = 'daily CI workflow changed; validate the whole workflow' }
    @{ Name = 'release-workflow'; Pattern = '^\.github/workflows/release-windows\.yml$'; Tasks = @(); Reason = 'release orchestration only'; Context = 'release-orchestration change; static release fixtures run in ci-config and package builds belong to the explicit release workflow' }
    @{ Name = 'unknown-workflow'; Pattern = '^\.github/workflows/'; Full = $true; Reason = 'unknown workflow file; the CI inventory allows only ci.yml and release-windows.yml' }
    @{ Name = 'github-metadata'; Pattern = '^\.github/'; Full = $true; Reason = 'GitHub configuration changed; validate the whole workflow' }
    @{ Name = 'public-ci-contract'; Pattern = '^scripts/ci/(?:plan\.psm1|get-plan\.ps1|assert-result\.ps1|check-config\.ps1|test-plan\.ps1)$'; Full = $true; Reason = 'public CI classifier/plan/result/config contract changed; validate the whole workflow' }
    # ---- 平台 CI 入口与轻量契约夹具（夹具由恒跑的 ci-config 执行，不需要平台重建）----
    @{ Name = 'platform-contract-fixture'; Pattern = '^scripts/ci/(?:android|macos)/tests/'; Tasks = @(); Reason = 'platform contract fixture only'; Context = 'platform contract fixture is executed by the always-on ci-config job; no platform rebuild is required' }
    @{ Name = 'report-contract-fixture'; Pattern = '^scripts/ci/(?:test-rust-report|test-windows-report)\.ps1$'; Tasks = @(); Reason = 'report contract fixture only'; Context = 'report contract fixture is executed by the always-on ci-config job; no platform rebuild is required' }
    @{ Name = 'rust-ci-entry'; Pattern = '^scripts/ci/(?:rust\.ps1|rust-report\.psm1)$'; Tasks = @('rust'); Reason = 'Rust CI entry/report producer is validated by the rust job' }
    @{ Name = 'windows-ci-entry'; Pattern = '^scripts/ci/(?:windows\.ps1|windows-report\.psm1)$'; Tasks = @('windows'); Reason = 'Windows CI entry/report producer is validated by the windows job' }
    @{ Name = 'android-ci-entry'; Pattern = '^scripts/ci/android\.sh$'; Tasks = @('android'); Reason = 'Android CI entry is executed by the android job' }
    @{ Name = 'android-production-python'; Pattern = '^scripts/ci/android/(?:verify_apk|osv_policy)\.py$'; Tasks = @('android'); Reason = 'Android APK/policy verifier is executed by the android job' }
    @{ Name = 'macos-ci-entry'; Pattern = '^scripts/ci/macos\.sh$'; Tasks = @('macos'); Reason = 'macOS CI entry is executed by the macos job' }
    @{ Name = 'macos-production-script'; Pattern = '^scripts/ci/macos/(?:probe_ffi\.c|validate_xcodebuild_list\.py|verify_embedded_dylib\.sh)$'; Tasks = @('macos'); Reason = 'macOS build/probe helper is executed by the macos job' }
    # ---- 纯打包/发布输入：日常 CI 不构建安装器，真实构建只在手动发布入口 ----
    @{ Name = 'packaging-ci-script'; Pattern = '^scripts/ci/(?:ClipBridge\.Packaging\.psm1|setup-wix\.ps1|sign-windows\.ps1|test-packaging\.ps1|test-release-[^/]+\.ps1)$'; Tasks = @(); Reason = 'packaging/release-only input'; Context = 'packaging/release-only change; real package builds run only through the explicit release workflow' }
    @{ Name = 'packaging-release-script'; Pattern = '^scripts/release/'; Tasks = @(); Reason = 'release orchestration only'; Context = 'release-orchestration change; real package builds run only through the explicit release workflow' }
    @{ Name = 'packaging-windows-script'; Pattern = '^scripts/build-windows-(?:installers|all|machine-msi|msi|user-exe|portable-zip)\.ps1$'; Tasks = @(); Reason = 'Windows packaging entry only'; Context = 'Windows installer packaging change; real package builds run only through the explicit release workflow' }
    @{ Name = 'packaging-wix-project'; Pattern = '^platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS\.(?:Installer|Bundle)/'; Tasks = @(); Reason = 'WiX installer/bundle project only'; Context = 'WiX installer/bundle project change; real package builds run only through the explicit release workflow' }
    @{ Name = 'scripts-readme'; Pattern = '^scripts/README\.md$'; Tasks = @(); Reason = 'documentation only'; Context = 'documentation-only change; no build is required' }
    # ---- 共享 Rust / ABI：所有平台消费者 ----
    @{ Name = 'shared-c-abi'; Pattern = '^crates/cb_core_ffi/'; Tasks = @('rust', 'windows', 'android', 'macos'); Reason = 'shared C ABI implementation is consumed by every platform' }
    @{ Name = 'shared-rust-core'; Pattern = '^cb_core/'; Tasks = @('rust', 'windows', 'android', 'macos'); Reason = 'shared Rust core (cb_core) is a build input of every platform' }
    @{ Name = 'shared-rust-crates'; Pattern = '^crates/'; Tasks = @('rust', 'windows', 'android', 'macos'); Reason = 'shared Rust crates are a build input of every platform' }
    @{ Name = 'root-rust-manifest'; Pattern = '^(?:Cargo\.toml|Cargo\.lock)$'; Tasks = @('rust', 'windows', 'android', 'macos'); Reason = 'root Rust manifest/lockfile changes dependency resolution for every platform build' }
    @{ Name = 'cargo-config'; Pattern = '^\.cargo/'; Tasks = @('rust', 'windows', 'android', 'macos'); Reason = 'cargo configuration affects every Rust build' }
    @{ Name = 'rust-toolchain'; Pattern = '^rust-toolchain(?:\.toml)?$'; Tasks = @('rust', 'windows', 'android', 'macos'); Reason = 'Rust toolchain pin changes every Rust build' }
    @{ Name = 'rust-policy'; Pattern = '^(?:rustfmt\.toml|deny\.toml)$'; Tasks = @('rust'); Reason = 'Rust formatting/dependency policy is enforced by the rust job' }
    @{ Name = 'ffi-surface-gate'; Pattern = '^scripts/verify-ffi-surface\.ps1$'; Tasks = @('rust'); Reason = 'shared FFI surface gate is executed by the rust job' }
    # ---- 工具链 / 编辑器配置 ----
    @{ Name = 'dotnet-toolchain'; Pattern = '^global\.json$'; Tasks = @('windows'); Reason = '.NET SDK pin is consumed by the Windows build' }
    @{ Name = 'editorconfig'; Pattern = '^\.editorconfig$'; Tasks = @('windows'); Reason = '.editorconfig drives the dotnet format gate' }
    # ---- 单平台 FFI 实现 / 头文件：rust + 对应消费者，不自动扩大 ----
    @{ Name = 'windows-core-ffi'; Pattern = '^platforms/windows/core-ffi/'; Tasks = @('rust', 'windows'); Reason = 'Windows FFI implementation is consumed by the rust and windows jobs' }
    @{ Name = 'android-core-ffi'; Pattern = '^platforms/android/core-ffi/'; Tasks = @('rust', 'android'); Reason = 'Android FFI implementation is consumed by the rust and android jobs' }
    @{ Name = 'macos-core-ffi'; Pattern = '^platforms/macos/core-ffi/'; Tasks = @('rust', 'macos'); Reason = 'macOS FFI implementation is consumed by the rust and macos jobs' }
    @{ Name = 'windows-abi-header'; Pattern = '^platforms/windows/include/'; Tasks = @('rust', 'windows'); Reason = 'Windows C ABI header is checked by the shared rust gate and the windows consumer' }
    @{ Name = 'android-abi-header'; Pattern = '^platforms/android/include/'; Tasks = @('rust', 'android'); Reason = 'Android C ABI header is checked by the shared rust gate and the android consumer' }
    @{ Name = 'macos-abi-header'; Pattern = '^platforms/macos/include/'; Tasks = @('rust', 'macos'); Reason = 'macOS C ABI header is checked by the shared rust gate and the macos consumer' }
    @{ Name = 'windows-manifest'; Pattern = '^platforms/windows/.*(?:Cargo\.toml|Cargo\.lock)$'; Tasks = @('rust', 'windows'); Reason = 'nested Windows Rust manifest changes the rust and windows builds' }
    @{ Name = 'android-manifest'; Pattern = '^platforms/android/.*(?:Cargo\.toml|Cargo\.lock)$'; Tasks = @('rust', 'android'); Reason = 'nested Android Rust manifest changes the rust and android builds' }
    @{ Name = 'macos-manifest'; Pattern = '^platforms/macos/.*(?:Cargo\.toml|Cargo\.lock)$'; Tasks = @('rust', 'macos'); Reason = 'nested macOS Rust manifest changes the rust and macos builds' }
    # ---- 平台生产/测试/资源输入（不构建安装器）----
    @{ Name = 'windows-input'; Pattern = '^platforms/windows/'; Tasks = @('windows'); Reason = 'Windows shell/build/test input is validated by the windows job' }
    @{ Name = 'android-input'; Pattern = '^platforms/android/'; Tasks = @('android'); Reason = 'Android shell/build input is validated by the android job' }
    @{ Name = 'macos-input'; Pattern = '^platforms/macos/'; Tasks = @('macos'); Reason = 'macOS shell/build input is validated by the macos job' }
    @{ Name = 'windows-version-script'; Pattern = '^scripts/version\.ps1$'; Tasks = @('windows'); Reason = 'version/build validation is owned by the windows job' }
    @{ Name = 'android-build-script'; Pattern = '^scripts/(?:check-android-shell-contracts\.sh|setup-ndk\.ps1)$'; Tasks = @('android'); Reason = 'Android build/contract script is executed by the android job' }
    @{ Name = 'macos-build-script'; Pattern = '^scripts/(?:build-macos-ffi\.sh|check-macos-shell-env\.sh)$'; Tasks = @('macos'); Reason = 'macOS build/contract script is executed by the macos job' }
)

function Get-CiTaskNames {
    return @($script:CiTasks)
}

function Get-CiWorkflowJobNames {
    return @($script:CiWorkflowJobs)
}

function Get-CiActionPins {
    return $script:CiActionPins
}

function Get-CiToolPins {
    return $script:CiToolPins
}

function Protect-CiText {
    param([AllowNull()][string]$Text)
    if ($null -eq $Text) { return '' }
    $clean = ($Text -replace '[\x00-\x1F\x7F]', ' ').Trim()
    if ($clean.Length -gt 300) { $clean = $clean.Substring(0, 297) + '...' }
    return $clean
}

function ConvertTo-CiPath {
    param([AllowNull()][string]$Path)
    if ($null -eq $Path) { return '' }
    # 不 Trim：首尾空格是路径身份的一部分，不能把未知路径悄悄变成已知路径。
    $p = $Path.Replace('\', '/')
    while ($p.StartsWith('./')) { $p = $p.Substring(2) }
    return $p
}

function Test-CiPattern {
    param([string]$Path, [string]$Pattern)
    return [regex]::IsMatch($Path, $Pattern, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
}

function Test-CiCommitSha {
    param([AllowNull()][string]$Commit)
    if ([string]::IsNullOrWhiteSpace($Commit)) { return $false }
    return $Commit -cmatch '^(?:[0-9a-f]{40}|[0-9a-f]{64})$'
}

function Get-CiPropertyValue {
    param([AllowNull()]$Object, [string]$Name)
    if ($null -eq $Object) { return $null }
    $value = $null
    if ($Object -is [System.Collections.IDictionary]) {
        if (-not $Object.Contains($Name)) { return $null }
        $value = $Object[$Name]
    } else {
        $prop = $Object.PSObject.Properties[$Name]
        if ($null -eq $prop) { return $null }
        $value = $prop.Value
    }
    # 返回数组时加逗号避免 PowerShell 把空数组/单元素数组展开。
    if ($value -is [System.Array]) { return ,$value }
    return $value
}

function Get-CiObjectKeys {
    param([AllowNull()]$Object)
    if ($null -eq $Object) { return @() }
    if ($Object -is [System.Collections.IDictionary]) {
        return @($Object.Keys | ForEach-Object { [string]$_ })
    }
    return @($Object.PSObject.Properties | ForEach-Object { $_.Name })
}

function New-CiPlanObject {
    param(
        [Parameter(Mandatory)][string]$Commit,
        [Parameter(Mandatory)][string]$Scope,
        [Parameter(Mandatory)][System.Collections.IDictionary]$Required,
        [Parameter(Mandatory)][System.Collections.IDictionary]$Reasons
    )
    $requiredOut = [ordered]@{}
    foreach ($task in $script:CiTasks) {
        $requiredOut[$task] = [bool]$Required[$task]
    }
    $reasonsOut = [ordered]@{}
    foreach ($task in $script:CiTasks) {
        $values = @()
        if ($Reasons.Contains($task)) { $values = @($Reasons[$task]) }
        $values = @($values | ForEach-Object { Protect-CiText ([string]$_) } | Where-Object { $_ -ne '' })
        if ($values.Count -eq 0) {
            $values = @('no reason recorded')
        } elseif ($values.Count -gt 20) {
            # plan 要作为 GitHub job output 传递；保留可读摘要并限制体积。
            $extra = $values.Count - 20
            $values = @($values | Select-Object -First 20) + @("(+$extra more matching input reason(s))")
        }
        $reasonsOut[$task] = $values
    }
    $plan = [pscustomobject][ordered]@{
        schemaVersion = $script:CiSchemaVersion
        scope         = $Scope
        commit        = $Commit
        required      = $requiredOut
        reasons       = $reasonsOut
    }
    return $plan
}

function New-CiFullPlan {
    param(
        [Parameter(Mandatory)][string]$Commit,
        [AllowNull()][string]$Reason
    )
    if (-not (Test-CiCommitSha $Commit)) { throw "full plan 需要完整的小写提交 SHA，实际: $Commit" }
    $why = Protect-CiText $Reason
    if (-not $why) { $why = 'scope=full requires every validation task; no installer is built in daily CI' }
    $required = [ordered]@{}
    $reasons = [ordered]@{}
    foreach ($task in $script:CiTasks) {
        $required[$task] = $true
        if ($task -eq 'ci-config') {
            $reasons[$task] = @('always: workflow syntax, script syntax, contract and doc-link checks')
        } else {
            $reasons[$task] = @("scope=full: $why")
        }
    }
    return New-CiPlanObject -Commit $Commit -Scope 'full' -Required $required -Reasons $reasons
}

function ConvertFrom-GitDiffNameStatusZ {
    <#
        Parse `git diff --name-status -z` 的 NUL 分隔输出。
        返回对象：Status / Path（新路径或唯一路径）/ OldPath（重命名/复制时）。
    #>
    param([AllowNull()][string]$Text)
    $result = [System.Collections.Generic.List[object]]::new()
    if ([string]::IsNullOrEmpty($Text)) { return @() }
    $tokens = $Text -split "`0"
    $i = 0
    while ($i -lt $tokens.Count) {
        $status = $tokens[$i]
        $i++
        if ([string]::IsNullOrEmpty($status)) { continue }
        if ($status -notmatch '^[A-Z][0-9]*$') {
            throw "无法解析 git diff status: '$status'"
        }
        if ($status -match '^[RC][0-9]*$') {
            if ($i + 1 -ge $tokens.Count) { throw "git diff rename 记录缺少路径: $status" }
            $oldPath = ConvertTo-CiPath $tokens[$i]
            $newPath = ConvertTo-CiPath $tokens[$i + 1]
            $i += 2
            $result.Add([pscustomobject]@{ Status = $status; Path = $newPath; OldPath = $oldPath })
        } else {
            if ($i -ge $tokens.Count) { throw "git diff 记录缺少路径: $status" }
            $path = ConvertTo-CiPath $tokens[$i]
            $i++
            $result.Add([pscustomobject]@{ Status = $status; Path = $path; OldPath = $null })
        }
    }
    return @($result)
}

function Get-CiChangedPaths {
    param(
        [Parameter(Mandatory)][string]$BaseSha,
        [Parameter(Mandatory)][string]$HeadSha,
        [Parameter(Mandatory)][string]$RepoRoot
    )
    if (-not (Test-CiCommitSha $BaseSha)) { throw "diff base 不是完整 SHA: $BaseSha" }
    if (-not (Test-CiCommitSha $HeadSha)) { throw "diff head 不是完整 SHA: $HeadSha" }
    if ($BaseSha -cmatch '^0+$') { throw 'diff base 是空提交（push 新建分支或浅历史）' }
    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ('clipbridge-ci-diff-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
    try {
        $outFile = Join-Path $tempDir 'diff.bin'
        & git -C $RepoRoot -c core.quotepath=false diff --name-status -z --find-renames --no-ext-diff "--output=$outFile" $BaseSha $HeadSha
        if ($LASTEXITCODE -ne 0) { throw "git diff 失败（exit $LASTEXITCODE）" }
        $text = [System.IO.File]::ReadAllText($outFile, [System.Text.Encoding]::UTF8)
        $entries = ConvertFrom-GitDiffNameStatusZ -Text $text
        $paths = [System.Collections.Generic.List[string]]::new()
        foreach ($entry in $entries) {
            if ($entry.OldPath) { $paths.Add($entry.OldPath) }
            if ($entry.Path) { $paths.Add($entry.Path) }
        }
        return @($paths | Sort-Object -Unique)
    } finally {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Get-CiImpact {
    <#
        纯函数：路径集合 -> required / reasons / unknown / localOnly / docOnly。
        每条路径只在首个命中的规则处生效（规则按具体到宽泛排序），不同路径取并集。
        不访问 git、网络或工作区其他状态。
    #>
    param(
        [AllowEmptyCollection()][string[]]$Paths,
        [Parameter(Mandatory)][string]$Commit
    )
    if (-not (Test-CiCommitSha $Commit)) { throw "需要完整的小写提交 SHA，实际: $Commit" }
    $normalized = @(
        @($Paths) |
            ForEach-Object { ConvertTo-CiPath $_ } |
            Where-Object { $_ -ne '' } |
            Sort-Object -Unique
    )

    $required = [ordered]@{}
    foreach ($task in $script:CiTasks) { $required[$task] = $false }
    $required['ci-config'] = $true
    $reasonsByTask = @{}
    foreach ($task in $script:CiTasks) { $reasonsByTask[$task] = [System.Collections.Generic.List[string]]::new() }
    $unknown = [System.Collections.Generic.List[string]]::new()
    $localOnly = [System.Collections.Generic.List[string]]::new()
    $docOnly = [System.Collections.Generic.List[string]]::new()
    $fullTriggers = [System.Collections.Generic.List[string]]::new()
    $contexts = [System.Collections.Generic.List[string]]::new()

    foreach ($path in $normalized) {
        # 专用本地渠道文件（精确路径）先识别；宽泛的 msix/winget 子串会吞掉真实源码，
        # 因此只允许这里列出的精确文件/目录，其它 msix/winget 命名路径继续走构建规则。
        $isLocal = $false
        foreach ($pattern in $script:CiLocalOnlyPatterns) {
            if (Test-CiPattern $path $pattern) { $isLocal = $true; break }
        }
        if ($isLocal) {
            $localOnly.Add($path)
            $contexts.Add('local-only channel (MSIX/StoreUpload/winget) input is excluded from GitHub CI/build')
            continue
        }

        $matched = $false
        foreach ($rule in $script:CiRules) {
            if (-not (Test-CiPattern $path $rule.Pattern)) { continue }
            $matched = $true
            if ($rule.ContainsKey('Full') -and [bool]$rule.Full) {
                $fullTriggers.Add("$($rule.Name): $path")
            }
            if ($rule.ContainsKey('Tasks')) {
                foreach ($task in @($rule.Tasks)) {
                    if ($script:CiTasks -notcontains $task) { throw "规则 $($rule.Name) 引用了未知任务 $task" }
                    $required[$task] = $true
                    $reason = if ($rule.ContainsKey('Reason')) { $rule.Reason } else { $rule.Name }
                    $reasonsByTask[$task].Add("$($rule.Name): $reason ($path)")
                }
            }
            if ($rule.ContainsKey('Context')) { $contexts.Add([string]$rule.Context) }
            break
        }
        if ($matched) { continue }

        # 显式文档目录/根文档才免重型检查；其他 .md 若未被构建规则命中则按未知路径全跑。
        $isDoc = $false
        foreach ($pattern in $script:CiDocOnlyPatterns) {
            if (Test-CiPattern $path $pattern) { $isDoc = $true; break }
        }
        if ($isDoc) {
            $docOnly.Add($path)
            $contexts.Add('documentation/non-build repository input; no platform validation is required')
            continue
        }

        $unknown.Add($path)
    }

    if ($unknown.Count -gt 0) {
        $shown = @($unknown | Select-Object -First 8)
        $why = 'unclassified path(s) force full validation: ' + ($shown -join ', ')
        if ($unknown.Count -gt $shown.Count) { $why += " (+$($unknown.Count - $shown.Count) more)" }
        return New-CiFullPlan -Commit $Commit -Reason $why
    }
    if ($fullTriggers.Count -gt 0) {
        $shown = @($fullTriggers | Select-Object -First 6)
        return New-CiFullPlan -Commit $Commit -Reason ('full validation trigger(s): ' + ($shown -join '; '))
    }

    $context = ''
    $hasBuildInput = $false
    foreach ($task in $script:CiTasks) {
        if ($task -ne 'ci-config' -and $required[$task]) { $hasBuildInput = $true; break }
    }
    if (-not $hasBuildInput) {
        if ($normalized.Count -eq 0) {
            $context = 'no changed paths; only the always-on lightweight checks are required'
        } elseif ($contexts.Count -gt 0) {
            $context = (@($contexts | Select-Object -Unique) -join '; ')
        }
    }

    $reasons = [ordered]@{}
    foreach ($task in $script:CiTasks) {
        if ($task -eq 'ci-config') {
            $reasons[$task] = @('always: workflow syntax, script syntax, contract and doc-link checks')
        } elseif ($required[$task]) {
            $values = @($reasonsByTask[$task] | Select-Object -Unique)
            if ($values.Count -eq 0) { $values = @('required by the changed inputs') }
            $reasons[$task] = $values
        } else {
            if ($context) {
                $reasons[$task] = @("not required: $context")
            } else {
                $reasons[$task] = @('not required: no matching build input in this change set')
            }
        }
    }

    return New-CiPlanObject -Commit $Commit -Scope 'affected' -Required $required -Reasons $reasons
}

function Get-CiPlanFromChangedPaths {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Paths,
        [Parameter(Mandatory)][string]$Commit
    )
    return Get-CiImpact -Paths $Paths -Commit $Commit
}

function Get-CiPlanErrors {
    param([AllowNull()]$Plan)
    $errors = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Plan) {
        $errors.Add('plan is null/empty')
        return @($errors)
    }
    $isPlanObject = ($Plan -is [System.Collections.IDictionary]) -or (@(Get-CiObjectKeys $Plan) -contains 'schemaVersion')
    if (-not $isPlanObject) {
        $errors.Add('plan must be a JSON object')
        return @($errors)
    }

    $allowed = @('schemaVersion', 'scope', 'commit', 'required', 'reasons')
    $keys = @(Get-CiObjectKeys $Plan)
    foreach ($key in $keys) {
        if ($allowed -notcontains $key) {
            if ($key -eq 'packages') {
                $errors.Add("obsolete top-level field 'packages': schemaVersion 2 plans carry no package list")
            } else {
                $errors.Add("unknown top-level field '$key'")
            }
        }
    }
    foreach ($key in $allowed) {
        if ($keys -notcontains $key) { $errors.Add("missing top-level field '$key'") }
    }

    $schema = Get-CiPropertyValue $Plan 'schemaVersion'
    if (-not (($schema -is [int]) -or ($schema -is [long]) -or ($schema -is [int64]))) {
        $errors.Add('schemaVersion must be an integer')
    } elseif ([int64]$schema -ne $script:CiSchemaVersion) {
        $errors.Add("unsupported schemaVersion '$schema' (expected $($script:CiSchemaVersion))")
    }

    $scope = Get-CiPropertyValue $Plan 'scope'
    if (($scope -isnot [string]) -or ($scope -notin @('affected', 'full'))) {
        $errors.Add("scope must be 'affected' or 'full'")
    }

    $commit = Get-CiPropertyValue $Plan 'commit'
    if (($commit -isnot [string]) -or ($commit -cnotmatch '^(?:[0-9a-f]{40}|[0-9a-f]{64})$')) {
        $errors.Add('commit must be a full lowercase commit SHA (40 or 64 hex characters)')
    }

    $required = Get-CiPropertyValue $Plan 'required'
    $requiredKeys = @(Get-CiObjectKeys $required)
    if ($null -eq $required -or ($required -isnot [System.Collections.IDictionary] -and $required -isnot [pscustomobject])) {
        $errors.Add('required must be an object')
    } else {
        foreach ($key in $requiredKeys) {
            if ($script:CiTasks -notcontains $key) { $errors.Add("required has unknown task '$key'") }
        }
        foreach ($task in $script:CiTasks) {
            if ($requiredKeys -notcontains $task) {
                $errors.Add("required is missing fixed task '$task'")
                continue
            }
            $value = Get-CiPropertyValue $required $task
            if ($value -isnot [bool]) { $errors.Add("required['$task'] must be a boolean") }
        }
        $ciConfig = Get-CiPropertyValue $required 'ci-config'
        if ($ciConfig -is [bool] -and -not $ciConfig) { $errors.Add("required['ci-config'] must always be true") }
    }

    $reasons = Get-CiPropertyValue $Plan 'reasons'
    $reasonKeys = @(Get-CiObjectKeys $reasons)
    if ($null -eq $reasons -or ($reasons -isnot [System.Collections.IDictionary] -and $reasons -isnot [pscustomobject])) {
        $errors.Add('reasons must be an object')
    } else {
        foreach ($key in $reasonKeys) {
            if ($script:CiTasks -notcontains $key) { $errors.Add("reasons has unknown task '$key'") }
        }
        foreach ($task in $script:CiTasks) {
            if ($reasonKeys -notcontains $task) {
                $errors.Add("reasons is missing fixed task '$task'")
                continue
            }
            $values = Get-CiPropertyValue $reasons $task
            if ($values -isnot [System.Collections.IList] -or $values -is [string]) {
                $errors.Add("reasons['$task'] must be an array of non-empty strings")
                continue
            }
            if (@($values).Count -eq 0) { $errors.Add("reasons['$task'] must not be empty") }
            foreach ($value in @($values)) {
                if ($value -isnot [string]) { $errors.Add("reasons['$task'] contains a non-string value"); continue }
                if ([string]::IsNullOrWhiteSpace($value)) { $errors.Add("reasons['$task'] contains an empty string"); continue }
                if ($value -match '[\x00-\x1F]') { $errors.Add("reasons['$task'] contains control characters") }
            }
        }
    }

    if ($errors.Count -eq 0) {
        if ($scope -eq 'full') {
            foreach ($task in $script:CiTasks) {
                if (-not [bool](Get-CiPropertyValue $required $task)) {
                    $errors.Add("scope=full requires required['$task']=true")
                }
            }
        }
    }

    return @($errors)
}

function Assert-CiPlan {
    param([AllowNull()]$Plan, [string]$Source = 'plan')
    $errors = @(Get-CiPlanErrors $Plan)
    if ($errors.Count -gt 0) {
        throw ("invalid ${Source}:`n - " + ($errors -join "`n - "))
    }
    return $Plan
}

function ConvertFrom-CiPlanJson {
    param(
        [Parameter(Mandatory)][AllowNull()][AllowEmptyString()][string]$Json,
        [string]$Source = 'plan JSON'
    )
    if ([string]::IsNullOrWhiteSpace($Json)) { throw "$Source is empty" }
    try {
        $plan = $Json | ConvertFrom-Json -Depth 16
    } catch {
        throw "invalid ${Source}: $($_.Exception.Message)"
    }
    if ($null -eq $plan -or $plan -is [System.Array]) {
        throw "invalid ${Source}: plan must be a JSON object"
    }
    return Assert-CiPlan -Plan $plan -Source $Source
}

function Read-CiPlan {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { throw "plan file not found: $Path" }
    return ConvertFrom-CiPlanJson -Json ([System.IO.File]::ReadAllText($Path)) -Source "plan file '$Path'"
}

function ConvertTo-CiPlanJson {
    param([Parameter(Mandatory)]$Plan)
    $valid = Assert-CiPlan -Plan $Plan
    return ($valid | ConvertTo-Json -Depth 16)
}

# ---------------------------------------------------------------------------
# 文档本地链接检查：只检查传入的（本次新增/修改）文档，不扫描整个文档库。
# ---------------------------------------------------------------------------

function Get-MarkdownLinkTargets {
    param([string[]]$Lines)
    $targets = [System.Collections.Generic.List[object]]::new()
    $inFence = $false
    for ($lineNo = 1; $lineNo -le $Lines.Count; $lineNo++) {
        $line = $Lines[$lineNo - 1]
        if ($line -match '^\s*(```|~~~)') { $inFence = -not $inFence; continue }
        if ($inFence) { continue }

        foreach ($match in [regex]::Matches($line, '!?\[[^\]]*\]\(\s*(?:<([^>]+)>|([^)\s]+))\s*(?:"[^"]*")?\s*\)')) {
            $target = if ($match.Groups[1].Success) { $match.Groups[1].Value } else { $match.Groups[2].Value }
            $targets.Add([pscustomobject]@{ Target = $target.Trim(); Line = $lineNo })
        }
        if ($line -match '^\s*\[[^\]]+\]:\s*(\S+)') {
            $targets.Add([pscustomobject]@{ Target = $Matches[1].Trim(); Line = $lineNo })
        }
    }
    return @($targets)
}

function Test-CiDocLinks {
    param(
        [Parameter(Mandatory)][string]$RepoRoot,
        [AllowEmptyCollection()][string[]]$DocPaths
    )
    $root = [System.IO.Path]::GetFullPath($RepoRoot).TrimEnd('\', '/')
    $errors = [System.Collections.Generic.List[string]]::new()
    $checkedFiles = 0
    $checkedLinks = 0
    $sectionAnchors = 0
    $lineCountCache = @{}

    foreach ($rawDoc in @($DocPaths)) {
        $docRel = ConvertTo-CiPath $rawDoc
        if (-not $docRel) { continue }
        $docFull = [System.IO.Path]::GetFullPath((Join-Path $root $docRel))
        if (-not (Test-Path -LiteralPath $docFull -PathType Leaf)) {
            $errors.Add("changed document does not exist: $docRel")
            continue
        }
        $checkedFiles++
        $lines = [System.IO.File]::ReadAllLines($docFull)
        $docDir = Split-Path -Parent $docFull
        foreach ($link in Get-MarkdownLinkTargets -Lines $lines) {
            $target = [string]$link.Target
            if ([string]::IsNullOrWhiteSpace($target)) { continue }
            if ($target -match '^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|//)') { continue }
            if ($target.StartsWith('#')) { $sectionAnchors++; continue }

            try { $target = [uri]::UnescapeDataString($target) } catch { }
            $hashIndex = $target.IndexOf('#')
            $fragment = ''
            if ($hashIndex -ge 0) {
                $fragment = $target.Substring($hashIndex + 1)
                $target = $target.Substring(0, $hashIndex)
            }
            $queryIndex = $target.IndexOf('?')
            if ($queryIndex -ge 0) { $target = $target.Substring(0, $queryIndex) }
            if ([string]::IsNullOrWhiteSpace($target)) { $sectionAnchors++; continue }

            $isDirectory = $target.EndsWith('/')
            $resolved = if ($target.StartsWith('/')) {
                [System.IO.Path]::GetFullPath((Join-Path $root $target.TrimStart('/')))
            } else {
                [System.IO.Path]::GetFullPath((Join-Path $docDir $target))
            }
            $resolvedNormalized = $resolved.Replace('\', '/')
            if ($resolvedNormalized -ne $root.Replace('\', '/') -and
                -not $resolvedNormalized.StartsWith($root.Replace('\', '/') + '/')) {
                $errors.Add("[${docRel}:$($link.Line)] local link escapes the repository: $($link.Target)")
                continue
            }

            $exists = if ($isDirectory) {
                Test-Path -LiteralPath $resolved -PathType Container
            } else {
                Test-Path -LiteralPath $resolved -PathType Leaf
            }
            if (-not $exists) {
                $errors.Add("[${docRel}:$($link.Line)] local link target does not exist: $($link.Target)")
                continue
            }
            $checkedLinks++

            if ($fragment -and $fragment -match '^L(\d+)(?:-L?(\d+))?$' -and -not $isDirectory) {
                $start = [int]$Matches[1]
                $end = if ($Matches[2]) { [int]$Matches[2] } else { $start }
                if (-not $lineCountCache.ContainsKey($resolved)) {
                    $lineCountCache[$resolved] = [System.IO.File]::ReadAllLines($resolved).Count
                }
                $total = [int]$lineCountCache[$resolved]
                if ($start -lt 1 -or $end -lt $start -or $end -gt $total) {
                    $errors.Add("[${docRel}:$($link.Line)] line target L$($Matches[1])-L$($Matches[2]) is outside $($link.Target) (file has $total lines)")
                }
            } elseif ($fragment) {
                $sectionAnchors++
            }
        }
    }

    return [pscustomobject]@{
        Errors              = @($errors)
        CheckedFiles        = $checkedFiles
        CheckedLinks        = $checkedLinks
        UnverifiedAnchors   = $sectionAnchors
    }
}

# ---------------------------------------------------------------------------
# 工作流结构契约：固定 job 集合、plan 单源条件、needs、超时、权限、并发与 pin。
# 只用轻量文本结构检查；不执行 workflow 内命令。
# ---------------------------------------------------------------------------

function Get-CiWorkflowJobBlocks {
    param([Parameter(Mandatory)][string]$Content)
    $lines = $Content -split "`r?`n"
    $jobsIndex = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^jobs:\s*$') { $jobsIndex = $i; break }
    }
    if ($jobsIndex -lt 0) { return @{} }
    $blocks = [ordered]@{}
    $current = $null
    $buffer = [System.Collections.Generic.List[string]]::new()
    for ($i = $jobsIndex + 1; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match '^\S') { break }
        if ($line -match '^  ([A-Za-z0-9_-]+):\s*(?:#.*)?$') {
            if ($null -ne $current) { $blocks[$current] = ($buffer -join "`n") }
            $current = $Matches[1]
            $buffer = [System.Collections.Generic.List[string]]::new()
            continue
        }
        if ($null -ne $current) { $buffer.Add($line) }
    }
    if ($null -ne $current) { $blocks[$current] = ($buffer -join "`n") }
    return $blocks
}

function Get-CiJobNeedSet {
    param([string]$Block)
    $match = [regex]::Match($Block, '(?m)^\s+needs:\s*(?<value>.+)$')
    if (-not $match.Success) { return @() }
    $value = ($match.Groups['value'].Value -replace '#.*$', '').Trim()
    return @([regex]::Matches($value, '[A-Za-z0-9_-]+') | ForEach-Object { $_.Value } | Where-Object { $_ -notin @('needs') })
}

function Get-CiWorkflowRunBlocks {
    <#
        提取 ci.yml 里 `run: |` 的脚本正文及其 shell，用于 AST / bash -n 解析。
        返回对象：Shell（pwsh/bash/其它）、Line、Text。
    #>
    param([Parameter(Mandatory)][string]$Content)
    $lines = $Content -split "`r?`n"
    $blocks = [System.Collections.Generic.List[object]]::new()
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -notmatch '^(?<indent>\s*)run:\s*\|\s*$') { continue }
        $runIndent = $Matches['indent'].Length
        $shell = 'bash'
        for ($j = $i - 1; $j -ge [Math]::Max(0, $i - 30); $j--) {
            if ($lines[$j] -match '^\s*shell:\s*(?<shell>\S+)') { $shell = $Matches['shell']; break }
            if ($lines[$j] -match '^\s*-\s+name:') { break }
        }
        $body = [System.Collections.Generic.List[string]]::new()
        $k = $i + 1
        while ($k -lt $lines.Count) {
            $line = $lines[$k]
            if ($line.Trim() -eq '') {
                $body.Add('')
                $k++
                continue
            }
            $lead = $line.Length - $line.TrimStart().Length
            if ($lead -le $runIndent) { break }
            $strip = [Math]::Min($runIndent + 2, $line.Length)
            $body.Add($line.Substring($strip))
            $k++
        }
        $blocks.Add([pscustomobject]@{
            Shell = $shell
            Line  = $i + 1
            Text  = ($body -join "`n")
        })
    }
    return @($blocks)
}

function Test-CiWorkflowContract {
    param(
        [Parameter(Mandatory)][string]$Content,
        [string]$Path = '.github/workflows/ci.yml'
    )
    $errors = [System.Collections.Generic.List[string]]::new()
    $jobs = Get-CiWorkflowJobBlocks -Content $Content
    $expectedJobs = @($script:CiWorkflowJobs)
    $jobNames = @($jobs.Keys)
    foreach ($job in $expectedJobs) {
        if ($jobNames -notcontains $job) { $errors.Add("missing fixed job '$job'") }
    }
    foreach ($job in $jobNames) {
        if ($expectedJobs -notcontains $job) { $errors.Add("unknown job '$job' (fixed job set or aggregation needs must be updated in the same change)") }
    }

    # 触发器与权限。只允许 PR / main push / 手动全量，不得用 on.paths 跳过整个工作流。
    if ($Content -notmatch '(?m)^on:\s*$') { $errors.Add("missing top-level 'on:'") }
    if ($Content -notmatch '(?m)^\s+pull_request:\s*$') { $errors.Add("missing pull_request trigger") }
    if ($Content -notmatch '(?m)^\s+push:\s*$' -or $Content -notmatch '(?m)^\s+branches:\s*\[\s*main\s*\]\s*$') { $errors.Add("missing main-only push trigger") }
    if ($Content -notmatch '(?m)^\s+workflow_dispatch:\s*$') { $errors.Add("missing workflow_dispatch trigger for manual full validation") }
    if ($Content -match '(?m)^\s+paths:\s*$') { $errors.Add("on.paths must not skip the whole workflow") }
    if ($Content -match 'pull_request_target') { $errors.Add("pull_request_target is forbidden") }
    if ($Content -match 'workflow_run') { $errors.Add("workflow_run is forbidden") }
    if ($Content -notmatch '(?m)^permissions:\s*$') { $errors.Add("missing top-level permissions") }
    if ($Content -notmatch '(?m)^\s+contents:\s*read\s*$') { $errors.Add("permissions must include contents: read") }
    if ($Content -notmatch '(?m)^concurrency:\s*$') { $errors.Add("missing concurrency block") }
    if ($Content -notmatch 'cancel-in-progress:\s*\$\{\{\s*github\.event_name\s*==\s*''pull_request''\s*\}\}') {
        $errors.Add("concurrency must cancel stale PR runs only (cancel-in-progress: github.event_name == 'pull_request')")
    }

    $codeLines = @()
    foreach ($line in ($Content -split "`r?`n")) {
        $trimmed = $line.Trim()
        if ($trimmed.StartsWith('#')) { continue }
        $codeLines += ($line -replace '\s+#.*$', '')
    }
    $code = $codeLines -join "`n"
    if ($code -match 'secrets\.') { $errors.Add("workflow must not reference secrets in ordinary CI") }
    if ($code -match 'continue-on-error:\s*true') { $errors.Add("continue-on-error: true is forbidden; fix the failing check") }
    foreach ($forbidden in @('build-windows-msix', 'build-windows-winget', 'build-winget-manifest', 'StoreUpload', 'winget validate', 'winget\s+validate')) {
        if ($code -match $forbidden) { $errors.Add("forbidden local-channel command in workflow: $forbidden") }
    }
    # 日常 CI 只编译/测试，不构建 MSI / Burn EXE / 便携 ZIP，也不进入发布流程。
    # 使用词边界匹配，避免把注释/步骤名里的 "lightweight" 当成 WiX light.exe。
    $forbiddenPackaging = [ordered]@{
        'windows-packages'        = 'windows-packages'
        'build-windows-installers' = 'build-windows-installers'
        'setup-wix'               = 'setup-wix'
        'sign-windows'            = 'sign-windows'
        'ClipBridge.Packaging'    = 'ClipBridge\.Packaging'
        'plan.packages'           = 'plan\.packages'
        'Formats'                 = '-Formats'
        'candle'                  = '(?i)\bcandle(?:\.exe)?\b'
        'light'                   = '(?i)\blight(?:\.exe)?\b'
        'heat'                    = '(?i)\bheat(?:\.exe)?\b'
    }
    foreach ($name in $forbiddenPackaging.Keys) {
        if ($code -match $forbiddenPackaging[$name]) {
            $errors.Add("daily CI must not build installers or reintroduce packaging: $name")
        }
    }
    foreach ($forbiddenRelease in @('gh\s+release', 'actions/create-release', 'softprops/action-gh-release', 'release-windows\.yml', 'Invoke-Publish\.ps1', 'Invoke-Stage\.ps1')) {
        if ($code -match $forbiddenRelease) {
            $errors.Add("daily CI must not require or run the release path: $forbiddenRelease")
        }
    }

    # Job 级检查。
    foreach ($job in $expectedJobs) {
        if ($jobNames -notcontains $job) { continue }
        $block = [string]$jobs[$job]
        if ($block -notmatch '(?m)^\s+runs-on:\s*\S+') { $errors.Add("job '$job' is missing runs-on") }
        if ($block -notmatch '(?m)^\s+timeout-minutes:\s*\d+\s*$') { $errors.Add("job '$job' is missing timeout-minutes") }
        if ($block -match '(?m)^\s+continue-on-error:\s*true') { $errors.Add("job '$job' uses continue-on-error: true") }
    }

    $changes = [string]$jobs['changes']
    if ($changes) {
        if ($changes -notmatch 'get-plan\.ps1') { $errors.Add("changes job must call scripts/ci/get-plan.ps1") }
        if ($changes -notmatch '(?m)^\s+outputs:\s*$' -or $changes -notmatch '(?m)^\s+plan:\s*\$\{\{\s*steps\.[^}]+\}\}\s*$') { $errors.Add("changes job must expose the structured plan output") }
    }

    foreach ($task in $script:CiTasks) {
        if ($jobNames -notcontains $task) { continue }
        $block = [string]$jobs[$task]
        $needs = @(Get-CiJobNeedSet $block)
        if ($needs -notcontains 'changes') { $errors.Add("job '$task' must need changes") }
        if ($block -notmatch [regex]::Escape("required['$task']")) {
            $errors.Add("job '$task' condition must read plan.required['$task']")
        }
        if ($block -notmatch 'fromJSON\s*\(\s*needs\.changes\.outputs\.plan\s*\)') {
            $errors.Add("job '$task' must take its condition from the changes plan output")
        }
    }

    foreach ($entry in @('scripts/ci/rust.ps1', 'scripts/ci/windows.ps1', 'scripts/ci/android.sh', 'scripts/ci/macos.sh')) {
        if ($Content -notmatch [regex]::Escape($entry)) { $errors.Add("platform job must call $entry") }
    }
    if ($Content -notmatch 'test-plan\.ps1') { $errors.Add("ci-config must run the public plan/classification contract tests") }
    if ($Content -notmatch 'test-rust-report\.ps1') { $errors.Add("ci-config must run the rust report contract tests") }
    if ($Content -notmatch 'test-windows-report\.ps1') { $errors.Add("ci-config must run the windows report contract tests") }
    if ($Content -notmatch 'test-packaging\.ps1') { $errors.Add("ci-config must run the packaging contract tests") }
    # release 套件使用固定四个文件名（不再用 glob）：任一缺失即失败；运行时由 ci.yml
    # 的 Test-Path/throw 保证“没有测试也是 fatal”。
    foreach ($releaseSuite in @(
            'test-release-model.ps1',
            'test-release-github-api.ps1',
            'test-release-candidate.ps1',
            'test-release-workflow.ps1'
        )) {
        if ($Content -notmatch [regex]::Escape($releaseSuite)) {
            $errors.Add("ci-config must run the fixed release contract suite '$releaseSuite'")
        }
    }
    if ($Content -notmatch 'missing required contract suite') {
        $errors.Add("ci-config release contract runner must fail when a required suite is missing")
    }
    if ($Content -notmatch 'scripts/ci/android/tests/run-tests\.sh' -or $Content -notmatch 'scripts/ci/macos/tests/run-tests\.sh') {
        $errors.Add("ci-config must run the Android and macOS lightweight contract tests")
    }
    if ($Content -notmatch 'test -f scripts/ci/android/tests/run-tests\.sh' -or $Content -notmatch 'test -f scripts/ci/macos/tests/run-tests\.sh') {
        $errors.Add("platform lightweight test scripts must be mandatory, not conditionally skipped")
    }
    # v3.2.2 的隐式 packages 默认值包含 Google 已移除的 tools 包。
    # action 只负责 sdkmanager/licenses；依赖仍由下面的固定 SDK 安装步骤提供。
    if ($code -notmatch '(?m)^\s*(?:-\s+)?uses:\s*android-actions/setup-android@[^\r\n]+\r?\n\s+with:\s*\r?\n\s+packages:\s*(?:''''|"")[ \t]*$') {
        $errors.Add("setup-android must explicitly use packages: '' to avoid the removed tools package; install pinned SDK dependencies in the following step")
    }
    foreach ($androidPin in @('platforms;android-36', 'build-tools;36.0.0', 'ndk;26.3.11579264', 'cmake;3.22.1')) {
        if ($code -notmatch [regex]::Escape($androidPin)) { $errors.Add("Android SDK package pin is missing from the setup step: $androidPin") }
    }
    if ($code -notmatch 'sdkmanager\s+--install(?!-)') { $errors.Add("Android job must install the pinned SDK packages before the platform entry") }

    $result = [string]$jobs['ci-result']
    if ($result) {
        $needs = @(Get-CiJobNeedSet $result)
        foreach ($requiredNeed in @('changes') + $script:CiTasks) {
            if ($needs -notcontains $requiredNeed) { $errors.Add("ci-result needs must include '$requiredNeed'") }
        }
        foreach ($need in $needs) {
            if ((@('changes') + $script:CiTasks) -notcontains $need) { $errors.Add("ci-result needs has unexpected job '$need'") }
        }
        if ($result -notmatch 'always\s*\(\s*\)') { $errors.Add("ci-result must run with always()") }
        if ($result -notmatch 'assert-result\.ps1') { $errors.Add("ci-result must call scripts/ci/assert-result.ps1") }
        if ($result -notmatch 'GITHUB_STEP_SUMMARY') { $errors.Add("ci-result must write the run summary") }
    }

    # 第三方 Action 必须 pin 到已核实的完整 SHA，并保留版本注释。
    foreach ($line in ($Content -split "`r?`n")) {
        if ($line -notmatch '^\s*(?:-\s+)?uses:\s*(?<spec>\S+)(?<comment>.*)$') { continue }
        $spec = $Matches['spec']
        $comment = $Matches['comment']
        if ($spec.StartsWith('./') -or $spec.StartsWith('../')) { continue }
        if ($spec -notmatch '^(?<repo>[^@]+)@(?<sha>[0-9a-f]+)$') {
            $errors.Add("action must be pinned to a full commit SHA: $spec")
            continue
        }
        $repo = $Matches['repo']
        $sha = $Matches['sha']
        if (-not $script:CiActionPins.Contains($repo)) {
            $errors.Add("action '$repo' is not in the verified pin allowlist")
            continue
        }
        $expected = $script:CiActionPins[$repo]
        if ($sha -ne $expected.Sha) {
            $errors.Add("action '$repo' must use SHA $($expected.Sha) ($($expected.Version)), got $sha")
        }
        if ($comment -notmatch [regex]::Escape($expected.Version)) {
            $errors.Add("action '$repo' must keep the version comment $($expected.Version)")
        }
    }

    # 下载工具必须固定版本 + 摘要。
    if ($Content -notmatch [regex]::Escape("actionlint_$($script:CiToolPins.ActionlintVersion)_linux_amd64.tar.gz")) {
        $errors.Add("ci-config must download actionlint $($script:CiToolPins.ActionlintVersion) (linux amd64)")
    }
    if ($Content -notmatch [regex]::Escape($script:CiToolPins.ActionlintLinuxAmd64Sha256)) {
        $errors.Add("ci-config must verify the pinned actionlint SHA256")
    }
    if ($Content -notmatch 'global-json-file:\s*global\.json') {
        $errors.Add("Windows jobs must install the .NET SDK from global.json")
    }

    return [pscustomobject]@{
        Errors = @($errors)
        Jobs   = @($jobNames)
    }
}

Export-ModuleMember -Function @(
    'Get-CiTaskNames',
    'Get-CiWorkflowJobNames',
    'Get-CiActionPins',
    'Get-CiToolPins',
    'Protect-CiText',
    'ConvertTo-CiPath',
    'Test-CiCommitSha',
    'ConvertFrom-GitDiffNameStatusZ',
    'Get-CiChangedPaths',
    'Get-CiImpact',
    'Get-CiPlanFromChangedPaths',
    'Get-CiPlanErrors',
    'Assert-CiPlan',
    'ConvertFrom-CiPlanJson',
    'Read-CiPlan',
    'ConvertTo-CiPlanJson',
    'New-CiFullPlan',
    'Test-CiDocLinks',
    'Get-CiWorkflowJobBlocks',
    'Get-CiJobNeedSet',
    'Get-CiWorkflowRunBlocks',
    'Test-CiWorkflowContract'
)
