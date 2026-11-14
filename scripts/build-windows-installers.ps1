<#
.SYNOPSIS
    构建 Windows 安装器：machine MSI + user MSI + per-user Burn EXE + 便携 ZIP。

.DESCRIPTION
    流程（按 ADR-004 v4.1）：
      1. version.ps1 从 Version.props 派生版本并校验
      2. cargo 构建 core-ffi（可用 -SkipRust 跳过）
      3. dotnet publish 到 stage（一次，去 pdb/xml）
      4. heat（一次）→ machine MSI、user MSI（两次 light，独立 obj/输出）
      5. Burn bundle 包 user MSI
      6. 断言：版本、无 pdb、user WordCount bit3、ARP 隐藏
      7. 便携 ZIP（stage 全部 + ClipBridge.portable + version.txt，放在删除 stage 之前）
    产物复制到 dist/：ClipBridge-<ver>-win-<arch>-machine.msi、ClipBridge-<ver>-win-<arch>-user.exe、
    ClipBridge-<ver>-win-<arch>-portable.zip

.EXAMPLE
    pwsh scripts/build-windows-installers.ps1 -WixInstallPath C:\tools\wix314
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Platform = 'x64',
    [string]$OutDir = 'dist',

    # WiX 3.14 目录（含 candle/light/heat/WixTasks.dll）。不传则探测标准安装位置。
    [string]$WixInstallPath,

    [switch]$SkipRust,
    [switch]$KeepStage
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$installerDir = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer'
$bundleDir = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Bundle'
$wixproj = Join-Path $installerDir 'ClipBridgeShell_CS.Installer.wixproj'
$bundleProj = Join-Path $bundleDir 'ClipBridgeShell_CS.Bundle.wixproj'
$outRoot = Join-Path $repoRoot $OutDir
$stageDir = Join-Path $outRoot 'stage'

function Resolve-Wix {
    param([string]$explicit)
    if ($explicit) {
        if (Test-Path (Join-Path $explicit 'candle.exe')) { return (Resolve-Path $explicit).Path }
        throw "WiX 目录里没有 candle.exe: $explicit"
    }
    foreach ($c in @(
            (Join-Path ${env:ProgramFiles(x86)} 'WiX Toolset v3.14'),
            (Join-Path $env:ProgramFiles 'WiX Toolset v3.14'))) {
        if ($c -and (Test-Path (Join-Path $c 'candle.exe'))) { return $c }
    }
    throw '未找到 WiX 3.14。请安装（winget install WiXToolset.WiXToolset）或用 -WixInstallPath 指定。'
}

function Resolve-MSBuild {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $p = & $vswhere -latest -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
        if ($p -and (Test-Path $p)) { return $p }
    }
    foreach ($c in @(
            'C:\Program Files\Microsoft Visual Studio\2022\Community\MSBuild\Current\Bin\MSBuild.exe',
            'C:\Program Files\Microsoft Visual Studio\2022\Professional\MSBuild\Current\Bin\MSBuild.exe',
            'C:\Program Files\Microsoft Visual Studio\2022\Enterprise\MSBuild\Current\Bin\MSBuild.exe')) {
        if (Test-Path $c) { return $c }
    }
    throw '未找到 MSBuild.exe（需要 VS 2022 或 Build Tools）。'
}

function Open-Msi([string]$path) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($path, 0))
    return $db
}
function Msi-Query($db, [string]$sql) {
    $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($sql))
    $null = $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null)
    $rows = @()
    while ($true) {
        $rec = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($null -eq $rec) { break }
        $n = $rec.GetType().InvokeMember('FieldCount', 'GetProperty', $null, $rec, $null)
        $row = @()
        for ($i = 1; $i -le $n; $i++) {
            $row += [string]$rec.GetType().InvokeMember('StringData', 'GetProperty', $null, $rec, @($i))
        }
        $rows += ($row -join ' | ')
    }
    return $rows
}
function Msi-Property($db, [string]$name) {
    $r = @(Msi-Query $db "SELECT Value FROM Property WHERE Property='$name'")
    if ($r.Count -eq 0) { return $null }
    return $r[0]
}
function Msi-WordCount($db) {
    $si = $db.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $db, @(0))
    $wc = [int]$si.GetType().InvokeMember('Property', 'GetProperty', $null, $si, @(15))
    return $wc
}

# ---------- 版本 ----------
$versionJson = & (Join-Path $PSScriptRoot 'version.ps1') -Json | ConvertFrom-Json
$msiVersion = $versionJson.MsiVersion
$bundleVersion = $versionJson.BundleVersion
if (-not $msiVersion) { throw 'version.ps1 没返回 MsiVersion' }
Write-Host "版本: $msiVersion (MSI/Bundle)   架构: $Platform"

$wix = Resolve-Wix $WixInstallPath
$msbuild = Resolve-MSBuild
$wixPath = ($wix -replace '\\', '/')
Write-Host "WiX:     $wix"
Write-Host "MSBuild: $msbuild"

New-Item -ItemType Directory -Force -Path $outRoot | Out-Null

