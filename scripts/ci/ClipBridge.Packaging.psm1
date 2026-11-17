#Requires -Version 7.0
<#
.SYNOPSIS
    ClipBridge Windows 打包的共享辅助模块（格式选择、产物计划、MSI/Burn/ZIP 产物表断言、
    package-summary.json、安全清理路径、签名状态）。

.DESCRIPTION
    该模块只包含可独立测试的辅助函数；真正的 native / dotnet / MSBuild 调用仍由
    scripts/build-windows-installers.ps1 负责。

    设计约束：
      - 公开格式固定为 machine-msi / user-exe / portable-zip；内部 user MSI 不是可选择的发布格式。
      - 所有产物断言优先读取最终产物本身（MSI 的 Windows Installer 表、Burn bundle 的 .wixpdb
        构建记录表、ZIP 条目），而不是只匹配源码文本。
      - 清理路径必须先解析为绝对路径，并验证其严格位于允许的 outdir 之下。
      - 签名状态必须如实表达；None 永远不等于可信的生产签名。
#>

Set-StrictMode -Off

$script:FormatOrder = @('machine-msi', 'user-exe', 'portable-zip')
$script:FormatListText = 'machine-msi, user-exe, portable-zip'
$script:PackageSummarySchemaVersion = 1
$script:ProjectOwnedPayloadNames = @(
    'ClipBridgeShell_CS.exe',
    'ClipBridgeShell_CS.dll',
    'ClipBridgeShell_CS.Core.dll',
    'core_ffi_windows.dll'
)

function Get-PackageFormatOrder {
    <# 返回公开格式的稳定顺序（不要修改调用方的原数组）。 #>
    return [string[]]$script:FormatOrder.Clone()
}

function Resolve-FormatSelection {
    <#
    .SYNOPSIS
        把 -Formats 或兼容的 -Only 解析成稳定的公开格式集合。

    .DESCRIPTION
        -Formats 与 -Only 互斥；两者都未显式给出时等价于 -Only All（保持旧默认）。
        -Formats 必须是非空集合，未知值、内部 user-msi、msix/winget 都直接失败。
        输入顺序不影响结果，重复值去重，结果按 machine-msi, user-exe, portable-zip 排序。
    #>
    [CmdletBinding()]
    param(
        [string[]]$Formats,
        [string]$Only,
        [bool]$OnlySpecified = $PSBoundParameters.ContainsKey('Only'),
        [bool]$FormatsSpecified = $PSBoundParameters.ContainsKey('Formats')
    )

    if ($OnlySpecified -and $FormatsSpecified) {
        throw [System.ArgumentException]::new(
            '-Formats 与 -Only 不能同时使用：请只选择一种输入方式（-Formats 用于格式集合，-Only 保留旧单渠道/All 兼容）。')
    }

    if (-not $OnlySpecified -and -not $FormatsSpecified) {
        $OnlySpecified = $true
        $Only = 'All'
    }

    $selected = @{}
    if ($OnlySpecified) {
        $onlyKey = ([string]$Only).ToLowerInvariant()
        switch ($onlyKey) {
            'all' { foreach ($f in $script:FormatOrder) { $selected[$f] = $true } }
            'machinemsi' { $selected['machine-msi'] = $true }
            'userexe' { $selected['user-exe'] = $true }
            'portablezip' { $selected['portable-zip'] = $true }
            default {
                throw [System.ArgumentException]::new(
                    "未知的 -Only 值 '$Only'；有效值：All, MachineMsi, UserExe, PortableZip。")
            }
        }
    }
    else {
        if ($null -eq $Formats) {
            throw [System.ArgumentException]::new(
                '-Formats 不能为空；有效格式：' + $script:FormatListText + '。')
        }
        $formatItems = @($Formats)
        if ($formatItems.Count -eq 0) {
            throw [System.ArgumentException]::new(
                '-Formats 至少需要一个格式；有效格式：' + $script:FormatListText + '。')
        }

        foreach ($raw in $formatItems) {
            $value = if ($null -eq $raw) { '' } else { [string]$raw }
            if ([string]::IsNullOrWhiteSpace($value)) {
                throw [System.ArgumentException]::new(
                    '-Formats 含空值；有效格式：' + $script:FormatListText + '。')
            }

            # 不 Trim：带前后空格的值是非法输入，避免悄悄接受拼写错误。
            $key = $value.ToLowerInvariant()
            if ($key -eq 'user-msi' -or $key -eq 'internal-user-msi' -or $key -eq 'usermsi') {
                throw [System.ArgumentException]::new(
                    "内部 user MSI 不是发布格式，不能通过 -Formats 单独选择；请选择 'user-exe'（会自动构建内部 user MSI）。")
            }
            if ($script:FormatOrder -notcontains $key) {
                throw [System.ArgumentException]::new(
                    "未知格式 '$value'；有效格式：$($script:FormatListText)。MSIX/winget 不在 GitHub MSI/EXE/ZIP 范围内。")
            }
            $selected[$key] = $true
        }
    }

    $result = [System.Collections.Generic.List[string]]::new()
    foreach ($f in $script:FormatOrder) {
        if ($selected.ContainsKey($f)) { $result.Add($f) }
    }
    if ($result.Count -eq 0) {
        throw [System.ArgumentException]::new(
            "格式集合为空；有效格式：$($script:FormatListText)。")
    }
    return [string[]]$result.ToArray()
}

function Assert-SupportedPlatform {
    <# 当前正式交付只支持 x64；arm64 保留参数但必须在任何构建动作前明确失败。 #>
    [CmdletBinding()]
    param([string]$Platform)
    if ([string]::IsNullOrWhiteSpace($Platform)) {
        throw [System.ArgumentException]::new('-Platform 不能为空；当前只支持 x64。')
    }
    $p = $Platform.ToLowerInvariant()
    if ($p -eq 'x64') { return 'x64' }
    if ($p -eq 'arm64') {
        throw [System.ArgumentException]::new(
            'arm64 当前没有交付支持（未验证），不能根据参数存在就宣称可用；请使用 -Platform x64。')
    }
    throw [System.ArgumentException]::new("不支持的平台 '$Platform'；当前只支持 x64。")
}

function Get-BuildPlan {
    <# 根据公开格式集合生成构建依赖计划；不读取任何旧产物。 #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string[]]$Formats)

    $set = @(Resolve-FormatSelection -Formats $Formats -FormatsSpecified $true)
    $needMachine = $set -contains 'machine-msi'
    $needUser = $set -contains 'user-exe'
    $needZip = $set -contains 'portable-zip'

    [pscustomobject]@{
        Formats          = [string[]]$set
        NeedMachineMsi   = [bool]$needMachine
        NeedUserExe      = [bool]$needUser
        NeedUserMsi      = [bool]$needUser   # user EXE 的内部依赖，不是公开发布格式
        NeedBundle       = [bool]$needUser
        NeedPortableZip  = [bool]$needZip
        NeedMsi          = [bool]($needMachine -or $needUser)
        NeedWix          = [bool]($needMachine -or $needUser)
        PublishOnce      = $true
        HeatOnce         = [bool]($needMachine -or $needUser)
        PublicFormats    = [string[]]$set
        InternalFormats  = [string[]]$(if ($needUser) { @('user-msi') } else { [string[]]@() })
    }
}

