#!/usr/bin/env bash
# ============================================================================
# Android 外壳的“跨语言契约”静态门禁（在 ubuntu-latest 的 android job 里运行）
#
# 为什么需要它：Java/AIDL 编译只能证明“自洽”，证明不了与平台/Rust 侧的契约一致。
# 下面三类问题都是历史上真实漏进 main 的缺陷，编译器与 assembleDebug 都发现不了：
#
#   1) IClipboard.aidl 的 Binder 事务码 = FIRST_CALL_TRANSACTION + 方法声明下标。
#      方法少一个或顺序错一位，AIDL 照样编译通过，但运行时 getPrimaryClip 会落到
#      别的事务码上（曾把“读剪贴板”变成“清剪贴板”）。这里把 API 36 的方法顺序钉死。
#   2) Android init JSON 由 CoreConfig.java 手工拼装，而 Rust 的 InitConfigDto 中
#      没有 serde(default) 的字段是必填的。漏掉 account_password 时 Java 编译完全
#      正常，只有运行时 init 才失败。这里从 marshal.rs 解析必填字段并要求 Java 源码
#      逐字出现对应的 put(...)。
#   3) cb_ensure_content_cached 的请求/响应是 JSON 字符串。Java 把裸 itemId 当请求、
#      把整个 envelope 当 transfer_id 也能编译通过，只在运行时报错。这里从
#      EnsureContentDto 解析必填字段，并在调用点检查请求键与 transfer_id 解析。
#
# 边界说明：这不是“平台真理”的完整替代。AIDL 顺序表需要在 compileSdk 升级时更新
#（见下方 AOSP 链接），运行时行为仍需设备/仪器测试；但它能挡住“静默漂移”。
# 用法：bash scripts/check-android-shell-contracts.sh   （失败退出码 1）
# ============================================================================
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_DIR="$ROOT_DIR/platforms/android/ClipBridgeShellAndroid"
AIDL_FILE="$ANDROID_DIR/app/src/main/aidl/android/content/IClipboard.aidl"
MARSHAL_FILE="$ROOT_DIR/crates/cb_core_ffi/src/marshal.rs"
CORE_CONFIG_FILE="$ANDROID_DIR/app/src/main/java/com/ryan416/clipbridgeshellandroid/core/CoreConfig.java"
JAVA_SRC_DIR="$ANDROID_DIR/app/src/main/java"

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

# ---- 1) IClipboard.aidl：方法与声明顺序（= 事务码 1..12） --------------------
# 权威顺序：AOSP android-16.0.0_r1（compileSdk = 36）
# https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-16.0.0_r1/core/java/android/content/IClipboard.aidl
# 只在末尾追加方法；不要删除或重排。升级 compileSdk 时同步更新本数组。
EXPECTED_AIDL_METHODS=(
	setPrimaryClip
	setPrimaryClipAsPackage
	clearPrimaryClip
	getPrimaryClip
	getPrimaryClipDescription
	hasPrimaryClip
	addPrimaryClipChangedListener
	removePrimaryClipChangedListener
	hasClipboardText
	getPrimaryClipSource
	areClipboardAccessNotificationsEnabledForUser
	setClipboardAccessNotificationsEnabledForUser
)

if [[ ! -f "$AIDL_FILE" ]]; then
	fail "找不到 AIDL 文件：$AIDL_FILE"
fi

mapfile -t actual_aidl_methods < <(
	sed -nE 's/^[[:space:]]*(void|boolean|String|ClipData|ClipDescription)[[:space:]]+([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*\(.*/\2/p' "$AIDL_FILE"
)

