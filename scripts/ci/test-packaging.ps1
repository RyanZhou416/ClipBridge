<#
.SYNOPSIS
    Windows 打包接口的纯 fixture 测试（不安装产品、不申请 UAC、不运行真实打包）。

.DESCRIPTION
    覆盖：
      - 所有非空格式子集、-Only 兼容映射、非法格式/冲突/arm64 fail-fast；
      - 依赖计划与一次 publish/一次 heat 的顺序；
      - 缺 payload / 缺必需产物 / 缺内部 user MSI 的 fatal 行为；
      - MSI 产物表（REMOVEUSERDATA、升级排除条件、scope、WordCount、pdb、core_ffi）；
      - Burn .wixpdb 表（RemoveUserData 默认/可覆盖、MsiProperty 传递、内嵌 MSI hash/size）；
      - 便携 ZIP 结构/内部载荷 hash/签名状态；
      - 签名顺序、签名入口退出码、package-summary.json 结构；
      - 清理路径必须位于允许的 outdir 下；
      - 主脚本/sign 脚本的 AST 接口契约。

    该脚本不调用 dotnet/msbuild/cargo，也不调用主构建脚本的有效格式路径；
    仅在 -SkipBuildScriptExitCode 未设置时用“非法格式”调用主脚本验证退出码
    （主脚本在任何 native/dotnet 动作之前 fail-fast）。

.PARAMETER SkipBuildScriptExitCode
    跳过对 build-windows-installers.ps1 的非法格式/参数退出码测试。
