#!/usr/bin/env pwsh
# ============================================================================
# 校验 C ABI 表面的一致性（CI 门禁）
#
# 背景：三个平台各自维护 C 头文件时曾发生漂移 —— Android 的导出比 Windows 少 21 个
# 函数（全部 cb_logs_* / cb_query_* / cb_clear_* 等），且头文件缺 cb_get_ffi_version，
# 而文档 4.1 明确要求三平台语义一致。此脚本把"应该一致"变成可执行的断言。
#
# 校验项：
#   1) crates/cb_core_ffi/src/exports.rs 是唯一的导出定义处（存在且非空）
#   2) 三个 platforms/*/include/clipbridge_core.h 声明的函数集 == exports.rs 的导出集
#   3) 导出集覆盖文档 4.1 规定的最小集合
#
# 用法：pwsh scripts/verify-ffi-surface.ps1        （失败时退出码 1）
# ============================================================================

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($root)) { $root = (Get-Location).Path }

$exportsFile = [IO.Path]::Combine($root, 'crates', 'cb_core_ffi', 'src', 'exports.rs')
if (-not (Test-Path -LiteralPath $exportsFile)) {
    Write-Host "FAIL: 找不到共享导出表 $exportsFile"
    Write-Host "      三个平台的 C ABI 必须由 crates/cb_core_ffi/src/exports.rs 唯一定义（见核心文档 4.3.2 C）。"
    exit 1
}

# ---- 1) 权威导出集 ---------------------------------------------------------
$canonical = @(
    Select-String -LiteralPath $exportsFile -Pattern 'pub\s+(?:unsafe\s+)?extern\s+"C"\s+fn\s+(cb_[A-Za-z0-9_]+)' -AllMatches |
        ForEach-Object { $_.Matches } |
        ForEach-Object { $_.Groups[1].Value } |
        Sort-Object -Unique
)
if ($canonical.Count -eq 0) {
    Write-Host "FAIL: $exportsFile 中解析不到任何 pub extern \"C\" fn cb_* 导出"
    exit 1
}
Write-Host ("权威导出集：{0} 个函数（来自 crates/cb_core_ffi/src/exports.rs）" -f $canonical.Count)

# ---- 2) 文档 4.1 的最小集合 ------------------------------------------------
# 见 ClipBridge项目MacOS外壳文档.md 4.1「函数集策略」
$required = @(
    # 生命周期
    'cb_init', 'cb_shutdown', 'cb_free_string',
    # 本机注入
    'cb_plan_local_ingest', 'cb_ingest_local_copy',
    # 状态设备
    'cb_get_status', 'cb_list_peers', 'cb_set_peer_policy',
    # 内容拉取
    'cb_ensure_content_cached', 'cb_cancel_transfer',
    # 历史查询
    'cb_list_history', 'cb_get_item_meta',
    # 诊断日志（文档要求"与 Windows 语义一致"，这里取代表性子集）
    'cb_logs_write', 'cb_logs_query_latest', 'cb_logs_stats',
    # 统计
    'cb_query_cache_stats',
    # ABI 探针（4.2.1 冻结口径）
    'cb_get_ffi_version'
)
$missingRequired = @($required | Where-Object { $_ -notin $canonical })
if ($missingRequired.Count -gt 0) {
    Write-Host "FAIL: 导出集缺少文档 4.1/4.2.1 要求的最小集合："
    $missingRequired | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host ("文档 4.1 最小集合：全部覆盖（{0} 项）" -f $required.Count)

# ---- 3) 三份头文件必须与导出集一致 ----------------------------------------
$failed = $false
foreach ($platform in @('windows', 'macos', 'android')) {
    $header = [IO.Path]::Combine($root, 'platforms', $platform, 'include', 'clipbridge_core.h')
    if (-not (Test-Path -LiteralPath $header)) {
        Write-Host "FAIL: [$platform] 缺少头文件 $header"
        $failed = $true
        continue
    }

    # 只解析声明本身，忽略注释里提到的函数名（否则注释会导致误判）
    $declared = @(
        Get-Content -LiteralPath $header |
            Where-Object { $_ -notmatch '^\s*(//|/\*|\*)' } |
            Select-String -Pattern '\b(cb_[A-Za-z0-9_]+)\s*\(' -AllMatches |
            ForEach-Object { $_.Matches } |
            ForEach-Object { $_.Groups[1].Value } |
            Sort-Object -Unique
    )

    $missingInHeader = @($canonical | Where-Object { $_ -notin $declared })
    $extraInHeader = @($declared | Where-Object { $_ -notin $canonical })

    if ($missingInHeader.Count -eq 0 -and $extraInHeader.Count -eq 0) {
        Write-Host ("PASS: [{0}] 头文件声明 {1} 个函数，与导出集一致" -f $platform, $declared.Count)
    }
    else {
        $failed = $true
        Write-Host ("FAIL: [{0}] 头文件与导出集不一致（声明 {1} 个）" -f $platform, $declared.Count)
        if ($missingInHeader.Count -gt 0) {
            Write-Host "  头文件缺少（Rust 已导出）：" + ($missingInHeader -join ', ')
        }
        if ($extraInHeader.Count -gt 0) {
            Write-Host "  头文件多出（Rust 未导出）：" + ($extraInHeader -join ', ')
        }
    }
}

if ($failed) {
    Write-Host ""
    Write-Host "提示：头文件应是导出表的投影。若刚改过 ABI，请同步更新三份 include/clipbridge_core.h。"
    exit 1
}

# ---- 4) 平台 crate 不得重新定义导出（防止实现被抄回去） --------------------
foreach ($platform in @('windows', 'macos', 'android')) {
    $srcDir = [IO.Path]::Combine($root, 'platforms', $platform, 'core-ffi', 'src')
    if (-not (Test-Path -LiteralPath $srcDir)) { continue }
    $dup = @(
        Get-ChildItem -LiteralPath $srcDir -Recurse -Filter *.rs -File |
            Select-String -Pattern '#\[no_mangle\]' -AllMatches |
            ForEach-Object { $_.Path }
    )
    if ($dup.Count -gt 0) {
        Write-Host "FAIL: [$platform] 平台 crate 内出现 #[no_mangle]，导出实现必须只在 crates/cb_core_ffi/src/exports.rs："
        $dup | Sort-Object -Unique | ForEach-Object { Write-Host "  - $_" }
        exit 1
    }
}
Write-Host "PASS: 三个平台 crate 均未重复定义导出（实现只在共享 crate）"

Write-Host "OK: C ABI 表面校验通过（三平台一致，且覆盖文档最小集合）"
exit 0
