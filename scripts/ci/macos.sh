#!/usr/bin/env bash
# =============================================================================
# ClipBridge macOS CI entry point.
#
#   bash scripts/ci/macos.sh <outdir>
#
# Only meaningful on the hosted macOS runner (macos-15, arm64). It:
#   1. validates Xcode 26.0.1 / macOS SDK 26.0 and the Rust target;
#   2. builds the dylib from the current checkout (Release,
#      aarch64-apple-darwin) via scripts/build-macos-ffi.sh;
#   3. builds the real Xcode target/scheme `ClipBridge` Release with
#      ARCHS=arm64 (the SwiftPM package is a stub and is never a substitute);
#   4. proves the freshly built dylib is what got embedded, that the bundle and
#      library are arm64 and the library only depends on system libraries;
#   5. checks the app and embedded dylib carry the expected local ad-hoc
#      signature (no Developer ID is used or required);
#   6. compiles a tiny C probe that dlopen()s the embedded dylib and checks
#      cb_get_ffi_version compatibility.
#
# The app is never launched, no UI/network/user data is touched. A pass is
# evidence for this runner only; it is not a universal/Intel build, notarization
# or a distributable artifact.
#
# Exit code: 0 only when every step passed; any failure exits non-zero.
# =============================================================================
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MACOS_DIR="$ROOT_DIR/platforms/macos/ClipBridgeShell_macOS"
XCODE_PROJECT="$MACOS_DIR/ClipBridge/ClipBridge.xcodeproj"
PBXPROJ="$XCODE_PROJECT/project.pbxproj"
SCHEME="ClipBridge"
DYLIB_NAME="libcore_ffi_macos.dylib"

XCODE_VERSION_REQUIRED="26.0.1"
SDK_VERSION_REQUIRED="26.0"
DEPLOYMENT_TARGET_REQUIRED="15.7"
RUST_TARGET="aarch64-apple-darwin"

usage() {
	cat <<'EOF'
Usage: bash scripts/ci/macos.sh <outdir>

Requires macOS 15+ on arm64 with Xcode 26.0.1 (SDK 26.0) selected through
DEVELOPER_DIR / xcode-select, plus the Rust aarch64-apple-darwin target.

Builds the current-checkout dylib, the real Xcode `ClipBridge` scheme
(Release, ARCHS=arm64), verifies the embedded library and runs a dlopen /
cb_get_ffi_version probe. <outdir> receives the app zip, dylib, reports,
toolchain details and checksums.

Exit status is non-zero on any failure. Running this script on a non-macOS host
is a hard failure by design: syntax checks are not build evidence.
EOF
}

