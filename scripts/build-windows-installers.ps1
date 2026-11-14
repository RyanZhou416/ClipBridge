<#
.SYNOPSIS
    构建 Windows 安装器：machine MSI + user MSI + per-user Burn EXE。

.DESCRIPTION
    流程（按 ADR-004 v4.1）：
      1. version.ps1 从 Version.props 派生版本并校验
      2. cargo 构建 core-ffi（可用 -SkipRust 跳过）
      3. dotnet publish 到 stage（一次，去 pdb/xml）
      4. heat（一次）→ machine MSI、user MSI（两次 light，独立 obj/输出）
      5. Burn bundle 包 user MSI
      6. 断言：版本、无 pdb、user WordCount bit3、ARP 隐藏
    产物复制到 dist/：ClipBridge-<ver>-win-<arch>-machine.msi、ClipBridge-<ver>-win-<arch>-user.exe

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
    Write-Host '[1/5] cargo build core-ffi-windows'
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
Write-Host '[2/5] machine MSI（publish + heat + light）'
& $msbuild $wixproj @commonArgs -p:PerUser=false
if ($LASTEXITCODE -ne 0) { throw 'machine MSI 构建失败' }
$machineMsi = Join-Path $installerDir "bin\Release\machine\ClipBridge-$msiVersion-win-$Platform-machine.msi"
if (-not (Test-Path $machineMsi)) { throw "找不到 $machineMsi" }

# ---------- user MSI（复用 stage/heat） ----------
Write-Host '[3/5] user MSI'
& $msbuild $wixproj @commonArgs -p:PerUser=true -p:SkipPublish=true -p:SkipHeat=true
if ($LASTEXITCODE -ne 0) { throw 'user MSI 构建失败' }
$userMsi = Join-Path $installerDir "bin\Release\user\ClipBridge-$msiVersion-win-$Platform-user.msi"
if (-not (Test-Path $userMsi)) { throw "找不到 $userMsi" }

# ---------- bundle ----------
Write-Host '[4/5] Burn bundle'
& $msbuild $bundleProj @commonArgs -p:UserMsiPath="$(($userMsi -replace '\\','/'))"
if ($LASTEXITCODE -ne 0) { throw 'bundle 构建失败' }
$userExe = Join-Path $bundleDir "bin\Release\ClipBridge-$msiVersion-win-$Platform-user.exe"
if (-not (Test-Path $userExe)) { throw "找不到 $userExe" }

# ---------- 断言 ----------
Write-Host '[5/5] 断言'
$fail = @()

$dbM = Open-Msi $machineMsi
if ((Msi-Property $dbM 'ProductVersion') -ne $msiVersion) { $fail += "machine ProductVersion != $msiVersion" }
if ((Msi-Property $dbM 'ALLUSERS') -ne '1') { $fail += 'machine ALLUSERS != 1' }
if ((Msi-WordCount $dbM) -band 8) { $fail += 'machine WordCount bit3 应为 0（需要提权）' }
if (@(Msi-Query $dbM "SELECT FileName FROM File") | Where-Object { $_ -match '\.pdb$' }) { $fail += 'machine MSI 含 .pdb' }

$dbU = Open-Msi $userMsi
if ((Msi-Property $dbU 'ProductVersion') -ne $msiVersion) { $fail += "user ProductVersion != $msiVersion" }
if ((Msi-Property $dbU 'ALLUSERS')) { $fail += 'user ALLUSERS 应为空' }
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
Write-Host '安装器验证通过：machine MSI（提权）+ user EXE（免 UAC，ARP 隐藏）。'
