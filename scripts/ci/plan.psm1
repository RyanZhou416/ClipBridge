#requires -version 7.0
<#
    ClipBridge CI 变更分类 / 计划契约 / 汇总契约的单一实现。

    本模块是 §4.2/§4.4 的唯一分类来源：只在这里把“变更路径 + 事件”映射成固定
    任务/包集合，工作流 job 条件与 ci-result 汇总都读取同一份 plan JSON。

    契约（严格）：
      schemaVersion = 1
      scope         = affected | full
      commit        = 当前实际检出的完整小写提交 SHA（40 或 64 个十六进制字符）
      required      = 固定 6 键布尔 map，ci-config 恒为 true
      packages      = machine-msi | user-exe | portable-zip 的去重数组
      reasons       = 与 required 同键的非空字符串数组
    不变量：
      packages 非空 <=> required.windows-packages；有包时 required.windows 必须为 true。
      scope=full 时 6 个任务全部 true 且 3 种包全部列入。
      MSIX / StoreUpload / winget 是本地渠道，不允许出现在 packages 或固定任务集合中。

    注意：本文件不读取 GitHub 事件，也不调用网络；工作流负责提供 base/head/commit。
#>

Set-StrictMode -Version Latest

$script:CiSchemaVersion = 1
$script:CiTasks = @('ci-config', 'rust', 'windows', 'android', 'macos', 'windows-packages')
$script:CiPackages = @('machine-msi', 'user-exe', 'portable-zip')
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
# 大型中文文档）。任何带路径的 .md（例如 platforms/.../Assets/README.md、scripts/README.md）
# 先走构建规则；没有构建规则时就按未知路径保守全跑。
$script:CiDocOnlyPatterns = @(
    '^docs/',
    '^[^/]+\.md$'
)