log() { printf '%s\n' "$*"; }
step() { printf '\n=== %s ===\n' "$*"; }
die() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}
require_cmd() {
	command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

# Delete a generated path only after proving it is the expected direct child of
# the expected directory, is not a symlink/reparse point, and is not a root.
safe_rm_rf() {
	local target="$1" parent="$2" expected_name="$3"
	[[ -n "$target" ]] || die "refusing to delete an empty path"
	[[ -d "$parent" ]] || die "delete guard: parent directory does not exist: $parent"
	local parent_abs target_parent
	parent_abs="$(cd "$parent" && pwd -P)"
	target_parent="$(cd "$(dirname "$target")" && pwd -P)" ||
		die "delete guard: cannot resolve parent of $target"
	[[ "$(basename "$target")" == "$expected_name" ]] ||
		die "delete guard: unexpected basename for $target (want $expected_name)"
	[[ "$target_parent" == "$parent_abs" ]] ||
		die "delete guard: $target is not a direct child of $parent_abs"
	[[ ! -L "$target" ]] || die "delete guard: refusing to delete symlink $target"
	rm -rf -- "$target"
}

trap 'printf "FAIL: macOS CI entry failed at line %s: %s\n" "$LINENO" "${BASH_COMMAND:-?}" >&2' ERR

if [[ $# -eq 1 && ( "$1" == "-h" || "$1" == "--help" ) ]]; then
	usage
	exit 0
fi
if [[ $# -ne 1 ]]; then
	usage >&2
	die "expected exactly one argument: <outdir>"
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
	die "this entry requires the macOS runner (macos-15, arm64); host is $(uname -s). Do not report a local syntax check as a macOS build."
fi
if [[ "$(uname -m)" != "arm64" ]]; then
	die "macOS runner must be arm64 (macos-15); found $(uname -m)"
fi

if [[ -e "$1" && ! -d "$1" ]]; then
	die "outdir exists and is not a directory: $1"
fi
mkdir -p "$1"
OUT_DIR="$(cd "$1" && pwd -P)"
[[ -n "$OUT_DIR" ]] || die "cannot resolve outdir"
[[ "$OUT_DIR" != "/" ]] || die "refusing to use / as outdir"

cd "$ROOT_DIR"

# ---------------------------------------------------------------------------
# 1. Toolchain contract.
# ---------------------------------------------------------------------------
step "Validate macOS toolchain contract"

for command_name in xcodebuild xcrun clang lipo otool codesign ditto shasum cargo rustup plutil python3; do
	require_cmd "$command_name"
done
[[ -f "$PBXPROJ" ]] || die "Xcode project not found: $PBXPROJ"
# The build script is invoked through bash; a clean checkout has no execute bit.
[[ -f "$ROOT_DIR/scripts/build-macos-ffi.sh" ]] || die "missing scripts/build-macos-ffi.sh"

XCODE_VERSION_LINE="$(xcodebuild -version | sed -n '1p')"
[[ "$XCODE_VERSION_LINE" == "Xcode $XCODE_VERSION_REQUIRED" ]] ||
	die "Xcode $XCODE_VERSION_REQUIRED required (found: ${XCODE_VERSION_LINE:-unknown}). Point DEVELOPER_DIR/xcode-select at /Applications/Xcode_26.0.1.app/Contents/Developer; do not fall back to another Xcode."

SDK_VERSION="$(xcrun --sdk macosx --show-sdk-version)"
[[ "$SDK_VERSION" == "$SDK_VERSION_REQUIRED" ]] ||
	die "macOS SDK $SDK_VERSION_REQUIRED required, found: $SDK_VERSION"
SDK_PATH="$(xcrun --sdk macosx --show-sdk-path)"

MACOS_VERSION="$(sw_vers -productVersion)"
MACOS_MAJOR="${MACOS_VERSION%%.*}"
[[ "$MACOS_MAJOR" =~ ^[0-9]+$ ]] || die "cannot parse macOS version: $MACOS_VERSION"
((MACOS_MAJOR >= 15)) || die "macOS 15+ runner required, found: $MACOS_VERSION"

grep -q "MACOSX_DEPLOYMENT_TARGET = $DEPLOYMENT_TARGET_REQUIRED;" "$PBXPROJ" ||
	die "project deployment target changed; expected MACOSX_DEPLOYMENT_TARGET = $DEPLOYMENT_TARGET_REQUIRED"

INSTALLED_TARGETS="$(rustup target list --installed)"
grep -qx "$RUST_TARGET" <<<"$INSTALLED_TARGETS" ||
	die "Rust target $RUST_TARGET is not installed (rustup target add $RUST_TARGET)"

# The shared scheme is a committed project input; an automatic/personal scheme
# is not acceptable and the SwiftPM stub is not a substitute for this target.
SCHEME_FILE="$XCODE_PROJECT/xcshareddata/xcschemes/$SCHEME.xcscheme"
[[ -f "$SCHEME_FILE" ]] ||
	die "shared scheme missing: $SCHEME_FILE (do not rely on a personal Xcode scheme or SwiftPM)"
xcodebuild -list -json -project "$XCODE_PROJECT" \
	>"$OUT_DIR/xcodebuild-list.json" 2>"$OUT_DIR/xcodebuild-list.stderr" ||
	{
		cat "$OUT_DIR/xcodebuild-list.stderr" >&2
		die "xcodebuild -list failed (see $OUT_DIR/xcodebuild-list.json)"
	}
python3 "$ROOT_DIR/scripts/ci/macos/validate_xcodebuild_list.py" \
	"$OUT_DIR/xcodebuild-list.json" "$SCHEME"

# The embed phase must fail on a missing dylib and must accept the fresh one.
grep -q 'CLIPBRIDGE_CORE_DYLIB' "$PBXPROJ" ||
	die "Embed Core FFI phase does not honor CLIPBRIDGE_CORE_DYLIB; CI would not prove which library was embedded"
if grep -q 'warning: Missing' "$PBXPROJ"; then
	die "Embed Core FFI phase still downgrades a missing dylib to a warning; it must fail the build"
fi

log "macOS $MACOS_VERSION ($(uname -m))"
log "$XCODE_VERSION_LINE"
log "SDK: $SDK_VERSION at $SDK_PATH"
log "Rust target: $RUST_TARGET"

# ---------------------------------------------------------------------------
# 2. Build this checkout's dylib (Release, arm64).
# ---------------------------------------------------------------------------
step "Build current-checkout dylib (Release, $RUST_TARGET)"
DYLIB_DIR="$OUT_DIR/dylib"
bash "$ROOT_DIR/scripts/build-macos-ffi.sh" --release --target "$RUST_TARGET" --out-dir "$DYLIB_DIR"
FRESH_DYLIB="$DYLIB_DIR/$DYLIB_NAME"
[[ -f "$FRESH_DYLIB" ]] || die "build-macos-ffi.sh produced no dylib at $FRESH_DYLIB"
# Sign with the exact local ad-hoc identity the embed phase uses. The embed
# phase preserves an existing ad-hoc signature, so the embedded file stays
# byte-identical and the provenance hash below is a real content check.
/usr/bin/codesign --force --sign - --timestamp=none "$FRESH_DYLIB"
FRESH_DYLIB_ARCHS="$(lipo -archs "$FRESH_DYLIB")"
[[ "$FRESH_DYLIB_ARCHS" == "arm64" ]] ||
	die "freshly built dylib is not arm64-only (lipo -archs: $FRESH_DYLIB_ARCHS)"
FRESH_DYLIB_SHA="$(shasum -a 256 "$FRESH_DYLIB" | awk '{print $1}')"
log "fresh dylib: $FRESH_DYLIB (sha256 $FRESH_DYLIB_SHA)"

# ---------------------------------------------------------------------------
# 3. Real Xcode build (target/scheme ClipBridge, Release, arm64).
# ---------------------------------------------------------------------------
step "xcodebuild scheme $SCHEME (Release, ARCHS=arm64)"
DERIVED_DATA="$OUT_DIR/DerivedData"
safe_rm_rf "$DERIVED_DATA" "$OUT_DIR" "DerivedData"
export CLIPBRIDGE_CORE_DYLIB="$FRESH_DYLIB"

if ! xcodebuild \
	-project "$XCODE_PROJECT" \
	-scheme "$SCHEME" \
	-configuration Release \
	-destination 'generic/platform=macOS' \
	-derivedDataPath "$DERIVED_DATA" \
	ARCHS=arm64 \
	ONLY_ACTIVE_ARCH=NO \
	CODE_SIGN_IDENTITY=- \
	CODE_SIGN_STYLE=Manual \
	build 2>&1 | tee "$OUT_DIR/xcodebuild.log"; then
	# pipefail propagates xcodebuild's exit status through tee.
	die "xcodebuild failed (see $OUT_DIR/xcodebuild.log)"
fi

APP="$DERIVED_DATA/Build/Products/Release/ClipBridge.app"
APP_EXE="$APP/Contents/MacOS/ClipBridge"
EMBEDDED_DYLIB="$APP/Contents/Frameworks/$DYLIB_NAME"
[[ -d "$APP" ]] || die "Xcode produced no ClipBridge.app at $APP"
[[ -f "$APP_EXE" ]] || die "app executable missing: $APP_EXE"
[[ -f "$EMBEDDED_DYLIB" ]] ||
	die "app bundle does not contain Frameworks/$DYLIB_NAME; the embed phase did not run or failed silently"

# ---------------------------------------------------------------------------
# 4. Architecture, embedded-library identity and dependency footprint.
# ---------------------------------------------------------------------------
step "Verify bundle architecture, embedded dylib and dependencies"

APP_ARCHS="$(lipo -archs "$APP_EXE")"
[[ "$APP_ARCHS" == "arm64" ]] || die "app executable is not arm64-only (lipo -archs: $APP_ARCHS)"
APP_FILE_INFO="$(file "$APP_EXE")"
grep -q 'Mach-O 64-bit executable arm64' <<<"$APP_FILE_INFO" ||
	die "app executable is not a Mach-O arm64 executable: $APP_FILE_INFO"

EMBEDDED_ARCHS="$(lipo -archs "$EMBEDDED_DYLIB")"
[[ "$EMBEDDED_ARCHS" == "arm64" ]] ||
	die "embedded dylib is not arm64-only (lipo -archs: $EMBEDDED_ARCHS)"
DYLIB_FILE_INFO="$(file "$EMBEDDED_DYLIB")"
grep -q 'Mach-O 64-bit dynamically linked shared library arm64' <<<"$DYLIB_FILE_INFO" ||
	die "embedded dylib is not a Mach-O arm64 dylib: $DYLIB_FILE_INFO"

EMBEDDED_SHA="$(shasum -a 256 "$EMBEDDED_DYLIB" | awk '{print $1}')"
bash "$ROOT_DIR/scripts/ci/macos/verify_embedded_dylib.sh" "$FRESH_DYLIB" "$EMBEDDED_DYLIB"

# otool -L includes the dylib's own LC_ID_DYLIB as its first load entry. Remove
# exactly that self identity (from otool -D) and require every remaining load to
# be a system library; arbitrary @rpath dependencies are not accepted.
SELF_ID="$(otool -D "$EMBEDDED_DYLIB" | awk 'NR==2 {gsub(/[[:space:]]/, ""); print}')"
otool -L "$EMBEDDED_DYLIB" >"$OUT_DIR/otool-dylib.txt"
while IFS= read -r dependency; do
	[[ -n "$dependency" ]] || continue
	if [[ -n "$SELF_ID" && "$dependency" == "$SELF_ID" ]]; then
		continue
	fi
	case "$dependency" in
	/usr/lib/* | /System/Library/*)
		;;
	*)
		die "embedded dylib depends on a non-system library, so it is not self-contained: $dependency (self id: ${SELF_ID:-none})"
		;;
	esac
done < <(otool -L "$EMBEDDED_DYLIB" | tail -n +2 | awk '{print $1}')

otool -l "$APP_EXE" >"$OUT_DIR/otool-app.txt"
grep -Fq '@executable_path/../Frameworks' "$OUT_DIR/otool-app.txt" ||
	die "app executable has no @executable_path/../Frameworks rpath; the embedded dylib would not be findable"

# ---------------------------------------------------------------------------
# 5. Local ad-hoc signature only (no Developer ID, no notarization).
# ---------------------------------------------------------------------------
step "Verify local ad-hoc code signatures"
codesign --verify --strict "$APP" || die "app bundle signature verification failed"
codesign -dv "$APP" >"$OUT_DIR/codesign-app.txt" 2>&1 || die "app has no usable code signature"
grep -q 'Signature=adhoc' "$OUT_DIR/codesign-app.txt" ||
	die "app is not ad-hoc signed; CI must not consume Developer ID identities"
if grep -q 'Authority=Developer ID' "$OUT_DIR/codesign-app.txt"; then
	die "app is Developer ID signed; CI scope is local ad-hoc signing only"
fi

codesign --verify --strict "$EMBEDDED_DYLIB" || die "embedded dylib signature verification failed"
codesign -dv "$EMBEDDED_DYLIB" >"$OUT_DIR/codesign-dylib.txt" 2>&1 ||
	die "embedded dylib has no usable code signature"
grep -q 'Signature=adhoc' "$OUT_DIR/codesign-dylib.txt" ||
	die "embedded dylib is not ad-hoc signed (the embed phase must ad-hoc sign it)"

# ---------------------------------------------------------------------------
# 6. Dynamic load + ABI probe (no app launch, no cb_init, no user data).
# ---------------------------------------------------------------------------
step "Dynamic load + cb_get_ffi_version compatibility probe"
PROBE_SOURCE="$ROOT_DIR/scripts/ci/macos/probe_ffi.c"
[[ -f "$PROBE_SOURCE" ]] || die "probe source missing: $PROBE_SOURCE"
PROBE_BIN="$OUT_DIR/probe_ffi"
xcrun --sdk macosx clang -arch arm64 -Os -Wall -Wextra -Werror -o "$PROBE_BIN" "$PROBE_SOURCE"
PROBE_OUTPUT="$("$PROBE_BIN" "$EMBEDDED_DYLIB" 2>&1)" || {
	printf '%s\n' "$PROBE_OUTPUT" >&2
	die "dlopen/cb_get_ffi_version probe failed"
}
printf '%s\n' "$PROBE_OUTPUT" | tee "$OUT_DIR/probe_ffi.log"
case "$PROBE_OUTPUT" in
cb_get_ffi_version=1.*) ;;
*) die "probe returned an unexpected FFI ABI: $PROBE_OUTPUT" ;;
esac

# ---------------------------------------------------------------------------
# 7. Publish artifacts + evidence.
# ---------------------------------------------------------------------------
step "Write artifacts and evidence to $OUT_DIR"
ditto -c -k --keepParent "$APP" "$OUT_DIR/ClipBridge-app-release-arm64.zip"
cp "$FRESH_DYLIB" "$OUT_DIR/$DYLIB_NAME"

{
	echo "platform: macos"
	echo "runner: $(uname -s) $(uname -m)"
	echo "date_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
	echo "macos: $MACOS_VERSION"
	echo "xcode: $XCODE_VERSION_LINE"
	echo "sdk: $SDK_VERSION"
	echo "sdk_path: $SDK_PATH"
	echo "scheme: $SCHEME"
	echo "configuration: Release"
	echo "archs: $APP_ARCHS"
	echo "deployment_target: $DEPLOYMENT_TARGET_REQUIRED"
	echo "rust_target: $RUST_TARGET"
	echo "rustc: $(rustc -V)"
	echo "cargo: $(cargo -V)"
	echo "dylib_sha256: $EMBEDDED_SHA"
	echo "evidence_scope: current-checkout Xcode app build + embed + dlopen ABI probe; no UI launch, no Developer ID/notarization/DMG"
} >"$OUT_DIR/toolchain.txt"

(
	cd "$OUT_DIR"
	shasum -a 256 ClipBridge-app-release-arm64.zip "$DYLIB_NAME" >"checksums.txt"
)

step "macOS CI entry passed"
log "App bundle: $OUT_DIR/ClipBridge-app-release-arm64.zip"
log "Dylib: $OUT_DIR/$DYLIB_NAME"
log "Probe: $OUT_DIR/probe_ffi.log"
log "Note: this is current-runner evidence, not a universal build or a distributable/notarized artifact."