#>
[CmdletBinding()]
param(
    [switch]$SkipBuildScriptExitCode
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$modulePath = Join-Path $PSScriptRoot 'ClipBridge.Packaging.psm1'
$mainScript = Join-Path $repoRoot 'scripts/build-windows-installers.ps1'
$signScript = Join-Path $PSScriptRoot 'sign-windows.ps1'
$pwsh = (Get-Command pwsh -ErrorAction Stop).Source
Import-Module $modulePath -Force

$script:Passed = 0
$script:Failed = 0
$script:FailureMessages = [System.Collections.Generic.List[string]]::new()
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("clipbridge-packaging-tests-$([guid]::NewGuid().ToString('N').Substring(0, 8))")
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

function Test-Case {
    param([string]$Name, [scriptblock]$Body)
    try {
        & $Body
        $script:Passed++
        Write-Host "PASS  $Name"
    }
    catch {
        $script:Failed++
        $message = "$Name :: $($_.Exception.Message)"
        $script:FailureMessages.Add($message)
        Write-Host "FAIL  $message" -ForegroundColor Red
    }
}

function Assert-True {
    param([bool]$Condition, [string]$Message = 'condition is false')
    if (-not $Condition) { throw $Message }
}

function Assert-Equal {
    param($Expected, $Actual, [string]$Message = 'values differ')
    if ("$Expected" -ne "$Actual") {
        throw "$Message (expected=$Expected actual=$Actual)"
    }
}

function Assert-SequenceEqual {
    param([string[]]$Expected, [string[]]$Actual, [string]$Message = 'sequence differs')
    $e = @($Expected) -join '|'
    $a = @($Actual) -join '|'
    if ($e -ne $a) { throw "$Message (expected=$e actual=$a)" }
}

function Assert-Throws {
    param(
        [scriptblock]$Body,
        [Type]$ExceptionType = [System.Exception],
        [string]$Message = 'expected an exception'
    )
    $caught = $null
    try { & $Body }
    catch { $caught = $_ }
    if ($null -eq $caught) { throw "$Message (no exception was thrown)" }
    if (-not ($caught.Exception -is $ExceptionType)) {
        throw "$Message (expected $($ExceptionType.FullName), got $($caught.Exception.GetType().FullName): $($caught.Exception.Message))"
    }
}

function New-TestDir {
    param([string]$Prefix)
    $path = Join-Path $tempRoot ("$Prefix-$([guid]::NewGuid().ToString('N').Substring(0, 8))")
    New-Item -ItemType Directory -Force -Path $path | Out-Null
    return $path
}

function New-TestFile {
    param([string]$Path, [string]$Content = 'x')
    $dir = Split-Path -Parent $Path
    if ($dir) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    [System.IO.File]::WriteAllText($Path, $Content, [System.Text.UTF8Encoding]::new($false))
    return $Path
}

function Invoke-External {
    param([string]$FilePath, [string[]]$Arguments)
    $output = & $FilePath @Arguments 2>&1
    return [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Output   = [object[]]@($output)
    }
}

# ---------------------------------------------------------------------------
# fixtures
# ---------------------------------------------------------------------------

function New-MsiProbeFixture {
    param(
        [ValidateSet('machine-msi', 'user-msi')][string]$Kind = 'machine-msi',
        [hashtable]$PropertyOverrides = @{},
        [string[]]$FileNames,
        [string]$RemoveDataCondition,
        [string]$StartupCondition,
        [string]$TraceCondition,
        [int]$WordCount,
        [string]$InstallParent,
        [object[]]$Sequences,
        [object[]]$Upgrade,
        [bool]$IncludeRemoveExisting = $true,
        [bool]$IncludeUpgrade = $true
    )

    $props = [ordered]@{
        ProductVersion = '0.1.0'
        REMOVEUSERDATA = '1'
    }
    if ($Kind -eq 'machine-msi') {
        $props['ALLUSERS'] = '1'
    }
    else {
        $props['ALLUSERS'] = '2'
        $props['MSIINSTALLPERUSER'] = '1'
        $props['ARPSYSTEMCOMPONENT'] = '1'
    }
    foreach ($key in @($PropertyOverrides.Keys)) {
        if ($null -eq $PropertyOverrides[$key]) { $props.Remove($key) }
        else { $props[[string]$key] = [string]$PropertyOverrides[$key] }
    }

    $word = if ($PSBoundParameters.ContainsKey('WordCount')) { $WordCount }
    else { if ($Kind -eq 'machine-msi') { 2 } else { 10 } }
    $parent = if ($PSBoundParameters.ContainsKey('InstallParent')) { $InstallParent }
    else { if ($Kind -eq 'machine-msi') { 'ProgramFiles64Folder' } else { 'ProgramsFolder' } }
    $files = if ($PSBoundParameters.ContainsKey('FileNames')) { [string[]]$FileNames }
    else { [string[]]@('short|ClipBridgeShell_CS.exe', 'core_ffi_windows.dll') }

    if ($PSBoundParameters.ContainsKey('Sequences')) {
        $sequenceRows = [System.Collections.Generic.List[object]]::new()
        foreach ($s in @($Sequences)) { $sequenceRows.Add($s) }
    }
    else {
        $sequenceRows = [System.Collections.Generic.List[object]]::new()
        $sequenceRows.Add([pscustomobject]@{
                Action    = 'RemoveUserDataCA'
                Condition = 'REMOVEUSERDATA="1" AND Installed AND (REMOVE~="ALL") AND NOT UPGRADINGPRODUCTCODE'
            })
        $sequenceRows.Add([pscustomobject]@{
                Action    = 'CleanupStartupEntryCA'
                Condition = 'Installed AND (REMOVE~="ALL") AND NOT UPGRADINGPRODUCTCODE'
            })
        if ($Kind -eq 'user-msi') {
            $sequenceRows.Add([pscustomobject]@{
                    Action    = 'CleanupInstallTraceCA'
                    Condition = 'Installed AND (REMOVE~="ALL") AND NOT UPGRADINGPRODUCTCODE'
                })
        }
        if ($IncludeRemoveExisting) {
            $sequenceRows.Add([pscustomobject]@{ Action = 'RemoveExistingProducts'; Condition = '' })
        }
    }

    if ($PSBoundParameters.ContainsKey('RemoveDataCondition')) {
        $row = @($sequenceRows | Where-Object { $_.Action -eq 'RemoveUserDataCA' })[0]
        if ($row) { $row.Condition = $RemoveDataCondition }
    }
    if ($PSBoundParameters.ContainsKey('StartupCondition')) {
        $row = @($sequenceRows | Where-Object { $_.Action -eq 'CleanupStartupEntryCA' })[0]
        if ($row) { $row.Condition = $StartupCondition }
    }
    if ($PSBoundParameters.ContainsKey('TraceCondition')) {
        $row = @($sequenceRows | Where-Object { $_.Action -eq 'CleanupInstallTraceCA' })[0]
        if ($row) { $row.Condition = $TraceCondition }
    }

    if ($PSBoundParameters.ContainsKey('Upgrade')) {
        $upgradeRows = [object[]]$Upgrade
    }
    elseif ($IncludeUpgrade) {
        $upgradeRows = [object[]]@(
            [pscustomobject]@{ UpgradeCode = '{A1B2C3D4-E5F6-7890-ABCD-EF1234567891}'; VersionMin = ''; VersionMax = '0.1.0'; ActionProperty = 'WIX_UPGRADE_DETECTED' },
            [pscustomobject]@{ UpgradeCode = '{A1B2C3D4-E5F6-7890-ABCD-EF1234567891}'; VersionMin = '0.1.0'; VersionMax = ''; ActionProperty = 'WIX_DOWNGRADE_DETECTED' }
        )
    }
    else {
        $upgradeRows = [object[]]@()
    }

    return [pscustomobject]@{
        Path             = "<fixture:$Kind>"
        Properties       = $props
        FileNames        = [string[]]$files
        Sequences        = [object[]]$sequenceRows.ToArray()
        Upgrade          = $upgradeRows
        DirectoryParents = @{ INSTALLFOLDER = $parent }
        WordCount        = $word
    }
}

function New-BundlePdbXml {
    param(
        [string]$PropertyValue = '[RemoveUserData]',
        [bool]$IncludeProperty = $true,
        [string]$VariableValue = '1',
        [string]$VariablePersisted = '0',
        [bool]$IncludeOverridable = $true,
        [string]$PayloadHash = 'A4DE38FF854CCC1D441D7DD2D4FDB9F5F447AE2D',
        [string]$PayloadSize = '123',
        [bool]$IncludeBundleRow = $true,
        [bool]$IncludeChainMsiRow = $true
    )
    $propertyRow = if ($IncludeProperty) {
        "<row><field>ClipBridgeUserMsi</field><field>REMOVEUSERDATA</field><field>$PropertyValue</field></row>"
    }
    else { '' }
    $overridableRow = if ($IncludeOverridable) { '<row><field>RemoveUserData</field></row>' } else { '' }
    $bundleTable = if ($IncludeBundleRow) { '<table name="WixBundle"><row><field>0.1.0</field></row></table>' } else { '' }
    $chainTable = if ($IncludeChainMsiRow) { '<table name="ChainMsiPackage"><row><field>0.1.0</field></row></table>' } else { '' }
    return @"
<?xml version="1.0" encoding="utf-8"?>
<wixPdb xmlns="http://schemas.microsoft.com/wix/2006/pdbs">
  <wixOutput type="Bundle" xmlns="http://schemas.microsoft.com/wix/2006/outputs">
    <tableDefinitions xmlns="http://schemas.microsoft.com/wix/2006/tables">
      <tableDefinition name="WixBundle"><columnDefinition name="Version" /></tableDefinition>
      <tableDefinition name="ChainMsiPackage"><columnDefinition name="ProductVersion" /></tableDefinition>
      <tableDefinition name="MsiProperty">
        <columnDefinition name="ChainPackage_" /><columnDefinition name="Name" /><columnDefinition name="Value" />
      </tableDefinition>
      <tableDefinition name="Variable">
        <columnDefinition name="Variable" /><columnDefinition name="Value" /><columnDefinition name="Type" />
        <columnDefinition name="Hidden" /><columnDefinition name="Persisted" />
      </tableDefinition>
      <tableDefinition name="WixStdbaOverridableVariable"><columnDefinition name="Name" /></tableDefinition>
      <tableDefinition name="PayloadInfo">
        <columnDefinition name="Payload" /><columnDefinition name="Name" />
        <columnDefinition name="FileSize" /><columnDefinition name="Hash" />
      </tableDefinition>
    </tableDefinitions>
    $bundleTable
    $chainTable
    <table name="MsiProperty">
      <row><field>ClipBridgeUserMsi</field><field>ARPSYSTEMCOMPONENT</field><field>1</field></row>
      $propertyRow
    </table>
    <table name="Variable">
      <row><field>RemoveUserData</field><field>$VariableValue</field><field>numeric</field><field>0</field><field>$VariablePersisted</field></row>
    </table>
    <table name="WixStdbaOverridableVariable">
      $overridableRow
    </table>
    <table name="PayloadInfo">
      <row><field>ClipBridgeUserMsi</field><field>ClipBridge-0.1.0-win-x64-user.msi</field><field>$PayloadSize</field><field>$PayloadHash</field></row>
    </table>
  </wixOutput>
</wixPdb>
"@
}

function New-ZipFixture {
    param(
        [string]$Path,
        [string]$TopDirectory = 'ClipBridge-0.1.0-win-x64',
        [switch]$NoMarker,
        [switch]$MissingCore,
        [switch]$WithPdb
    )
    $sourceRoot = Join-Path (New-TestDir 'zip-parent') $TopDirectory
    New-Item -ItemType Directory -Force -Path $sourceRoot | Out-Null
    $top = $sourceRoot
    if (-not $NoMarker) { New-TestFile -Path (Join-Path $top 'ClipBridge.portable') -Content '' | Out-Null }
    New-TestFile -Path (Join-Path $top 'version.txt') -Content 'ClipBridge fixture' | Out-Null
    if (-not $MissingCore) { New-TestFile -Path (Join-Path $top 'core_ffi_windows.dll') -Content 'fixture-rust-dll' | Out-Null }
    New-TestFile -Path (Join-Path $top 'ClipBridgeShell_CS.exe') -Content 'fixture-app-exe' | Out-Null
    if ($WithPdb) { New-TestFile -Path (Join-Path $top 'bad.pdb') -Content 'pdb' | Out-Null }

    $zipDir = Split-Path -Parent $Path
    if ($zipDir) { New-Item -ItemType Directory -Force -Path $zipDir | Out-Null }
    if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $sourceRoot, $Path, [System.IO.Compression.CompressionLevel]::Optimal, $true)

    $probe = Read-PortableZipProbe -Path $Path
    $hashes = @{}
    $exe = Join-Path $top 'ClipBridgeShell_CS.exe'
    if (Test-Path -LiteralPath $exe) { $hashes["$TopDirectory/ClipBridgeShell_CS.exe"] = Get-FileSha256 -Path $exe }
    $core = Join-Path $top 'core_ffi_windows.dll'
    if (Test-Path -LiteralPath $core) { $hashes["$TopDirectory/core_ffi_windows.dll"] = Get-FileSha256 -Path $core }
    return [pscustomobject]@{ Probe = $probe; Hashes = $hashes; Top = $TopDirectory }
}

