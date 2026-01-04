# ClipBridge - Setup Android NDK env vars (User scope)
# Usage:
#   PowerShell (Normal) : .\tools\setup-ndk.ps1
#   PowerShell (Admin not required)

$ErrorActionPreference = "Stop"

Write-Host "== ClipBridge: Android NDK environment setup =="

# 1) Detect Android SDK root (most common default)
$sdkRoot = Join-Path $env:LOCALAPPDATA "Android\Sdk"
if (-not (Test-Path $sdkRoot)) {
    throw "Android SDK not found at: $sdkRoot`nInstall Android SDK/NDK via Android Studio -> SDK Manager first."
}

$ndkParent = Join-Path $sdkRoot "ndk"
if (-not (Test-Path $ndkParent)) {
    throw "NDK directory not found at: $ndkParent`nInstall NDK (Side by side) via Android Studio -> SDK Manager -> SDK Tools."
}

# 2) Pick the highest version folder under ndk/
$ndkDirs = Get-ChildItem -Path $ndkParent -Directory -ErrorAction Stop
if ($ndkDirs.Count -eq 0) {
    throw "No NDK versions found under: $ndkParent`nInstall NDK (Side by side) via Android Studio."
}

function Parse-Version([string]$name) {
    # Example: "26.1.10909125"
    # Return a tuple-like array for sorting: [major, minor, patch]
    $parts = $name.Split(".")
    $maj = 0; $min = 0; $pat = 0
    if ($parts.Length -ge 1) { [int]::TryParse($parts[0], [ref]$maj) | Out-Null }
    if ($parts.Length -ge 2) { [int]::TryParse($parts[1], [ref]$min) | Out-Null }
    if ($parts.Length -ge 3) { [int]::TryParse($parts[2], [ref]$pat) | Out-Null }
    return @($maj, $min, $pat)
}

$ndkSelected = $ndkDirs |
    Sort-Object -Property @{
        Expression = { (Parse-Version $_.Name)[0] }
    }, @{
        Expression = { (Parse-Version $_.Name)[1] }
    }, @{
        Expression = { (Parse-Version $_.Name)[2] }
    } -Descending |
    Select-Object -First 1

$ndkRoot = $ndkSelected.FullName

# 3) Validate ndk-build.cmd exists (side-by-side NDK has it)
$ndkBuild = Join-Path $ndkRoot "ndk-build.cmd"
if (-not (Test-Path $ndkBuild)) {
    throw "Selected NDK does not contain ndk-build.cmd: $ndkBuild`nNDK install may be incomplete."
}

# 4) Set env vars (User scope + Current Process)
function Set-EnvVar([string]$name, [string]$value) {
    # Persist to User scope (registry)
    [Environment]::SetEnvironmentVariable($name, $value, "User")

    # Apply to current PowerShell process immediately
    Set-Item -Path "Env:$name" -Value $value
}


Set-EnvVar "ANDROID_NDK_HOME" $ndkRoot
Set-EnvVar "ANDROID_NDK_ROOT" $ndkRoot
Set-EnvVar "ANDROID_NDK"      $ndkRoot

Set-EnvVar "ANDROID_HOME"     $sdkRoot
Set-EnvVar "ANDROID_SDK_ROOT" $sdkRoot

$cmakeToolchain = Join-Path $ndkRoot "build\cmake\android.toolchain.cmake"
if (-not (Test-Path $cmakeToolchain)) {
    throw "CMake toolchain file not found: $cmakeToolchain`nNDK install may be incomplete."
}

Write-Host ""
Write-Host "Configured (User scope + current session):"
Write-Host "  ANDROID_NDK_HOME     = $env:ANDROID_NDK_HOME"
Write-Host "  ANDROID_NDK_ROOT     = $env:ANDROID_NDK_ROOT"
Write-Host "  ANDROID_NDK          = $env:ANDROID_NDK"
Write-Host "  ANDROID_HOME         = $env:ANDROID_HOME"
Write-Host "  ANDROID_SDK_ROOT     = $env:ANDROID_SDK_ROOT"
Write-Host "  CMAKE_TOOLCHAIN_FILE = $env:CMAKE_TOOLCHAIN_FILE"
Write-Host ""
Write-Host "Verification:"
Write-Host "  Found: $ndkBuild"
Write-Host "  Found: $cmakeToolchain"

# Optional: warn if Ninja missing while generator is Ninja
if ($env:CMAKE_GENERATOR -eq "Ninja" -or $env:AWS_LC_SYS_CMAKE_GENERATOR -eq "Ninja") {
    $ninja = Get-Command ninja -ErrorAction SilentlyContinue
    if (-not $ninja) {
        Write-Host ""
        Write-Host "WARNING: 'ninja' not found in PATH, but CMAKE_GENERATOR=Ninja is set."
        Write-Host "Install Ninja or remove CMAKE_GENERATOR/AWS_LC_SYS_CMAKE_GENERATOR."
        Write-Host "Example: winget install Ninja-build.Ninja"
    } else {
        Write-Host ""
        Write-Host "Ninja found: $($ninja.Source)"
    }
}