function Get-ArtifactName {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Format,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$Platform
    )
    switch ($Format.ToLowerInvariant()) {
        'machine-msi' { return "ClipBridge-$Version-win-$Platform-machine.msi" }
        'user-exe' { return "ClipBridge-$Version-win-$Platform-user.exe" }
        'user-msi' { return "ClipBridge-$Version-win-$Platform-user.msi" }
        'portable-zip' { return "ClipBridge-$Version-win-$Platform-portable.zip" }
        default { throw [System.ArgumentException]::new("未知产物格式 '$Format'。") }
    }
}

function Get-ExpectedArtifacts {
    <#
    .SYNOPSIS
        返回本次选择必须产出的完整产物清单（包含内部 user MSI）。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string[]]$Formats,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$Platform
    )
    $plan = Get-BuildPlan -Formats $Formats
    $list = [System.Collections.Generic.List[object]]::new()

    foreach ($f in $plan.Formats) {
        switch ($f) {
            'machine-msi' {
                $list.Add([pscustomobject]@{
                        Format = 'machine-msi'
                        Name   = (Get-ArtifactName -Format 'machine-msi' -Version $Version -Platform $Platform)
                        Role   = 'release'
                        Internal = $false
                    })
            }
            'user-exe' {
                $list.Add([pscustomobject]@{
                        Format = 'user-exe'
                        Name   = (Get-ArtifactName -Format 'user-exe' -Version $Version -Platform $Platform)
                        Role   = 'release'
                        Internal = $false
                    })
                $list.Add([pscustomobject]@{
                        Format = 'user-msi'
                        Name   = (Get-ArtifactName -Format 'user-msi' -Version $Version -Platform $Platform)
                        Role   = 'internal'
                        Internal = $true
                    })
            }
            'portable-zip' {
                $list.Add([pscustomobject]@{
                        Format = 'portable-zip'
                        Name   = (Get-ArtifactName -Format 'portable-zip' -Version $Version -Platform $Platform)
                        Role   = 'release'
                        Internal = $false
                    })
            }
        }
    }
    return [object[]]$list.ToArray()
}

function Get-BuildSteps {
    <#
    .SYNOPSIS
        返回构建步骤顺序；用于驱动主脚本并让签名顺序可以被纯 fixture 测试。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string[]]$Formats,
        [ValidateSet('None', 'Certificate')][string]$SigningMode = 'None',
        [bool]$SkipRust = $false
    )
    $plan = Get-BuildPlan -Formats $Formats
    $steps = [System.Collections.Generic.List[string]]::new()
    if (-not $SkipRust) { $steps.Add('rust') }
    $steps.Add('publish')

    if ($SigningMode -eq 'Certificate') {
        $steps.Add('sign-payload')
    }
    if ($plan.NeedMsi) { $steps.Add('heat') }
    if ($plan.NeedMachineMsi) { $steps.Add('machine-msi') }
    if ($plan.NeedUserMsi) { $steps.Add('user-msi') }
    if ($SigningMode -eq 'Certificate' -and $plan.NeedMsi) { $steps.Add('sign-msi') }
    if ($plan.NeedBundle) { $steps.Add('bundle') }
    if ($SigningMode -eq 'Certificate' -and $plan.NeedBundle) { $steps.Add('sign-bundle') }
    if ($plan.NeedPortableZip) { $steps.Add('portable-zip') }
    $steps.Add('verify-artifacts')
    if ($SigningMode -eq 'Certificate' -and $plan.NeedPortableZip) {
        $steps.Add('verify-zip-payload-signatures')
    }
    $steps.Add('summary')
    return [string[]]$steps.ToArray()
}

function Get-MissingWixFiles {
    <#
    .SYNOPSIS
        返回 WiX 3.14 目录中缺失的必需文件；Burn bundle 还需要 x86\burn.exe。
        setup-wix 解压不完整时必须在 publish 之前明确失败，而不是到 light 才报错。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WixRoot,
        [bool]$NeedBundle = $false
    )
    $required = @(
        'candle.exe',
        'light.exe',
        'heat.exe',
        'WixTasks.dll',
        'WixUtilExtension.dll',
        'WixUIExtension.dll',
        'wix.targets'
    )
    if ($NeedBundle) {
        $required += (Join-Path 'x86' 'burn.exe')
        $required += 'WixBalExtension.dll'
    }
    return [string[]]@($required | Where-Object {
            -not (Test-Path -LiteralPath (Join-Path $WixRoot $_) -PathType Leaf)
        })
}

function Get-ProjectOwnedPayloadNames {
    <# 项目自有的可执行载荷白名单；第三方 DLL 不在此列，不会被 blind resign。 #>
    return [string[]]$script:ProjectOwnedPayloadNames.Clone()
}

function Get-ProjectOwnedPayloadFiles {
    <#
    .SYNOPSIS
        从 stage 目录取得项目自有载荷的绝对路径，按 EXE 优先顺序返回。
        缺少 app EXE 或 Rust DLL 时失败（缺失发布载荷必须 fatal）。
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$StageDir)

    if (-not (Test-Path -LiteralPath $StageDir -PathType Container)) {
        throw [System.IO.DirectoryNotFoundException]::new("stage 目录不存在：$StageDir")
    }
    $root = [System.IO.Path]::GetFullPath($StageDir)
    $files = [System.Collections.Generic.List[string]]::new()

    foreach ($name in $script:ProjectOwnedPayloadNames) {
        $path = Join-Path $root $name
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            if ((Get-Item -LiteralPath $path).Length -le 0) {
                throw [System.IO.InvalidDataException]::new("项目自有载荷为空文件：$path")
            }
            $files.Add((Get-Item -LiteralPath $path).FullName)
        }
    }

    $required = @('ClipBridgeShell_CS.exe', 'core_ffi_windows.dll')
    foreach ($name in $required) {
        if (-not ($files | Where-Object { [System.IO.Path]::GetFileName($_) -ieq $name })) {
            throw [System.IO.FileNotFoundException]::new(
                "stage 缺少必需载荷 '$name'；publish 结果不完整：$root")
        }
    }
    return [string[]]$files.ToArray()
}

function Assert-Sha256Hex {
    <# 验证并规范化 64 位十六进制 SHA256；参数错误抛 ArgumentException。 #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][AllowNull()][AllowEmptyString()][string]$Value,
        [string]$Label = 'SHA256'
    )
    if ($Value -notmatch '^[0-9a-fA-F]{64}$') {
        throw [System.ArgumentException]::new("$Label 必须是 64 位十六进制 SHA256，实际 '$Value'。")
    }
    return $Value.ToLowerInvariant()
}

function Assert-CoreDllHashMatch {
    <# 在 publish 之前验证期望的 core DLL hash；不匹配抛 InvalidOperationException。 #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][AllowNull()][string]$Expected,
        [Parameter(Mandatory)][AllowNull()][string]$Actual,
        [string]$DllPath = ''
    )
    $e = Assert-Sha256Hex -Value $Expected -Label 'ExpectedCoreDllSha256'
    $a = Assert-Sha256Hex -Value $Actual -Label '实际 core DLL SHA256'
    if ($e -cne $a) {
        throw [System.InvalidOperationException]::new(
            "core DLL hash mismatch：expected=$e actual=$a path=$DllPath。拒绝在未验证的 DLL 上 publish。")
    }
    return $a
}