function New-SummaryFixture {
    param(
        [string[]]$Formats = @('machine-msi', 'user-exe', 'portable-zip'),
        [string]$Version = '0.1.0',
        [string]$Platform = 'x64',
        [bool]$SourceReusedVerified = $false
    )
    $out = New-TestDir 'summary-out'
    $specs = @(Get-ExpectedArtifacts -Formats $Formats -Version $Version -Platform $Platform)
    foreach ($spec in $specs) {
        $path = Join-Path $out $spec.Name
        if ($spec.Format -eq 'portable-zip') {
            New-TestFile -Path $path -Content 'zip-fixture' | Out-Null
        }
        else {
            [System.IO.File]::WriteAllBytes($path, [byte[]](1..16))
        }
    }
    $states = @{}
    foreach ($spec in $specs) {
        if ($spec.Format -eq 'portable-zip') {
            $states[$spec.Name] = [pscustomobject]@{ Authenticode = 'not-applicable'; Timestamped = $false; PayloadsSigned = $false }
        }
        else {
            $states[$spec.Name] = [pscustomobject]@{ Authenticode = 'unsigned'; Timestamped = $false }
        }
    }
    $signing = Assert-PackageSigningPolicy -Mode None -ArtifactSigningStates $states
    $nativeHash = 'a' * 64
    $native = [pscustomobject]@{
        inputDllSha256       = $nativeHash
        stageDllSha256       = $nativeHash
        sourceReusedVerified = $SourceReusedVerified
    }
    $summary = New-PackageSummary `
        -OutDir $out -Version $Version -Platform $Platform -Formats $Formats `
        -ExpectedArtifacts $specs -Signing $signing -ArtifactSigningStates $states `
        -NativeEvidence $native -GeneratedAtUtc '2026-01-01T00:00:00Z'
    return [pscustomobject]@{ OutDir = $out; Specs = $specs; States = $states; Signing = $signing; NativeEvidence = $native; Summary = $summary }
}

# ---------------------------------------------------------------------------
# format selection / dependencies
# ---------------------------------------------------------------------------

$allFormats = @('machine-msi', 'user-exe', 'portable-zip')
$subsets = @()
for ($mask = 1; $mask -lt 8; $mask++) {
    $set = @()
    for ($i = 0; $i -lt 3; $i++) {
        if ($mask -band (1 -shl $i)) { $set += $allFormats[$i] }
    }
    $subsets += , [string[]]$set
}

Test-Case 'every non-empty format subset resolves canonically' {
    foreach ($subset in $subsets) {
        $actual = @(Resolve-FormatSelection -Formats $subset -FormatsSpecified $true)
        $expected = @($allFormats | Where-Object { $subset -contains $_ })
        Assert-SequenceEqual $expected $actual "subset $($subset -join ',')"
    }
}

Test-Case 'format input order/duplicates do not change the canonical set' {
    $actual = @(Resolve-FormatSelection -Formats @('portable-zip', 'machine-msi', 'machine-msi') -FormatsSpecified $true)
    Assert-SequenceEqual @('machine-msi', 'portable-zip') $actual
}

Test-Case 'only compatibility maps to the same sets' {
    Assert-SequenceEqual $allFormats @(Resolve-FormatSelection -Only All -OnlySpecified $true)
    Assert-SequenceEqual @('machine-msi') @(Resolve-FormatSelection -Only MachineMsi -OnlySpecified $true)
    Assert-SequenceEqual @('user-exe') @(Resolve-FormatSelection -Only UserExe -OnlySpecified $true)
    Assert-SequenceEqual @('portable-zip') @(Resolve-FormatSelection -Only PortableZip -OnlySpecified $true)
}

Test-Case 'no format input defaults to all three (legacy behavior)' {
    Assert-SequenceEqual $allFormats @(Resolve-FormatSelection)
}

Test-Case 'formats and only together are rejected' {
    Assert-Throws {
        Resolve-FormatSelection -Formats @('machine-msi') -FormatsSpecified $true -Only MachineMsi -OnlySpecified $true
    } ([System.ArgumentException])
}

Test-Case 'invalid/empty format values are rejected' {
    foreach ($bad in @('msix', 'winget', 'user-msi', 'all', 'machine_msi', '')) {
        Assert-Throws {
            Resolve-FormatSelection -Formats @($bad) -FormatsSpecified $true
        } ([System.ArgumentException]) "format '$bad'"
    }
    Assert-Throws {
        Resolve-FormatSelection -Formats @() -FormatsSpecified $true
    } ([System.ArgumentException])
}

Test-Case 'platform is x64-only with an explicit arm64 failure' {
    Assert-Equal 'x64' (Assert-SupportedPlatform -Platform x64)
    Assert-Throws { Assert-SupportedPlatform -Platform arm64 } ([System.ArgumentException])
    Assert-Throws { Assert-SupportedPlatform -Platform x86 } ([System.ArgumentException])
}

Test-Case 'build plan dependencies and one-publish/one-heat invariants' {
    foreach ($subset in $subsets) {
        $plan = Get-BuildPlan -Formats $subset
        Assert-Equal ($subset -contains 'machine-msi') $plan.NeedMachineMsi "machine $subset"
        Assert-Equal ($subset -contains 'user-exe') $plan.NeedUserMsi "user $subset"
        Assert-Equal ($subset -contains 'user-exe') $plan.NeedBundle "bundle $subset"
        Assert-Equal ($subset -contains 'portable-zip') $plan.NeedPortableZip "zip $subset"
        Assert-Equal ($plan.NeedMachineMsi -or $plan.NeedUserMsi) $plan.NeedMsi "msi $subset"
        Assert-Equal $plan.NeedMsi $plan.NeedWix "wix $subset"
        Assert-Equal $plan.NeedMsi $plan.HeatOnce "heat $subset"
        Assert-Equal ($subset -contains 'user-exe') (@($plan.InternalFormats) -contains 'user-msi') "internal $subset"
    }
}

Test-Case 'expected artifact plan includes internal user MSI only for user-exe' {
    $all = @(Get-ExpectedArtifacts -Formats $allFormats -Version '0.1.0' -Platform x64)
    Assert-Equal 4 $all.Count
    Assert-SequenceEqual @('machine-msi', 'user-exe', 'user-msi', 'portable-zip') @($all | ForEach-Object { $_.Format })
    $internal = @($all | Where-Object { $_.Format -eq 'user-msi' })[0]
    Assert-Equal $true $internal.Internal
    Assert-Equal 'internal' $internal.Role

    $zipOnly = @(Get-ExpectedArtifacts -Formats @('portable-zip') -Version '0.1.0' -Platform x64)
    Assert-Equal 1 $zipOnly.Count
    Assert-Equal 'portable-zip' $zipOnly[0].Format
}

Test-Case 'signing order is payload -> MSI -> bundle -> ZIP' {
    $steps = @(Get-BuildSteps -Formats $allFormats -SigningMode Certificate)
    $index = @{}
    for ($i = 0; $i -lt $steps.Count; $i++) { $index[$steps[$i]] = $i }
    foreach ($required in @('publish', 'sign-payload', 'heat', 'machine-msi', 'user-msi', 'sign-msi', 'bundle', 'sign-bundle', 'portable-zip', 'verify-zip-payload-signatures', 'verify-artifacts', 'summary')) {
        Assert-True $index.ContainsKey($required) "missing step $required"
    }
    Assert-True ($index['sign-payload'] -lt $index['heat']) 'payload must be signed before heat'
    Assert-True ($index['heat'] -lt $index['machine-msi']) 'heat before machine MSI'
    Assert-True ($index['heat'] -lt $index['user-msi']) 'heat before user MSI'
    Assert-True ($index['user-msi'] -lt $index['sign-msi']) 'MSI built before signed'
    Assert-True ($index['sign-msi'] -lt $index['bundle']) 'internal MSI signed before bundle'
    Assert-True ($index['bundle'] -lt $index['sign-bundle']) 'bundle built before signed'
    Assert-True ($index['sign-bundle'] -lt $index['portable-zip']) 'bundle signed before ZIP'
    Assert-True ($index['portable-zip'] -lt $index['verify-zip-payload-signatures']) 'ZIP verified after creation'
    Assert-Equal 1 @($steps | Where-Object { $_ -eq 'publish' }).Count
    Assert-Equal 1 @($steps | Where-Object { $_ -eq 'heat' }).Count
}

Test-Case 'None build plan contains no signing steps' {
    $steps = @(Get-BuildSteps -Formats $allFormats -SigningMode None)
    Assert-Equal 0 @($steps | Where-Object { $_ -like 'sign-*' -or $_ -eq 'verify-zip-payload-signatures' }).Count
    Assert-True ($steps -contains 'heat') 'MSI selection needs heat'
    $zipSteps = @(Get-BuildSteps -Formats @('portable-zip') -SigningMode None)
    Assert-True (-not ($zipSteps -contains 'heat')) 'ZIP-only must not heat'
    Assert-True (-not ($zipSteps -contains 'machine-msi')) 'ZIP-only must not build MSI'
    Assert-True (-not ($zipSteps -contains 'user-msi')) 'ZIP-only must not build user MSI'
}

# ---------------------------------------------------------------------------
# path safety / project-owned payload
# ---------------------------------------------------------------------------

Test-Case 'safe output root rejects dangerous roots' {
    $safe = Join-Path $tempRoot 'out-root'
    New-Item -ItemType Directory -Force -Path $safe | Out-Null
    Assert-Equal $safe (Assert-SafeOutputRoot -Path $safe -RepoRoot $repoRoot)
    $homeRoot = if ($env:USERPROFILE) { $env:USERPROFILE } elseif ($env:HOME) { $env:HOME } else { $null }
    if ($homeRoot) {
        Assert-Throws { Assert-SafeOutputRoot -Path $homeRoot -RepoRoot $repoRoot } ([System.InvalidOperationException])
    }
    Assert-Throws { Assert-SafeOutputRoot -Path $repoRoot -RepoRoot $repoRoot } ([System.InvalidOperationException])
    Assert-Throws { Assert-SafeOutputRoot -Path ([System.IO.Path]::GetPathRoot($safe)) -RepoRoot $repoRoot } ([System.InvalidOperationException])
}

Test-Case 'cleanup targets must be strictly inside outdir' {
    $out = New-TestDir 'cleanup-out'
    $stage = Join-Path $out 'stage'
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Assert-Equal $stage (Assert-SafeCleanupTarget -Path $stage -AllowedRoot $out)
    Assert-Throws { Assert-SafeCleanupTarget -Path $out -AllowedRoot $out } ([System.InvalidOperationException])
    Assert-Throws { Assert-SafeCleanupTarget -Path (Join-Path $out '..' 'outside') -AllowedRoot $out } ([System.InvalidOperationException])
    Assert-Throws { Assert-SafeCleanupTarget -Path ([System.IO.Path]::GetTempPath()) -AllowedRoot $out } ([System.InvalidOperationException])
}

Test-Case 'owned payload list excludes third-party DLLs and requires exe/core' {
    $stage = New-TestDir 'payload-stage'
    New-TestFile -Path (Join-Path $stage 'ClipBridgeShell_CS.exe') -Content 'exe' | Out-Null
    New-TestFile -Path (Join-Path $stage 'ClipBridgeShell_CS.dll') -Content 'app' | Out-Null
    New-TestFile -Path (Join-Path $stage 'ClipBridgeShell_CS.Core.dll') -Content 'core' | Out-Null
    New-TestFile -Path (Join-Path $stage 'core_ffi_windows.dll') -Content 'rust' | Out-Null
    New-TestFile -Path (Join-Path $stage 'e_sqlite3.dll') -Content 'third-party' | Out-Null

    $owned = @(Get-ProjectOwnedPayloadFiles -StageDir $stage | ForEach-Object { [System.IO.Path]::GetFileName($_) })
    Assert-SequenceEqual @('ClipBridgeShell_CS.exe', 'ClipBridgeShell_CS.dll', 'ClipBridgeShell_CS.Core.dll', 'core_ffi_windows.dll') $owned
    Assert-True (-not ($owned -contains 'e_sqlite3.dll')) 'third-party DLL must not be signed as project-owned'

    Remove-Item -LiteralPath (Join-Path $stage 'ClipBridgeShell_CS.exe') -Force
    Assert-Throws { Get-ProjectOwnedPayloadFiles -StageDir $stage } ([System.IO.FileNotFoundException])
    New-TestFile -Path (Join-Path $stage 'ClipBridgeShell_CS.exe') -Content 'exe' | Out-Null
    Remove-Item -LiteralPath (Join-Path $stage 'core_ffi_windows.dll') -Force
    Assert-Throws { Get-ProjectOwnedPayloadFiles -StageDir $stage } ([System.IO.FileNotFoundException])
}

# ---------------------------------------------------------------------------
# MSI product tables
# ---------------------------------------------------------------------------

Test-Case 'machine MSI fixture passes all product-table assertions' {
    $probe = New-MsiProbeFixture -Kind machine-msi
    $result = Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0'
    Assert-True $result.Ok ($result.Failures -join '; ')
}

Test-Case 'user MSI fixture passes all product-table assertions' {
    $probe = New-MsiProbeFixture -Kind user-msi
    $result = Test-MsiArtifact -Probe $probe -Kind user-msi -ExpectedVersion '0.1.0'
    Assert-True $result.Ok ($result.Failures -join '; ')
}

Test-Case 'MSI missing/default REMOVEUSERDATA is fatal' {
    foreach ($overrides in @(@{ REMOVEUSERDATA = '0' }, @{ REMOVEUSERDATA = $null })) {
        $probe = New-MsiProbeFixture -Kind machine-msi -PropertyOverrides $overrides
        $result = Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0'
        Assert-True (-not $result.Ok) 'REMOVEUSERDATA override should fail'
    }
}

Test-Case 'MSI upgrade exclusion conditions are fatal when missing' {
    $probe = New-MsiProbeFixture -Kind machine-msi -RemoveDataCondition 'REMOVEUSERDATA="1" AND Installed AND (REMOVE~="ALL")'
    $result = Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0'
    Assert-True (-not $result.Ok) 'missing NOT UPGRADINGPRODUCTCODE should fail'
    Assert-True (($result.Failures -join ';') -match 'NOT UPGRADINGPRODUCTCODE') 'failure should mention upgrade exclusion'

    $probe = New-MsiProbeFixture -Kind machine-msi -RemoveDataCondition 'Installed AND (REMOVE~="ALL") AND NOT UPGRADINGPRODUCTCODE'
    $result = Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0'
    Assert-True (-not $result.Ok) 'missing REMOVEUSERDATA condition should fail'

    $probe = New-MsiProbeFixture -Kind machine-msi -StartupCondition 'Installed AND (REMOVE~="ALL")'
    $result = Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0'
    Assert-True (-not $result.Ok) 'startup cleanup missing upgrade exclusion should fail'
}

Test-Case 'MSI scope/bit3/pdb/core_ffi fixture assertions are fatal' {
    $probe = New-MsiProbeFixture -Kind machine-msi -PropertyOverrides @{ ALLUSERS = '2' }
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0').Ok)
    $probe = New-MsiProbeFixture -Kind machine-msi -WordCount 10
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0').Ok)
    $probe = New-MsiProbeFixture -Kind machine-msi -InstallParent 'ProgramsFolder'
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0').Ok)
    $probe = New-MsiProbeFixture -Kind machine-msi -FileNames @('x.pdb', 'core_ffi_windows.dll')
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0').Ok)
    $probe = New-MsiProbeFixture -Kind machine-msi -FileNames @('short|ClipBridgeShell_CS.exe')
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0').Ok)

    $probe = New-MsiProbeFixture -Kind user-msi -PropertyOverrides @{ MSIINSTALLPERUSER = $null }
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind user-msi -ExpectedVersion '0.1.0').Ok)
    $probe = New-MsiProbeFixture -Kind user-msi -PropertyOverrides @{ ARPSYSTEMCOMPONENT = $null }
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind user-msi -ExpectedVersion '0.1.0').Ok)
    $probe = New-MsiProbeFixture -Kind user-msi -WordCount 2
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind user-msi -ExpectedVersion '0.1.0').Ok)
}

Test-Case 'MSI missing upgrade table / RemoveExistingProducts is fatal' {
    $probe = New-MsiProbeFixture -Kind machine-msi -IncludeUpgrade $false
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0').Ok)
    $probe = New-MsiProbeFixture -Kind machine-msi -IncludeRemoveExisting $false
    Assert-True (-not (Test-MsiArtifact -Probe $probe -Kind machine-msi -ExpectedVersion '0.1.0').Ok)
    Assert-True (-not (Test-MsiArtifact -Probe $null -Kind machine-msi -ExpectedVersion '0.1.0').Ok)
}

# ---------------------------------------------------------------------------
# Burn bundle tables
# ---------------------------------------------------------------------------

Test-Case 'bundle .wixpdb fixture passes variable/MsiProperty/payload checks' {
    $xml = New-BundlePdbXml
    $probe = Read-BundleWixPdbXml -XmlText $xml
    $result = Test-BundleArtifact -Probe $probe -ExpectedVersion '0.1.0' `
        -ExpectedMsiName 'ClipBridge-0.1.0-win-x64-user.msi' -ExpectedMsiSize 123 `
        -ExpectedMsiSha1 'A4DE38FF854CCC1D441D7DD2D4FDB9F5F447AE2D'
    Assert-True $result.Ok ($result.Failures -join '; ')
}

Test-Case 'bundle REMOVEUSERDATA mapping/default/overridable are fatal when wrong' {
    $variants = @(
        @{ Name = 'missing MsiProperty'; Xml = (New-BundlePdbXml -IncludeProperty $false) },
        @{ Name = 'wrong MsiProperty'; Xml = (New-BundlePdbXml -PropertyValue '1') },
        @{ Name = 'variable default 0'; Xml = (New-BundlePdbXml -VariableValue '0') },
        @{ Name = 'variable persisted'; Xml = (New-BundlePdbXml -VariablePersisted '1') },
        @{ Name = 'not overridable'; Xml = (New-BundlePdbXml -IncludeOverridable $false) },
        @{ Name = 'missing WixBundle'; Xml = (New-BundlePdbXml -IncludeBundleRow $false) },
        @{ Name = 'missing ChainMsiPackage'; Xml = (New-BundlePdbXml -IncludeChainMsiRow $false) }
    )
    foreach ($variant in $variants) {
        $probe = Read-BundleWixPdbXml -XmlText $variant.Xml
        $result = Test-BundleArtifact -Probe $probe -ExpectedVersion '0.1.0' `
            -ExpectedMsiName 'ClipBridge-0.1.0-win-x64-user.msi' -ExpectedMsiSize 123 `
            -ExpectedMsiSha1 'A4DE38FF854CCC1D441D7DD2D4FDB9F5F447AE2D'
        Assert-True (-not $result.Ok) $variant.Name
    }
}

Test-Case 'bundle embedded MSI hash/size mismatch is fatal' {
    $xml = New-BundlePdbXml -PayloadHash ('0' * 40) -PayloadSize '999'
    $probe = Read-BundleWixPdbXml -XmlText $xml
    $result = Test-BundleArtifact -Probe $probe -ExpectedVersion '0.1.0' `
        -ExpectedMsiName 'ClipBridge-0.1.0-win-x64-user.msi' -ExpectedMsiSize 123 `
        -ExpectedMsiSha1 'A4DE38FF854CCC1D441D7DD2D4FDB9F5F447AE2D'
    Assert-True (-not $result.Ok)
}

Test-Case 'bundle .wixpdb binary prefix is parsed from the actual file form' {
    $path = Join-Path (New-TestDir 'wixpdb') 'fixture.wixpdb'
    $prefix = [byte[]](0..255)
    $xmlBytes = [System.Text.Encoding]::UTF8.GetBytes((New-BundlePdbXml))
    $bytes = [byte[]]::new($prefix.Length + $xmlBytes.Length)
    [Array]::Copy($prefix, 0, $bytes, 0, $prefix.Length)
    [Array]::Copy($xmlBytes, 0, $bytes, $prefix.Length, $xmlBytes.Length)
    [System.IO.File]::WriteAllBytes($path, $bytes)
    $probe = Read-BundleWixPdbProbe -Path $path
    $result = Test-BundleArtifact -Probe $probe -ExpectedVersion '0.1.0' `
        -ExpectedMsiName 'ClipBridge-0.1.0-win-x64-user.msi' -ExpectedMsiSize 123 `
        -ExpectedMsiSha1 'A4DE38FF854CCC1D441D7DD2D4FDB9F5F447AE2D'
    Assert-True $result.Ok ($result.Failures -join '; ')
}

# ---------------------------------------------------------------------------
# portable ZIP
# ---------------------------------------------------------------------------

Test-Case 'portable ZIP fixture passes structure/hash checks' {
    $fixture = New-ZipFixture -Path (Join-Path (New-TestDir 'zip') 'portable.zip')
    $result = Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $fixture.Hashes
    Assert-True $result.Ok ($result.Failures -join '; ')
}

Test-Case 'portable ZIP missing marker/core/pdb/wrong top/garbage are fatal' {
    $base = New-TestDir 'zip-bad'

    $fixture = New-ZipFixture -Path (Join-Path $base 'no-marker.zip') -NoMarker
    Assert-True (-not (Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $fixture.Hashes).Ok)

    $fixture = New-ZipFixture -Path (Join-Path $base 'no-core.zip') -MissingCore
    Assert-True (-not (Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $fixture.Hashes).Ok)

    $fixture = New-ZipFixture -Path (Join-Path $base 'with-pdb.zip') -WithPdb
    Assert-True (-not (Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $fixture.Hashes).Ok)

    $fixture = New-ZipFixture -Path (Join-Path $base 'wrong-top.zip') -TopDirectory 'WrongTop'
    Assert-True (-not (Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $fixture.Hashes).Ok)

    $fixture = New-ZipFixture -Path (Join-Path $base 'hash-mismatch.zip')
    $badHashes = @{ 'ClipBridge-0.1.0-win-x64/ClipBridgeShell_CS.exe' = ('0' * 64) }
    Assert-True (-not (Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $badHashes).Ok)
}

Test-Case 'portable ZIP signed payload requirement is enforced' {
    $fixture = New-ZipFixture -Path (Join-Path (New-TestDir 'zip-signed') 'portable.zip')
    $unsigned = @{
        'ClipBridge-0.1.0-win-x64/ClipBridgeShell_CS.exe' = [pscustomobject]@{ Authenticode = 'unsigned'; Timestamped = $false; Path = 'exe' }
    }
    Assert-True (-not (Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $fixture.Hashes -PayloadSignatureStates $unsigned -RequireSignedPayloads $true).Ok)
    $signed = @{
        'ClipBridge-0.1.0-win-x64/ClipBridgeShell_CS.exe' = [pscustomobject]@{ Authenticode = 'signed'; Timestamped = $true; Path = 'exe' }
    }
    Assert-True (Test-PortableZipArtifact -Probe $fixture.Probe -Version '0.1.0' -Platform x64 -ExpectedPayloadHashes $fixture.Hashes -PayloadSignatureStates $signed -RequireSignedPayloads $true).Ok
}

# ---------------------------------------------------------------------------
# signatures / summary
# ---------------------------------------------------------------------------

Test-Case 'None signing state is explicitly unsigned/not trusted' {
    $state = Assert-PackageSigningPolicy -Mode None
    Assert-Equal 'None' $state.mode
    Assert-Equal 'unsigned' $state.status
    Assert-Equal $false $state.trusted
}

Test-Case 'None signing policy rejects artifacts that are already signed/invalid' {
    $states = @{
        'a.msi' = [pscustomobject]@{ Authenticode = 'signed'; Timestamped = $true }
    }
    Assert-Throws { Assert-PackageSigningPolicy -Mode None -ArtifactSigningStates $states } ([System.InvalidOperationException])
    $states = @{
        'a.msi' = [pscustomobject]@{ Authenticode = 'invalid'; Timestamped = $false }
    }
    Assert-Throws { Assert-PackageSigningPolicy -Mode None -ArtifactSigningStates $states } ([System.InvalidOperationException])
}

Test-Case 'Certificate signing policy fails closed on unsigned or untimestamped artifacts' {
    $states = @{
        'a.msi' = [pscustomobject]@{ Authenticode = 'unsigned'; Timestamped = $false }
    }
    Assert-Throws {
        Assert-PackageSigningPolicy -Mode Certificate -ArtifactSigningStates $states -HasPortableZip $false `
            -CertificateThumbprint '0123456789ABCDEF0123456789ABCDEF01234567'
    } ([System.InvalidOperationException])

    $states = @{
        'a.msi' = [pscustomobject]@{ Authenticode = 'signed'; Timestamped = $false }
    }
    Assert-Throws {
        Assert-PackageSigningPolicy -Mode Certificate -ArtifactSigningStates $states -HasPortableZip $false `
            -CertificateThumbprint '0123456789ABCDEF0123456789ABCDEF01234567'
    } ([System.InvalidOperationException])

    $states = @{
        'a.msi'    = [pscustomobject]@{ Authenticode = 'signed'; Timestamped = $true; SignerThumbprint = 'FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF' }
    }
    Assert-Throws {
        Assert-PackageSigningPolicy -Mode Certificate -ArtifactSigningStates $states -HasPortableZip $false `
            -CertificateThumbprint '0123456789ABCDEF0123456789ABCDEF01234567'
    } ([System.InvalidOperationException])

    $states = @{
        'a.msi'    = [pscustomobject]@{ Authenticode = 'signed'; Timestamped = $true; SignerThumbprint = '0123456789ABCDEF0123456789ABCDEF01234567' }
        'a.zip'    = [pscustomobject]@{ Authenticode = 'not-applicable'; Timestamped = $false; PayloadsSigned = $true }
    }
    $ok = Assert-PackageSigningPolicy -Mode Certificate -ArtifactSigningStates $states -ZipPayloadsSigned $true -HasPortableZip $true `
        -CertificateThumbprint '0123456789ABCDEF0123456789ABCDEF01234567'
    Assert-Equal 'signed' $ok.status
    Assert-Equal $true $ok.trusted
}

Test-Case 'package-summary marks internal user MSI and hashes final artifacts' {
    $fixture = New-SummaryFixture
    $summary = $fixture.Summary
    Assert-Equal 1 $summary.schemaVersion
    Assert-Equal 'ClipBridge' $summary.product
    Assert-Equal '0.1.0' $summary.version
    Assert-Equal 'x64' $summary.platform
    Assert-SequenceEqual $allFormats @($summary.formats)
    Assert-Equal ('a' * 64) $summary.nativeEvidence.inputDllSha256
    Assert-Equal ('a' * 64) $summary.nativeEvidence.stageDllSha256
    Assert-Equal $false $summary.nativeEvidence.sourceReusedVerified
    Assert-Equal 4 @($summary.artifacts).Count
    $internal = @($summary.artifacts | Where-Object { $_['format'] -eq 'user-msi' })[0]
    Assert-Equal $true $internal['internal']
    Assert-Equal 'internal' $internal['role']
    foreach ($artifact in @($summary.artifacts)) {
        $path = Join-Path $fixture.OutDir ([string]$artifact['name'])
        Assert-Equal (Get-FileSha256 -Path $path) ([string]$artifact['sha256'])
    }
    $json = Get-Content -Raw -LiteralPath (Join-Path $fixture.OutDir 'package-summary.json') | ConvertFrom-Json
    Assert-Equal 1 $json.schemaVersion
    Assert-Equal 4 @($json.artifacts).Count
    Assert-True (-not (@($json.artifacts | Where-Object { $_['name'] -eq 'package-summary.json' }).Count)) 'summary must not hash itself'
}

Test-Case 'package-summary missing required artifact/internal user MSI is fatal' {
    $fixture = New-SummaryFixture
    $machinePath = Join-Path $fixture.OutDir $fixture.Specs[0].Name
    Remove-Item -LiteralPath $machinePath -Force
    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
            -ExpectedArtifacts $fixture.Specs -Signing $fixture.Signing -ArtifactSigningStates $fixture.States -NativeEvidence $fixture.NativeEvidence
    } ([System.IO.FileNotFoundException])

    $fixture = New-SummaryFixture
    $internal = @($fixture.Specs | Where-Object { $_.Format -eq 'user-msi' })[0]
    Remove-Item -LiteralPath (Join-Path $fixture.OutDir $internal.Name) -Force
    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
            -ExpectedArtifacts $fixture.Specs -Signing $fixture.Signing -ArtifactSigningStates $fixture.States -NativeEvidence $fixture.NativeEvidence
    } ([System.IO.FileNotFoundException])
}

Test-Case 'package-summary format plan mismatch is fatal' {
    $fixture = New-SummaryFixture
    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats @('machine-msi') `
            -ExpectedArtifacts $fixture.Specs -Signing $fixture.Signing -ArtifactSigningStates $fixture.States -NativeEvidence $fixture.NativeEvidence
    } ([System.InvalidOperationException])

    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
            -ExpectedArtifacts @($fixture.Specs | Where-Object { $_.Format -ne 'user-msi' }) `
            -Signing $fixture.Signing -ArtifactSigningStates $fixture.States -NativeEvidence $fixture.NativeEvidence
    } ([System.InvalidOperationException])
}

Test-Case 'package-summary cannot claim Certificate with unsigned artifact states' {
    $fixture = New-SummaryFixture
    $signing = [pscustomobject]@{ mode = 'Certificate'; status = 'signed'; trusted = $true; timestamped = $true }
    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
            -ExpectedArtifacts $fixture.Specs -Signing $signing -ArtifactSigningStates $fixture.States -NativeEvidence $fixture.NativeEvidence
    } ([System.InvalidOperationException])
}

Test-Case 'package-summary records a verified Certificate build' {
    $fixture = New-SummaryFixture
    $thumb = '0123456789ABCDEF0123456789ABCDEF01234567'
    $states = @{}
    foreach ($spec in $fixture.Specs) {
        if ($spec.Format -eq 'portable-zip') {
            $states[$spec.Name] = [pscustomobject]@{ Authenticode = 'not-applicable'; Timestamped = $false; PayloadsSigned = $true }
        }
        else {
            $states[$spec.Name] = [pscustomobject]@{ Authenticode = 'signed'; Timestamped = $true; SignerThumbprint = $thumb }
        }
    }
    $signing = [pscustomobject]@{ mode = 'Certificate'; status = 'signed'; trusted = $true; timestamped = $true }
    $summary = New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
        -ExpectedArtifacts $fixture.Specs -Signing $signing -ArtifactSigningStates $states -NativeEvidence $fixture.NativeEvidence
    Assert-Equal 'signed' $summary.signing.status
    Assert-Equal $true $summary.signing.trusted
    $zipArtifact = @($summary.artifacts | Where-Object { $_['format'] -eq 'portable-zip' })[0]
    Assert-Equal $true $zipArtifact['signing']['payloadsSigned']
}

Test-Case 'package-summary None mode rejects signed artifact states' {
    $fixture = New-SummaryFixture
    $states = @{}
    foreach ($spec in $fixture.Specs) {
        if ($spec.Format -eq 'portable-zip') {
            $states[$spec.Name] = [pscustomobject]@{ Authenticode = 'not-applicable'; Timestamped = $false; PayloadsSigned = $false }
        }
        else {
            $states[$spec.Name] = [pscustomobject]@{ Authenticode = 'signed'; Timestamped = $true; SignerThumbprint = '0123456789ABCDEF0123456789ABCDEF01234567' }
        }
    }
    $signing = [pscustomobject]@{ mode = 'None'; status = 'unsigned'; trusted = $false; timestamped = $false }
    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
            -ExpectedArtifacts $fixture.Specs -Signing $signing -ArtifactSigningStates $states -NativeEvidence $fixture.NativeEvidence
    } ([System.InvalidOperationException])
}

Test-Case 'native DLL hash helpers validate/normalize and fail closed' {
    Assert-Equal ('a' * 64) (Assert-Sha256Hex -Value ('A' * 64) -Label 'test')
    Assert-Throws { Assert-Sha256Hex -Value 'xyz' -Label 'test' } ([System.ArgumentException])
    Assert-Equal ('a' * 64) (Assert-CoreDllHashMatch -Expected ('A' * 64) -Actual ('a' * 64) -DllPath 'x')
    Assert-Throws { Assert-CoreDllHashMatch -Expected ('a' * 64) -Actual ('b' * 64) -DllPath 'x' } ([System.InvalidOperationException])
    Assert-Throws { Assert-CoreDllHashMatch -Expected 'bad' -Actual ('a' * 64) -DllPath 'x' } ([System.ArgumentException])
}

Test-Case 'package-summary validates nativeEvidence hashes and stage equality' {
    $fixture = New-SummaryFixture
    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
            -ExpectedArtifacts $fixture.Specs -Signing $fixture.Signing -ArtifactSigningStates $fixture.States `
            -NativeEvidence ([pscustomobject]@{ inputDllSha256 = 'bad'; stageDllSha256 = ('a' * 64); sourceReusedVerified = $false })
    } ([System.ArgumentException])
    Assert-Throws {
        New-PackageSummary -OutDir $fixture.OutDir -Version '0.1.0' -Platform 'x64' -Formats $allFormats `
            -ExpectedArtifacts $fixture.Specs -Signing $fixture.Signing -ArtifactSigningStates $fixture.States `
            -NativeEvidence ([pscustomobject]@{ inputDllSha256 = ('a' * 64); stageDllSha256 = ('b' * 64); sourceReusedVerified = $true })
    } ([System.InvalidOperationException])
}