# ---------- Rust ----------
if (-not $SkipRust) {
    Write-Host '[1/6] cargo build core-ffi-windows'
    Push-Location $repoRoot
    try {
        & cargo build -p core-ffi-windows --release --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw 'cargo 构建失败' }
    } finally { Pop-Location }
}
$rustDll = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release\core_ffi_windows.dll'
if (-not (Test-Path $rustDll)) { throw "缺少 $rustDll" }

$commonArgs = @(
    '-p:Configuration=Release',
    "-p:Platform=$Platform",
    "-p:StageDir=$($stageDir -replace '\\','/')",
    "-p:WixTargetsPath=$wixPath/wix.targets",
    "-p:WixInstallPath=$wixPath",
    "-p:WixTasksPath=$wixPath/WixTasks.dll",
    "-p:WixExtDir=$wixPath",
    "-p:WixToolPath=$wixPath",
    "-p:HeatPath=$wixPath/heat.exe",
    '-v:m', '-nologo'
)

# ---------- machine MSI（顺带 publish + heat） ----------
Write-Host '[2/6] machine MSI（publish + heat + light）'
& $msbuild $wixproj @commonArgs -p:PerUser=false
if ($LASTEXITCODE -ne 0) { throw 'machine MSI 构建失败' }
$machineMsi = Join-Path $installerDir "bin\Release\machine\ClipBridge-$msiVersion-win-$Platform-machine.msi"
if (-not (Test-Path $machineMsi)) { throw "找不到 $machineMsi" }

# ---------- user MSI（复用 stage/heat） ----------
Write-Host '[3/6] user MSI'
& $msbuild $wixproj @commonArgs -p:PerUser=true -p:SkipPublish=true -p:SkipHeat=true
if ($LASTEXITCODE -ne 0) { throw 'user MSI 构建失败' }
$userMsi = Join-Path $installerDir "bin\Release\user\ClipBridge-$msiVersion-win-$Platform-user.msi"
if (-not (Test-Path $userMsi)) { throw "找不到 $userMsi" }

# ---------- bundle ----------
Write-Host '[4/6] Burn bundle'
& $msbuild $bundleProj @commonArgs -p:UserMsiPath="$(($userMsi -replace '\\','/'))"
if ($LASTEXITCODE -ne 0) { throw 'bundle 构建失败' }
$userExe = Join-Path $bundleDir "bin\Release\ClipBridge-$msiVersion-win-$Platform-user.exe"
if (-not (Test-Path $userExe)) { throw "找不到 $userExe" }

# ---------- 断言 ----------
Write-Host '[5/6] 断言'
$fail = @()

$dbM = Open-Msi $machineMsi
if ((Msi-Property $dbM 'ProductVersion') -ne $msiVersion) { $fail += "machine ProductVersion != $msiVersion" }
if ((Msi-Property $dbM 'ALLUSERS') -ne '1') { $fail += 'machine ALLUSERS != 1' }
if ((Msi-WordCount $dbM) -band 8) { $fail += 'machine WordCount bit3 应为 0（需要提权）' }
if (@(Msi-Query $dbM "SELECT FileName FROM File") | Where-Object { $_ -match '\.pdb$' }) { $fail += 'machine MSI 含 .pdb' }

$dbU = Open-Msi $userMsi
if ((Msi-Property $dbU 'ProductVersion') -ne $msiVersion) { $fail += "user ProductVersion != $msiVersion" }
if ((Msi-Property $dbU 'ALLUSERS') -ne '2') { $fail += 'user ALLUSERS 应为 2（双用途授权）' }
if ((Msi-Property $dbU 'MSIINSTALLPERUSER') -ne '1') { $fail += 'user MSIINSTALLPERUSER 应为 1' }
if ((Msi-Property $dbU 'ARPSYSTEMCOMPONENT') -ne '1') { $fail += 'user ARPSYSTEMCOMPONENT != 1' }
if (-not ((Msi-WordCount $dbU) -band 8)) { $fail += 'user WordCount bit3 应为 1（per-user 免提权）' }
if (@(Msi-Query $dbU "SELECT FileName FROM File") | Where-Object { $_ -match '\.pdb$' }) { $fail += 'user MSI 含 .pdb' }
$dirParent = @(Msi-Query $dbU "SELECT Directory_Parent FROM Directory WHERE Directory='INSTALLFOLDER'") | Select-Object -First 1
if ($dirParent -ne 'ProgramsFolder') { $fail += "user INSTALLFOLDER 父目录应为 ProgramsFolder，实际 $dirParent" }

$exeVersion = (Get-Item $userExe).VersionInfo.ProductVersion
if ($exeVersion -ne $bundleVersion) { $fail += "bundle ProductVersion=$exeVersion，期望 $bundleVersion" }

if ($fail.Count -gt 0) {
    $fail | ForEach-Object { Write-Error $_ }
    throw "断言失败 $($fail.Count) 项"
}

