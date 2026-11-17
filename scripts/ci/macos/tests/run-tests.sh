#!/usr/bin/env bash
# Offline checks for the macOS CI entry, shared scheme and embed-phase contract.
# Runs on Git for Windows / Linux; genuine macOS build evidence requires the
# macos-15 runner, which this script deliberately cannot fake.
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

PBXPROJ="platforms/macos/ClipBridgeShell_macOS/ClipBridge/ClipBridge.xcodeproj/project.pbxproj"
SCHEME="platforms/macos/ClipBridgeShell_macOS/ClipBridge/ClipBridge.xcodeproj/xcshareddata/xcschemes/ClipBridge.xcscheme"

for script in scripts/ci/macos.sh scripts/build-macos-ffi.sh scripts/ci/android.sh; do
	if bash -n "$script"; then
		pass "bash -n $script"
	else
		fail "bash -n $script"
	fi
done

if bash scripts/ci/macos.sh --help >/dev/null 2>&1; then
	pass "macos.sh --help"
else
	fail "macos.sh --help"
fi
if bash scripts/ci/macos.sh >/dev/null 2>&1; then
	fail "macos.sh accepts a missing outdir"
else
	pass "macos.sh rejects a missing outdir"
fi

# A shared scheme committed to the project, not a personal/auto-generated one.
if [[ -f "$SCHEME" ]]; then
	pass "shared scheme exists: $SCHEME"
else
	fail "shared scheme is missing: $SCHEME"
fi
if "$PYTHON" -c 'import sys, xml.etree.ElementTree as ET; ET.parse(sys.argv[1])' "$SCHEME" 2>/dev/null; then
	pass "shared scheme is well-formed XML"
else
	fail "shared scheme is not well-formed XML"
fi
if grep -q 'BlueprintName = "ClipBridge"' "$SCHEME" && grep -q 'BuildableName = "ClipBridge.app"' "$SCHEME"; then
	pass "shared scheme references the ClipBridge app target"
else
	fail "shared scheme does not reference the ClipBridge app target"
fi

# The Xcode embed phase must be fatal and must consume the current-checkout dylib.
if grep -Fq 'CLIPBRIDGE_CORE_DYLIB' "$PBXPROJ"; then
	pass "embed phase honors CLIPBRIDGE_CORE_DYLIB"
else
	fail "embed phase does not honor CLIPBRIDGE_CORE_DYLIB"
fi
if grep -Fq 'error: core FFI dylib not found' "$PBXPROJ"; then
	pass "embed phase fails when the dylib is missing"
else
	fail "embed phase does not fail when the dylib is missing"
fi
if grep -Fq 'warning: Missing' "$PBXPROJ"; then
	fail "embed phase still downgrades a missing dylib to a warning"
else
	pass "embed phase no longer downgrades a missing dylib to a warning"
fi

for needle in \
	'ARCHS=arm64' \
	'CLIPBRIDGE_CORE_DYLIB' \
	'-scheme "$SCHEME"' \
	"XCODE_VERSION_REQUIRED=\"26.0.1\"" \
	"SDK_VERSION_REQUIRED=\"26.0\"" \
	'aarch64-apple-darwin' \
	'--release'; do
	if grep -Fq -- "$needle" scripts/ci/macos.sh || grep -Fq -- "$needle" scripts/build-macos-ffi.sh; then
		pass "macOS entry/build script contains: $needle"
	else
		fail "macOS entry/build script is missing: $needle"
	fi
done

if grep -Fq 'probe_ffi.c' scripts/ci/macos.sh && grep -Fq 'cb_get_ffi_version' scripts/ci/macos/probe_ffi.c; then
	pass "dlopen/cb_get_ffi_version probe is wired"
else
	fail "dlopen/cb_get_ffi_version probe is not wired"
fi

if grep -Fq 'xcodebuild -list -json' scripts/ci/macos.sh && grep -Fq 'validate_xcodebuild_list.py' scripts/ci/macos.sh; then
	pass "xcodebuild -list uses JSON + validator (no indented text grep)"
else
	fail "xcodebuild -list does not use JSON + validator"
fi
if grep -Fq 'Signature=adhoc' "$PBXPROJ"; then
	pass "embed phase preserves an existing ad-hoc signature"
else
	fail "embed phase does not preserve an existing ad-hoc signature"
fi
if grep -Fq 'verify_embedded_dylib.sh' scripts/ci/macos.sh; then
	pass "macOS entry verifies embedded dylib provenance"
else
	fail "macOS entry does not verify embedded dylib provenance"
fi

# Fixture shaped like real `xcodebuild -list -json` output.
LIST_FIXTURE="scripts/ci/macos/tests/fixtures/xcodebuild-list.json"
if "$PYTHON" scripts/ci/macos/validate_xcodebuild_list.py "$LIST_FIXTURE" ClipBridge >/dev/null 2>&1; then
	pass "xcodebuild-list fixture: ClipBridge target/scheme found"
else
	fail "xcodebuild-list fixture: ClipBridge target/scheme not found"
fi
if "$PYTHON" scripts/ci/macos/validate_xcodebuild_list.py "$LIST_FIXTURE" ClipBridgeTests >/dev/null 2>&1; then
	fail "xcodebuild-list validator accepts a non-shared scheme"
else
	pass "xcodebuild-list validator rejects a non-shared scheme"
fi
if "$PYTHON" scripts/ci/macos/validate_xcodebuild_list.py "$LIST_FIXTURE" NoSuchScheme >/dev/null 2>&1; then
	fail "xcodebuild-list validator accepts an unknown scheme"
else
	pass "xcodebuild-list validator rejects an unknown scheme"
fi

# Provenance/tamper test: only an identical copy may pass.
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
printf 'not-a-real-mach-o' >"$TMP_DIR/fresh.dylib"
cp "$TMP_DIR/fresh.dylib" "$TMP_DIR/embedded.dylib"
if bash scripts/ci/macos/verify_embedded_dylib.sh "$TMP_DIR/fresh.dylib" "$TMP_DIR/embedded.dylib" >/dev/null 2>&1; then
	pass "embedded dylib provenance: identical copy passes"
else
	fail "embedded dylib provenance: identical copy fails"
fi
printf 'tampered' >>"$TMP_DIR/embedded.dylib"
if bash scripts/ci/macos/verify_embedded_dylib.sh "$TMP_DIR/fresh.dylib" "$TMP_DIR/embedded.dylib" >/dev/null 2>&1; then
	fail "embedded dylib provenance: tampered copy passed"
else
	pass "embedded dylib provenance: tampered copy fails"
fi
if bash scripts/ci/macos/verify_embedded_dylib.sh "$TMP_DIR/fresh.dylib" "$TMP_DIR/missing.dylib" >/dev/null 2>&1; then
	fail "embedded dylib provenance: missing dylib passed"
else
	pass "embedded dylib provenance: missing dylib fails"
fi

# On a non-macOS host the entry must fail loudly instead of pretending to pass.
if [[ "$(uname -s)" != "Darwin" ]]; then
	OUTPUT="$(bash scripts/ci/macos.sh "$(mktemp -d)" 2>&1 || true)"
	case "$OUTPUT" in
	*"requires the macOS runner"*)
		pass "macos.sh refuses non-macOS hosts"
		;;
	*)
		fail "macos.sh did not refuse the non-macOS host: $OUTPUT"
		;;
	esac
fi

printf '\nmacOS offline tests: %d passed, %d failed\n' "$PASS" "$FAILED"
[[ "$FAILED" -eq 0 ]]