Test-Case 'package-summary records sourceReusedVerified native evidence' {
    $fixture = New-SummaryFixture -SourceReusedVerified $true
    Assert-Equal $true $fixture.Summary.nativeEvidence.sourceReusedVerified
    $json = Get-Content -Raw -LiteralPath (Join-Path $fixture.OutDir 'package-summary.json') | ConvertFrom-Json
    Assert-Equal $true $json.nativeEvidence.sourceReusedVerified
    Assert-Equal ('a' * 64) $json.nativeEvidence.inputDllSha256
}

Test-Case 'authenticode artifact state checks reject unsigned/untimestamped/thumb mismatch' {
    $good = [pscustomobject]@{
        Path = 'a.exe'; IsSigned = $true; IsValid = $true; Timestamped = $true
        SignatureStatus = 'Valid'; SignerThumbprint = '0123456789ABCDEF0123456789ABCDEF01234567'
    }
    Assert-True (Test-AuthenticodeArtifactState -State $good -ExpectedThumbprint '0123456789ABCDEF0123456789ABCDEF01234567').Ok
    $noTs = [pscustomobject]@{
        Path = 'a.exe'; IsSigned = $true; IsValid = $true; Timestamped = $false
        SignatureStatus = 'Valid'; SignerThumbprint = '0123456789ABCDEF0123456789ABCDEF01234567'
    }
    Assert-True (-not (Test-AuthenticodeArtifactState -State $noTs).Ok)
    $unsigned = [pscustomobject]@{
        Path = 'a.exe'; IsSigned = $false; IsValid = $false; Timestamped = $false
        SignatureStatus = 'NotSigned'; SignerThumbprint = $null
    }
    Assert-True (-not (Test-AuthenticodeArtifactState -State $unsigned).Ok)
    Assert-True (-not (Test-AuthenticodeArtifactState -State $good -ExpectedThumbprint ('F' * 40)).Ok)
}