function Get-ValidatedNativeEvidence {
    <# 校验并规范化 package-summary 的 nativeEvidence。 #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][AllowNull()]$NativeEvidence)
    if ($null -eq $NativeEvidence) {
        throw [System.InvalidOperationException]::new('nativeEvidence 缺失（input/stage DLL hash 未记录）。')
    }
    $inputHash = Assert-Sha256Hex -Value ([string]$NativeEvidence.inputDllSha256) -Label 'nativeEvidence.inputDllSha256'
    $stageHash = Assert-Sha256Hex -Value ([string]$NativeEvidence.stageDllSha256) -Label 'nativeEvidence.stageDllSha256'
    if ($inputHash -cne $stageHash) {
        throw [System.InvalidOperationException]::new(
            "nativeEvidence stageDllSha256 ($stageHash) 必须等于预签名 inputDllSha256 ($inputHash)。")
    }
    return [ordered]@{
        inputDllSha256      = $inputHash
        stageDllSha256      = $stageHash
        sourceReusedVerified = [bool]$NativeEvidence.sourceReusedVerified
    }
}

function Get-FileSha256 {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw [System.IO.FileNotFoundException]::new("文件不存在：$Path")
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-FileSha1 {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw [System.IO.FileNotFoundException]::new("文件不存在：$Path")
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA1).Hash.ToUpperInvariant()
}

# ---------------------------------------------------------------------------
# 安全路径 / 清理
# ---------------------------------------------------------------------------

function Get-NormalizedFullPath {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) {
        throw [System.ArgumentException]::new('路径不能为空。')
    }
    return [System.IO.Path]::GetFullPath($Path)
}

function Test-PathStrictlyUnder {
    <# 判断 $Path 是否严格位于 $Root 之下（不包括 Root 本身），大小写不敏感。 #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Root
    )
    $p = Get-NormalizedFullPath $Path
    $r = Get-NormalizedFullPath $Root
    $r = $r.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    if ($p.Equals($r, [System.StringComparison]::OrdinalIgnoreCase)) { return $false }
    $prefix = $r + [System.IO.Path]::DirectorySeparatorChar
    return $p.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)
}

function Assert-SafeOutputRoot {
    <#
    .SYNOPSIS
        验证 outdir 不是一个危险根（盘符根、用户目录、TEMP 根本身、仓库根本身）。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [string]$RepoRoot
    )
    $p = Get-NormalizedFullPath $Path
    $forbidden = @(
        [System.IO.Path]::GetPathRoot($p),
        $env:USERPROFILE,
        $env:LOCALAPPDATA,
        $env:APPDATA,
        $env:TEMP,
        $env:TMP,
        $env:HOME
    )
    if ($RepoRoot) { $forbidden += $RepoRoot }
    foreach ($f in $forbidden) {
        if ([string]::IsNullOrWhiteSpace($f)) { continue }
        $n = Get-NormalizedFullPath $f
        if ($p.Equals($n, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw [System.InvalidOperationException]::new(
                "拒绝把输出根直接设为 '$p'（可能是盘符根、用户根、TEMP/TMP 根或仓库根）。请使用其下的独立子目录。")
        }
    }
    return $p
}

function Assert-SafeCleanupTarget {
    <#
    .SYNOPSIS
        验证清理目标是绝对路径、严格位于 AllowedRoot 之下，且不是用户/系统根。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$AllowedRoot
    )
    $p = Get-NormalizedFullPath $Path
    $root = Get-NormalizedFullPath $AllowedRoot
    if (-not (Test-PathStrictlyUnder -Path $p -Root $root)) {
        throw [System.InvalidOperationException]::new(
            "拒绝清理 '$p'：必须严格位于允许的根 '$root' 之下。")
    }
    $forbidden = @(
        [System.IO.Path]::GetPathRoot($p),
        $env:USERPROFILE,
        $env:LOCALAPPDATA,
        $env:APPDATA,
        $env:TEMP,
        $env:TMP,
        $env:HOME
    )
    foreach ($f in $forbidden) {
        if ([string]::IsNullOrWhiteSpace($f)) { continue }
        $n = Get-NormalizedFullPath $f
        if ($p.Equals($n, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw [System.InvalidOperationException]::new("拒绝清理根目录 '$p'。")
        }
    }
    return $p
}

function Remove-ItemSafely {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$AllowedRoot,
        [switch]$Recurse
    )
    $safe = Assert-SafeCleanupTarget -Path $Path -AllowedRoot $AllowedRoot
    if (-not (Test-Path -LiteralPath $safe)) { return }
    if ($Recurse) {
        Remove-Item -LiteralPath $safe -Recurse -Force -ErrorAction Stop
    }
    else {
        Remove-Item -LiteralPath $safe -Force -ErrorAction Stop
    }
}

# ---------------------------------------------------------------------------
# MSI 产物表（Windows Installer COM）
# ---------------------------------------------------------------------------

function Invoke-MsiQueryRows {
    <# 执行只读 MSI SQL，返回 string[] 行；表不存在时可 -Optional 返回空。 #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$Database,
        [Parameter(Mandatory)][string]$Sql,
        [switch]$Optional
    )
    try {
        $view = $Database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $Database, @([string]$Sql))
    }
    catch {
        if ($Optional) { return [string[]]@() }
        throw [System.InvalidOperationException]::new("MSI 查询失败（$Sql）：$($_.Exception.Message)", $_.Exception)
    }

    $rows = [System.Collections.Generic.List[string[]]]::new()
    try {
        $null = $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null)
        while ($true) {
            $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
            if ($null -eq $record) { break }
            $count = [int]$record.GetType().InvokeMember('FieldCount', 'GetProperty', $null, $record, $null)
            $row = [string[]]::new($count)
            for ($i = 1; $i -le $count; $i++) {
                $row[$i - 1] = [string]$record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @($i))
            }
            $rows.Add($row)
        }
    }
    finally {
        try { $null = [System.Runtime.InteropServices.Marshal]::ReleaseComObject($view) } catch { }
    }
    return [string[][]]$rows.ToArray()
}