# ---------- 便携 ZIP（stage 已去 pdb/xml；必须在删除 stage 之前） ----------
# 解压即用：顶层目录与资产名一致，避免“解压到当前目录”时把上百个文件散落一地。
Write-Host '[6/6] 便携 ZIP'
$portableName = "ClipBridge-$msiVersion-win-$Platform"
$portableStageRoot = Join-Path $outRoot 'portable'
$portableDir = Join-Path $portableStageRoot $portableName
$portableZip = Join-Path $outRoot "$portableName-portable.zip"

if (Test-Path $portableStageRoot) { Remove-Item $portableStageRoot -Recurse -Force }
New-Item -ItemType Directory -Force -Path $portableDir | Out-Null

# 1) stage 全部内容（publish 时已删除 pdb/xml）复制进顶层目录。
Copy-Item -Path (Join-Path $stageDir '*') -Destination $portableDir -Recurse -Force

# 2) 空标记：exe 旁存在 ClipBridge.portable 时，数据写到 <exeDir>\data。
New-Item -ItemType File -Path (Join-Path $portableDir 'ClipBridge.portable') -Force | Out-Null

# 3) 版本 + 简短说明。
@(
    "ClipBridge $msiVersion portable (win-$Platform)"
    ''
    'How to use:'
    "  1. Extract the whole $portableName folder to a writable location."
    '  2. Run ClipBridgeShell_CS.exe.'
    ''
    'Portable mode:'
    "  The empty marker file 'ClipBridge.portable' next to the executable keeps all data"
    "  (core.db, tls, Cache, logs, Strings) in the 'data' subfolder next to the executable."
    '  If that folder is not writable (e.g. Program Files, read-only USB drive), ClipBridge'
    '  falls back to %LOCALAPPDATA%\ClipBridge, shows a warning, and appends the reason to'
    '  %LOCALAPPDATA%\ClipBridge\portable-fallback.log.'
    '  Delete ClipBridge.portable to use the per-user %LOCALAPPDATA%\ClipBridge location.'
) | Set-Content -Path (Join-Path $portableDir 'version.txt') -Encoding utf8

# 4) 打包：includeBaseDirectory=$true 才会把顶层目录写进 zip。
if (Test-Path $portableZip) { Remove-Item $portableZip -Force }
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $portableDir,
    $portableZip,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $true)

# 5) 断言：marker、core_ffi_windows.dll、无 pdb、顶层目录正确。
$zipFail = @()
$zipEntryCount = 0
$zip = [System.IO.Compression.ZipFile]::OpenRead($portableZip)
try {
    $entries = @($zip.Entries | ForEach-Object { $_.FullName -replace '\\', '/' })
    $zipEntryCount = $entries.Count
    $prefix = "$portableName/"
    if ($entries -notcontains "${prefix}ClipBridge.portable") { $zipFail += '便携 zip 缺少 ClipBridge.portable' }
    if ($entries -notcontains "${prefix}core_ffi_windows.dll") { $zipFail += '便携 zip 缺少 core_ffi_windows.dll' }
    $zipPdbs = @($entries | Where-Object { $_ -match '\.pdb$' })
    if ($zipPdbs.Count -gt 0) { $zipFail += "便携 zip 含 .pdb：$($zipPdbs -join ', ')" }
    $topDirs = @($entries | ForEach-Object { ($_ -split '/')[0] } | Sort-Object -Unique)
    if ($topDirs.Count -ne 1 -or $topDirs[0] -ne $portableName) {
        $zipFail += "便携 zip 顶层目录应为 $portableName，实际：$($topDirs -join ', ')"
    }
} finally {
    $zip.Dispose()
}
if ($zipFail.Count -gt 0) {
    $zipFail | ForEach-Object { Write-Error $_ }
    throw "便携 ZIP 断言失败 $($zipFail.Count) 项"
}
Remove-Item $portableStageRoot -Recurse -Force -ErrorAction SilentlyContinue
Write-Host ("便携 ZIP: {0}  ({1:N1} MB, {2} 个条目)" -f $portableName, ((Get-Item $portableZip).Length / 1MB), $zipEntryCount)

# ---------- 输出 ----------
Copy-Item $machineMsi (Join-Path $outRoot "ClipBridge-$msiVersion-win-$Platform-machine.msi") -Force
Copy-Item $userExe (Join-Path $outRoot "ClipBridge-$msiVersion-win-$Platform-user.exe") -Force
Copy-Item $userMsi (Join-Path $outRoot "ClipBridge-$msiVersion-win-$Platform-user.msi") -Force
if (-not $KeepStage) { Remove-Item $stageDir -Recurse -Force -ErrorAction SilentlyContinue }

Write-Host ''
Write-Host '========== 产物 =========='
Get-ChildItem $outRoot -File | Sort-Object Name | ForEach-Object {
    "{0,-48} {1,8:N1} MB  sha256:{2}" -f $_.Name, ($_.Length / 1MB), (Get-FileHash -Algorithm SHA256 $_.FullName).Hash.Substring(0, 16)
}
Write-Host ''
Write-Host '安装器验证通过：machine MSI（提权）+ user EXE（免 UAC，ARP 隐藏）+ 便携 ZIP（ClipBridge.portable）。'