# ---------------------------------------------------------------------------
# signing entry / exit codes
# ---------------------------------------------------------------------------

Test-Case 'sign-windows Mode None is a no-op for files (exit 0)' {
    $dir = New-TestDir 'sign-none'
    $file = New-TestFile -Path (Join-Path $dir 'a.exe') -Content 'unsigned'
    $before = Get-FileSha256 -Path $file
    $result = Invoke-External -FilePath $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $signScript, '-Mode', 'None', '-Files', $file)
    Assert-Equal 0 $result.ExitCode
    Assert-Equal $before (Get-FileSha256 -Path $file)
}

Test-Case 'sign-windows rejects invalid Certificate config with exit 2' {
    $dir = New-TestDir 'sign-bad'
    $file = New-TestFile -Path (Join-Path $dir 'a.exe') -Content 'x'
    $base = @('-NoProfile', '-NonInteractive', '-File', $signScript, '-Mode', 'Certificate', '-Files', $file)
    $thumb = '0123456789ABCDEF0123456789ABCDEF01234567'
    $ts = 'https://timestamp.example.invalid/rfc3161'

    Assert-Equal 2 (Invoke-External -FilePath $pwsh -Arguments ($base + @('-TimestampUrl', $ts))).ExitCode
    Assert-Equal 2 (Invoke-External -FilePath $pwsh -Arguments ($base + @('-CertificateThumbprint', 'not-a-thumbprint', '-TimestampUrl', $ts))).ExitCode
    Assert-Equal 2 (Invoke-External -FilePath $pwsh -Arguments ($base + @('-CertificateThumbprint', $thumb, '-TimestampUrl', 'ftp://insecure.invalid'))).ExitCode
    Assert-Equal 2 (Invoke-External -FilePath $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $signScript, '-Mode', 'None', '-Files', $file, '-CertificateThumbprint', $thumb)).ExitCode
    Assert-Equal 2 (Invoke-External -FilePath $pwsh -Arguments @(
            '-NoProfile', '-NonInteractive', '-File', $signScript, '-Mode', 'Certificate',
            '-CertificateThumbprint', $thumb, '-TimestampUrl', $ts, '-ValidateOnly',
            '-SigntoolPath', (Join-Path $dir 'missing-signtool.exe')
        )).ExitCode
    $zip = New-TestFile -Path (Join-Path $dir 'a.zip') -Content 'zip'
    Assert-Equal 2 (Invoke-External -FilePath $pwsh -Arguments @(
            '-NoProfile', '-NonInteractive', '-File', $signScript, '-Mode', 'Certificate',
            '-CertificateThumbprint', $thumb, '-TimestampUrl', $ts, '-Files', $zip
        )).ExitCode
}