# 规则是“变更输入 -> 固定任务/包集合”。命中多个规则取并集；未知路径回退 full。
# Tasks 里的任务名不得出现固定集合之外的名字；Packages 不得出现白名单之外的格式。
$allPkgs = $script:CiPackages
$script:CiRules = @(
    @{ Name = 'shared-rust-core'; Pattern = '^cb_core/'; Tasks = @('rust', 'windows', 'android', 'macos'); Packages = @(); Reason = 'shared Rust core (cb_core) is a build input of every platform' }
    @{ Name = 'shared-rust-crates'; Pattern = '^crates/'; Tasks = @('rust', 'windows', 'android', 'macos'); Packages = @(); Reason = 'shared Rust crates are a build input of every platform' }
    @{ Name = 'shared-c-abi'; Pattern = '^crates/cb_core_ffi/'; Tasks = @('rust', 'windows', 'android', 'macos'); Packages = $allPkgs; Reason = 'shared C ABI implementation is embedded in every Windows package' }
    @{ Name = 'platform-abi-headers'; Pattern = '^platforms/[^/]+/include/'; Tasks = @('rust', 'windows', 'android', 'macos'); Packages = $allPkgs; Reason = 'platform C ABI header is consumed by the Rust and native integration checks' }
    @{ Name = 'rust-manifests'; Pattern = '(^|/)(?:Cargo\.toml|Cargo\.lock)$'; Tasks = @('rust', 'windows', 'android', 'macos'); Packages = $allPkgs; Reason = 'Rust manifest/lockfile (including nested crate manifests) changes dependency resolution and every packaged native payload' }
    @{ Name = 'cargo-config'; Pattern = '^\.cargo/'; Tasks = @('rust', 'windows', 'android', 'macos'); Packages = $allPkgs; Reason = 'cargo configuration affects every Rust build' }
    @{ Name = 'rustfmt-policy'; Pattern = '^rustfmt\.toml$'; Tasks = @('rust'); Packages = @(); Reason = 'rustfmt policy is enforced by the rust job' }
    @{ Name = 'cargo-deny-policy'; Pattern = '^deny\.toml$'; Tasks = @('rust'); Packages = @(); Reason = 'cargo-deny policy is enforced by the rust job' }
    @{ Name = 'editorconfig'; Pattern = '^\.editorconfig$'; Tasks = @('windows'); Packages = @(); Reason = '.editorconfig drives the dotnet format gate' }
    @{ Name = 'windows-core-ffi'; Pattern = '^platforms/windows/core-ffi/'; Tasks = @('rust', 'windows'); Packages = $allPkgs; Reason = 'Windows FFI implementation is embedded in all Windows packages' }
    @{ Name = 'android-core-ffi'; Pattern = '^platforms/android/core-ffi/'; Tasks = @('rust', 'android'); Packages = @(); Reason = 'Android FFI implementation is consumed by the Android job' }
    @{ Name = 'macos-core-ffi'; Pattern = '^platforms/macos/core-ffi/'; Tasks = @('rust', 'macos'); Packages = @(); Reason = 'macOS FFI implementation is consumed by the macOS job' }
    @{ Name = 'windows-test-only'; Pattern = '^platforms/windows/(?:.*/)?(?:[^/]+\.Tests\.[^/]+|tests?)/'; Tasks = @('windows'); Packages = @(); Reason = 'Windows test-only input; no package is rebuilt' }
    @{ Name = 'windows-installer-project'; Pattern = '^platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS\.Installer/'; Tasks = @('windows'); Packages = @('machine-msi', 'user-exe'); Reason = 'WiX installer project feeds the machine MSI and the user MSI payload of the Burn EXE' }
    @{ Name = 'windows-bundle-project'; Pattern = '^platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS\.Bundle/'; Tasks = @('windows'); Packages = @('user-exe'); Reason = 'Burn bundle project produces the per-user EXE' }
    @{ Name = 'windows-version-source'; Pattern = '^platforms/windows/ClipBridgeShell_CS/(?:Version\.props|Directory\.Build\.(?:props|targets))$'; Tasks = @('windows'); Packages = $allPkgs; Reason = 'Windows version/shared build properties are inputs of every package' }
    @{ Name = 'windows-solution'; Pattern = '^platforms/windows/ClipBridgeShell_CS/[^/]+\.sln$'; Tasks = @('windows'); Packages = $allPkgs; Reason = 'Windows solution structure is a package build input' }
    @{ Name = 'windows-project-inputs'; Pattern = '^platforms/windows/ClipBridgeShell_CS/(?!.*\.Tests\.[^/]+/).*\.(?:csproj|props|targets)$'; Tasks = @('windows'); Packages = $allPkgs; Reason = 'Windows project dependency/build configuration is a package build input' }
    @{ Name = 'windows-app-resources'; Pattern = '^platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/(?:Assets|Strings)/'; Tasks = @('windows'); Packages = $allPkgs; Reason = 'Windows application resources are packaged into the published payload' }
    @{ Name = 'windows-publish-profile'; Pattern = '^platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Properties/PublishProfiles/'; Tasks = @('windows'); Packages = $allPkgs; Reason = 'unpackaged publish profile is a package build input' }
    @{ Name = 'windows-production-code'; Pattern = '^platforms/windows/'; Tasks = @('windows'); Packages = @(); Reason = 'Windows C#/XAML production or build input' }
    @{ Name = 'android-shell'; Pattern = '^platforms/android/'; Tasks = @('android'); Packages = @(); Reason = 'Android shell/build input' }
    @{ Name = 'macos-shell'; Pattern = '^platforms/macos/'; Tasks = @('macos'); Packages = @(); Reason = 'macOS shell/build input' }
    @{ Name = 'windows-installer-script'; Pattern = '^scripts/(build-windows-installers|build-windows-all|version)\.ps1$'; Tasks = @('windows'); Packages = $allPkgs; Reason = 'shared Windows packaging entry/version script changes affect every selected package' }
    @{ Name = 'windows-machine-msi-script'; Pattern = '^scripts/build-windows-machine-msi\.ps1$'; Tasks = @('windows'); Packages = @('machine-msi'); Reason = 'machine MSI wrapper change' }
    @{ Name = 'windows-msi-script'; Pattern = '^scripts/build-windows-msi\.ps1$'; Tasks = @('windows'); Packages = @('machine-msi', 'user-exe'); Reason = 'generic WiX MSI wrapper affects both MSI formats' }
    @{ Name = 'windows-user-exe-script'; Pattern = '^scripts/build-windows-user-exe\.ps1$'; Tasks = @('windows'); Packages = @('user-exe'); Reason = 'per-user EXE wrapper change' }
    @{ Name = 'windows-portable-script'; Pattern = '^scripts/build-windows-portable-zip\.ps1$'; Tasks = @('windows'); Packages = @('portable-zip'); Reason = 'portable ZIP wrapper change' }
    @{ Name = 'android-scripts'; Pattern = '^scripts/(check-android-shell-contracts\.sh|setup-ndk\.ps1)$'; Tasks = @('android'); Packages = @(); Reason = 'Android build/contract script' }
    @{ Name = 'macos-scripts'; Pattern = '^scripts/(build-macos-ffi\.sh|check-macos-shell-env\.sh)$'; Tasks = @('macos'); Packages = @(); Reason = 'macOS build/contract script' }
    @{ Name = 'ffi-surface-gate'; Pattern = '^scripts/verify-ffi-surface\.ps1$'; Full = $true; Reason = 'shared FFI surface gate changed; validate the whole workflow' }
    @{ Name = 'ci-workflow'; Pattern = '^\.github/'; Full = $true; Reason = 'workflow/CI configuration changed; validate the whole workflow' }
    @{ Name = 'ci-scripts'; Pattern = '^scripts/ci/'; Full = $true; Reason = 'CI classification/aggregation logic changed; validate the whole workflow' }
    @{ Name = 'dotnet-toolchain'; Pattern = '^global\.json$'; Full = $true; Reason = '.NET SDK pin changed; validate the whole workflow' }
    @{ Name = 'rust-toolchain'; Pattern = '^rust-toolchain(?:\.toml)?$'; Full = $true; Reason = 'Rust toolchain pin changed; validate the whole workflow' }
)

