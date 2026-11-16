<#
.SYNOPSIS
    构建 Windows MSIX（Microsoft Store 渠道）安装包：注入身份/版本、裁剪 AI/ML、重打包、断言、可选自签名或 StoreUpload。

.DESCRIPTION
    只做 MSIX 渠道，不影响 MSI / EXE / 便携 / winget 的既有流程。

    流程：
      1. 版本：默认从 scripts/version.ps1 -Json 取。major<1 时默认直接失败
         （Store 要求 MSIX 版本 >= 1.0.0）；用 -Version 1.0.0 或 -AllowPreviewVersion
         显式覆盖。最终包版本固定为 4 段：<major>.<minor>.<patch>.0。
      2. 生成 manifest：把源 Package.appxmanifest 复制到项目 obj/msix 并注入
         Identity Name / Publisher / Version、DisplayName / PublisherDisplayName。
         通过 -p:ClipBridgeAppxManifest=<生成的 manifest> 传给 MSIX 构建。
         原因：Microsoft.Windows.SDK.BuildTools.MSIX（WindowsAppSDK 单项目 MSIX 工具链）
         没有 AppxPackageVersion 属性；manifest 的 Identity/@Version 才是权威，
         而且 resources.pri 的 resource map 名就是 Identity Name，必须在 MakePri 之前定稿。
      3. dotnet build Release x64：GenerateAppxPackageOnBuild=true、AppxBundle=Never、
         AppxPackageDir=<obj/appx>、AppxPackageSigningEnabled=false；
         UapAppxPackageBuildMode 默认 SideloadOnly，-StoreUpload 时用 StoreUpload
         （旧名，等价 StoreAndSideload：同时产出 .msix 与 .msixupload）。
      4. 取待裁剪的包：
         - 默认模式：dotnet build 直接产出的 .msix；
         - -StoreUpload：从 .msixupload（zip）里取出唯一的内部 .msix。
         然后 makeappx unpack → 裁剪：
         - 删除未使用的 WindowsAppSDK AI/ML（onnxruntime / DirectML / Microsoft.Windows.AI* /
           Microsoft.Windows.Internal.AI* / Microsoft.ML.OnnxRuntime*）；
         - 删除 *.pdb；
         - 删除未保留语言的 WinUI MUI 目录，只留 zh-CN / zh-TW / en-us（en-US 兼容大小写）。
      5. 校验 PRI（makepri dump 的 ResourceMap 名 == Identity Name、AppDisplayName 存在）、
         Assets、Strings，然后 makeappx pack 重打包。
      6. -StoreUpload：把裁剪后的 .msix 用 ZipArchive（Update 模式）写回 .msixupload 的内部
         条目，保留 AppxMetadata/ 等其它外层条目。不裁剪时 Store 会收到 116MB 原始包
         （含 42.5MB AI/ML），所以裁剪必须进入 upload。
      7. 断言最终包：版本 4 段且 major>=1、无 pdb、无 AI/ML、Assets/资源存在、
         DisplayName/Publisher/TargetDeviceFamily/startupTask 正确。
         -StoreUpload 额外断言：upload 是有效 zip、至少一个 .msix、内部 msix 可 makeappx
         unpack、内部包未签名、外层条目全部保留。
      8. -SelfSign（仅本地验收，默认关闭）：生成/复用 CN=<Publisher CN> 自签名证书，
         导入 CurrentUser\TrustedPeople，用 signtool 签最终 .msix。
         生产 Store 由 Store 重签，不要用 -SelfSign 产物提交 Store。
         与 -StoreUpload 互斥（Store 不接收自签名产物；upload 内部包保持未签名）。

    产物（默认 OutDir=dist/msix）：
      dist/msix/ClipBridge-<ver>-win-x64.msix                 最终包（默认模式；-SelfSign 时已签名）
      dist/msix/ClipBridge-<ver>-win-x64.msixupload           Store 提交包（-StoreUpload；未签名）
      dist/msix/unsigned/ClipBridge-<ver>-win-x64-unsigned.msix  裁剪后未签名 .msix（两种模式都保留；
                                                              -StoreUpload 时它就是 upload 的内部包）
      dist/msix/layout/ClipBridge-<ver>-win-x64/              裁剪后的布局（-KeepLayout 时保留）
      dist/msix/logs/build-msix.log                           本次构建完整日志
      dist/msix/logs/msix-summary.json                        机器可读摘要（路径/大小/SHA256/身份/模式）
      dist/msix/logs/resources.pri.inner.xml                  -StoreUpload 内部包的 PRI dump
      dist/msix/certs/*.cer                                   -SelfSign 的公开证书

.PARAMETER Version
    覆盖版本，接受 3 段（1.0.0）或 4 段（1.0.0.0，第 4 段必须为 0）。
    不传时使用 Version.props 经 version.ps1 派生的 AppxVersion。
    0.x 版本需要配合 -AllowPreviewVersion。

.PARAMETER AllowPreviewVersion
    允许 major<1 的开发/验收版本（映射为 max(1,major).minor.patch.0）。
    仅用于开发/Store 预演，不能替代正式抬版本。

.PARAMETER IdentityName
    MSIX Identity Name。默认开发值 40a26821-...；Store 提交时换成 Partner Center 分配的名称。

.PARAMETER Publisher
    MSIX Publisher（必须是 CN=... 形式的 X.500 名称）。默认 CN=Ryan416；
    Store 提交时换成 Partner Center 的 CN=<GUID>。自签名证书的 Subject 必须与它一致。

.PARAMETER PublisherDisplayName
    控制面板/Store 显示的发布者名称，默认 Ryan416。

.PARAMETER DisplayName
    包显示名，默认 ClipBridge。

.PARAMETER SelfSign
    生成/复用自签名证书并签名最终包（本地验收用，不得作为 Store 产物）。与 -StoreUpload 互斥。

.PARAMETER StoreUpload
    产出 Partner Center 用的 .msixupload（MSBuild UapAppxPackageBuildMode=StoreUpload、
    AppxBundle=Never、签名关闭）。裁剪后的 .msix 通过 ZipArchive 写回 .msixupload 的内部条目，
    外层其它条目（如 AppxMetadata/）原样保留。产物保持未签名，由 Store 重签；与 -SelfSign 互斥。

.PARAMETER OutDir
    产物根目录，默认 dist/msix（相对仓库根目录）。

.PARAMETER WindowsSdkBin
    Windows SDK x64 工具目录（含 makeappx.exe/signtool.exe/makepri.exe）。
    不传时在 C:\Program Files (x86)\Windows Kits\10\bin\10.*\x64 下自动找最高版本。

.PARAMETER SkipRust
    跳过 cargo 构建（要求 target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll 已存在）。

.PARAMETER KeepLayout
    保留裁剪后的解包布局（dist/msix/layout/...）。本地用 Add-AppxPackage -Register 验收时需要。

.EXAMPLE
    # 正式版本（Version.props 已 >= 1.0.0）
    pwsh scripts/build-windows-msix.ps1

.EXAMPLE
    # 当前 Version.props 是 0.1.0，开发/验收时显式用 1.0.0
    pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 -SelfSign -KeepLayout

.EXAMPLE
    # Store 身份替换
    pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 `
        -IdentityName '12345Ryan416.ClipBridge' -Publisher 'CN=01234567-89ab-cdef-0123-456789abcdef' `
        -PublisherDisplayName 'Ryan416'

.EXAMPLE
    # Partner Center 提交包（.msixupload，未签名，内部 .msix 已裁剪）
    pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 -StoreUpload

.EXAMPLE
    # 说明：AppxPackageVersion 在 Microsoft.Windows.SDK.BuildTools.MSIX 中不被识别，
    # 版本通过生成 manifest 注入（见脚本 DESCRIPTION 第 2 步）。
#>
[CmdletBinding()]
param(
    [string]$Version = '',
    [switch]$AllowPreviewVersion,
    [string]$IdentityName = '40a26821-a97d-4eb8-ae89-804c2cccf4a0',
    [string]$Publisher = 'CN=Ryan416',
    [string]$PublisherDisplayName = 'Ryan416',
    [string]$DisplayName = 'ClipBridge',
    [switch]$SelfSign,
    [switch]$StoreUpload,
    [string]$OutDir = 'dist/msix',
    [string]$WindowsSdkBin = '',
    [switch]$SkipRust,
    [switch]$KeepLayout
)

$ErrorActionPreference = 'Stop'
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$repoRoot = Split-Path -Parent $PSScriptRoot
$appProject = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/ClipBridgeShell_CS.csproj'
$appDir = Split-Path -Parent $appProject
$sourceManifest = Join-Path $appDir 'Package.appxmanifest'
$platform = 'x64'

$outRoot = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repoRoot $OutDir }
$logsDir = Join-Path $outRoot 'logs'
$objDir = Join-Path $outRoot 'obj'
$appxOutDir = Join-Path $objDir 'appx'
$layoutRoot = Join-Path $outRoot 'layout'
$unsignedDir = Join-Path $outRoot 'unsigned'
$certsDir = Join-Path $outRoot 'certs'
$genManifestDir = Join-Path $appDir 'obj/msix'
$genManifest = Join-Path $genManifestDir 'Package.appxmanifest'
$buildLog = Join-Path $logsDir 'build-msix.log'

New-Item -ItemType Directory -Force -Path $outRoot, $logsDir, $objDir | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Write-Log {
    param([Parameter(ValueFromRemainingArguments = $true)]$Parts)
    $line = ($Parts | ForEach-Object { [string]$_ }) -join ' '
    Write-Host $line
    Add-Content -LiteralPath $buildLog -Value $line -Encoding utf8
}

Set-Content -LiteralPath $buildLog -Value "ClipBridge MSIX build log - $(Get-Date -Format o)" -Encoding utf8

$exitCode = 0
$summary = [ordered]@{}
try {
    Write-Log "==== ClipBridge MSIX build ===="
    Write-Log ("repo     : {0}" -f $repoRoot)
    Write-Log ("out      : {0}" -f $outRoot)
    Write-Log ("started  : {0:O}" -f [DateTimeOffset]::Now)

    if (-not (Test-Path -LiteralPath $sourceManifest)) { throw "找不到源 manifest: $sourceManifest" }
    if (-not (Test-Path -LiteralPath $appProject)) { throw "找不到项目: $appProject" }
    if ($Publisher -notmatch '^CN=') { throw "Publisher 必须是 CN=... 形式（实际: '$Publisher'）" }

    # Store 只接收由 Store 重签的未签名 .msixupload；自签名产物会被 Partner Center 拒绝。
    if ($SelfSign -and $StoreUpload) {
        throw "-SelfSign 与 -StoreUpload 互斥：Store 提交包必须保持未签名（由 Store 重签），自签名 .msix 只用于本地验收。"
    }
    $buildMode = if ($StoreUpload) { 'StoreUpload' } else { 'SideloadOnly' }
    Write-Log ("mode     : {0}{1}" -f $buildMode, $(if ($StoreUpload) { '（Partner Center .msixupload，未签名）' } else { '' }))

    # ---------------------------------------------------------------- 版本
    function Resolve-MsixVersion {
        param([string]$VersionOverride, [bool]$AllowPreview)

        $requested = $null
        if ($VersionOverride) {
            $requested = $VersionOverride.Trim()
        } else {
            $json = & (Join-Path $PSScriptRoot 'version.ps1') -Json | ConvertFrom-Json
            if ($json.AppxVersion) {
                $requested = [string]$json.AppxVersion
            } else {
                if ($AllowPreview) {
                    $requested = [string]$json.SemVer
                    Write-Log ("[version] Version.props={0}，-AllowPreviewVersion 映射为 MSIX 版本（开发用）" -f $json.SemVer)
                } else {
                    throw (("MSIX 需 >=1.0.0：Version.props 当前为 {0}（major=0，Store 不允许）。" +
                        "请先把 Version.props 升到 1.0.0，或用 -Version 1.0.0（开发/验收），或 -AllowPreviewVersion。") -f $json.SemVer)
                }
            }
        }

        if ($requested -notmatch '^(\d+)\.(\d+)\.(\d+)(?:\.(\d+))?$') {
            throw "版本必须是 3 段或 4 段数字（如 1.0.0 / 1.0.0.0），实际: '$requested'"
        }
        $major = [int]$Matches[1]
        $minor = [int]$Matches[2]
        $patch = [int]$Matches[3]
        $revision = if ($Matches[4]) { [int]$Matches[4] } else { 0 }

        if ($revision -ne 0) { throw "MSIX 版本第 4 段（Revision）必须为 0，实际: '$requested'" }
        if ($major -lt 1) {
            if ($AllowPreview) {
                Write-Log ("[version] major=0 仅用于开发/验收；包版本映射为 1.{0}.{1}.0" -f $minor, $patch)
                $major = 1
            } else {
                throw (("MSIX 需 >=1.0.0：请求版本为 {0}。开发/验收请加 -AllowPreviewVersion，" +
                    "正式发布请把 Version.props 抬到 >=1.0.0。") -f $requested)
            }
        }
        if ($major -gt 65535 -or $minor -gt 65535 -or $patch -gt 65535) {
            throw "版本越界：每段必须 <= 65535，实际 $major.$minor.$patch.0"
        }

        return [pscustomobject]@{
            AppxVersion = "$major.$minor.$patch.0"
            SemVer     = "$major.$minor.$patch"
            Requested  = $requested
        }
    }

    $ver = Resolve-MsixVersion -VersionOverride $Version -AllowPreview ([bool]$AllowPreviewVersion)
    $appxVersion = $ver.AppxVersion
    $semVer = $ver.SemVer
    Write-Log ("[version] requested={0}  package={1}  filename={2}" -f $ver.Requested, $appxVersion, $semVer)
    if ($Version) {
        Write-Log "[version] 注意：-Version 覆盖只作用于本次构建，Version.props 未被修改。"
    }

    # ------------------------------------------------------------ Windows SDK
    function Resolve-WindowsSdkTools {
        param([string]$ExplicitBin, [bool]$NeedSignTool)

        $bins = @()
        if ($ExplicitBin) {
            $bins = @($ExplicitBin)
        } else {
            $roots = @(
                (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'),
                (Join-Path $env:ProgramFiles 'Windows Kits\10\bin')
            ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }
            foreach ($root in $roots) {
                $bins += Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue |
                    Where-Object { $_.Name -match '^10\.\d+' } |
                    Sort-Object { try { [version]$_.Name } catch { [version]'0.0.0.0' } } -Descending |
                    ForEach-Object { Join-Path $_.FullName 'x64' }
            }
        }

        foreach ($bin in $bins) {
            $makeappx = Join-Path $bin 'makeappx.exe'
            if (-not (Test-Path -LiteralPath $makeappx)) { continue }
            $signtool = Join-Path $bin 'signtool.exe'
            $makepri = Join-Path $bin 'makepri.exe'
            if ($NeedSignTool -and -not (Test-Path -LiteralPath $signtool)) { continue }
            return [pscustomobject]@{
                Bin      = $bin
                MakeAppx = $makeappx
                SignTool = if (Test-Path -LiteralPath $signtool) { $signtool } else { $null }
                MakePri  = if (Test-Path -LiteralPath $makepri) { $makepri } else { $null }
            }
        }

        throw ((("未找到 Windows SDK x64 工具（makeappx.exe{0}）。请安装 Windows 10/11 SDK" +
            "（含 MakeAppx/SignTool），或用 -WindowsSdkBin 指定 ...\Windows Kits\10\bin\<version>\x64。") -f
            $(if ($NeedSignTool) { '/signtool.exe' } else { '' })))
    }

    $sdk = Resolve-WindowsSdkTools -ExplicitBin $WindowsSdkBin -NeedSignTool ([bool]$SelfSign)
    Write-Log ("[sdk] bin      = {0}" -f $sdk.Bin)
    Write-Log ("[sdk] makeappx = {0}" -f $sdk.MakeAppx)
    if ($sdk.SignTool) { Write-Log ("[sdk] signtool = {0}" -f $sdk.SignTool) }
    if ($sdk.MakePri) { Write-Log ("[sdk] makepri  = {0}" -f $sdk.MakePri) }

    # -------------------------------------------------------- 生成 manifest
    function New-MsixManifest {
        param(
            [string]$SourcePath, [string]$DestinationPath,
            [string]$IdName, [string]$IdPublisher, [string]$AppxVer,
            [string]$PackageDisplayName, [string]$PublisherDisplay
        )

        [xml]$doc = Get-Content -LiteralPath $SourcePath -Raw
        $ns = New-Object System.Xml.XmlNamespaceManager($doc.NameTable)
        $ns.AddNamespace('f', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')

        $identity = $doc.SelectSingleNode('/f:Package/f:Identity', $ns)
        if (-not $identity) { throw "源 manifest 缺少 /Package/Identity: $SourcePath" }
        $identity.SetAttribute('Name', $IdName)
        $identity.SetAttribute('Publisher', $IdPublisher)
        $identity.SetAttribute('Version', $AppxVer)

        $props = $doc.SelectSingleNode('/f:Package/f:Properties', $ns)
        if (-not $props) { throw "源 manifest 缺少 /Package/Properties" }
        $displayNode = $props.SelectSingleNode('f:DisplayName', $ns)
        if (-not $displayNode) {
            $displayNode = $doc.CreateElement('DisplayName', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
            [void]$props.AppendChild($displayNode)
        }
        $displayNode.InnerText = $PackageDisplayName
        $publisherNode = $props.SelectSingleNode('f:PublisherDisplayName', $ns)
        if (-not $publisherNode) {
            $publisherNode = $doc.CreateElement('PublisherDisplayName', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
            [void]$props.AppendChild($publisherNode)
        }
        $publisherNode.InnerText = $PublisherDisplay

        # TargetDeviceFamily 只留 Windows.Desktop。
        $deps = $doc.SelectSingleNode('/f:Package/f:Dependencies', $ns)
        if (-not $deps) { throw "源 manifest 缺少 /Package/Dependencies" }
        $families = @($deps.SelectNodes('f:TargetDeviceFamily', $ns))
        $desktop = $families | Where-Object { $_.GetAttribute('Name') -eq 'Windows.Desktop' } | Select-Object -First 1
        if (-not $desktop) { throw "源 manifest 缺少 TargetDeviceFamily Name=Windows.Desktop" }
        foreach ($family in $families) {
            if (-not [object]::ReferenceEquals($family, $desktop)) {
                [void]$deps.RemoveChild($family)
            }
        }

        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $DestinationPath) | Out-Null
        $settings = New-Object System.Xml.XmlWriterSettings
        $settings.Indent = $true
        $settings.Encoding = New-Object System.Text.UTF8Encoding($false)
        $writer = [System.Xml.XmlWriter]::Create($DestinationPath, $settings)
        try { $doc.Save($writer) } finally { $writer.Close() }
    }

    Write-Log "[manifest] 生成构建用 manifest（注入身份/版本/显示名）"
    New-MsixManifest -SourcePath $sourceManifest -DestinationPath $genManifest `
        -IdName $IdentityName -IdPublisher $Publisher -AppxVer $appxVersion `
        -PackageDisplayName $DisplayName -PublisherDisplay $PublisherDisplayName
    Copy-Item -LiteralPath $genManifest -Destination (Join-Path $logsDir 'AppxManifest.generated.xml') -Force
    Write-Log ("[manifest] {0}" -f $genManifest)

    # 生成后先做一次低成本校验，避免把明显错误带入 30s 的构建。
    [xml]$check = Get-Content -LiteralPath $genManifest -Raw
    $checkNs = New-Object System.Xml.XmlNamespaceManager($check.NameTable)
    $checkNs.AddNamespace('f', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
    if ($check.SelectSingleNode('/f:Package/f:Identity', $checkNs).GetAttribute('Version') -ne $appxVersion) {
        throw "生成的 manifest 版本不匹配: $genManifest"
    }

    # ------------------------------------------------- .msixupload 内部包读写
    # StoreUpload 的 upload 是 zip（stored 不压缩），里层是待裁剪的 .msix；
    # 只替换这一个条目，AppxMetadata/ 等其它外层条目必须原样留给 Partner Center。
    function Expand-MsixUploadPayload {
        param([string]$UploadPath, [string]$DestinationPath)

        try {
            $zip = [System.IO.Compression.ZipFile]::OpenRead($UploadPath)
        } catch {
            throw "不是有效的 zip/.msixupload: $UploadPath（$($_.Exception.Message)）"
        }
        try {
            $names = @($zip.Entries | ForEach-Object { $_.FullName })
            $msixEntries = @($zip.Entries | Where-Object { $_.FullName -match '(?i)\.msix$' })
            if ($msixEntries.Count -eq 0) {
                throw ".msixupload 内没有 .msix 条目（$UploadPath）；条目: $($names -join ', ')"
            }
            if ($msixEntries.Count -gt 1) {
                $entryList = ($msixEntries | ForEach-Object { $_.FullName }) -join ', '
                throw ((".msixupload 内有 {0} 个 .msix（{1}）；当前脚本只支持单主包（AppxBundle=Never），" +
                    "多包 upload 需要先确认主包再裁剪。") -f $msixEntries.Count, $entryList)
            }
            $entry = $msixEntries[0]
            $destDir = Split-Path -Parent $DestinationPath
            if (-not (Test-Path -LiteralPath $destDir)) {
                New-Item -ItemType Directory -Force -Path $destDir | Out-Null
            }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $DestinationPath, $true)
            return [pscustomobject]@{
                EntryName       = $entry.FullName
                OuterEntryNames = $names
                Length          = $entry.Length
            }
        } finally { $zip.Dispose() }
    }

    function Update-MsixUploadPayload {
        param([string]$UploadPath, [string]$PayloadMsixPath, [string]$InnerEntryName)

        # Update 模式只改目标条目，其余条目（含 AppxMetadata/）原样保留。
        $zip = [System.IO.Compression.ZipFile]::Open($UploadPath, [System.IO.Compression.ZipArchiveMode]::Update)
        try {
            $entry = $zip.GetEntry($InnerEntryName)
            if (-not $entry) { throw "更新失败：$UploadPath 内找不到条目 '$InnerEntryName'" }
            $entry.Delete()
            # 与 MSBuild 产出的 upload 一致：内部 .msix 用 NoCompression（stored）保存。
            [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $zip, $PayloadMsixPath, $InnerEntryName, [System.IO.Compression.CompressionLevel]::NoCompression)
        } finally { $zip.Dispose() }
    }

    # ---------------------------------------------------------------- 构建
    if (-not $SkipRust) {
        $rustDll = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll'
        if (-not (Test-Path -LiteralPath $rustDll)) {
            Write-Log "[rust] 未找到已有 core_ffi_windows.dll，先 cargo build"
            Push-Location $repoRoot
            try {
                & cargo build -p core-ffi-windows --release --target x86_64-pc-windows-msvc 2>&1 |
                    Tee-Object -FilePath (Join-Path $logsDir 'cargo.log') -Append | Out-Host
                if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（exit $LASTEXITCODE）" }
            } finally { Pop-Location }
        }
    }

    if (Test-Path -LiteralPath $appxOutDir) { Remove-Item -LiteralPath $appxOutDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $appxOutDir | Out-Null

    $skipRustValue = if ($SkipRust) { 'true' } else { 'false' }
    $buildArgs = @(
        'build', $appProject,
        '-c', 'Release',
        '-nologo',
        '-v:minimal',
        "-p:Platform=$platform",
        '-p:GenerateAppxPackageOnBuild=true',
        '-p:AppxBundle=Never',
        '-p:AppxPackageSigningEnabled=false',
        "-p:UapAppxPackageBuildMode=$buildMode",
        "-p:AppxPackageDir=$appxOutDir\",
        "-p:ClipBridgeAppxManifest=$genManifest",
        "-p:SkipRustBuild=$skipRustValue"
    )
    Write-Log "[build] dotnet $($buildArgs -join ' ')"
    $buildStart = Get-Date
    Push-Location $repoRoot
    try {
        & dotnet @buildArgs 2>&1 | Tee-Object -FilePath (Join-Path $logsDir 'dotnet-build.log') -Append | Out-Host
    } finally { Pop-Location }
    $buildExit = $LASTEXITCODE
    Write-Log ("[build] exit={0} elapsed={1:N1}s" -f $buildExit, ((Get-Date) - $buildStart).TotalSeconds)
    if ($buildExit -ne 0) {
        throw "dotnet build 失败（exit $buildExit）；完整输出: $(Join-Path $logsDir 'dotnet-build.log')"
    }

    # dotnet build 产物：默认模式是 .msix；StoreUpload 模式额外产出 .msixupload（内含 .msix）。
    # 两种模式最后都归一到「待裁剪的 .msix」，后面 unpack/trim/pack 流程完全共用。
    $builtUpload = $null
    $payloadMsixPath = $null
    $uploadInnerEntry = $null
    $uploadOuterEntries = @()
    if ($StoreUpload) {
        $builtUpload = Get-ChildItem -LiteralPath $appxOutDir -Recurse -File -Filter '*.msixupload' -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if (-not $builtUpload) {
            throw (("dotnet build 成功但没有找到 .msixupload（StoreUpload 模式，{0}）；" +
                "检查 UapAppxPackageBuildMode / AppxBundle / Configuration 属性") -f $appxOutDir)
        }
        Write-Log ("[build] msixupload={0}  {1:N1} MB" -f $builtUpload.FullName, ($builtUpload.Length / 1MB))

        $uploadStage = Join-Path $objDir 'storeupload'
        if (Test-Path -LiteralPath $uploadStage) { Remove-Item -LiteralPath $uploadStage -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $uploadStage | Out-Null
        $payloadMsixPath = Join-Path $uploadStage "ClipBridge-$semVer-win-$platform-inner.msix"
        $extracted = Expand-MsixUploadPayload -UploadPath $builtUpload.FullName -DestinationPath $payloadMsixPath
        $uploadInnerEntry = $extracted.EntryName
        $uploadOuterEntries = @($extracted.OuterEntryNames)
        Write-Log ("[upload] 外层条目 {0} 个: {1}" -f $uploadOuterEntries.Count, ($uploadOuterEntries -join ', '))
        Write-Log ("[upload] 内部 msix entry='{0}'，提取 {1:N1} MB" -f $uploadInnerEntry, ((Get-Item -LiteralPath $payloadMsixPath).Length / 1MB))
        $builtMsix = Get-Item -LiteralPath $payloadMsixPath
        $originalSize = $builtMsix.Length
    } else {
        $builtMsix = Get-ChildItem -LiteralPath $appxOutDir -Recurse -File -Filter '*.msix' -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if (-not $builtMsix) { throw "dotnet build 成功但没有找到 .msix（$appxOutDir）" }
        Write-Log ("[build] msix={0}  {1:N1} MB" -f $builtMsix.FullName, ($builtMsix.Length / 1MB))
        $payloadMsixPath = $builtMsix.FullName
        $originalSize = $builtMsix.Length
    }

    # ------------------------------------------------------ unpack + 裁剪
    $layout = Join-Path $layoutRoot "ClipBridge-$semVer-win-$platform"
    if (Test-Path -LiteralPath $layout) { Remove-Item -LiteralPath $layout -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $layout | Out-Null

    function Invoke-MakeAppx {
        param([string[]]$Arguments, [string]$LogName, [string]$What)
        $log = Join-Path $logsDir $LogName
        & $sdk.MakeAppx @Arguments *> $log
        $code = $LASTEXITCODE
        if ($code -ne 0) {
            Write-Log "[makeappx] $What failed (exit $code): $log"
            Get-Content -LiteralPath $log -Tail 40 | ForEach-Object { Write-Log "  | $_" }
            throw "$What 失败（makeappx exit $code）"
        }
    }

    Write-Log "[unpack] $payloadMsixPath → $layout"
    Invoke-MakeAppx -Arguments @('unpack', '/p', $payloadMsixPath, '/d', $layout, '/o') `
        -LogName 'makeappx-unpack.log' -What 'makeappx unpack'

    Write-Log "[trim] 移除 AI/ML、pdb、未保留语言的 MUI 目录"
    $aiLeafPatterns = @(
        'onnxruntime*.dll',
        'DirectML*.dll',
        'Microsoft.Windows.AI*',
        'Microsoft.Windows.Internal.AI*',
        'Microsoft.ML.OnnxRuntime*'
    )
    $layoutFiles = @(Get-ChildItem -LiteralPath $layout -Recurse -File -ErrorAction SilentlyContinue)
    $aiFiles = @($layoutFiles | Where-Object {
        $leaf = $_.Name
        ($aiLeafPatterns | Where-Object { $leaf -like $_ }).Count -gt 0
    })
    $pdbFiles = @($layoutFiles | Where-Object { $_.Extension -ieq '.pdb' })
    $aiBytes = ($aiFiles | Measure-Object Length -Sum).Sum
    $pdbBytes = ($pdbFiles | Measure-Object Length -Sum).Sum
    if ($aiFiles.Count -gt 0) { $aiFiles | Remove-Item -Force }
    if ($pdbFiles.Count -gt 0) { $pdbFiles | Remove-Item -Force }
    Write-Log ("[trim] AI/ML: {0} file(s), {1:N1} MB" -f $aiFiles.Count, ($aiBytes / 1MB))
    Write-Log ("[trim] pdb  : {0} file(s), {1:N1} MB" -f $pdbFiles.Count, ($pdbBytes / 1MB))

    $keepCulture = @('zh-CN', 'zh-TW', 'en-us', 'en-US')
    $cultureRegex = '^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$'
    $cultureDirs = @(Get-ChildItem -LiteralPath $layout -Directory | Where-Object {
        $_.Name -match $cultureRegex -and $keepCulture -notcontains $_.Name
    })
    $cultureBytes = 0
    foreach ($dir in $cultureDirs) {
        $cultureBytes += (Get-ChildItem -LiteralPath $dir.FullName -Recurse -File | Measure-Object Length -Sum).Sum
        Remove-Item -LiteralPath $dir.FullName -Recurse -Force
    }
    Write-Log ("[trim] 未保留语言目录: {0} 个, {1:N1} MB" -f $cultureDirs.Count, ($cultureBytes / 1MB))

    $keptFiles = @(Get-ChildItem -LiteralPath $layout -Recurse -File)
    Write-Log ("[trim] 布局: {0} file(s), {1:N1} MB" -f $keptFiles.Count, (($keptFiles | Measure-Object Length -Sum).Sum / 1MB))

    # 裁剪后的布局完整性（PRI/资源/主程序）
    $requiredLayoutFiles = @(
        'AppxManifest.xml',
        'ClipBridgeShell_CS.exe',
        'core_ffi_windows.dll',
        'resources.pri',
        'Assets\StoreLogo.png',
        'Assets\Square150x150Logo.scale-200.png',
        'Assets\Square44x44Logo.scale-200.png',
        'Assets\Wide310x150Logo.scale-200.png',
        'Assets\SplashScreen.scale-200.png',
        'Strings\en-us\Resources.resw',
        'Strings\zh-cn\Resources.resw'
    )
    foreach ($rel in $requiredLayoutFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $layout $rel))) { throw "裁剪后布局缺少: $rel" }
    }
    # Microsoft.Data.Sqlite 的原生依赖：MSIX 首启导入旧数据时实际调用 VACUUM INTO，
    # 必须确认 AI/ML/pdb 裁剪没有把它误删（包内断言在 Assert-MsixPackage 里还有一次）。
    $sqliteNative = @(Get-ChildItem -LiteralPath $layout -Recurse -File -Filter 'e_sqlite3.dll' -ErrorAction SilentlyContinue)
    if ($sqliteNative.Count -eq 0) {
        throw "裁剪后布局缺少 e_sqlite3.dll（Microsoft.Data.Sqlite 原生依赖；检查 AI/ML 裁剪规则）"
    }
    Write-Log ("[sqlite] e_sqlite3.dll: {0}" -f (($sqliteNative | ForEach-Object { $_.FullName.Substring($layout.Length + 1) }) -join ', '))
    foreach ($culture in @('zh-CN', 'zh-TW', 'en-us')) {
        if (-not (Test-Path -LiteralPath (Join-Path $layout $culture))) { throw "裁剪后布局缺少语言目录: $culture" }
    }
    if ((Get-Item -LiteralPath (Join-Path $layout 'resources.pri')).Length -lt 100KB) {
        throw "resources.pri 异常偏小，可能资源未生成"
    }

    # PRI 校验：ResourceMap 名 == Identity Name，且 AppDisplayName/Strings 存在。
    function Assert-PriFile {
        param([string]$PriPath, [string]$DumpPath, [string]$LogName)

        if (-not $sdk.MakePri) {
            Write-Log "[pri] 警告：未找到 makepri.exe，跳过 PRI 结构校验"
            return
        }
        & $sdk.MakePri dump /if $PriPath /of $DumpPath /o *> (Join-Path $logsDir $LogName)
        if ($LASTEXITCODE -ne 0) { throw "makepri dump 失败（exit $LASTEXITCODE；$PriPath）" }
        [xml]$pri = Get-Content -LiteralPath $DumpPath -Raw
        $resourceMap = $pri.SelectSingleNode('/PriInfo/ResourceMap')
        if (-not $resourceMap) { throw "resources.pri 没有 ResourceMap（$PriPath）" }
        $priName = $resourceMap.GetAttribute('name')
        Write-Log ("[pri] ResourceMap='{0}'（{1}）" -f $priName, $PriPath)
        if ($priName -ne $IdentityName) {
            throw "resources.pri 的 ResourceMap('$priName') != Identity Name('$IdentityName')；身份必须在构建前注入"
        }
        $appDisplay = $pri.SelectSingleNode("/PriInfo/ResourceMap/ResourceMapSubtree[@name='Resources']/NamedResource[@name='AppDisplayName']")
        if (-not $appDisplay) { throw "resources.pri 缺少 AppDisplayName 资源（PRI/资源不可用）" }
        $stringsTree = $pri.SelectSingleNode("//*[local-name()='ResourceMapSubtree' and @name='Strings']")
        if (-not $stringsTree) { throw "resources.pri 缺少 Strings 资源树" }
    }

    $priDump = Join-Path $logsDir 'resources.pri.xml'
    Assert-PriFile -PriPath (Join-Path $layout 'resources.pri') -DumpPath $priDump -LogName 'makepri.log'

    # ---------------------------------------------------------------- 重打包
    New-Item -ItemType Directory -Force -Path $unsignedDir | Out-Null
    $unsignedMsix = Join-Path $unsignedDir "ClipBridge-$semVer-win-$platform-unsigned.msix"
    if (Test-Path -LiteralPath $unsignedMsix) { Remove-Item -LiteralPath $unsignedMsix -Force }
    Write-Log "[pack] → $unsignedMsix"
    Invoke-MakeAppx -Arguments @('pack', '/d', $layout, '/p', $unsignedMsix, '/o') `
        -LogName 'makeappx-pack.log' -What 'makeappx pack'
    $trimmedSize = (Get-Item -LiteralPath $unsignedMsix).Length
    Write-Log ("[pack] {0:N1} MB（原始 {1:N1} MB，-{2:N1} MB / -{3:P1}）" -f
        ($trimmedSize / 1MB), ($originalSize / 1MB), (($originalSize - $trimmedSize) / 1MB),
        (1 - ($trimmedSize / $originalSize)))
    if ($trimmedSize -ge $originalSize) {
        throw "裁剪后包没有变小（原始 $originalSize，裁剪后 $trimmedSize 字节），裁剪未生效"
    }

    # ------------------------------------------------------------ 校验最终包
    function Get-PackageEntryNames {
        param([string]$Path)
        $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
        try { return @($zip.Entries | ForEach-Object { $_.FullName }) } finally { $zip.Dispose() }
    }
    function Get-PackageEntryText {
        param([string]$Path, [string]$EntryName)
        $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
        try {
            $entry = $zip.GetEntry($EntryName)
            if (-not $entry) { throw "包里没有 $EntryName" }
            $reader = New-Object System.IO.StreamReader($entry.Open())
            try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
        } finally { $zip.Dispose() }
    }

    function Assert-MsixPackage {
        param([string]$Path, [string]$AppxVer, [string]$IdName, [string]$IdPublisher,
              [string]$PackageDisplayName, [string]$PublisherDisplay)

        $errors = New-Object System.Collections.Generic.List[string]
        $names = @(Get-PackageEntryNames -Path $Path)
        $manifestXml = Get-PackageEntryText -Path $Path -EntryName 'AppxManifest.xml'
        [xml]$manifest = $manifestXml
        $ns = New-Object System.Xml.XmlNamespaceManager($manifest.NameTable)
        $ns.AddNamespace('f', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
        $ns.AddNamespace('uap5', 'http://schemas.microsoft.com/appx/manifest/uap/windows10/5')
        $ns.AddNamespace('rescap', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities')

        $identity = $manifest.SelectSingleNode('/f:Package/f:Identity', $ns)
        if ($identity.GetAttribute('Version') -ne $AppxVer) { $errors.Add("Identity Version != $AppxVer（实际 $($identity.GetAttribute('Version'))）") }
        if ($identity.GetAttribute('Name') -ne $IdName) { $errors.Add("Identity Name != $IdName（实际 $($identity.GetAttribute('Name'))）") }
        if ($identity.GetAttribute('Publisher') -ne $IdPublisher) { $errors.Add("Identity Publisher != $IdPublisher") }
        if ($identity.GetAttribute('ProcessorArchitecture') -ne 'x64') { $errors.Add("ProcessorArchitecture != x64（实际 $($identity.GetAttribute('ProcessorArchitecture'))）") }
        if ($AppxVer -notmatch '^\d+\.\d+\.\d+\.0$') { $errors.Add("包版本不是 4 段且 Revision=0: $AppxVer") }
        if ([int]($AppxVer.Split('.')[0]) -lt 1) { $errors.Add("包版本 major < 1: $AppxVer") }

        $props = $manifest.SelectSingleNode('/f:Package/f:Properties', $ns)
        if ($props.SelectSingleNode('f:DisplayName', $ns).InnerText -ne $PackageDisplayName) {
            $errors.Add("DisplayName != $PackageDisplayName（实际 $($props.SelectSingleNode('f:DisplayName', $ns).InnerText)）")
        }
        if ($props.SelectSingleNode('f:PublisherDisplayName', $ns).InnerText -ne $PublisherDisplay) {
            $errors.Add("PublisherDisplayName != $PublisherDisplay")
        }

        $deviceFamilies = @($manifest.SelectNodes('/f:Package/f:Dependencies/f:TargetDeviceFamily', $ns))
        if ($deviceFamilies.Count -ne 1 -or $deviceFamilies[0].GetAttribute('Name') -ne 'Windows.Desktop') {
            $errors.Add("TargetDeviceFamily 必须只有 Windows.Desktop（实际 $((@($deviceFamilies | ForEach-Object { $_.GetAttribute('Name') })) -join ','))")
        }

        $startup = $manifest.SelectSingleNode('/f:Package/f:Applications/f:Application/f:Extensions/uap5:Extension[@Category="windows.startupTask"]/uap5:StartupTask', $ns)
        if (-not $startup) {
            $errors.Add("缺少 uap5:Extension Category=windows.startupTask / uap5:StartupTask")
        } else {
            if ($startup.GetAttribute('TaskId') -ne 'ClipBridgeStartupTask') { $errors.Add("StartupTask TaskId != ClipBridgeStartupTask") }
            if ($startup.GetAttribute('Enabled') -ne 'false') { $errors.Add("StartupTask Enabled 应为 false（用户从设置里启用）") }
            if ($startup.GetAttribute('DisplayName') -ne 'ClipBridge') { $errors.Add("StartupTask DisplayName != ClipBridge") }
            $startupExtension = $startup.ParentNode
            if ($startupExtension.GetAttribute('Executable') -ne 'ClipBridgeShell_CS.exe') { $errors.Add("startupTask Executable != ClipBridgeShell_CS.exe") }
            if ($startupExtension.GetAttribute('EntryPoint') -ne 'Windows.FullTrustApplication') { $errors.Add("startupTask EntryPoint != Windows.FullTrustApplication") }
        }

        if (-not $manifest.SelectSingleNode('/f:Package/f:Capabilities/rescap:Capability[@Name="runFullTrust"]', $ns)) {
            $errors.Add("缺少 runFullTrust 能力")
        }

        foreach ($required in @(
                'ClipBridgeShell_CS.exe', 'core_ffi_windows.dll', 'resources.pri',
                'Assets/StoreLogo.png', 'Assets/Square150x150Logo.scale-200.png',
                'Assets/Square44x44Logo.scale-200.png', 'Assets/Square44x44Logo.targetsize-24_altform-unplated.png',
                'Assets/Wide310x150Logo.scale-200.png', 'Assets/SplashScreen.scale-200.png',
                'Strings/en-us/Resources.resw', 'Strings/zh-cn/Resources.resw')) {
            if ($names -notcontains $required) { $errors.Add("包内缺少 $required") }
        }

        $pdb = @($names | Where-Object { $_ -match '\.pdb$' })
        if ($pdb.Count -gt 0) { $errors.Add("包内仍有 pdb: $($pdb -join ', ')") }

        # MSIX 首启导入旧数据依赖 Microsoft.Data.Sqlite（VACUUM INTO），原生 e_sqlite3.dll 必须入包。
        $sqliteNative = @($names | Where-Object { ($_ -split '/')[-1] -eq 'e_sqlite3.dll' })
        if ($sqliteNative.Count -eq 0) { $errors.Add("包内缺少 e_sqlite3.dll（Microsoft.Data.Sqlite 原生依赖）") }
        if ($names -notcontains 'Microsoft.Data.Sqlite.dll') { $errors.Add("包内缺少 Microsoft.Data.Sqlite.dll") }

        $ai = @($names | Where-Object {
            $leaf = ($_ -split '/')[-1]
            ($aiLeafPatterns | Where-Object { $leaf -like $_ }).Count -gt 0
        })
        if ($ai.Count -gt 0) { $errors.Add("包内仍有 AI/ML: $($ai -join ', ')") }

        $cultureTop = @($names | ForEach-Object {
            $parts = $_ -split '/'
            if ($parts.Count -gt 1 -and $parts[0] -match $cultureRegex) { $parts[0] } else { $null }
        } | Where-Object { $_ } | Sort-Object -Unique)
        $unexpectedCulture = @($cultureTop | Where-Object { $keepCulture -notcontains $_ })
        if ($unexpectedCulture.Count -gt 0) { $errors.Add("包内有未保留语言目录: $($unexpectedCulture -join ', ')") }
        foreach ($culture in @('zh-CN', 'zh-TW', 'en-us')) {
            if ($cultureTop -notcontains $culture) { $errors.Add("包内缺少语言目录 $culture") }
        }

        if ($names -notcontains 'AppxManifest.xml') { $errors.Add("包内缺少 AppxManifest.xml") }
        if ($names -notcontains 'AppxBlockMap.xml') { $errors.Add("包内缺少 AppxBlockMap.xml") }
        if ($names -notcontains '[Content_Types].xml') { $errors.Add("包内缺少 [Content_Types].xml") }

        return $errors
    }

    # StoreUpload 专用：外层 upload（zip）+ 内部 msix（复用 Assert-MsixPackage）。
    function Assert-MsixUpload {
        param([string]$Path, [string[]]$OriginalOuterEntries, [string]$InnerEntryName,
              [string]$WorkspaceDir, [string]$AppxVer, [string]$IdName, [string]$IdPublisher,
              [string]$PackageDisplayName, [string]$PublisherDisplay)

        $errors = New-Object System.Collections.Generic.List[string]

        try {
            $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
        } catch {
            throw "upload 不是有效 zip：$Path（$($_.Exception.Message)）"
        }
        try {
            $names = @($zip.Entries | ForEach-Object { $_.FullName })
            $msixEntries = @($zip.Entries | Where-Object { $_.FullName -match '(?i)\.msix$' })
            if ($msixEntries.Count -eq 0) { $errors.Add("upload 内没有 .msix 条目") }
            if ($msixEntries.Count -gt 1) { $errors.Add("upload 内有多个 .msix（$($msixEntries.Count)），不符合当前单主包约定") }
            $inner = $msixEntries | Where-Object { $_.FullName -eq $InnerEntryName } | Select-Object -First 1
            if (-not $inner) { $errors.Add("upload 内找不到内部包条目 '$InnerEntryName'") }

            # AppxMetadata/ 等外层条目必须原样保留（Update 模式只应替换内部 msix）。
            foreach ($name in $OriginalOuterEntries) {
                if ($names -notcontains $name) { $errors.Add("upload 丢失外层条目: $name") }
            }
            $unexpected = @($names | Where-Object { $OriginalOuterEntries -notcontains $_ })
            if ($unexpected.Count -gt 0) { $errors.Add("upload 出现意外外层条目: $($unexpected -join ', ')") }

            if ($errors.Count -gt 0) { return $errors }

            # 从最终 upload 里重新提取内部 msix，验证写回的就是它。
            $verifyDir = Join-Path $WorkspaceDir 'verify'
            if (Test-Path -LiteralPath $verifyDir) { Remove-Item -LiteralPath $verifyDir -Recurse -Force }
            New-Item -ItemType Directory -Force -Path $verifyDir | Out-Null
            $verifyMsix = Join-Path $verifyDir "ClipBridge-$AppxVer-inner.msix"
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($inner, $verifyMsix, $true)
        } finally { $zip.Dispose() }

        $innerNames = @(Get-PackageEntryNames -Path $verifyMsix)
        if ($innerNames -contains 'AppxSignature.p7x') {
            $errors.Add("upload 内部 msix 是签名包（Store 只接收未签名 upload；-StoreUpload 不要配合 -SelfSign）")
        }
        $verifyMsixItem = Get-Item -LiteralPath $verifyMsix
        $innerHash = (Get-FileHash -LiteralPath $verifyMsix -Algorithm SHA256).Hash
        Write-Log ("[assert] upload 内部 msix: entry='{0}'，{1:N1} MB，{2} file(s)，SHA256 {3}" -f
            $InnerEntryName, ($verifyMsixItem.Length / 1MB), $innerNames.Count, $innerHash)
        Write-Log ("[assert] upload 外层条目 {0} 个: {1}" -f $OriginalOuterEntries.Count, ($OriginalOuterEntries -join ', '))

        $msixErrors = @(Assert-MsixPackage -Path $verifyMsix -AppxVer $AppxVer -IdName $IdName `
            -IdPublisher $IdPublisher -PackageDisplayName $PackageDisplayName -PublisherDisplay $PublisherDisplay)
        foreach ($e in $msixErrors) { $errors.Add("内部 msix: $e") }

        # Store 侧同样会解包校验；这里用 makeappx 真实解开最终 upload 的内部包。
        $verifyLayout = Join-Path $verifyDir 'layout'
        Invoke-MakeAppx -Arguments @('unpack', '/p', $verifyMsix, '/d', $verifyLayout, '/o') `
            -LogName 'makeappx-upload-unpack.log' -What 'makeappx unpack（upload 内部 msix）'
        foreach ($rel in @('AppxManifest.xml', 'ClipBridgeShell_CS.exe', 'core_ffi_windows.dll', 'resources.pri',
                'Microsoft.Data.Sqlite.dll', 'e_sqlite3.dll')) {
            if (-not (Test-Path -LiteralPath (Join-Path $verifyLayout $rel))) {
                $errors.Add("upload 内部 msix 解包后缺少 $rel")
            }
        }
        $verifyFiles = @(Get-ChildItem -LiteralPath $verifyLayout -Recurse -File)
        Write-Log ("[assert] upload 内部 msix 解包: {0} file(s)" -f $verifyFiles.Count)
        Assert-PriFile -PriPath (Join-Path $verifyLayout 'resources.pri') `
            -DumpPath (Join-Path $logsDir 'resources.pri.inner.xml') -LogName 'makepri-inner.log'

        return $errors
    }

    # ------------------------------------------------------------ 签名 / 组装
    $finalMsix = Join-Path $outRoot "ClipBridge-$semVer-win-$platform.msix"
    $finalUpload = Join-Path $outRoot "ClipBridge-$semVer-win-$platform.msixupload"
    $finalArtifact = if ($StoreUpload) { $finalUpload } else { $finalMsix }
    $signThumbprint = $null
    $signatureStatus = 'unsigned'
    $signatureMessage = ''

    if ($StoreUpload) {
        # 用裁剪后的 .msix 替换 upload 内部条目；外层其它条目原样保留，产物保持未签名。
        if (Test-Path -LiteralPath $finalUpload) { Remove-Item -LiteralPath $finalUpload -Force }
        Copy-Item -LiteralPath $builtUpload.FullName -Destination $finalUpload -Force
        Write-Log ("[upload] 写回裁剪后内部包: {0} ← {1}" -f $uploadInnerEntry, $unsignedMsix)
        Update-MsixUploadPayload -UploadPath $finalUpload -PayloadMsixPath $unsignedMsix -InnerEntryName $uploadInnerEntry
        Write-Log "[sign] -StoreUpload：产物未签名（Partner Center/Store 重签）；不自签名"
    } elseif ($SelfSign) {
        if (Test-Path -LiteralPath $finalMsix) { Remove-Item -LiteralPath $finalMsix -Force }
        if (-not $sdk.SignTool) { throw "-SelfSign 需要 signtool.exe；请用 -WindowsSdkBin 指定 SDK x64 目录" }
        New-Item -ItemType Directory -Force -Path $certsDir | Out-Null
        Write-Log "[sign] 查找/创建自签名证书 Subject='$Publisher'"

        function Get-CodeSigningCertificate {
            param([string]$Subject)
            $certs = @(Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue | Where-Object {
                $_.Subject -eq $Subject -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date).AddDays(30)
            })
            foreach ($candidate in ($certs | Sort-Object NotAfter -Descending)) {
                # EnhancedKeyUsageList 的元素是 EnhancedKeyUsageRepresentation，
                # 它的 ObjectId 是字符串（不是 Oid 对象），所以直接取 .ObjectId。
                $eku = @($candidate.EnhancedKeyUsageList | ForEach-Object { [string]$_.ObjectId })
                if ($eku -contains '1.3.6.1.5.5.7.3.3') { return $candidate }
            }
            return $null
        }

        $cert = Get-CodeSigningCertificate -Subject $Publisher
        if (-not $cert) {
            Write-Log "[sign] 当前用户证书库没有匹配证书，创建 CN 自签名证书"
            $cert = New-SelfSignedCertificate `
                -Type Custom -Subject $Publisher `
                -KeyUsage DigitalSignature -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 `
                -KeyExportPolicy Exportable -CertStoreLocation Cert:\CurrentUser\My `
                -NotAfter (Get-Date).AddYears(3) -FriendlyName 'ClipBridge MSIX dev' `
                -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
        }
        $signThumbprint = $cert.Thumbprint
        Write-Log ("[sign] cert={0} thumbprint={1} notAfter={2:yyyy-MM-dd}" -f $cert.Subject, $cert.Thumbprint, $cert.NotAfter)

        $safeSubject = ($Publisher -replace '[^A-Za-z0-9._-]', '_')
        $cerPath = Join-Path $certsDir "ClipBridge-$safeSubject.cer"
        Export-Certificate -Cert $cert -FilePath $cerPath -Type CERT | Out-Null
        Write-Log ("[sign] 导出公钥证书: {0}" -f $cerPath)

        # 任务要求：CurrentUser\TrustedPeople（不需要管理员）。
        $userTrusted = @(Get-ChildItem Cert:\CurrentUser\TrustedPeople -ErrorAction SilentlyContinue | Where-Object { $_.Thumbprint -eq $signThumbprint })
        if ($userTrusted.Count -eq 0) {
            Import-Certificate -FilePath $cerPath -CertStoreLocation Cert:\CurrentUser\TrustedPeople | Out-Null
            Write-Log "[sign] 已导入 CurrentUser\TrustedPeople"
        } else {
            Write-Log "[sign] CurrentUser\TrustedPeople 已有该证书"
        }

        # 若当前进程已提权，再导入 LocalMachine\TrustedPeople（Add-AppxPackage 真正要求的位置）。
        $isElevated = $false
        try {
            $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
            $isElevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        } catch { }
        if ($isElevated) {
            $machineTrusted = @(Get-ChildItem Cert:\LocalMachine\TrustedPeople -ErrorAction SilentlyContinue | Where-Object { $_.Thumbprint -eq $signThumbprint })
            if ($machineTrusted.Count -eq 0) {
                Import-Certificate -FilePath $cerPath -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
                Write-Log "[sign] 已导入 LocalMachine\TrustedPeople（提权会话）"
            }
        } else {
            Write-Log "[sign] 当前会话未提权：跳过 LocalMachine\TrustedPeople；Add-AppxPackage 需要管理员导入后才能真正信任自签名包"
        }

        Copy-Item -LiteralPath $unsignedMsix -Destination $finalMsix -Force
        Write-Log "[sign] signtool sign → $finalMsix"
        & $sdk.SignTool sign /fd SHA256 /sha1 $signThumbprint $finalMsix 2>&1 |
            Tee-Object -FilePath (Join-Path $logsDir 'signtool-sign.log') -Append | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "signtool sign 失败（exit $LASTEXITCODE）" }

        $sig = Get-AuthenticodeSignature -LiteralPath $finalMsix
        $signatureStatus = [string]$sig.Status
        $signatureMessage = [string]$sig.StatusMessage
        Write-Log ("[sign] AuthenticodeStatus={0}  message={1}" -f $signatureStatus, $signatureMessage)
        if (-not $sig.SignerCertificate) { throw "签名后没有 SignerCertificate：$finalMsix" }

        # 只报告验证结果，不因自签名根不受信而让构建失败（本机验收说明见 docs/windows-build.md）。
        # 注意：用户的 PowerShell profile 可能把 $PSNativeCommandUseErrorActionPreference 设为
        # true；那样 verify 的“预期 exit 1”会变成 NativeCommandExitException，直接跳过下面的
        # 显式 exit code 检查。这里只在本段临时关闭该偏好，退出后原样恢复。
        $nativeExitPreference = $PSNativeCommandUseErrorActionPreference
        $PSNativeCommandUseErrorActionPreference = $false
        try {
            & $sdk.SignTool verify /pa /v $finalMsix *> (Join-Path $logsDir 'signtool-verify.log')
            $verifyExit = $LASTEXITCODE
        } finally {
            $PSNativeCommandUseErrorActionPreference = $nativeExitPreference
        }
        Write-Log ("[sign] signtool verify exit={0}（1=自签名根不在 LocalMachine 信任链，属预期；详见 signtool-verify.log）" -f $verifyExit)
    } else {
        Copy-Item -LiteralPath $unsignedMsix -Destination $finalMsix -Force
        Write-Log "[sign] 未指定 -SelfSign：输出未签名包（Store 提交由 Store 重签）"
    }

    # ------------------------------------------------------------ 断言与摘要
    Write-Log "[assert] 校验最终产物结构/身份/manifest"
    if ($StoreUpload) {
        $assertErrors = @(Assert-MsixUpload -Path $finalUpload -OriginalOuterEntries $uploadOuterEntries `
            -InnerEntryName $uploadInnerEntry -WorkspaceDir (Join-Path $objDir 'storeupload') `
            -AppxVer $appxVersion -IdName $IdentityName -IdPublisher $Publisher `
            -PackageDisplayName $DisplayName -PublisherDisplay $PublisherDisplayName)
    } else {
        $finalNames = @(Get-PackageEntryNames -Path $finalMsix)
        if ($SelfSign) {
            if ($finalNames -notcontains 'AppxSignature.p7x') { throw "签名包缺少 AppxSignature.p7x：$finalMsix" }
        } else {
            if ($finalNames -contains 'AppxSignature.p7x') { throw "未签名包却包含 AppxSignature.p7x：$finalMsix" }
        }
        $assertErrors = @(Assert-MsixPackage -Path $finalMsix -AppxVer $appxVersion -IdName $IdentityName `
            -IdPublisher $Publisher -PackageDisplayName $DisplayName -PublisherDisplay $PublisherDisplayName)
    }
    if ($assertErrors.Count -gt 0) {
        foreach ($e in $assertErrors) { Write-Log "  [X] $e" }
        $what = if ($StoreUpload) { '.msixupload' } else { '.msix' }
        throw ("{0} 断言失败 {1} 项" -f $what, $assertErrors.Count)
    }
    Write-Log "[assert] OK"

    $finalItem = Get-Item -LiteralPath $finalArtifact
    $sha256 = (Get-FileHash -LiteralPath $finalArtifact -Algorithm SHA256).Hash
    Write-Log ("[result] path   : {0}" -f $finalItem.FullName)
    Write-Log ("[result] size   : {0:N1} MB ({1} bytes)" -f ($finalItem.Length / 1MB), $finalItem.Length)
    Write-Log ("[result] sha256 : {0}" -f $sha256)
    Write-Log ("[result] version: {0}（Identity Version）" -f $appxVersion)
    Write-Log ("[result] identity: {0} / {1} / {2}" -f $IdentityName, $Publisher, $PublisherDisplayName)
    Write-Log ("[result] mode   : {0}" -f $buildMode)
    Write-Log ("[result] signed : {0}" -f $signatureStatus)
    if ($StoreUpload) {
        Write-Log ("[result] upload : inner='{0}'，裁剪后 .msix={1}" -f $uploadInnerEntry, $unsignedMsix)
    }
    if ($SelfSign) {
        Write-Log ("[result] 签名证书: {0}" -f $signThumbprint)
    }

    # 便于本地 Add-AppxPackage -Register 的布局（默认删除，-KeepLayout 保留）。
    if (-not $KeepLayout) {
        if (Test-Path -LiteralPath $layout) { Remove-Item -LiteralPath $layout -Recurse -Force }
        Write-Log "[cleanup] 已删除布局（-KeepLayout 可保留）"
    } else {
        Write-Log ("[layout] {0}" -f $layout)
    }

    $summary = [ordered]@{
        schemaVersion         = 1
        builtAt               = [DateTimeOffset]::Now.ToString('o')
        buildMode             = $buildMode
        storeUpload           = [bool]$StoreUpload
        semVer                = $semVer
        appxVersion           = $appxVersion
        identityName          = $IdentityName
        publisher             = $Publisher
        publisherDisplayName  = $PublisherDisplayName
        displayName           = $DisplayName
        architecture          = $platform
        packagePath           = $finalItem.FullName
        packageSizeBytes      = $finalItem.Length
        sha256                = $sha256
        unsignedPackagePath   = $unsignedMsix
        uploadInnerEntry      = $uploadInnerEntry
        uploadOuterEntries    = @($uploadOuterEntries)
        layoutPath            = if ($KeepLayout) { $layout } else { $null }
        generatedManifestPath = $genManifest
        originalPackageSizeBytes = $originalSize
        trimmedPackageSizeBytes  = $trimmedSize
        removedAiMlFiles      = $aiFiles.Count
        removedAiMlBytes      = $aiBytes
        removedPdbFiles       = $pdbFiles.Count
        removedCultureDirs    = $cultureDirs.Count
        selfSigned            = [bool]$SelfSign
        signerThumbprint      = $signThumbprint
        signatureStatus       = $signatureStatus
        buildExitCode         = $buildExit
        elapsedSeconds        = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    }
    $summaryPath = Join-Path $logsDir 'msix-summary.json'
    $summary | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $summaryPath -Encoding utf8
    Write-Log ("[summary] {0}" -f $summaryPath)
    Write-Log ("[done] {0:N1}s" -f $sw.Elapsed.TotalSeconds)
} catch {
    $exitCode = 1
    Write-Log ""
    Write-Log "ERROR: $($_.Exception.Message)"
    if ($_.ScriptStackTrace) { Write-Log $_.ScriptStackTrace }
    Write-Log "[failed] 现场保留在 $outRoot（日志: $buildLog）"
} finally {
    $sw.Stop()
}

if ($exitCode -eq 0 -and (Test-Path -LiteralPath (Join-Path $logsDir 'msix-summary.json'))) {
    Write-Host ''
    Write-Host '========== MSIX 产物 =========='
    $s = Get-Content -LiteralPath (Join-Path $logsDir 'msix-summary.json') -Raw | ConvertFrom-Json
    Write-Host ("  {0}" -f $s.packagePath)
    Write-Host ("  {0:N1} MB   SHA256 {1}" -f ($s.packageSizeBytes / 1MB), $s.sha256)
    Write-Host ("  Identity {0} / {1}" -f $s.identityName, $s.publisher)
    Write-Host ("  Version  {0}   Mode={1}   Signed={2}" -f $s.appxVersion, $s.buildMode, $s.selfSigned)
    if ($s.storeUpload) {
        Write-Host ("  Upload   inner='{0}'   trimmedMsix={1}" -f $s.uploadInnerEntry, $s.unsignedPackagePath)
    }
}
Write-Host ("耗时: {0:N1}s   exit code: {1}" -f $sw.Elapsed.TotalSeconds, $exitCode)
exit $exitCode