if (-not $SkipBuildScriptExitCode) {
    Test-Case 'main script rejects invalid formats/conflicts/arm64 before any build' {
        $out = New-TestDir 'main-invalid'
        $invalid = Invoke-External -FilePath $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $mainScript, '-Formats', 'msix', '-OutDir', $out)
        Assert-Equal 2 $invalid.ExitCode
        $internal = Invoke-External -FilePath $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $mainScript, '-Formats', 'user-msi', '-OutDir', $out)
        Assert-Equal 2 $internal.ExitCode
        $conflict = Invoke-External -FilePath $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $mainScript, '-Formats', 'machine-msi', '-Only', 'MachineMsi', '-OutDir', $out)
        Assert-Equal 2 $conflict.ExitCode
        $arm = Invoke-External -FilePath $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $mainScript, '-Platform', 'arm64', '-Formats', 'portable-zip', '-OutDir', $out)
        Assert-Equal 2 $arm.ExitCode
        $signConflict = Invoke-External -FilePath $pwsh -Arguments @(
            '-NoProfile', '-NonInteractive', '-File', $mainScript, '-Formats', 'portable-zip', '-OutDir', $out,
            '-SigningMode', 'None', '-CertificateThumbprint', '0123456789ABCDEF0123456789ABCDEF01234567'
        )
        Assert-Equal 2 $signConflict.ExitCode

        $badExpected = Invoke-External -FilePath $pwsh -Arguments @(
            '-NoProfile', '-NonInteractive', '-File', $mainScript, '-Formats', 'portable-zip', '-OutDir', $out,
            '-ExpectedCoreDllSha256', 'not-a-sha256'
        )
        Assert-Equal 2 $badExpected.ExitCode

        $dllPath = Join-Path $repoRoot 'target/x86_64-pc-windows-msvc/release/core_ffi_windows.dll'
        if ($IsWindows -and (Test-Path -LiteralPath $dllPath -PathType Leaf)) {
            $mismatch = Invoke-External -FilePath $pwsh -Arguments @(
                '-NoProfile', '-NonInteractive', '-File', $mainScript, '-Formats', 'portable-zip', '-OutDir', $out,
                '-SkipRust', '-ExpectedCoreDllSha256', ('0' * 64)
            )
            Assert-Equal 1 $mismatch.ExitCode
            Assert-True ((($mismatch.Output -join "`n") -match 'core DLL hash mismatch')) 'mismatch failure should mention core DLL hash'
        }
    }
}
else {
    Write-Host 'SKIP  main script invalid-input exit-code tests (-SkipBuildScriptExitCode)'
}