function Get-CiTaskNames {
    return @($script:CiTasks)
}

function Get-CiPackageFormats {
    return @($script:CiPackages)
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
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Packages,
        [Parameter(Mandatory)][System.Collections.IDictionary]$Reasons
    )
    $requiredOut = [ordered]@{}
    foreach ($task in $script:CiTasks) {
        $requiredOut[$task] = [bool]$Required[$task]
    }
    $packageOut = [System.Collections.Generic.List[string]]::new()
    foreach ($format in $script:CiPackages) {
        if (@($Packages) -contains $format) { $packageOut.Add($format) }
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
        packages      = $packageOut.ToArray()
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
    if (-not $why) { $why = 'scope=full requires every task and every supported package format' }
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
    return New-CiPlanObject -Commit $Commit -Scope 'full' -Required $required -Packages $script:CiPackages -Reasons $reasons
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
        纯函数：路径集合 -> required / packages / reasons / unknown / localOnly / docOnly。
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
    $packageSet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    $unknown = [System.Collections.Generic.List[string]]::new()
    $localOnly = [System.Collections.Generic.List[string]]::new()
    $docOnly = [System.Collections.Generic.List[string]]::new()
    $fullTriggers = [System.Collections.Generic.List[string]]::new()

    foreach ($path in $normalized) {
        # 专用本地渠道文件（精确路径）先识别；宽泛的 msix/winget 子串会吞掉真实源码，
        # 因此只允许这里列出的精确文件/目录，其它 msix/winget 命名路径继续走构建规则。
        $isLocal = $false
        foreach ($pattern in $script:CiLocalOnlyPatterns) {
            if (Test-CiPattern $path $pattern) { $isLocal = $true; break }
        }
        if ($isLocal) { $localOnly.Add($path); continue }

        $matched = $false
        foreach ($rule in $script:CiRules) {
            if (-not (Test-CiPattern $path $rule.Pattern)) { continue }
            $matched = $true
            if ($rule.ContainsKey('Tasks')) {
                foreach ($task in @($rule.Tasks)) {
                    if ($script:CiTasks -notcontains $task) { throw "规则 $($rule.Name) 引用了未知任务 $task" }
                    $required[$task] = $true
                    $reason = if ($rule.ContainsKey('Reason')) { $rule.Reason } else { $rule.Name }
                    $reasonsByTask[$task].Add("$($rule.Name): $reason ($path)")
                }
            }
            if ($rule.ContainsKey('Packages')) {
                foreach ($format in @($rule.Packages)) {
                    if ($script:CiPackages -notcontains $format) { throw "规则 $($rule.Name) 引用了非法包格式 $format" }
                    [void]$packageSet.Add($format)
                }
            }
            if ($rule.ContainsKey('Full') -and [bool]$rule.Full) {
                $fullTriggers.Add("$($rule.Name): $path")
            }
        }
        if ($matched) { continue }

        # 显式文档目录/根文档才免重型检查；其他 .md 若未被构建规则命中则按未知路径全跑。
        $isDoc = $false
        foreach ($pattern in $script:CiDocOnlyPatterns) {
            if (Test-CiPattern $path $pattern) { $isDoc = $true; break }
        }
        if ($isDoc) { $docOnly.Add($path); continue }

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

    $packages = @($script:CiPackages | Where-Object { $packageSet.Contains($_) })
    if ($packages.Count -gt 0) {
        $required['windows-packages'] = $true
        $required['windows'] = $true
        $reasonsByTask['windows-packages'].Add('selected package format(s): ' + ($packages -join ', '))
        $reasonsByTask['windows'].Add('Windows validation is a prerequisite for the selected packages')
    }

    $context = ''
    $hasBuildInput = $false
    foreach ($task in $script:CiTasks) {
        if ($task -ne 'ci-config' -and $required[$task]) { $hasBuildInput = $true; break }
    }
    if (-not $hasBuildInput) {
        if ($localOnly.Count -gt 0 -and $docOnly.Count -gt 0) {
            $context = 'only local-only channel and documentation inputs; excluded from GitHub CI/build'
        } elseif ($localOnly.Count -gt 0) {
            $context = 'only local-only channel (MSIX/StoreUpload/winget) inputs; excluded from GitHub CI/build'
        } elseif ($docOnly.Count -gt 0) {
            $context = 'documentation/non-build repository input; no platform build or package is required'
        } elseif ($normalized.Count -eq 0) {
            $context = 'no changed paths; only the always-on lightweight checks are required'
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
            if ($task -eq 'windows-packages' -and $packages.Count -eq 0) {
                $reasons[$task] = @('packages=[]: no packaging input changed')
            } elseif ($context) {
                $reasons[$task] = @("not required: $context")
            } else {
                $reasons[$task] = @('not required: no matching build input in this change set')
            }
        }
    }

    return New-CiPlanObject -Commit $Commit -Scope 'affected' -Required $required -Packages $packages -Reasons $reasons
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

    $allowed = @('schemaVersion', 'scope', 'commit', 'required', 'packages', 'reasons')
    $keys = @(Get-CiObjectKeys $Plan)
    foreach ($key in $keys) {
        if ($allowed -notcontains $key) { $errors.Add("unknown top-level field '$key'") }
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

    $packages = Get-CiPropertyValue $Plan 'packages'
    $packageList = @()
    if ($packages -isnot [System.Collections.IList] -or $packages -is [string]) {
        $errors.Add('packages must be an array')
    } else {
        $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
        foreach ($item in $packages) {
            if ($item -isnot [string]) { $errors.Add('packages entries must be strings'); continue }
            if ($script:CiPackages -notcontains $item) {
                $errors.Add("packages entry '$item' is outside the supported GitHub scope (machine-msi, user-exe, portable-zip)")
            }
            if (-not $seen.Add($item)) { $errors.Add("packages entry '$item' is duplicated") }
            $packageList += $item
        }
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
        $hasPackages = @($packageList).Count -gt 0
        $windowsPackages = Get-CiPropertyValue $required 'windows-packages'
        $windows = Get-CiPropertyValue $required 'windows'
        if ($hasPackages -ne [bool]$windowsPackages) {
            $errors.Add("packages must be non-empty if and only if required['windows-packages'] is true")
        }
        if ($hasPackages -and -not [bool]$windows) {
            $errors.Add("required['windows-packages'] requires required['windows']=true")
        }
        if ($scope -eq 'full') {
            foreach ($task in $script:CiTasks) {
                if (-not [bool](Get-CiPropertyValue $required $task)) {
                    $errors.Add("scope=full requires required['$task']=true")
                }
            }
            foreach ($format in $script:CiPackages) {
                if ($packageList -notcontains $format) { $errors.Add("scope=full requires packages to include '$format'") }
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

    $packagesJob = [string]$jobs['windows-packages']
    if ($packagesJob) {
        $needs = @(Get-CiJobNeedSet $packagesJob)
        if ($needs -notcontains 'windows') { $errors.Add("windows-packages must depend on windows (plan contract requires windows=true when packages are selected)") }
        if ($packagesJob -notmatch 'setup-wix\.ps1') { $errors.Add("windows-packages must use the pinned WiX bootstrap script") }
        if ($packagesJob -notmatch 'build-windows-installers\.ps1') { $errors.Add("windows-packages must call the shared installer entry") }
        if ($packagesJob -notmatch 'Formats') { $errors.Add("windows-packages must pass -Formats from plan.packages") }
        if ($packagesJob -notmatch 'packages') { $errors.Add("windows-packages must derive formats from the plan output") }
    }

    foreach ($entry in @('scripts/ci/rust.ps1', 'scripts/ci/windows.ps1', 'scripts/ci/android.sh', 'scripts/ci/macos.sh')) {
        if ($Content -notmatch [regex]::Escape($entry)) { $errors.Add("platform job must call $entry") }
    }
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
    'Get-CiPackageFormats',
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
