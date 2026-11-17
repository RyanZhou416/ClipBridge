<#
.SYNOPSIS
    按格式集合构建 Windows 安装器：machine MSI / per-user EXE / 便携 ZIP。

.DESCRIPTION
    公开格式（-Formats）：
      machine-msi   只 publish + heat + machine MSI（不构建 user MSI / bundle / ZIP）
      user-exe      publish + heat + 内部 user MSI + per-user Burn EXE（不构建 machine MSI / ZIP）
      portable-zip  publish + 便携 ZIP（不构建 MSI / bundle，不经过 WiX）

    兼容旧入口：
      -Only All            == -Formats machine-msi,user-exe,portable-zip（旧默认）
      -Only MachineMsi     == -Formats machine-msi
      -Only UserExe        == -Formats user-exe
      -Only PortableZip    == -Formats portable-zip
      -Only 与 -Formats 同时出现会被拒绝（fail-fast），不会猜测调用者意图。
      内部 user MSI 不是第四个发布选项；只有选择 user-exe 时才作为 bundle 载荷构建，
      package-summary.json 会把它标记为 internal。

    构建流程（所有选中的格式共享一次 publish/stage）：
      1. 版本来自 scripts/version.ps1（Version.props 不变）
      2. cargo 构建 core-ffi（显式 --locked；可用 -SkipRust 跳过，但跳过不证明 DLL 来自本次源码）
      3. dotnet publish 到独立 stage（每次运行一次；显式禁用 MSIX 自动出包目标）
      4. 如启用证书签名：先签项目自有 EXE/DLL 载荷，再 heat（需要 MSI 时一次）
      5. 按选择构建 machine MSI / 内部 user MSI；签名后再构建 Burn bundle
      6. 按选择从最终 stage 生成便携 ZIP（ZIP 自身不套 Authenticode；检查内部项目自有载荷）
      7. 从最终产物表断言（MSI 的 Property/File/InstallExecuteSequence/Upgrade 表、
         Burn 的 .wixpdb 表、ZIP 条目），全部通过后写 package-summary.json

    签名接口：
      -SigningMode None（默认）不签名；summary 明确 unsigned/trusted=false，只允许用于
      CI/draft/明确接受的预发布，不能冒充可信生产签名。
      -SigningMode Certificate 必须同时提供 -CertificateThumbprint、-TimestampUrl 和可用的
      signtool；缺失/无效时在 publish 之前失败。本脚本不会导入/创建证书、不会安装 SDK、
      不会配置签名服务，也不会 blind resign 第三方 DLL。签名顺序为
      项目自有载荷 → MSI（含内部 user MSI）→ Burn → ZIP（从已签 stage 生成）。

    产物（OutDir 下）：
      ClipBridge-<ver>-win-x64-machine.msi
      ClipBridge-<ver>-win-x64-user.exe
      ClipBridge-<ver>-win-x64-user.msi   （内部 user MSI，仅 user-exe）
      ClipBridge-<ver>-win-x64-portable.zip
      package-summary.json                （机器可读；供 CI/release 复用）

    退出码：0 成功；2 输入/格式/签名配置错误；1 构建、产物表断言或签名失败。

.PARAMETER Formats
    要构建的公开格式集合，任意非空子集：
    machine-msi, user-exe, portable-zip。不能单独选择 user-msi/MSIX/winget。

.PARAMETER Only
    兼容旧调用的单渠道/All 选择。与 -Formats 互斥；不传时默认 All。

.PARAMETER Platform
    当前只支持 x64。arm64 参数保留只为让旧 wrapper 给出明确错误，而不是虚构交付能力。

.PARAMETER OutDir
    本次运行的独立输出根目录，默认 dist（相对仓库根目录）。stage、便携临时目录和最终产物
    都在该根目录下；清理前会验证绝对路径严格位于 OutDir 内，不会碰用户数据根。

.PARAMETER WixInstallPath
    WiX 3.14 目录（含 candle/light/heat/WixTasks.dll）。不传则探测标准安装位置。
    仅选择 portable-zip 时不需要 WiX；此时该参数被忽略。

.PARAMETER SkipRust
    跳过 cargo 构建（要求 target 里的 core_ffi_windows.dll 已存在）。复用 DLL 不能作为
    本次源码已验证的证据；正式发布不应使用。

.PARAMETER KeepStage
    保留 OutDir/stage（默认构建结束后删除）。package-summary.json 始终保留。

.PARAMETER SigningMode
    None（默认）或 Certificate。Certificate 需要明确的证书 thumbprint + timestamp URL。

.PARAMETER CertificateThumbprint
    已在 Windows 证书库中的代码签名证书 SHA1 thumbprint（40 位十六进制）。
    不会读取/导出私钥，也不会导入或创建证书。

.PARAMETER TimestampUrl
    Certificate 模式必填的 RFC3161 时间戳 URL（http/https；生产优先 https）。

.PARAMETER SigntoolPath
    可选的 signtool.exe 绝对路径；不传时从 Windows SDK 10 x64 工具目录查找。
    不会下载或安装。