# ---------------------------------------------------------------------------
# AST / source contract
# ---------------------------------------------------------------------------

function Get-ScriptAst {
    param([string]$Path)
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path $Path).Path, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) {
        throw "parse errors: $($errors[0].Message)"
    }
    return $ast
}

function Get-ParameterAst {
    param($Ast, [string]$Name)
    return @($Ast.ParamBlock.Parameters | Where-Object { $_.Name.VariablePath.UserPath -eq $Name })[0]
}

Test-Case 'main script exposes -Formats and controlled signing parameters' {
    $ast = Get-ScriptAst -Path $mainScript
    Assert-True ($null -ne $ast.ParamBlock) 'missing param block'
    foreach ($name in @('Formats', 'Only', 'SigningMode', 'CertificateThumbprint', 'TimestampUrl', 'SigntoolPath', 'ExpectedCoreDllSha256')) {
        Assert-True ($null -ne (Get-ParameterAst -Ast $ast -Name $name)) "missing parameter -$name"
    }
    $only = Get-ParameterAst -Ast $ast -Name 'Only'
    $onlySet = @($only.Attributes | Where-Object { $_.TypeName.Name -eq 'ValidateSet' })[0]
    Assert-SequenceEqual @('All', 'MachineMsi', 'UserExe', 'PortableZip') @($onlySet.PositionalArguments | ForEach-Object { $_.Value })
    $mode = Get-ParameterAst -Ast $ast -Name 'SigningMode'
    $modeSet = @($mode.Attributes | Where-Object { $_.TypeName.Name -eq 'ValidateSet' })[0]
    Assert-SequenceEqual @('None', 'Certificate') @($modeSet.PositionalArguments | ForEach-Object { $_.Value })
    $text = Get-Content -Raw -LiteralPath $mainScript
    Assert-True $text.Contains('--locked') 'cargo must be locked'
    Assert-True $text.Contains('GenerateAppxPackageOnBuild=false') 'MSIX auto-package must be disabled'
    Assert-True (-not $text.Contains('build-windows-msix.ps1')) 'main script must not call MSIX script'
    Assert-True (-not $text.Contains('build-windows-winget.ps1')) 'main script must not call winget script'
}