function Read-MsiArtifactProbe {
    <#
    .SYNOPSIS
        读取最终 MSI 的 Property / File / InstallExecuteSequence / Upgrade / Directory 表
        和 SummaryInformation WordCount。只读打开，不安装。
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)

    $full = Get-NormalizedFullPath $Path
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
        throw [System.IO.FileNotFoundException]::new("MSI 不存在：$full")
    }

    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $null
    try {
        $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @([string]$full, 0))

        $properties = @{}
        foreach ($row in @(Invoke-MsiQueryRows -Database $db -Sql 'SELECT Property,Value FROM Property')) {
            if ($row.Count -ge 2) { $properties[[string]$row[0]] = [string]$row[1] }
        }

        $fileNames = @(Invoke-MsiQueryRows -Database $db -Sql 'SELECT FileName FROM File' | ForEach-Object { [string]$_[0] })

        $sequences = @(Invoke-MsiQueryRows -Database $db -Sql 'SELECT Action,Condition FROM InstallExecuteSequence' |
            ForEach-Object { [pscustomobject]@{ Action = [string]$_[0]; Condition = [string]$_[1] } })

        $upgrade = @(Invoke-MsiQueryRows -Database $db -Sql 'SELECT UpgradeCode,VersionMin,VersionMax,ActionProperty FROM Upgrade' -Optional |
            ForEach-Object { [pscustomobject]@{ UpgradeCode = [string]$_[0]; VersionMin = [string]$_[1]; VersionMax = [string]$_[2]; ActionProperty = [string]$_[3] } })

        $directoryParents = @{}
        foreach ($row in @(Invoke-MsiQueryRows -Database $db -Sql 'SELECT Directory,Directory_Parent FROM Directory' -Optional)) {
            if ($row.Count -ge 2) { $directoryParents[[string]$row[0]] = [string]$row[1] }
        }

        $wordCount = 0
        try {
            $si = $db.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $db, @(0))
            $wordCount = [int]$si.GetType().InvokeMember('Property', 'GetProperty', $null, $si, @(15))
        }
        catch {
            throw [System.InvalidOperationException]::new("无法读取 MSI SummaryInformation WordCount：$full", $_.Exception)
        }

        [pscustomobject]@{
            Path             = $full
            Properties       = $properties
            FileNames        = [string[]]$fileNames
            Sequences        = [object[]]$sequences
            Upgrade          = [object[]]$upgrade
            DirectoryParents = $directoryParents
            WordCount        = $wordCount
        }
    }
    finally {
        if ($db) {
            try { $null = [System.Runtime.InteropServices.Marshal]::ReleaseComObject($db) } catch { }
        }
    }
}

function Test-ConditionContainsToken {
    <# 归一化 MSI 条件（去掉所有空白、转大写）后检查 token。 #>
    [CmdletBinding()]
    param(
        [string]$Condition,
        [Parameter(Mandatory)][string]$Token
    )
    if ([string]::IsNullOrWhiteSpace($Condition)) { return $false }
    $normalized = ($Condition -replace '\s', '').ToUpperInvariant()
    $normalizedToken = ($Token -replace '\s', '').ToUpperInvariant()
    return $normalized.Contains($normalizedToken)
}

function Get-MsiSequenceCondition {
    [CmdletBinding()]
    param($Probe, [Parameter(Mandatory)][string]$Action)
    $match = @($Probe.Sequences | Where-Object { [string]$_.Action -ieq $Action })
    if ($match.Count -eq 0) { return $null }
    return [string]$match[0].Condition
}

function Test-MsiArtifact {
    <#
    .SYNOPSIS
        对最终 MSI 产物表做断言。返回 Ok/Failures，不抛异常（由调用方 fatal）。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][AllowNull()]$Probe,
        [Parameter(Mandatory)][ValidateSet('machine-msi', 'user-msi')][string]$Kind,
        [Parameter(Mandatory)][string]$ExpectedVersion
    )
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Probe) {
        $failures.Add('MSI probe 为空')
        return [pscustomobject]@{ Ok = $false; Failures = [string[]]$failures.ToArray() }
    }

    $props = $Probe.Properties
    if ([string]$props['ProductVersion'] -ne $ExpectedVersion) {
        $failures.Add("ProductVersion=$($props['ProductVersion'])，期望 $ExpectedVersion")
    }
    if ([string]$props['REMOVEUSERDATA'] -ne '1') {
        $failures.Add("REMOVEUSERDATA 默认应为 1（卸载默认删除），实际 $($props['REMOVEUSERDATA'])")
    }

    if ($Kind -eq 'machine-msi') {
        if ([string]$props['ALLUSERS'] -ne '1') { $failures.Add("machine ALLUSERS=$($props['ALLUSERS'])，期望 1") }
        if (($Probe.WordCount -band 8) -ne 0) {
            $failures.Add("machine WordCount bit3 应为 0（真实提权），实际 WordCount=$($Probe.WordCount)")
        }
        $parent = [string]$Probe.DirectoryParents['INSTALLFOLDER']
        if ($parent -ne 'ProgramFiles64Folder') {
            $failures.Add("machine INSTALLFOLDER 父目录应为 ProgramFiles64Folder，实际 '$parent'")
        }
    }
    else {
        if ([string]$props['ALLUSERS'] -ne '2') { $failures.Add("user ALLUSERS=$($props['ALLUSERS'])，期望 2") }
        if ([string]$props['MSIINSTALLPERUSER'] -ne '1') { $failures.Add("user MSIINSTALLPERUSER=$($props['MSIINSTALLPERUSER'])，期望 1") }
        if ([string]$props['ARPSYSTEMCOMPONENT'] -ne '1') { $failures.Add("user ARPSYSTEMCOMPONENT=$($props['ARPSYSTEMCOMPONENT'])，期望 1") }
        if ((($Probe.WordCount -band 8) -eq 0)) {
            $failures.Add("user WordCount bit3 应为 1（per-user 免提权），实际 WordCount=$($Probe.WordCount)")
        }
        $parent = [string]$Probe.DirectoryParents['INSTALLFOLDER']
        if ($parent -ne 'ProgramsFolder') {
            $failures.Add("user INSTALLFOLDER 父目录应为 ProgramsFolder，实际 '$parent'")
        }
    }

    $longFileNames = @($Probe.FileNames | ForEach-Object { if ($_ -match '\|') { ($_ -split '\|')[-1] } else { $_ } })
    $pdbFiles = @($longFileNames | Where-Object { $_ -match '(?i)\.pdb$' })
    if ($pdbFiles.Count -gt 0) { $failures.Add("MSI 含 .pdb：$($pdbFiles -join ', ')") }
    $coreFiles = @($longFileNames | Where-Object { $_ -match '(?i)^core_ffi_windows\.dll$' })
    if ($coreFiles.Count -eq 0) { $failures.Add('MSI File 表缺少 core_ffi_windows.dll') }

    # 卸载清理必须显式排除升级；删除用户数据的 CA 必须同时受 REMOVEUSERDATA 控制。
    $removeDataCondition = Get-MsiSequenceCondition -Probe $Probe -Action 'RemoveUserDataCA'
    if ($null -eq $removeDataCondition) {
        $failures.Add('缺少 RemoveUserDataCA 卸载清理动作')
    }
    else {
        foreach ($token in @('REMOVEUSERDATA="1"', 'NOT UPGRADINGPRODUCTCODE', 'REMOVE~="ALL"', 'INSTALLED')) {
            if (-not (Test-ConditionContainsToken -Condition $removeDataCondition -Token $token)) {
                $failures.Add("RemoveUserDataCA 条件缺少 '$token'：$removeDataCondition")
            }
        }
    }

    $startupCondition = Get-MsiSequenceCondition -Probe $Probe -Action 'CleanupStartupEntryCA'
    if ($null -eq $startupCondition) {
        $failures.Add('缺少 CleanupStartupEntryCA 清理动作')
    }
    elseif (-not (Test-ConditionContainsToken -Condition $startupCondition -Token 'NOT UPGRADINGPRODUCTCODE')) {
        $failures.Add("CleanupStartupEntryCA 条件缺少 NOT UPGRADINGPRODUCTCODE：$startupCondition")
    }

    if ($Kind -eq 'user-msi') {
        $traceCondition = Get-MsiSequenceCondition -Probe $Probe -Action 'CleanupInstallTraceCA'
        if ($null -eq $traceCondition) {
            $failures.Add('缺少 user 变体的 CleanupInstallTraceCA')
        }
        elseif (-not (Test-ConditionContainsToken -Condition $traceCondition -Token 'NOT UPGRADINGPRODUCTCODE')) {
            $failures.Add("CleanupInstallTraceCA 条件缺少 NOT UPGRADINGPRODUCTCODE：$traceCondition")
        }
    }

    if (-not (Get-MsiSequenceCondition -Probe $Probe -Action 'RemoveExistingProducts')) {
        # 空条件也是合法行（条件为空字符串）；用序列存在性单独判断。
        $hasRemoveExisting = @($Probe.Sequences | Where-Object { [string]$_.Action -ieq 'RemoveExistingProducts' }).Count -gt 0
        if (-not $hasRemoveExisting) { $failures.Add('InstallExecuteSequence 缺少 RemoveExistingProducts（升级路径）') }
    }

    $upgradeActions = @($Probe.Upgrade | Where-Object {
            [string]$_.UpgradeCode -and ([string]$_.ActionProperty -match '(?i)^WIX_(UPGRADE|DOWNGRADE)_DETECTED$')
        })
    if ($upgradeActions.Count -eq 0) {
        $failures.Add('Upgrade 表缺少 WIX_UPGRADE_DETECTED/WIX_DOWNGRADE_DETECTED 行（无法识别旧版本）')
    }

    return [pscustomobject]@{
        Ok       = ($failures.Count -eq 0)
        Failures = [string[]]$failures.ToArray()
    }
}