.PARAMETER ExpectedCoreDllSha256
    可选的 64 位十六进制 SHA256。提供时在 publish 前校验 target 里实际 core_ffi_windows.dll
    的 hash；不匹配立即失败。与 -SkipRust 一起使用时，只有匹配才会在 package-summary 的
    nativeEvidence.sourceReusedVerified 标为 true，供 release 证明打包使用了刚验证过的同一 DLL。
    不提供时保持普通 CI 的显式 cargo 构建行为；非法格式在构建前失败。

.EXAMPLE
    pwsh scripts/build-windows-installers.ps1 -Formats machine-msi,portable-zip -WixInstallPath $env:TEMP\wix314

.EXAMPLE
    pwsh scripts/build-windows-installers.ps1 -Only PortableZip -SkipRust

.EXAMPLE
    pwsh scripts/build-windows-installers.ps1 -Formats user-exe -SigningMode None

.EXAMPLE
    pwsh scripts/build-windows-installers.ps1 -Formats machine-msi,user-exe,portable-zip `
        -SigningMode Certificate -CertificateThumbprint <40hex> -TimestampUrl https://timestamp.example/rfc3161
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Platform = 'x64',
    [string]$OutDir = 'dist',

    # WiX 3.14 目录（含 candle/light/heat/WixTasks.dll）。不传则探测标准安装位置。
    [string]$WixInstallPath,

    [ValidateSet('All', 'MachineMsi', 'UserExe', 'PortableZip')]
    [string]$Only,

    # 旧参数位置保持不变，避免旧的 positional 调用把 -SkipRust 误绑到新参数。
    [switch]$SkipRust,
    [switch]$KeepStage,

    # 新接口统一使用命名参数；置于旧参数之后以保留兼容性。
    [string[]]$Formats,
    [ValidateSet('None', 'Certificate')]
    [string]$SigningMode = 'None',
    [string]$CertificateThumbprint,
    [string]$TimestampUrl,
    [string]$SigntoolPath,
    [string]$ExpectedCoreDllSha256
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$modulePath = Join-Path $PSScriptRoot 'ci/ClipBridge.Packaging.psm1'
$signScript = Join-Path $PSScriptRoot 'ci/sign-windows.ps1'
Import-Module $modulePath -Force

$installerDir = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer'
$bundleDir = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle'
$wixproj = Join-Path $installerDir 'ClipBridgeShell_CS.Installer.wixproj'
$bundleProj = Join-Path $bundleDir 'ClipBridgeShell_CS.Bundle.wixproj'
$appProject = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/ClipBridgeShell_CS.csproj'
$installerBin = Join-Path $installerDir 'bin'
$bundleBin = Join-Path $bundleDir 'bin'
$installerObj = Join-Path $installerDir 'obj'

# ---------------------------------------------------------------------------
# 工具链定位
# ---------------------------------------------------------------------------

function Resolve-Wix {
    param([string]$Explicit)
    if ($Explicit) {
        if (-not (Test-Path -LiteralPath $Explicit -PathType Container)) {
            throw [System.InvalidOperationException]::new("WiX 目录不存在：$Explicit")
        }
        $resolved = (Resolve-Path -LiteralPath $Explicit).Path
        if (-not (Test-Path -LiteralPath (Join-Path $resolved 'candle.exe') -PathType Leaf)) {
            throw [System.InvalidOperationException]::new("WiX 目录里没有 candle.exe：$resolved")
        }
        return $resolved
    }
    $roots = @()
    if (${env:ProgramFiles(x86)}) { $roots += (Join-Path ${env:ProgramFiles(x86)} 'WiX Toolset v3.14') }
    if ($env:ProgramFiles) { $roots += (Join-Path $env:ProgramFiles 'WiX Toolset v3.14') }
    foreach ($candidate in $roots) {
        if ((Test-Path -LiteralPath (Join-Path $candidate 'candle.exe') -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    throw [System.InvalidOperationException]::new(
        '未找到 WiX 3.14。请安装（winget install WiXToolset.WiXToolset）或用 -WixInstallPath 指定。')
}

function Assert-WixLayout {
    param([string]$WixRoot, [bool]$NeedBundle)
    # Burn 的 light 需要 x86\burn.exe；不完整解压必须在这里 fail-fast，而不是构建到 bundle 才失败。
    $missing = @(Get-MissingWixFiles -WixRoot $WixRoot -NeedBundle $NeedBundle)
    if ($missing.Count -gt 0) {
        throw [System.InvalidOperationException]::new(
            "WiX 目录缺少必需文件：$($missing -join ', ')（$WixRoot）")
    }
}

function Resolve-MSBuild {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        $p = & $vswhere -latest -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' |
            Select-Object -First 1
        if ($p -and (Test-Path -LiteralPath $p -PathType Leaf)) { return $p }
    }
    foreach ($candidate in @(
            'C:\Program Files\Microsoft Visual Studio\2022\Community\MSBuild\Current\Bin\MSBuild.exe',
            'C:\Program Files\Microsoft Visual Studio\2022\Professional\MSBuild\Current\Bin\MSBuild.exe',
            'C:\Program Files\Microsoft Visual Studio\2022\Enterprise\MSBuild\Current\Bin\MSBuild.exe')) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    throw [System.InvalidOperationException]::new('未找到 MSBuild.exe（需要 VS 2022 或 Build Tools）。')
}

# ---------------------------------------------------------------------------
# 构建辅助
# ---------------------------------------------------------------------------

function Invoke-AppPublish {
    param([string]$StageDir)
    Write-Host "publish → $StageDir"
    $publishArgs = @(
        'publish', $appProject,
        '-c', 'Release',
        "-p:Platform=$Platform",
        "-p:PublishProfile=win10-$Platform",
        '-p:SkipRustBuild=true',
        # 显式禁止 MSIX 自动出包/签名/符号包目标；MSI/EXE/ZIP 不附带 Store 渠道产物。
        '-p:GenerateAppxPackageOnBuild=false',
        '-p:GenerateAppxPackageOnPublish=false',
        '-p:AppxPackageSigningEnabled=false',
        '-p:AppxSymbolPackageEnabled=false',
        '-p:AppxBundle=Never',
        '-o', $StageDir
    )
    & dotnet @publishArgs
    if ($LASTEXITCODE -ne 0) {
        throw [System.InvalidOperationException]::new("dotnet publish 失败（exit $LASTEXITCODE）")
    }

    $msixUnexpected = @(Get-ChildItem -LiteralPath $StageDir -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -in @('.msix', '.msixbundle', '.msixupload', '.appx', '.appxbundle') })
    if ($msixUnexpected.Count -gt 0) {
        throw [System.InvalidOperationException]::new(
            "publish 意外产出 MSIX/APPX 文件（目标 GitHub 范围不含 MSIX）：$($msixUnexpected.FullName -join ', ')")
    }

    # 与 wixproj 的 BeforeBuild 一致：stage 内去 pdb/xml（便携 ZIP 断言无 pdb）。
    $junk = @(Get-ChildItem -LiteralPath $StageDir -Recurse -File -Include '*.pdb', '*.xml' -ErrorAction SilentlyContinue)
    foreach ($file in $junk) {
        Remove-ItemSafely -Path $file.FullName -AllowedRoot $StageDir
    }

    foreach ($required in @('ClipBridgeShell_CS.exe', 'core_ffi_windows.dll')) {
        if (-not (Test-Path -LiteralPath (Join-Path $StageDir $required) -PathType Leaf)) {
            throw [System.IO.FileNotFoundException]::new("publish 后 stage 缺少必需载荷：$required（$StageDir）")
        }
    }
}

function Invoke-MsiVariant {
    param(
        [bool]$PerUser,
        [bool]$SkipHeat,
        [string[]]$CommonArgs
    )
    $msbuildArgs = @()
    $msbuildArgs += $CommonArgs
    $msbuildArgs += "-p:PerUser=$($PerUser.ToString().ToLowerInvariant())"
    $msbuildArgs += '-p:SkipPublish=true'
    if ($SkipHeat) { $msbuildArgs += '-p:SkipHeat=true' }
    & $msbuild $wixproj @msbuildArgs
    if ($LASTEXITCODE -ne 0) {
        throw [System.InvalidOperationException]::new("MSI 构建失败（PerUser=$PerUser，exit $LASTEXITCODE）")
    }
}

function Invoke-SignFiles {
    param([string[]]$Files, [string]$Phase)
    if ($SigningMode -ne 'Certificate') { return }
    if (-not $Files -or @($Files).Count -eq 0) {
        throw [System.InvalidOperationException]::new("签名阶段 '$Phase' 没有待签名文件。")
    }
    $signArgs = @{
        Mode                  = 'Certificate'
        Files                 = [string[]]$Files
        CertificateThumbprint = $CertificateThumbprint
        TimestampUrl          = $TimestampUrl
    }
    if ($SigntoolPath) { $signArgs.SigntoolPath = $SigntoolPath }
    Write-Host "[sign] $Phase -> $($Files.Count) 个文件"
    & $signScript @signArgs
    if ($LASTEXITCODE -ne 0) {
        throw [System.InvalidOperationException]::new("签名失败（$Phase，exit $LASTEXITCODE）")
    }
}

function Remove-StaleIntermediate {
    param([string]$Path, [string]$AllowedRoot)
    if (Test-Path -LiteralPath $Path) {
        Remove-ItemSafely -Path $Path -AllowedRoot $AllowedRoot
    }
}

function New-PortableZip {
    param(
        [string]$StageDir,
        [string]$OutRoot,
        [string]$ZipPath,
        [string]$PortableName
    )
    $portableStageRoot = Join-Path $OutRoot 'portable'
    $portableDir = Join-Path $portableStageRoot $PortableName

    if (Test-Path -LiteralPath $portableStageRoot) {
        Remove-ItemSafely -Path $portableStageRoot -AllowedRoot $OutRoot -Recurse
    }
    New-Item -ItemType Directory -Force -Path $portableDir | Out-Null

    foreach ($item in @(Get-ChildItem -LiteralPath $StageDir -Force)) {
        Copy-Item -LiteralPath $item.FullName -Destination $portableDir -Recurse -Force
    }

    # 解压即用：顶层目录与资产名一致；空标记启用便携数据根。
    $markerPath = Join-Path $portableDir 'ClipBridge.portable'
    [System.IO.File]::WriteAllText($markerPath, '', [System.Text.UTF8Encoding]::new($false))

    $versionText = @(
        "ClipBridge $($msiVersion) portable (win-$Platform)"
        ''
        'How to use:'
        "  1. Extract the whole $PortableName folder to a writable location."
        '  2. Run ClipBridgeShell_CS.exe.'
        ''
        'Portable mode:'
        "  The empty marker file 'ClipBridge.portable' next to the executable keeps all data"
        "  (core.db, tls, Cache, logs, Strings) in the 'data' subfolder next to the executable."
        '  If that folder is not writable (e.g. Program Files, read-only USB drive), ClipBridge'
        '  falls back to %LOCALAPPDATA%\ClipBridge, shows a warning, and appends the reason to'
        '  %LOCALAPPDATA%\ClipBridge\portable-fallback.log.'
        '  Delete ClipBridge.portable to use the per-user %LOCALAPPDATA%\ClipBridge location.'
    ) -join [Environment]::NewLine
    [System.IO.File]::WriteAllText((Join-Path $portableDir 'version.txt'), $versionText + [Environment]::NewLine,
        [System.Text.UTF8Encoding]::new($false))

    if (Test-Path -LiteralPath $ZipPath) {
        Remove-ItemSafely -Path $ZipPath -AllowedRoot $OutRoot
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $portableDir,
        $ZipPath,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $true)

    return $portableStageRoot
}

# ---------------------------------------------------------------------------
# 选择 / 参数 fail-fast（早于任何 native/dotnet 调用）
# ---------------------------------------------------------------------------

$exitCode = 0
try {
    $onlySpecified = $PSBoundParameters.ContainsKey('Only')
    $formatsSpecified = $PSBoundParameters.ContainsKey('Formats')

    $formats = @(Resolve-FormatSelection `
            -Formats $Formats `
            -Only $Only `
            -OnlySpecified $onlySpecified `
            -FormatsSpecified $formatsSpecified)
    $Platform = Assert-SupportedPlatform -Platform $Platform
    if ([string]::IsNullOrWhiteSpace($OutDir)) {
        throw [System.ArgumentException]::new('-OutDir 不能为空；请指定独立输出目录。')
    }
    $expectedCoreProvided = $PSBoundParameters.ContainsKey('ExpectedCoreDllSha256')
    if ($expectedCoreProvided) {
        $ExpectedCoreDllSha256 = Assert-Sha256Hex -Value $ExpectedCoreDllSha256 -Label 'ExpectedCoreDllSha256'
    }

    if ($SigningMode -eq 'None') {
        foreach ($name in @('CertificateThumbprint', 'TimestampUrl', 'SigntoolPath')) {
            if ($PSBoundParameters.ContainsKey($name) -and -not [string]::IsNullOrWhiteSpace([string]$PSBoundParameters[$name])) {
                throw [System.ArgumentException]::new(
                    "SigningMode=None 时不能提供 -$name；None 只用于 CI/draft/明确预发布，不能伪装成可信生产签名。")
            }
        }
    }
    else {
        if ([string]::IsNullOrWhiteSpace($CertificateThumbprint)) {
            throw [System.ArgumentException]::new('-SigningMode Certificate 必须提供 -CertificateThumbprint（证书库中已有证书）。')
        }
        if ([string]::IsNullOrWhiteSpace($TimestampUrl)) {
            throw [System.ArgumentException]::new('-SigningMode Certificate 必须提供 -TimestampUrl（RFC3161 http/https URL）。')
        }
        $validateArgs = @{
            Mode                  = 'Certificate'
            CertificateThumbprint = $CertificateThumbprint
            TimestampUrl          = $TimestampUrl
            ValidateOnly          = $true
        }
        if ($SigntoolPath) { $validateArgs.SigntoolPath = $SigntoolPath }
        & $signScript @validateArgs
        if ($LASTEXITCODE -ne 0) {
            if ($LASTEXITCODE -eq 2) {
                throw [System.ArgumentException]::new('签名配置验证失败（生产签名 provider 未配置或参数无效）。')
            }
            throw [System.InvalidOperationException]::new("签名配置验证失败（exit $LASTEXITCODE）。")
        }
    }

    $plan = Get-BuildPlan -Formats $formats
    $versionJson = & (Join-Path $PSScriptRoot 'version.ps1') -Json | ConvertFrom-Json
    $msiVersion = [string]$versionJson.MsiVersion
    $bundleVersion = [string]$versionJson.BundleVersion
    if (-not $msiVersion) { throw [System.InvalidOperationException]::new('version.ps1 没返回 MsiVersion') }

    $expectedArtifacts = @(Get-ExpectedArtifacts -Formats $formats -Version $msiVersion -Platform $Platform)
    $buildSteps = @(Get-BuildSteps -Formats $formats -SigningMode $SigningMode -SkipRust ([bool]$SkipRust))
    $progressSteps = @($buildSteps | Where-Object { $_ -ne 'heat' })

    Write-Host "版本: $msiVersion (MSI/Bundle)   架构: $Platform   格式: $($formats -join ', ')   签名: $SigningMode"
    if ($formats -notcontains 'machine-msi') {
        Write-Host 'machine MSI 未选择：不会构建或断言 machine MSI。'
    }
    if ($formats -notcontains 'user-exe') {
        Write-Host 'user EXE 未选择：不会构建内部 user MSI 或 Burn bundle。'
    }
    if ($formats -notcontains 'portable-zip') {
        Write-Host '便携 ZIP 未选择：不会生成便携 ZIP。'
    }

    $outRoot = if ([System.IO.Path]::IsPathRooted($OutDir)) {
        [System.IO.Path]::GetFullPath($OutDir)
    }
    else {
        [System.IO.Path]::GetFullPath((Join-Path $repoRoot $OutDir))
    }
    $outRoot = Assert-SafeOutputRoot -Path $outRoot -RepoRoot $repoRoot
    $stageDir = Join-Path $outRoot 'stage'
    $portableStageRoot = Join-Path $outRoot 'portable'
    $summaryPath = Join-Path $outRoot 'package-summary.json'

    # 清理只在 OutDir 内；先创建 outdir（若不存在），随后所有目标都做绝对路径校验。
    New-Item -ItemType Directory -Force -Path $outRoot | Out-Null
    Remove-ItemSafely -Path $stageDir -AllowedRoot $outRoot -Recurse
    Remove-ItemSafely -Path $portableStageRoot -AllowedRoot $outRoot -Recurse
    Remove-ItemSafely -Path $summaryPath -AllowedRoot $outRoot

    # 不读取旧产物：本次选中的最终文件先删除，避免旧文件冒充本次输出。
    foreach ($spec in $expectedArtifacts) {
        Remove-ItemSafely -Path (Join-Path $outRoot $spec.Name) -AllowedRoot $outRoot
    }

    # 中间 MSI/EXE 和 Burn .wixpdb 也先删除，确保断言对应本次构建。
    $machineMsiSource = Join-Path $installerDir "bin\Release\machine\ClipBridge-$msiVersion-win-$Platform-machine.msi"
    $userMsiSource = Join-Path $installerDir "bin\Release\user\ClipBridge-$msiVersion-win-$Platform-user.msi"
    $userExeSource = Join-Path $bundleBin "Release\ClipBridge-$msiVersion-win-$Platform-user.exe"
    $bundlePdbSource = [System.IO.Path]::ChangeExtension($userExeSource, '.wixpdb')
    $heatFile = Join-Path $installerObj 'HeatGenerated.wxs'
    if ($plan.NeedMachineMsi) { Remove-StaleIntermediate -Path $machineMsiSource -AllowedRoot $installerBin }
    if ($plan.NeedUserMsi) {
        Remove-StaleIntermediate -Path $userMsiSource -AllowedRoot $installerBin
        Remove-StaleIntermediate -Path $userExeSource -AllowedRoot $bundleBin
        Remove-StaleIntermediate -Path $bundlePdbSource -AllowedRoot $bundleBin
    }
    if ($plan.NeedMsi) { Remove-StaleIntermediate -Path $heatFile -AllowedRoot $installerObj }

    # 工具链定位早于 Rust / publish；仅 ZIP 不解析 WiX/MSBuild。
    $commonArgs = @()
    $wix = $null
    $msbuild = $null
    if ($plan.NeedWix) {
        $wix = Resolve-Wix -Explicit $WixInstallPath
        Assert-WixLayout -WixRoot $wix -NeedBundle $plan.NeedBundle
        $msbuild = Resolve-MSBuild
        $wixPath = $wix -replace '\\', '/'
        Write-Host "WiX:     $wix"
        Write-Host "MSBuild: $msbuild"
        $commonArgs = @(
            '-p:Configuration=Release',
            "-p:Platform=$Platform",
            "-p:StageDir=$($stageDir -replace '\\', '/')",
            "-p:WixTargetsPath=$wixPath/wix.targets",
            "-p:WixInstallPath=$wixPath",
            "-p:WixTasksPath=$wixPath/WixTasks.dll",
            "-p:WixExtDir=$wixPath",
            "-p:WixToolPath=$wixPath",
            "-p:HeatPath=$wixPath/heat.exe",
            '-v:m', '-nologo'
        )
    }
    elseif ($WixInstallPath) {
        Write-Warning '仅选择 portable-zip：不经过 WiX，-WixInstallPath 被忽略。'
    }

    $script:totalSteps = $progressSteps.Count
    $script:stepIndex = 0
    function Write-Step {
        param([string]$Text)
        $script:stepIndex++
        Write-Host "[$($script:stepIndex)/$($script:totalSteps)] $Text"
    }

    # ---------- Rust ----------
    if (-not $SkipRust) {
        Write-Step 'cargo build core-ffi-windows（--locked）'
        Push-Location $repoRoot
        try {
            & cargo build -p core-ffi-windows --release --locked --target x86_64-pc-windows-msvc
            if ($LASTEXITCODE -ne 0) {
                throw [System.InvalidOperationException]::new("cargo 构建失败（exit $LASTEXITCODE）")
            }
        }
        finally { Pop-Location }
    }
    else {
        Write-Warning '已跳过 cargo build：复用已有 DLL，不能作为“本次源码已验证”的证据。'
    }
    $rustDll = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll'
    if (-not (Test-Path -LiteralPath $rustDll -PathType Leaf)) {
        throw [System.IO.FileNotFoundException]::new("缺少 $rustDll")
    }
    $inputDllSha256 = Get-FileSha256 -Path $rustDll
    if ($expectedCoreProvided) {
        # 必须在任何 publish 之前完成；-SkipRust + 期望值是 release 复用已测 DLL 的关键证据。
        $null = Assert-CoreDllHashMatch -Expected $ExpectedCoreDllSha256 -Actual $inputDllSha256 -DllPath $rustDll
    }
    $sourceReusedVerified = [bool]($SkipRust -and $expectedCoreProvided -and ($inputDllSha256 -ieq $ExpectedCoreDllSha256))

    # ---------- publish / stage（一次） ----------
    Write-Step 'dotnet publish → 独立 stage（一次）'
    Invoke-AppPublish -StageDir $stageDir
    $stageDllPath = Join-Path $stageDir 'core_ffi_windows.dll'
    $stageDllSha256 = Get-FileSha256 -Path $stageDllPath
    if ($stageDllSha256 -ine $inputDllSha256) {
        throw [System.InvalidOperationException]::new(
            "publish stage core DLL hash ($stageDllSha256) != 预签名输入 ($inputDllSha256)：$stageDllPath")
    }
    $nativeEvidence = [ordered]@{
        inputDllSha256       = $inputDllSha256
        stageDllSha256       = $stageDllSha256
        sourceReusedVerified = $sourceReusedVerified
    }
    Write-Host "core DLL: input=$inputDllSha256 stage=$stageDllSha256 reusedVerified=$sourceReusedVerified"

    # ---------- 项目自有载荷签名（先于 heat/MSI） ----------
    if ($SigningMode -eq 'Certificate') {
        Write-Step '签名项目自有 EXE/DLL 载荷'
        $ownedPayloads = @(Get-ProjectOwnedPayloadFiles -StageDir $stageDir)
        Invoke-SignFiles -Files $ownedPayloads -Phase 'payload'
    }

    # ---------- MSI（需要 MSI 时 heat 一次；所有变体复用 stage/heat） ----------
    $firstMsiBuild = $true
    if ($plan.NeedMachineMsi) {
        Write-Step $(if ($firstMsiBuild) { 'machine MSI（publish 已复用；heat 一次）' } else { 'machine MSI（复用 stage/heat）' })
        Invoke-MsiVariant -PerUser $false -SkipHeat (-not $firstMsiBuild) -CommonArgs $commonArgs
        if (-not (Test-Path -LiteralPath $machineMsiSource -PathType Leaf)) {
            throw [System.IO.FileNotFoundException]::new("找不到 machine MSI：$machineMsiSource")
        }
        $firstMsiBuild = $false
    }
    if ($plan.NeedUserMsi) {
        Write-Step $(if ($firstMsiBuild) { '内部 user MSI（publish 已复用；heat 一次）' } else { '内部 user MSI（复用 stage/heat）' })
        Invoke-MsiVariant -PerUser $true -SkipHeat (-not $firstMsiBuild) -CommonArgs $commonArgs
        if (-not (Test-Path -LiteralPath $userMsiSource -PathType Leaf)) {
            throw [System.IO.FileNotFoundException]::new("找不到 user MSI：$userMsiSource")
        }
        $firstMsiBuild = $false
    }

    # ---------- MSI 签名（含内部 user MSI；bundle 必须嵌入已签 MSI） ----------
    if ($SigningMode -eq 'Certificate' -and $plan.NeedMsi) {
        Write-Step '签名 machine/user MSI（bundle 之前）'
        $msiToSign = @()
        if ($plan.NeedMachineMsi) { $msiToSign += $machineMsiSource }
        if ($plan.NeedUserMsi) { $msiToSign += $userMsiSource }
        Invoke-SignFiles -Files $msiToSign -Phase 'msi'
    }

    # ---------- Burn bundle（仅 user-exe） ----------
    if ($plan.NeedBundle) {
        Write-Step 'Burn bundle（per-user EXE）'
        $bundleArgs = @()
        $bundleArgs += $commonArgs
        $bundleArgs += "-p:UserMsiPath=$(($userMsiSource -replace '\\', '/'))"
        & $msbuild $bundleProj @bundleArgs
        if ($LASTEXITCODE -ne 0) {
            throw [System.InvalidOperationException]::new("bundle 构建失败（exit $LASTEXITCODE）")
        }
        if (-not (Test-Path -LiteralPath $userExeSource -PathType Leaf)) {
            throw [System.IO.FileNotFoundException]::new("找不到 Burn bundle：$userExeSource")
        }
        if (-not (Test-Path -LiteralPath $bundlePdbSource -PathType Leaf)) {
            throw [System.IO.FileNotFoundException]::new(
                "缺少 Burn .wixpdb（无法验证变量/MsiProperty/payload）：$bundlePdbSource")
        }
    }

    # ---------- Burn 签名（MSI 签名之后） ----------
    if ($SigningMode -eq 'Certificate' -and $plan.NeedBundle) {
        Write-Step '签名 Burn EXE'
        Invoke-SignFiles -Files @($userExeSource) -Phase 'bundle'
    }

    # ---------- 便携 ZIP（从最终 stage；内部载荷已签） ----------
    $zipPath = Join-Path $outRoot "ClipBridge-$msiVersion-win-$Platform-portable.zip"
    if ($plan.NeedPortableZip) {
        Write-Step '便携 ZIP（从最终 stage）'
        $portableName = "ClipBridge-$msiVersion-win-$Platform"
        $null = New-PortableZip -StageDir $stageDir -OutRoot $outRoot -ZipPath $zipPath -PortableName $portableName
    }

    # ---------- 复制最终产物到 OutDir ----------
    $destinations = @{}
    if ($plan.NeedMachineMsi) {
        $dest = Join-Path $outRoot "ClipBridge-$msiVersion-win-$Platform-machine.msi"
        Copy-Item -LiteralPath $machineMsiSource -Destination $dest -Force
        $destinations['machine-msi'] = $dest
    }
    if ($plan.NeedUserMsi) {
        $dest = Join-Path $outRoot "ClipBridge-$msiVersion-win-$Platform-user.msi"
        Copy-Item -LiteralPath $userMsiSource -Destination $dest -Force
        $destinations['user-msi'] = $dest
    }
    if ($plan.NeedBundle) {
        $dest = Join-Path $outRoot "ClipBridge-$msiVersion-win-$Platform-user.exe"
        Copy-Item -LiteralPath $userExeSource -Destination $dest -Force
        $destinations['user-exe'] = $dest
    }
    if ($plan.NeedPortableZip) { $destinations['portable-zip'] = $zipPath }

    # ---------- 最终产物表断言（从 OutDir 的最终文件） ----------
    Write-Step '验证最终产物表（MSI/Burn/ZIP）'
    if ($plan.NeedMachineMsi) {
        $probe = Read-MsiArtifactProbe -Path $destinations['machine-msi']
        $result = Test-MsiArtifact -Probe $probe -Kind 'machine-msi' -ExpectedVersion $msiVersion
        if (-not $result.Ok) {
            throw [System.InvalidOperationException]::new(
                "machine MSI 产物表断言失败：`n  - " + ($result.Failures -join "`n  - "))
        }
    }
    if ($plan.NeedUserMsi) {
        $probe = Read-MsiArtifactProbe -Path $destinations['user-msi']
        $result = Test-MsiArtifact -Probe $probe -Kind 'user-msi' -ExpectedVersion $msiVersion
        if (-not $result.Ok) {
            throw [System.InvalidOperationException]::new(
                "内部 user MSI 产物表断言失败：`n  - " + ($result.Failures -join "`n  - "))
        }
    }
    if ($plan.NeedBundle) {
        $bundleProbe = Read-BundleWixPdbProbe -Path $bundlePdbSource
        $bundleResult = Test-BundleArtifact `
            -Probe $bundleProbe `
            -ExpectedVersion $bundleVersion `
            -ExpectedMsiName ([System.IO.Path]::GetFileName($userMsiSource)) `
            -ExpectedMsiSize (Get-Item -LiteralPath $destinations['user-msi']).Length `
            -ExpectedMsiSha1 (Get-FileSha1 -Path $destinations['user-msi'])
        if (-not $bundleResult.Ok) {
            throw [System.InvalidOperationException]::new(
                "Burn bundle 产物表断言失败：`n  - " + ($bundleResult.Failures -join "`n  - "))
        }
    }

    $zipPayloadStates = @{}
    $zipPayloadHashes = @{}
    if ($plan.NeedPortableZip) {
        $portableName = "ClipBridge-$msiVersion-win-$Platform"
        $zipProbe = Read-PortableZipProbe -Path $destinations['portable-zip']
        foreach ($stageFile in @(Get-ProjectOwnedPayloadFiles -StageDir $stageDir)) {
            $leaf = [System.IO.Path]::GetFileName($stageFile)
            $entryName = "$portableName/$leaf"
            $zipPayloadHashes[$entryName] = Get-FileSha256 -Path $stageFile
            if ($SigningMode -eq 'Certificate') {
                $state = Get-AuthenticodeArtifactState -Path $stageFile
                $auth = if ($state.IsValid) { 'signed' } elseif ($state.SignatureStatus -eq 'NotSigned' -and -not $state.IsSigned) { 'unsigned' } else { 'invalid' }
                $zipPayloadStates[$entryName] = [pscustomobject]@{
                    Authenticode     = $auth
                    Timestamped      = [bool]$state.Timestamped
                    SignerThumbprint = [string]$state.SignerThumbprint
                    Path             = $stageFile
                }
            }
        }
        if ($SigningMode -eq 'Certificate') {
            Write-Step '验证 ZIP 内项目自有载荷 hash/签名'
        }
        $zipResult = Test-PortableZipArtifact `
            -Probe $zipProbe `
            -Version $msiVersion `
            -Platform $Platform `
            -ExpectedPayloadHashes $zipPayloadHashes `
            -PayloadSignatureStates $zipPayloadStates `
            -RequireSignedPayloads ($SigningMode -eq 'Certificate')
        if (-not $zipResult.Ok) {
            throw [System.InvalidOperationException]::new(
                "便携 ZIP 产物表断言失败：`n  - " + ($zipResult.Failures -join "`n  - "))
        }
        if ($SigningMode -eq 'Certificate' -and @($zipPayloadStates).Count -eq 0) {
            throw [System.InvalidOperationException]::new('Certificate 模式无法验证 ZIP 内项目自有载荷签名。')
        }
    }

    # ---------- 签名状态 + package-summary.json ----------
    $artifactSigningStates = @{}
    foreach ($spec in $expectedArtifacts) {
        $dest = Join-Path $outRoot $spec.Name
        if ($spec.Format -eq 'portable-zip') {
            $artifactSigningStates[$spec.Name] = [pscustomobject]@{
                Authenticode   = 'not-applicable'
                Timestamped    = $false
                PayloadsSigned = [bool]($SigningMode -eq 'Certificate')
            }
        }
        else {
            $state = Get-AuthenticodeArtifactState -Path $dest
            $auth = if ($state.IsValid) { 'signed' } elseif ($state.SignatureStatus -eq 'NotSigned' -and -not $state.IsSigned) { 'unsigned' } else { 'invalid' }
            $artifactSigningStates[$spec.Name] = [pscustomobject]@{
                Authenticode     = $auth
                Timestamped      = [bool]$state.Timestamped
                SignerThumbprint = [string]$state.SignerThumbprint
                Path             = $dest
            }
        }
    }

    $signingSummary = Assert-PackageSigningPolicy `
        -Mode $SigningMode `
        -ArtifactSigningStates $artifactSigningStates `
        -ZipPayloadsSigned ([bool]($SigningMode -eq 'Certificate')) `
        -HasPortableZip ([bool]$plan.NeedPortableZip) `
        -CertificateThumbprint $CertificateThumbprint

    Write-Step '写 package-summary.json'
    $summary = New-PackageSummary `
        -OutDir $outRoot `
        -Version $msiVersion `
        -Platform $Platform `
        -Formats $formats `
        -ExpectedArtifacts $expectedArtifacts `
        -Signing $signingSummary `
        -ArtifactSigningStates $artifactSigningStates `
        -NativeEvidence $nativeEvidence

    if ($plan.NeedPortableZip) {
        try { Remove-ItemSafely -Path (Join-Path $outRoot 'portable') -AllowedRoot $outRoot -Recurse }
        catch { Write-Warning "便携临时目录清理失败（不影响已写出的 package-summary）：$($_.Exception.Message)" }
    }
    if (-not $KeepStage) {
        try { Remove-ItemSafely -Path $stageDir -AllowedRoot $outRoot -Recurse }
        catch { Write-Warning "stage 清理失败（不影响已写出的 package-summary）：$($_.Exception.Message)" }
    }

    Write-Host ''
    Write-Host '========== 产物 =========='
    foreach ($artifact in @($summary.artifacts)) {
        $name = [string]$artifact['name']
        $size = [long]$artifact['size']
        $sha = [string]$artifact['sha256']
        $role = [string]$artifact['role']
        $auth = [string]$artifact['signing']['authenticode']
        $payload = [string]$artifact['signing']['payloadsSigned']
        $extra = if ($role -eq 'internal') { '  [internal]' } else { '' }
        if ($artifact['format'] -eq 'portable-zip') {
            Write-Host ("{0,-48} {1,8:N1} MB  sha256:{2}  payloadsSigned={3}{4}" -f $name, ($size / 1MB), $sha.Substring(0, 16), $payload, $extra)
        }
        else {
            Write-Host ("{0,-48} {1,8:N1} MB  sha256:{2}  authenticode={3}{4}" -f $name, ($size / 1MB), $sha.Substring(0, 16), $auth, $extra)
        }
    }
    Write-Host ''
    Write-Host "nativeEvidence: input=$($nativeEvidence.inputDllSha256) stage=$($nativeEvidence.stageDllSha256) sourceReusedVerified=$($nativeEvidence.sourceReusedVerified)"
    Write-Host "package-summary: $summaryPath"
    if ($KeepStage) { Write-Host "stage 已保留: $stageDir" }
    Write-Host ''
    if ($SigningMode -eq 'None') {
        Write-Host '签名: None（未签名；不是 trusted signature，只允许 CI/draft/明确接受的预发布）。'
    }
    else {
        Write-Host '签名: Certificate（已验签 + 时间戳；生产签名 provider 由调用方配置）。'
    }
}
catch {
    Write-Error -ErrorAction Continue $_
    if ($_.Exception -is [System.ArgumentException]) {
        $exitCode = 2
    }
    else {
        $exitCode = 1
    }
}

exit $exitCode
