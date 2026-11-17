#!/usr/bin/env bash
# Regression test for the Android Rust cross-compilation environment.
#
# Hosted run 44115863192 (Android job 134153885061) failed at
# scripts/ci/android.sh line 305:
#   export "CC_${triple}=.../aarch64-linux-android26-clang"
# because bash cannot export a name containing '-': while the syntax is valid,
# the export fails at runtime with "not a valid identifier" and (under set -e)
# aborts before either cargo build.
#
# This test imports the real cargo_env_for_abi() function text from the CI entry
# (no copy), runs it with errexit active exactly like android.sh does, and
# proves that a child process sees the compiler, archiver and ranlib mapping in
# both the hyphenated and underscore spellings plus the rustc linker variable.
# It never needs an Android SDK/NDK: all tool paths are synthetic.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
cd "$ROOT_DIR"

ENTRY="scripts/ci/android.sh"

# Import the real function text from the CI entry (same pattern as the
# safe_rm_rf guard test). A source copy would not catch the hosted failure.
eval "$(sed -n '/^cargo_env_for_abi() {/,/^}/p' "$ENTRY")"
if ! declare -F cargo_env_for_abi >/dev/null; then
	echo "FAIL: $ENTRY does not define cargo_env_for_abi()" >&2
	exit 1
fi

NDK_TOOLCHAIN="/fake-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
ANDROID_API_LEVEL=26

for triple in aarch64-linux-android x86_64-linux-android; do
	underscore_triple="${triple//-/_}"
	upper_triple="$(printf '%s' "$underscore_triple" | tr '[:lower:]' '[:upper:]')"

	# With errexit active, an unexportable name aborts here just as it did in CI.
	cargo_env_for_abi "$triple"

	# Exactly the three hyphenated assignments must travel through env(1);
	# anything extra would mean untested names are being smuggled in.
	[[ "${#CC_TARGET_ENV[@]}" -eq 3 ]] || {
		echo "FAIL: expected 3 env(1) assignments for $triple, got ${#CC_TARGET_ENV[@]}" >&2
		exit 1
	}

	for assignment in "${CC_TARGET_ENV[@]}"; do
		case "$assignment" in
		*"=${NDK_TOOLCHAIN}/"*) ;;
		*)
			echo "FAIL: unexpected env(1) assignment for $triple: ${assignment%%=*}" >&2
			exit 1
			;;
		esac
		case "${assignment%%=*}" in
		"CC_${triple}" | "AR_${triple}" | "RANLIB_${triple}") ;;
		*)
			echo "FAIL: unexpected env(1) name for $triple: ${assignment%%=*}" >&2
			exit 1
			;;
		esac
	done

	# The child process is the source of truth: it sees the exported underscore
	# spellings and linker variable plus the env(1)-injected hyphenated ones,
	# which is exactly what cc-rs and rustc read.
	env "${CC_TARGET_ENV[@]}" bash -c '
		set -euo pipefail
		triple="$1"; underscore_triple="$2"; upper_triple="$3"
		toolchain="$4"; api="$5"
		expected_cc="$toolchain/$triple$api-clang"
		[[ "$(printenv "CC_$triple")" == "$expected_cc" ]]
		[[ "$(printenv "AR_$triple")" == "$toolchain/llvm-ar" ]]
		[[ "$(printenv "RANLIB_$triple")" == "$toolchain/llvm-ranlib" ]]
		[[ "$(printenv "CC_$underscore_triple")" == "$expected_cc" ]]
		[[ "$(printenv "AR_$underscore_triple")" == "$toolchain/llvm-ar" ]]
		[[ "$(printenv "RANLIB_$underscore_triple")" == "$toolchain/llvm-ranlib" ]]
		[[ "$(printenv "CARGO_TARGET_${upper_triple}_LINKER")" == "$expected_cc" ]]
	' _ "$triple" "$underscore_triple" "$upper_triple" \
		"$NDK_TOOLCHAIN" "$ANDROID_API_LEVEL"

	printf 'PASS: cargo env mapping for %s (hyphen + underscore + linker)\n' "$triple"
done

# Never reintroduce an export that bash cannot perform.
if grep -Eq 'export[[:space:]]+"[^"]*_\$\{triple\}[^"]*=' "$ENTRY"; then
	echo "FAIL: $ENTRY exports a hyphenated (or otherwise unexportable) name" >&2
	exit 1
fi
printf 'PASS: no hyphenated export in %s\n' "$ENTRY"

# The build loop must apply the collected assignments to the cargo invocation
# and keep the locked release build contract.
for needle in \
	'cargo_env_for_abi "$triple"' \
	'env "${CC_TARGET_ENV[@]}"' \
	'cargo build -p core-ffi-android --release --locked --target "$triple"'; do
	if ! grep -Fq -- "$needle" "$ENTRY"; then
		echo "FAIL: $ENTRY is missing cargo env wiring: $needle" >&2
		exit 1
	fi
done
printf 'PASS: android.sh wires the per-ABI env into the locked cargo build\n'