# ---------------------------------------------------------------------------
# Burn bundle .wixpdb 产物表
# ---------------------------------------------------------------------------

function Read-BundleWixPdbXml {
    <#
    .SYNOPSIS
        从 .wixpdb 的 XML 部分读取可查询的 WiX 表（纯函数，fixture 可直接传 XML 文本）。
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$XmlText)

    [xml]$xml = $XmlText
    $definitions = @{}
    foreach ($def in @($xml.SelectNodes("//*[local-name()='tableDefinition']"))) {
        $name = [string]$def.GetAttribute('name')
        if (-not $name) { continue }
        $columns = @($def.SelectNodes("./*[local-name()='columnDefinition']") |
            ForEach-Object { [string]$_.GetAttribute('name') })
        $definitions[$name] = [string[]]$columns
    }

    $tables = @{}
    foreach ($table in @($xml.SelectNodes("//*[local-name()='table']"))) {
        $name = [string]$table.GetAttribute('name')
        if (-not $name) { continue }
        $columns = $definitions[$name]
        $rows = [System.Collections.Generic.List[object]]::new()
        foreach ($row in @($table.SelectNodes("./*[local-name()='row']"))) {
            $fields = @($row.SelectNodes("./*[local-name()='field']") | ForEach-Object { [string]$_.InnerText })
            $dict = [ordered]@{}
            for ($i = 0; $i -lt $fields.Count; $i++) {
                $key = if ($columns -and $i -lt $columns.Count) { [string]$columns[$i] } else { "Field$($i + 1)" }
                $dict[$key] = $fields[$i]
            }
            $rows.Add([pscustomobject]$dict)
        }
        $tables[$name] = [object[]]$rows.ToArray()
    }

    return [pscustomobject]@{ Tables = $tables }
}

function Read-BundleWixPdbProbe {
    <#
    .SYNOPSIS
        读取 Burn bundle 旁边的 .wixpdb。该文件是实际 bundle 链接时生成的解析后表，
        不是 Bundle.wxs 源码文本；用于验证 MsiProperty / Variable / PayloadInfo。
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)

    $full = Get-NormalizedFullPath $Path
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
        throw [System.IO.FileNotFoundException]::new("Burn .wixpdb 不存在：$full")
    }
    $bytes = [System.IO.File]::ReadAllBytes($full)
    $marker = [System.Text.Encoding]::ASCII.GetBytes('<?xml')
    $offset = -1
    for ($i = 0; $i -le $bytes.Length - $marker.Length; $i++) {
        $matched = $true
        for ($j = 0; $j -lt $marker.Length; $j++) {
            if ($bytes[$i + $j] -ne $marker[$j]) { $matched = $false; break }
        }
        if ($matched) { $offset = $i; break }
    }
    if ($offset -lt 0) {
        throw [System.IO.InvalidDataException]::new("无法在 .wixpdb 中找到 XML 表数据：$full")
    }
    $xmlText = [System.Text.Encoding]::UTF8.GetString($bytes, $offset, $bytes.Length - $offset)
    $parsed = Read-BundleWixPdbXml -XmlText $xmlText
    return [pscustomobject]@{ Path = $full; Tables = $parsed.Tables }
}

function Test-BundleArtifact {
    <#
    .SYNOPSIS
        检查 Burn bundle 的产物表：变量默认值/可覆盖性、MsiProperty 传递、内嵌 MSI 身份和 hash。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][AllowNull()]$Probe,
        [Parameter(Mandatory)][string]$ExpectedVersion,
        [Parameter(Mandatory)][string]$ExpectedMsiName,
        [Parameter(Mandatory)][long]$ExpectedMsiSize,
        [Parameter(Mandatory)][string]$ExpectedMsiSha1
    )
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Probe -or $null -eq $Probe.Tables) {
        $failures.Add('Burn .wixpdb probe 为空')
        return [pscustomobject]@{ Ok = $false; Failures = [string[]]$failures.ToArray() }
    }

    $bundleRow = @($Probe.Tables['WixBundle'])[0]
    if ($null -eq $bundleRow) {
        $failures.Add('WixBundle 表缺失')
    }
    elseif ([string]$bundleRow.Version -ne $ExpectedVersion) {
        $failures.Add("Bundle Version=$($bundleRow.Version)，期望 $ExpectedVersion")
    }

    $chainMsi = @($Probe.Tables['ChainMsiPackage'])[0]
    if ($null -eq $chainMsi) {
        $failures.Add('ChainMsiPackage 表缺失')
    }
    elseif ([string]$chainMsi.ProductVersion -ne $ExpectedVersion) {
        $failures.Add("ChainMsiPackage ProductVersion=$($chainMsi.ProductVersion)，期望 $ExpectedVersion")
    }

    $msiPropertyRows = @($Probe.Tables['MsiProperty'])
    $removeUserDataProperty = @($msiPropertyRows | Where-Object { [string]$_.Name -ieq 'REMOVEUSERDATA' })
    if ($removeUserDataProperty.Count -eq 0) {
        $failures.Add('Burn MsiProperty 缺少 REMOVEUSERDATA 传递')
    }
    elseif (([string]$removeUserDataProperty[0].Value -replace '\s', '') -ine '[RemoveUserData]') {
        $failures.Add("Burn MsiProperty REMOVEUSERDATA=$($removeUserDataProperty[0].Value)，期望 [RemoveUserData]")
    }
    $arpRow = @($msiPropertyRows | Where-Object { [string]$_.Name -ieq 'ARPSYSTEMCOMPONENT' })
    if ($arpRow.Count -eq 0 -or [string]$arpRow[0].Value -ne '1') {
        $failures.Add('Burn MsiProperty ARPSYSTEMCOMPONENT 应为 1')
    }

    $varRows = @($Probe.Tables['Variable'])
    $removeUserDataVar = @($varRows | Where-Object { [string]$_.Variable -ieq 'RemoveUserData' })
    if ($removeUserDataVar.Count -eq 0) {
        $failures.Add('Burn Variable 缺少 RemoveUserData')
    }
    else {
        $v = $removeUserDataVar[0]
        if ([string]$v.Value -ne '1') { $failures.Add("Burn RemoveUserData 默认值=$($v.Value)，期望 1") }
        if ([string]$v.Hidden -eq '1') { $failures.Add('Burn RemoveUserData 不应 hidden（需要可覆盖）') }
        if ([string]$v.Persisted -ne '0') { $failures.Add("Burn RemoveUserData Persisted=$($v.Persisted)，期望 0") }
    }

    $overridable = @($Probe.Tables['WixStdbaOverridableVariable'] | Where-Object { [string]$_.Name -ieq 'RemoveUserData' })
    if ($overridable.Count -eq 0) {
        $failures.Add('Burn WixStdbaOverridableVariable 缺少 RemoveUserData（无法通过命令行保留数据）')
    }

    $payloadRows = @($Probe.Tables['PayloadInfo'])
    $payload = @($payloadRows | Where-Object { [string]$_.Name -ieq $ExpectedMsiName })
    if ($payload.Count -eq 0) {
        $failures.Add("Burn PayloadInfo 缺少内嵌 MSI '$ExpectedMsiName'")
    }
    else {
        $p = $payload[0]
        if ([string]$p.FileSize -ne [string]$ExpectedMsiSize) {
            $failures.Add("Burn 内嵌 MSI FileSize=$($p.FileSize)，期望 $ExpectedMsiSize")
        }
        if ([string]$p.Hash -ine $ExpectedMsiSha1) {
            $failures.Add("Burn 内嵌 MSI Hash=$($p.Hash)，期望 $ExpectedMsiSha1（bundle 不是在当前 MSI 上构建或 payload 不一致）")
        }
    }

    return [pscustomobject]@{
        Ok       = ($failures.Count -eq 0)
        Failures = [string[]]$failures.ToArray()
    }
}