Test-Case 'WiX layout guard requires x86\burn.exe for bundle builds' {
    $root = New-TestDir 'wix-layout'
    foreach ($name in @('candle.exe', 'light.exe', 'heat.exe', 'WixTasks.dll', 'WixUtilExtension.dll', 'WixUIExtension.dll', 'WixBalExtension.dll', 'wix.targets')) {
        New-TestFile -Path (Join-Path $root $name) -Content 'fixture' | Out-Null
    }
    Assert-Equal 0 @(Get-MissingWixFiles -WixRoot $root -NeedBundle $false).Count
    $missing = @(Get-MissingWixFiles -WixRoot $root -NeedBundle $true)
    Assert-Equal 1 $missing.Count
    Assert-True ([string]$missing[0] -match 'burn\.exe$') "expected missing burn.exe, got $($missing -join ',')"
    New-TestFile -Path (Join-Path $root (Join-Path 'x86' 'burn.exe')) -Content 'fixture' | Out-Null
    Assert-Equal 0 @(Get-MissingWixFiles -WixRoot $root -NeedBundle $true).Count
}

Test-Case 'thin wrappers still route through the compatible -Only values' {
    $expectations = @{
        'build-windows-all.ps1'          = 'All'
        'build-windows-machine-msi.ps1'  = 'MachineMsi'
        'build-windows-user-exe.ps1'     = 'UserExe'
        'build-windows-portable-zip.ps1' = 'PortableZip'
    }
    foreach ($name in $expectations.Keys) {
        $text = Get-Content -Raw -LiteralPath (Join-Path $repoRoot "scripts/$name")
        Assert-True ($text -match ("Only\s*=\s*'" + [regex]::Escape($expectations[$name]) + "'")) "$name must pass -Only $($expectations[$name])"
        Assert-True ($text.Contains('build-windows-installers.ps1')) "$name must call the shared entry"
    }
}

Test-Case 'sign entry is controlled and does not create/import certificates' {
    $text = Get-Content -Raw -LiteralPath $signScript
    foreach ($forbidden in @('New-SelfSignedCertificate', 'Import-PfxCertificate', 'Import-Certificate', 'certutil', 'Partner Center')) {
        Assert-True (-not $text.Contains($forbidden)) "sign script must not contain $forbidden"
    }
    $ast = Get-ScriptAst -Path $signScript
    $mode = Get-ParameterAst -Ast $ast -Name 'Mode'
    $modeSet = @($mode.Attributes | Where-Object { $_.TypeName.Name -eq 'ValidateSet' })[0]
    Assert-SequenceEqual @('None', 'Certificate') @($modeSet.PositionalArguments | ForEach-Object { $_.Value })
}

# ---------------------------------------------------------------------------
# cleanup / result
# ---------------------------------------------------------------------------

try {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
catch {
    Write-Warning "测试临时目录清理失败：$tempRoot ($($_.Exception.Message))"
}

Write-Host ''
Write-Host "通过: $script:Passed   失败: $script:Failed"
if ($script:Failed -gt 0) {
    Write-Host ''
    Write-Host '失败详情:'
    foreach ($message in $script:FailureMessages) { Write-Host "  - $message" -ForegroundColor Red }
    exit 1
}
exit 0