if (( ${#actual_aidl_methods[@]} != ${#EXPECTED_AIDL_METHODS[@]} )); then
	echo "本地 IClipboard.aidl 声明了 ${#actual_aidl_methods[@]} 个方法，期望 ${#EXPECTED_AIDL_METHODS[@]} 个：" >&2
	printf '  - %s\n' "${actual_aidl_methods[@]}" >&2
	fail "方法集合与 API 36 不一致；事务码 = 1 + 声明下标，方法数不对会让所有后续调用错位"
fi

for i in "${!EXPECTED_AIDL_METHODS[@]}"; do
	expected="${EXPECTED_AIDL_METHODS[$i]}"
	actual="${actual_aidl_methods[$i]}"
	if [[ "$actual" != "$expected" ]]; then
		fail "IClipboard.aidl 第 $((i + 1)) 个方法（事务码 $((i + 1))）应为 ${expected}，实际为 ${actual}"
	fi
done
echo "PASS: IClipboard.aidl 的 12 个方法及声明顺序与 API 36 一致（事务码 1..12）"

# ---- 从 marshal.rs 的 DTO 解析“必填 JSON 字段” ------------------------------
# 规则：结构体内既没有 #[serde(default)]、类型也不是 Option<...> 的字段为必填。
# serde 未指定 rename 时，JSON 键名就是 Rust 字段名。
required_dto_fields() {
	local struct_name="$1"
	awk -v struct="$struct_name" '
		function trim(s) { sub(/^[[:space:]]+/, "", s); sub(/[[:space:]]+$/, "", s); return s }
		!in_struct && index($0, "struct " struct " {") > 0 && $0 !~ /^[[:space:]]*\/\// {
			in_struct = 1
			has_default = 0
			next
		}
		in_struct && $0 ~ /^}/ {
			in_struct = 0
			next
		}
		in_struct {
			if ($0 ~ /#\[serde\(default\)\]/) { has_default = 1; next }
			if ($0 ~ /^[[:space:]]*\/\// || $0 ~ /^[[:space:]]*$/) { next }
			if ($0 ~ /^[[:space:]]*#\[/) { next }
			if ($0 ~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?[A-Za-z_][A-Za-z0-9_]*[[:space:]]*:/) {
				line = trim($0)
				sub(/^pub(\([^)]*\))?[[:space:]]+/, "", line)
				name = line
				sub(/[[:space:]]*:.*/, "", name)
				type = line
				sub(/^[^:]*:[[:space:]]*/, "", type)
				sub(/,$/, "", type)
				type = trim(type)
				if (!has_default && type !~ /^Option</) { print name }
				has_default = 0
				next
			}
			if ($0 !~ /^[[:space:]]*$/) { has_default = 0 }
		}
	' "$MARSHAL_FILE"
}

# ---- 2) init JSON：InitConfigDto 的必填字段必须真的写进请求 -----------------
if [[ ! -f "$MARSHAL_FILE" ]]; then
	fail "找不到共享 FFI DTO 文件：$MARSHAL_FILE"
fi
if [[ ! -f "$CORE_CONFIG_FILE" ]]; then
	fail "找不到 Android init 配置构造器：$CORE_CONFIG_FILE"
fi

init_fields="$(required_dto_fields InitConfigDto)"
if [[ -z "$init_fields" ]]; then
	fail "无法从 $MARSHAL_FILE 解析 InitConfigDto 的必填字段（结构体改名或格式变化？请同步本脚本）"
fi
while IFS= read -r field; do
	[[ -z "$field" ]] && continue
	# 只认真正的 JSON 写入：root.put("field", ...)；只出现在常量名里不算。
	if ! grep -Eq "put\\([[:space:]]*\"${field}\"" "$CORE_CONFIG_FILE"; then
		fail "CoreConfig.java 没有把 InitConfigDto 的必填字段 \"${field}\" 写进 init JSON（运行时 cb_init 会失败）"
	fi
done <<< "$init_fields"
echo "PASS: Android init JSON 覆盖了 InitConfigDto 的全部必填字段"

# ---- 3) cb_ensure_content_cached：请求键与响应 transfer_id -------------------
ensure_fields="$(required_dto_fields EnsureContentDto)"
if [[ -z "$ensure_fields" ]]; then
	fail "无法从 $MARSHAL_FILE 解析 EnsureContentDto 的必填字段（结构体改名或格式变化？请同步本脚本）"
fi

mapfile -t ensure_callers < <(grep -rlF '.ensureContentCached(' --include='*.java' "$JAVA_SRC_DIR" 2>/dev/null || true)
if (( ${#ensure_callers[@]} == 0 )); then
	fail "在 $JAVA_SRC_DIR 下找不到 .ensureContentCached( 的 Java 调用点（调用点被删除或改名？）"
fi

for caller in "${ensure_callers[@]}"; do
	relative="${caller#"$ROOT_DIR/"}"
	while IFS= read -r field; do
		[[ -z "$field" ]] && continue
		if ! grep -Eq "put\\([[:space:]]*\"${field}\"" "$caller"; then
			fail "${relative} 调用 cb_ensure_content_cached，却没有构造必填请求字段 \"${field}\"（FFI 请求是 JSON，不是裸字符串）"
		fi
	done <<< "$ensure_fields"
	if ! grep -Fq '"transfer_id"' "$caller"; then
		fail "${relative} 没有解析响应 data.transfer_id（FFI 响应是 {ok,data:{transfer_id}} envelope，不能把整个 envelope 当 id）"
	fi
done
echo "PASS: cb_ensure_content_cached 的调用点构造了必填请求字段并解析 transfer_id"

echo "OK: Android 外壳跨语言契约检查通过"
