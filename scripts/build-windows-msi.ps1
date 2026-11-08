<#
.SYNOPSIS
    构建 Windows 安装包（WiX 3 MSI）。

.DESCRIPTION
    流程：MSBuild 调用 ClipBridgeShell_CS.Installer.wixproj
          1) BeforeBuild 里 dotnet publish 应用（含 Rust core-ffi，Release + R2R）
          2) heat.exe 收集 publish 目录
          3) candle/light 编译并链接出 MSI

    前置条件：WiX Toolset v3.14（winget install WiXToolset.WiXToolset）。
    可用 -WixInstallPath 指定解压目录（例如免安装的 wix314-binaries.zip）。

.EXAMPLE
    pwsh scripts/build-windows-msi.ps1
    pwsh scripts/build-windows-msi.ps1 -Platform arm64
    pwsh scripts/build-windows-msi.ps1 -WixInstallPath C:\tools\wix314
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Platform = 'x64',

    [ValidateSet('Release', 'Debug')]
    [string]$Configuration = 'Release',

    # WiX 3.14 安装目录（包含 candle.exe/light.exe/heat.exe/WixTasks.dll）。
    [string]$WixInstallPath,

    # 跳过签名；当前安装器未签名（SmartScreen 会提示）。
    [switch]$NoSign
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$proj = Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/ClipBridgeShell_CS.Installer.wixproj'
if (-not (Test-Path $proj)) { throw "找不到安装器工程: $proj" }

# 1) 定位 WiX
function Resolve-WixInstallPath([string]$explicit) {
    if ($explicit) {
        if (Test-Path (Join-Path $explicit 'candle.exe')) { return (Resolve-Path $explicit).Path }
        throw "指定的 WiX 目录里没有 candle.exe: $explicit"
    }

    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} 'WiX Toolset v3.14'),
        (Join-Path $env:ProgramFiles 'WiX Toolset v3.14'),
        'C:\Program Files (x86)\WiX Toolset v3.11'
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path (Join-Path $c 'candle.exe'))) { return $c }
    }

    # 注册表兜底（winget 安装会写这里）
    foreach ($k in @(
            'HKLM:\SOFTWARE\Microsoft\Windows Installer XML\3.14',
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Installer XML\3.14')) {
        if (Test-Path $k) {
            $root = (Get-ItemProperty $k -ErrorAction SilentlyContinue).InstallRoot
            if ($root -and (Test-Path (Join-Path $root 'candle.exe'))) { return $root }
        }
    }

    throw @"
未找到 WiX Toolset v3.14。请任选一种方式安装：
  winget install WiXToolset.WiXToolset
或下载解压 https://github.com/wixtoolset/wix3/releases 的 wix314-binaries.zip，
然后运行：pwsh scripts/build-windows-msi.ps1 -WixInstallPath <解压目录>
"@
}

$wix = Resolve-WixInstallPath $WixInstallPath
$wixMsbuildPath = ($wix -replace '\\', '/')

# 2) 找 MSBuild（WiX 3 需要 .NET Framework MSBuild，不能用 dotnet msbuild）
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$msbuild = $null
if (Test-Path $vswhere) {
    $msbuild = & $vswhere -latest -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
}
if (-not $msbuild) {
    foreach ($c in @(
            'C:\Program Files\Microsoft Visual Studio\2022\Community\MSBuild\Current\Bin\MSBuild.exe',
            'C:\Program Files\Microsoft Visual Studio\2022\Professional\MSBuild\Current\Bin\MSBuild.exe',
            'C:\Program Files\Microsoft Visual Studio\2022\Enterprise\MSBuild\Current\Bin\MSBuild.exe')) {
        if (Test-Path $c) { $msbuild = $c; break }
    }
}
if (-not $msbuild) { throw '未找到 MSBuild.exe（需要 Visual Studio 2022 或 Build Tools）。' }

Write-Host "WiX:      $wix"
Write-Host "MSBuild:  $msbuild"
Write-Host "Platform: $Platform / $Configuration"
Write-Host ''

$sw = [System.Diagnostics.Stopwatch]::StartNew()
& $msbuild $proj `
    -p:Configuration=$Configuration `
    -p:Platform=$Platform `
    -p:WixTargetsPath="$wixMsbuildPath/wix.targets" `
    -p:WixInstallPath="$wixMsbuildPath" `
    -p:WixTasksPath="$wixMsbuildPath/WixTasks.dll" `
    -p:WixExtDir="$wixMsbuildPath" `
    -p:WixToolPath="$wixMsbuildPath" `
    -p:HeatPath="$wixMsbuildPath/heat.exe" `
    -v:m -nologo
$exit = $LASTEXITCODE
$sw.Stop()

if ($exit -ne 0) { throw "WiX/MSBuild 构建失败（exit $exit）。" }

$msi = Get-ChildItem (Join-Path $repoRoot 'platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Installer/bin') `
    -Recurse -Filter '*.msi' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $msi) { throw '构建成功但找不到 MSI 产物。' }

Write-Host ''
Write-Host ("MSI: {0}" -f $msi.FullName)
Write-Host ("大小: {0:N1} MB" -f ($msi.Length / 1MB))
Write-Host ("耗时: {0:N1}s" -f $sw.Elapsed.TotalSeconds)
Write-Host ''
Write-Host '提示：未签名安装包首次运行会被 SmartScreen 拦一次（“仍要运行”）；'
Write-Host '      未勾选“删除用户数据”的卸载会保留 %LOCALAPPDATA%\ClipBridge。'
if (-not $NoSign) { Write-Host '      （-NoSign 仅用于跳过未来可能加入的签名步骤，当前没有签名。）' }