# ---------------------------------------------------------------------------
# 便携 ZIP 产物表
# ---------------------------------------------------------------------------

function Read-PortableZipProbe {
    <#
    .SYNOPSIS
        读取 ZIP 条目；对项目自有载荷计算 SHA256，供“ZIP 内是已签 stage”验证。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [string[]]$HashNames = $script:ProjectOwnedPayloadNames
    )

    $full = Get-NormalizedFullPath $Path
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
        throw [System.IO.FileNotFoundException]::new("ZIP 不存在：$full")
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $hashLeafNames = @($HashNames | ForEach-Object { [System.IO.Path]::GetFileName($_) })

    $zip = [System.IO.Compression.ZipFile]::OpenRead($full)
    try {
        $entries = [System.Collections.Generic.List[object]]::new()
        foreach ($entry in $zip.Entries) {
            $name = ([string]$entry.FullName) -replace '\\', '/'
            $sha = $null
            $leaf = [System.IO.Path]::GetFileName($name)
            if ($hashLeafNames -contains $leaf -and $entry.Length -gt 0) {
                $stream = $entry.Open()
                try {
                    $hasher = [System.Security.Cryptography.SHA256]::Create()
                    try {
                        $sha = [Convert]::ToHexString($hasher.ComputeHash($stream)).ToLowerInvariant()
                    }
                    finally { $hasher.Dispose() }
                }
                finally { $stream.Dispose() }
            }
            $entries.Add([pscustomobject]@{
                    FullName = $name
                    Length   = [long]$entry.Length
                    Sha256   = $sha
                })
        }
        $top = @($entries | ForEach-Object { ($_.FullName -split '/')[0] } | Sort-Object -Unique)
        return [pscustomobject]@{
            Path           = $full
            Entries        = [object[]]$entries.ToArray()
            TopDirectories = [string[]]$top
            EntryCount     = $entries.Count
        }
    }
    finally {
        $zip.Dispose()
    }
}

function Test-PortableZipArtifact {
    <#
    .SYNOPSIS
        检查便携 ZIP 的标记、说明、必需载荷、无 pdb、顶层目录，以及内部项目自有载荷的
        hash/签名状态是否与已签 stage 一致。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][AllowNull()]$Probe,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$Platform,
        [hashtable]$ExpectedPayloadHashes,
        [hashtable]$PayloadSignatureStates,
        [bool]$RequireSignedPayloads = $false
    )
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Probe) {
        $failures.Add('ZIP probe 为空')
        return [pscustomobject]@{ Ok = $false; Failures = [string[]]$failures.ToArray() }
    }

    $prefix = "ClipBridge-$Version-win-$Platform"
    $requiredEntries = @(
        "$prefix/ClipBridge.portable",
        "$prefix/version.txt",
        "$prefix/core_ffi_windows.dll",
        "$prefix/ClipBridgeShell_CS.exe"
    )
    $entryNames = @($Probe.Entries | ForEach-Object { [string]$_.FullName })
    foreach ($required in $requiredEntries) {
        if ($entryNames -notcontains $required) { $failures.Add("ZIP 缺少 $required") }
    }
    $pdbEntries = @($entryNames | Where-Object { $_ -match '(?i)\.pdb$' })
    if ($pdbEntries.Count -gt 0) { $failures.Add("ZIP 含 .pdb：$($pdbEntries -join ', ')") }
    $forbidden = @($entryNames | Where-Object { $_ -match '(?i)\.(msi|msix|msixupload|wixpdb)$' })
    if ($forbidden.Count -gt 0) { $failures.Add("ZIP 含安装器/调试文件：$($forbidden -join ', ')") }
    $dataEntries = @($entryNames | Where-Object { $_ -match "^$([regex]::Escape($prefix))/data/" })
    if ($dataEntries.Count -gt 0) { $failures.Add('ZIP 不应包含用户 data 目录') }

    if ($Probe.TopDirectories.Count -ne 1 -or [string]$Probe.TopDirectories[0] -ne $prefix) {
        $failures.Add("ZIP 顶层目录应为 '$prefix'，实际：$($Probe.TopDirectories -join ', ')")
    }

    if ($ExpectedPayloadHashes) {
        foreach ($key in $ExpectedPayloadHashes.Keys) {
            $entryName = if ([string]$key -match '/') { [string]$key } else { "$prefix/$key" }
            $entry = @($Probe.Entries | Where-Object { [string]$_.FullName -eq $entryName })
            if ($entry.Count -eq 0) {
                $failures.Add("ZIP 缺少已签载荷 $entryName")
                continue
            }
            if ($null -eq $entry[0].Sha256) {
                $failures.Add("ZIP 载荷未计算 hash：$entryName")
                continue
            }
            if ([string]$entry[0].Sha256 -ine [string]$ExpectedPayloadHashes[$key]) {
                $failures.Add("ZIP 载荷 hash 与已签 stage 不一致：$entryName")
            }
        }
    }

    if ($RequireSignedPayloads) {
        if (-not $PayloadSignatureStates) {
            $failures.Add('要求已签载荷但没有提供签名状态')
        }
        else {
            foreach ($state in $PayloadSignatureStates.Values) {
                if ([string]$state.Authenticode -ne 'signed' -or -not [bool]$state.Timestamped) {
                    $failures.Add("ZIP 内项目自有载荷签名无效或缺少时间戳：$($state.Path)")
                }
            }
        }
    }

    return [pscustomobject]@{
        Ok       = ($failures.Count -eq 0)
        Failures = [string[]]$failures.ToArray()
    }
}

