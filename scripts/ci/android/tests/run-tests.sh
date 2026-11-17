#!/usr/bin/env bash
# Offline checks for the Android CI entry and its helper checks.
# Runs on Git for Windows / Linux / macOS; it never needs the Android toolchain.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
cd "$ROOT_DIR"

PYTHON="${PYTHON:-}"
if [[ -n "$PYTHON" ]]; then
	:
elif python3 -c 'import json, struct, zipfile, xml.etree.ElementTree' >/dev/null 2>&1; then
	PYTHON=python3
elif python -c 'import json, struct, zipfile, xml.etree.ElementTree' >/dev/null 2>&1; then
	PYTHON=python
else
	echo "FAIL: no working Python 3 interpreter (set PYTHON=...)" >&2
	exit 1
fi

PASS=0
FAILED=0
pass() {
	PASS=$((PASS + 1))
	printf 'PASS: %s\n' "$*"
}
fail() {
	FAILED=$((FAILED + 1))
	printf 'FAIL: %s\n' "$*" >&2
}

assert_help() {
	local script="$1"
	if bash "$script" --help >/dev/null 2>&1; then
		pass "$script --help"
	else
		fail "$script --help"
	fi
}

assert_rejects_no_args() {
	local script="$1"
	if bash "$script" >/dev/null 2>&1; then
		fail "$script accepts a missing outdir"
	else
		pass "$script rejects a missing outdir"
	fi
}

for script in scripts/ci/android.sh scripts/build-macos-ffi.sh; do
	if bash -n "$script"; then
		pass "bash -n $script"
	else
		fail "bash -n $script"
	fi
done

assert_help scripts/ci/android.sh
assert_help scripts/build-macos-ffi.sh
assert_rejects_no_args scripts/ci/android.sh

# The entry must keep the extracted ci.yml contract: two explicit Rust targets,
# Release + --locked, and the explicit Gradle ABI property.
for needle in \
	'-Pclipbridge.android.abis=' \
	'--release --locked --target' \
	'aarch64-linux-android' \
	'x86_64-linux-android' \
	'SDK_PLATFORM="android-36"' \
	'BUILD_TOOLS_VERSION="36.0.0"' \
	'NDK_VERSION="26.3.11579264"' \
	'CMAKE_VERSION="3.22.1"' \
	'check-android-shell-contracts.sh' \
	'OSV_VERSION="2.7.1"' \
	'buildscript-gradle.lockfile' \
	'APP_LOCK='; do
	if grep -Fq -- "$needle" scripts/ci/android.sh; then
		pass "android.sh contains: $needle"
	else
		fail "android.sh is missing: $needle"
	fi
done

if "$PYTHON" scripts/ci/android/tests/test_policy_and_apk.py; then
	pass "OSV policy + APK verifier fixtures"
else
	fail "OSV policy + APK verifier fixtures"
fi

# Regression for the generated-path deletion guard: import the real function
# text from android.sh (no copy) with a non-exiting die stub.
GUARD_DIR="$(mktemp -d)"
trap 'rm -rf "$GUARD_DIR"' EXIT
eval "$(sed -n '/^safe_rm_rf() {/,/^}/p' scripts/ci/android.sh)"
die() {
	printf 'guard refused: %s\n' "$*" >&2
	exit 1
}
mkdir -p "$GUARD_DIR/parent"
mkdir -p "$GUARD_DIR/parent/jniLibs"
printf 'x' >"$GUARD_DIR/parent/jniLibs/file"
if (safe_rm_rf "$GUARD_DIR/parent/jniLibs" "$GUARD_DIR/parent" "jniLibs"); then
	pass "delete guard removes the expected direct child"
else
	fail "delete guard refused the expected direct child"
fi
[[ ! -e "$GUARD_DIR/parent/jniLibs" ]] && pass "delete guard actually removed the tree" || fail "delete guard did not remove the tree"

mkdir -p "$GUARD_DIR/parent/other"
if (safe_rm_rf "$GUARD_DIR/parent/other" "$GUARD_DIR/parent" "jniLibs" 2>/dev/null); then
	fail "delete guard accepted a mismatched basename"
else
	pass "delete guard refuses a mismatched basename"
fi
mkdir -p "$GUARD_DIR/outside"
if (safe_rm_rf "$GUARD_DIR/outside" "$GUARD_DIR/parent" "outside" 2>/dev/null); then
	fail "delete guard accepted a path outside the parent"
else
	pass "delete guard refuses a path outside the parent"
fi

printf '\nAndroid offline tests: %d passed, %d failed\n' "$PASS" "$FAILED"
[[ "$FAILED" -eq 0 ]]