# ---------------------------------------------------------------------------
# Authenticode 与 package-summary
# ---------------------------------------------------------------------------

function Get-AuthenticodeArtifactState {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw [System.IO.FileNotFoundException]::new("文件不存在：$Path")
    }
    $sig = Get-AuthenticodeSignature -LiteralPath $Path
    $signed = $null -ne $sig.SignerCertificate
    $status = [string]$sig.Status
    return [pscustomobject]@{
        Path              = (Get-Item -LiteralPath $Path).FullName
        SignatureStatus   = $status
        IsSigned          = [bool]$signed
        IsValid           = [bool]($status -eq 'Valid' -and $signed)
        Timestamped       = [bool]($null -ne $sig.TimeStamperCertificate)
        SignerThumbprint  = if ($sig.SignerCertificate) { [string]$sig.SignerCertificate.Thumbprint } else { $null }
    }
}

function Test-AuthenticodeArtifactState {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$State,
        [string]$ExpectedThumbprint,
        [bool]$RequireTimestamp = $true
    )
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $State) {
        $failures.Add('签名状态为空')
    }
    else {
        if (-not $State.IsSigned) { $failures.Add("未签名：$($State.Path)（$($State.SignatureStatus)）") }
        if (-not $State.IsValid) { $failures.Add("签名验证未通过：$($State.Path)（$($State.SignatureStatus)）") }
        if ($ExpectedThumbprint -and ([string]$State.SignerThumbprint -ine $ExpectedThumbprint)) {
            $failures.Add("签名证书 thumbprint 不匹配：$($State.Path)")
        }
        if ($RequireTimestamp -and -not $State.Timestamped) {
            $failures.Add("签名缺少时间戳：$($State.Path)")
        }
    }
    return [pscustomobject]@{ Ok = ($failures.Count -eq 0); Failures = [string[]]$failures.ToArray() }
}

function Assert-PackageSigningPolicy {
    <#
    .SYNOPSIS
        根据签名模式检查所有产物签名状态，并返回可用于 package-summary 的顶层 signing 对象。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][ValidateSet('None', 'Certificate')][string]$Mode,
        [hashtable]$ArtifactSigningStates = @{},
        [bool]$ZipPayloadsSigned = $false,
        [bool]$HasPortableZip = $false,
        [string]$CertificateThumbprint
    )
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Mode -eq 'Certificate') {
        if ([string]::IsNullOrWhiteSpace($CertificateThumbprint)) {
            $failures.Add('Certificate 模式缺少证书 thumbprint')
        }
        foreach ($key in $ArtifactSigningStates.Keys) {
            $state = $ArtifactSigningStates[$key]
            if ([string]$state.Authenticode -eq 'not-applicable') { continue }
            if ([string]$state.Authenticode -ne 'signed' -or -not [bool]$state.Timestamped) {
                $failures.Add("产物签名无效或缺少时间戳：$key")
            }
            if ($CertificateThumbprint -and $state.PSObject.Properties['SignerThumbprint'] -and
                ([string]$state.SignerThumbprint -ine $CertificateThumbprint)) {
                $failures.Add("产物签名者 thumbprint 不匹配：$key")
            }
        }
        if ($HasPortableZip -and -not $ZipPayloadsSigned) {
            $failures.Add('Certificate 模式要求 ZIP 内项目自有载荷已签名')
        }
        if ($failures.Count -gt 0) {
            throw [System.InvalidOperationException]::new(
                "生产签名验证失败：`n  - " + ($failures -join "`n  - "))
        }
        return [pscustomobject]@{
            mode        = 'Certificate'
            status      = 'signed'
            trusted     = $true
            timestamped = $true
        }
    }

    # None：明确未签名，绝不能装作可信生产签名；也不接受已签名/坏签名的产物冒充 None。
    $failures = [System.Collections.Generic.List[string]]::new()
    foreach ($key in $ArtifactSigningStates.Keys) {
        $state = $ArtifactSigningStates[$key]
        $auth = [string]$state.Authenticode
        if ($auth -notin @('unsigned', 'not-applicable')) {
            $failures.Add("None 模式要求未签名产物，实际 '$auth'：$key")
        }
    }
    if ($failures.Count -gt 0) {
        throw [System.InvalidOperationException]::new(
            "None 模式签名状态不一致：`n  - " + ($failures -join "`n  - "))
    }
    return [pscustomobject]@{
        mode        = 'None'
        status      = 'unsigned'
        trusted     = $false
        timestamped = $false
    }
}

function New-PackageSummary {
    <#
    .SYNOPSIS
        校验所有必需产物存在、非空并计算最终 SHA256，然后写 package-summary.json。
        缺文件、缺内部 user MSI、签名状态不符合模式均 fatal。
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$OutDir,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$Platform,
        [Parameter(Mandatory)][string[]]$Formats,
        [Parameter(Mandatory)][object[]]$ExpectedArtifacts,
        [Parameter(Mandatory)][object]$Signing,
        [hashtable]$ArtifactSigningStates = @{},
        [Parameter(Mandatory)][object]$NativeEvidence,
        [string]$GeneratedAtUtc
    )
    $outRoot = Get-NormalizedFullPath $OutDir
    if (-not (Test-Path -LiteralPath $outRoot -PathType Container)) {
        throw [System.IO.DirectoryNotFoundException]::new("输出目录不存在：$outRoot")
    }
    $formatSet = @(Resolve-FormatSelection -Formats $Formats -FormatsSpecified $true)
    $native = Get-ValidatedNativeEvidence -NativeEvidence $NativeEvidence

    if (-not $Signing) { throw [System.ArgumentException]::new('Signing 状态不能为空。') }
    if ($Signing.mode -notin @('None', 'Certificate')) {
        throw [System.ArgumentException]::new("Signing.mode 非法：$($Signing.mode)")
    }
    if ($Signing.mode -eq 'Certificate') {
        if ($Signing.status -ne 'signed' -or -not [bool]$Signing.trusted -or -not [bool]$Signing.timestamped) {
            throw [System.InvalidOperationException]::new('Certificate 模式必须提供 signed/trusted/timestamped 的顶层签名状态。')
        }
    }
    else {
        if ($Signing.status -ne 'unsigned' -or [bool]$Signing.trusted) {
            throw [System.InvalidOperationException]::new('None 模式必须如实标记 unsigned 且 trusted=false。')
        }
    }

    $specs = @($ExpectedArtifacts)
    $byFormat = @{}
    $knownFormats = @('machine-msi', 'user-exe', 'user-msi', 'portable-zip')
    foreach ($spec in $specs) {
        $fmt = [string]$spec.Format
        if ($fmt -notin $knownFormats) {
            throw [System.InvalidOperationException]::new("产物计划包含未知格式：$fmt")
        }
        if ($byFormat.ContainsKey($fmt)) {
            throw [System.InvalidOperationException]::new("产物计划重复格式：$fmt")
        }
        $byFormat[$fmt] = $spec
    }
    foreach ($fmt in $formatSet) {
        if (-not $byFormat.ContainsKey($fmt)) {
            throw [System.InvalidOperationException]::new("产物计划缺少所选格式：$fmt")
        }
        if ([bool]$byFormat[$fmt].Internal -or [string]$byFormat[$fmt].Role -ne 'release') {
            throw [System.InvalidOperationException]::new("公开格式 $fmt 不能被标记为 internal。")
        }
    }
    if ($formatSet -contains 'user-exe') {
        if (-not $byFormat.ContainsKey('user-msi') -or -not [bool]$byFormat['user-msi'].Internal) {
            throw [System.InvalidOperationException]::new('选择 user-exe 时必须产出并标记内部 user-msi。')
        }
    }
    elseif ($byFormat.ContainsKey('user-msi')) {
        throw [System.InvalidOperationException]::new('未选择 user-exe 却出现内部 user-msi。')
    }
    if ($formatSet -notcontains 'machine-msi' -and $byFormat.ContainsKey('machine-msi')) {
        throw [System.InvalidOperationException]::new('未选择 machine-msi 却出现 machine MSI。')
    }
    if ($formatSet -notcontains 'portable-zip' -and $byFormat.ContainsKey('portable-zip')) {
        throw [System.InvalidOperationException]::new('未选择 portable-zip 却出现便携 ZIP。')
    }

    $artifacts = [System.Collections.Generic.List[object]]::new()
    $seenNames = @{}
    foreach ($spec in $specs) {
        $name = [string]$spec.Name
        if ([string]::IsNullOrWhiteSpace($name)) {
            throw [System.InvalidOperationException]::new('产物计划含空文件名。')
        }
        if ($seenNames.ContainsKey($name)) {
            throw [System.InvalidOperationException]::new("产物计划重名：$name")
        }
        $seenNames[$name] = $true

        $path = Join-Path $outRoot $name
        if (-not (Test-PathStrictlyUnder -Path $path -Root $outRoot)) {
            throw [System.InvalidOperationException]::new("产物路径逃出 outdir：$path")
        }
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw [System.IO.FileNotFoundException]::new("必需产物缺失（fatal）：$path")
        }
        $item = Get-Item -LiteralPath $path
        if ($item.Length -le 0) {
            throw [System.IO.InvalidDataException]::new("必需产物为空（fatal）：$path")
        }

        $isZip = ($spec.Format -eq 'portable-zip')
        $state = $ArtifactSigningStates[$name]
        if ($null -eq $state) {
            if ($isZip) {
                $state = [pscustomobject]@{ Authenticode = 'not-applicable'; Timestamped = $false; PayloadsSigned = $false }
            }
            else {
                $state = [pscustomobject]@{ Authenticode = 'unsigned'; Timestamped = $false }
            }
        }
        $auth = [string]$state.Authenticode
        if ($isZip) {
            if ($auth -ne 'not-applicable') {
                throw [System.InvalidOperationException]::new("ZIP 的 Authenticode 状态应为 not-applicable，实际 '$auth'。")
            }
        }
        elseif ($auth -notin @('signed', 'unsigned')) {
            throw [System.InvalidOperationException]::new("产物 $name 的 Authenticode 状态非法：'$auth'")
        }

        if ($Signing.mode -eq 'Certificate') {
            if ($isZip) {
                if (-not [bool]$state.PayloadsSigned) {
                    throw [System.InvalidOperationException]::new("Certificate 模式要求 ZIP 内项目自有载荷已签名：$name")
                }
            }
            elseif ($auth -ne 'signed' -or -not [bool]$state.Timestamped) {
                throw [System.InvalidOperationException]::new("Certificate 模式要求 $name 已签名并带时间戳。")
            }
        }
        elseif (-not $isZip -and $auth -ne 'unsigned') {
            throw [System.InvalidOperationException]::new("None 模式要求 $name 为未签名，实际 '$auth'。")
        }

        $artifacts.Add([ordered]@{
                name   = $name
                format = [string]$spec.Format
                role   = [string]$spec.Role
                internal = [bool]$spec.Internal
                size   = [long]$item.Length
                sha256 = (Get-FileSha256 -Path $path)
                signing = [ordered]@{
                    authenticode   = $auth
                    timestamped    = [bool]$state.Timestamped
                    payloadsSigned = $(if ($isZip) { [bool]$state.PayloadsSigned } else { $null })
                }
            })
    }

    if ($artifacts.Count -ne $specs.Count) {
        throw [System.InvalidOperationException]::new('产物清单数量不一致（缺失断言证据）。')
    }

    $summary = [ordered]@{
        schemaVersion  = $script:PackageSummarySchemaVersion
        product        = 'ClipBridge'
        version        = [string]$Version
        platform       = [string]$Platform
        formats        = [string[]]$formatSet
        signing        = [ordered]@{
            mode        = [string]$Signing.mode
            status      = [string]$Signing.status
            trusted     = [bool]$Signing.trusted
            timestamped = [bool]$Signing.timestamped
        }
        nativeEvidence = $native
        generatedAtUtc = if ($GeneratedAtUtc) { [string]$GeneratedAtUtc } else { (Get-Date).ToUniversalTime().ToString('o') }
        artifacts      = [object[]]$artifacts.ToArray()
    }

    $json = $summary | ConvertTo-Json -Depth 8
    $summaryPath = Join-Path $outRoot 'package-summary.json'
    if (-not (Test-PathStrictlyUnder -Path $summaryPath -Root $outRoot)) {
        throw [System.InvalidOperationException]::new("summary 路径逃出 outdir：$summaryPath")
    }
    [System.IO.File]::WriteAllText($summaryPath, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
    return [pscustomobject]$summary
}

Export-ModuleMember -Function @(
    'Get-MissingWixFiles',
    'Assert-Sha256Hex',
    'Assert-CoreDllHashMatch',
    'Get-ValidatedNativeEvidence',
    'Get-PackageFormatOrder',
    'Resolve-FormatSelection',
    'Assert-SupportedPlatform',
    'Get-BuildPlan',
    'Get-ArtifactName',
    'Get-ExpectedArtifacts',
    'Get-BuildSteps',
    'Get-ProjectOwnedPayloadNames',
    'Get-ProjectOwnedPayloadFiles',
    'Get-FileSha256',
    'Get-FileSha1',
    'Get-NormalizedFullPath',
    'Test-PathStrictlyUnder',
    'Assert-SafeOutputRoot',
    'Assert-SafeCleanupTarget',
    'Remove-ItemSafely',
    'Invoke-MsiQueryRows',
    'Read-MsiArtifactProbe',
    'Test-ConditionContainsToken',
    'Get-MsiSequenceCondition',
    'Test-MsiArtifact',
    'Read-BundleWixPdbXml',
    'Read-BundleWixPdbProbe',
    'Test-BundleArtifact',
    'Read-PortableZipProbe',
    'Test-PortableZipArtifact',
    'Get-AuthenticodeArtifactState',
    'Test-AuthenticodeArtifactState',
    'Assert-PackageSigningPolicy',
    'New-PackageSummary'
)
