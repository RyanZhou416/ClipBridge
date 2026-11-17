#!/usr/bin/env bash
# =============================================================================
# ClipBridge Android CI entry point.
#
#   bash scripts/ci/android.sh <outdir>
#
# Extracted from the long shell steps of .github/workflows/ci.yml without
# changing their contract: build the two Rust cdylibs first (explicit
# arm64-v8a + x86_64 contract), then run the Gradle wrapper's
# :app:assembleDebug, then assert the APK really contains both fresh libraries.
#
# Additions required by docs/ci-architecture.md:
#   * toolchain versions are validated instead of assumed (JDK 21, Gradle 8.13,
#     SDK 36, build-tools 36.0.0, NDK 26.3.11579264, CMake 3.22.1);
#   * the packaged ABI set is passed explicitly (-Pclipbridge.android.abis=…)
#     so a library that happens to be missing from disk can never silently
#     shrink the verification set;
#   * Gradle dependency lock inputs are required and checked for the direct plus
#     transitive modules and for debug/release configurations;
#   * a pinned OSV-Scanner reviews exactly those lockfiles and blocks
#     HIGH/CRITICAL/unknown findings.
#
# The script never fabricates lock state or vulnerability data: missing inputs
# fail. It does not run on a device/emulator, so a pass means "build + static
# package contract", not device runtime.
#
# Exit code: 0 only when every step passed; any failure exits non-zero.
# =============================================================================
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ANDROID_DIR="$ROOT_DIR/platforms/android/ClipBridgeShellAndroid"
APP_DIR="$ANDROID_DIR/app"
GRADLEW="$ANDROID_DIR/gradlew"
WRAPPER_PROPS="$ANDROID_DIR/gradle/wrapper/gradle-wrapper.properties"

# Fixed toolchain contract (mirrors docs/ci-architecture.md section 1.3 and the
# project configuration; the workflow prepares these exact versions).
GRADLE_VERSION="8.13"
SDK_PLATFORM="android-36"
BUILD_TOOLS_VERSION="36.0.0"
NDK_VERSION="26.3.11579264"
CMAKE_VERSION="3.22.1"
ANDROID_API_LEVEL=26

# Explicit ABI contract for CI. Order matters only for deterministic output.
ABI_LIST=(arm64-v8a x86_64)
declare -A ABI_TRIPLE=(
	[arm64-v8a]="aarch64-linux-android"
	[x86_64]="x86_64-linux-android"
)

# Pinned OSV-Scanner. The SHA-256 is the digest published on the v2.7.1 GitHub
# release for osv-scanner_linux_amd64; we re-check it for every download and
# every preinstalled binary we use.
OSV_VERSION="2.7.1"
OSV_LINUX_AMD64_SHA256="fb7dab19a8bd9bbe5c3189f09d4b7ee1820c9553a23bac202648a56efd2f6ee8"
OSV_LINUX_AMD64_URL="https://github.com/google/osv-scanner/releases/download/v${OSV_VERSION}/osv-scanner_linux_amd64"

usage() {
	cat <<'EOF'
Usage: bash scripts/ci/android.sh <outdir>

Builds the two ABI-specific Rust cdylibs, runs :app:assembleDebug with an
explicit ABI contract, verifies the APK carries this build's libraries, and
runs the pinned OSV-Scanner against the committed Gradle lockfiles.

<outdir> is created if needed and receives the APK, the two .so files, the
scanner reports, a toolchain report and checksums.

Required environment: JAVA_HOME (JDK 21), ANDROID_SDK_ROOT or ANDROID_HOME
(pointing at SDK 36 + build-tools 36.0.0 + NDK 26.3.11579264 + CMake 3.22.1).
Optional: OSV_SCANNER_BIN to reuse an already installed osv-scanner v2.7.1.

Exit status is non-zero on any failure, missing input or policy violation.
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
normalize_path() {
	python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"
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

trap 'printf "FAIL: android CI entry failed at line %s: %s\n" "$LINENO" "${BASH_COMMAND:-?}" >&2' ERR

if [[ $# -eq 1 && ( "$1" == "-h" || "$1" == "--help" ) ]]; then
	usage
	exit 0
fi
if [[ $# -ne 1 ]]; then
	usage >&2
	die "expected exactly one argument: <outdir>"
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
# 1. Toolchain contract: fail loudly instead of silently using another version.
# ---------------------------------------------------------------------------
step "Validate Android toolchain contract"

require_cmd cargo
require_cmd rustup
require_cmd python3
require_cmd sha256sum
require_cmd curl
require_cmd chmod
require_cmd sed
require_cmd grep
require_cmd env

[[ -f "$GRADLEW" ]] || die "Gradle wrapper missing: $GRADLEW"
# The checked-in wrapper is mode 100644; a clean Linux checkout has no execute
# bit, so make it executable here instead of relying on a removed ci.yml step.
chmod +x "$GRADLEW" 2>/dev/null || die "cannot make the Gradle wrapper executable: $GRADLEW"
[[ -f "$WRAPPER_PROPS" ]] || die "Gradle wrapper properties missing: $WRAPPER_PROPS"
grep -Eq "gradle-${GRADLE_VERSION//./\\.}-bin\.zip" "$WRAPPER_PROPS" ||
	die "wrapper distribution is not Gradle ${GRADLE_VERSION}: $(grep -E '^distributionUrl' "$WRAPPER_PROPS" || true)"

[[ -n "${JAVA_HOME:-}" ]] || die "JAVA_HOME must point at JDK 21 (Gradle ${GRADLE_VERSION} refuses newer JDKs)"
JAVA_BIN="$JAVA_HOME/bin/java"
[[ -x "$JAVA_BIN" ]] || die "JAVA_HOME/bin/java is not executable: $JAVA_BIN"
JAVA_VERSION_LINE="$("$JAVA_BIN" -version 2>&1 | sed -n '1p')"
JAVA_MAJOR="$(printf '%s\n' "$JAVA_VERSION_LINE" | sed -E 's/.*version "([0-9]+).*/\1/')"
[[ "$JAVA_MAJOR" == "21" ]] || die "JDK 21 required, found: ${JAVA_VERSION_LINE:-unknown} ($JAVA_BIN)"

SDK_ROOT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
[[ -n "$SDK_ROOT" ]] || die "ANDROID_SDK_ROOT (or ANDROID_HOME) must point at the Android SDK"
[[ -d "$SDK_ROOT" ]] || die "Android SDK directory not found: $SDK_ROOT"
SDK_ROOT="$(cd "$SDK_ROOT" && pwd -P)"

# Toolchain provisioning belongs to the workflow (setup-java/setup-android plus
# the pinned sdkmanager install step). This entry only validates the exact
# versions; it never downloads SDK components or silently substitutes another
# version.
[[ -f "$SDK_ROOT/platforms/$SDK_PLATFORM/android.jar" ]] ||
	die "SDK platform $SDK_PLATFORM is missing; install \"platforms;$SDK_PLATFORM\""
[[ -d "$SDK_ROOT/build-tools/$BUILD_TOOLS_VERSION" ]] ||
	die "build-tools $BUILD_TOOLS_VERSION is missing; install \"build-tools;$BUILD_TOOLS_VERSION\""

NDK_DIR="$SDK_ROOT/ndk/$NDK_VERSION"
[[ -f "$NDK_DIR/source.properties" ]] || die "NDK $NDK_VERSION is missing; install \"ndk;$NDK_VERSION\""
grep -Eq "^Pkg\\.Revision[[:space:]]*=[[:space:]]*${NDK_VERSION}[[:space:]]*$" "$NDK_DIR/source.properties" ||
	die "NDK revision mismatch in $NDK_DIR/source.properties (want $NDK_VERSION)"

CMAKE_BIN="$SDK_ROOT/cmake/$CMAKE_VERSION/bin/cmake"
[[ -x "$CMAKE_BIN" ]] || die "CMake $CMAKE_VERSION is missing; install \"cmake;$CMAKE_VERSION\""
CMAKE_VERSION_LINE="$("$CMAKE_BIN" --version | sed -n '1p')"
case "$CMAKE_VERSION_LINE" in
*"$CMAKE_VERSION"*) ;;
*) die "CMake version mismatch: $CMAKE_VERSION_LINE (want $CMAKE_VERSION)" ;;
esac
grep -Eq "cmake_minimum_required\(VERSION[[:space:]]+${CMAKE_VERSION//./\\.}\)" \
	"$APP_DIR/src/main/cpp/CMakeLists.txt" ||
	die "app/src/main/cpp/CMakeLists.txt does not require CMake $CMAKE_VERSION"

INSTALLED_TARGETS="$(rustup target list --installed)"
for abi in "${ABI_LIST[@]}"; do
	triple="${ABI_TRIPLE[$abi]}"
	grep -qx "$triple" <<<"$INSTALLED_TARGETS" || die "Rust target $triple is not installed (rustup target add $triple)"
done

# app/build.gradle.kts reads the repository target/ directory directly, so a
# CARGO_TARGET_DIR pointing elsewhere would silently break the ABI copy.
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	CARGO_TARGET_ABS="$(normalize_path "$CARGO_TARGET_DIR")"
	REPO_TARGET_ABS="$(normalize_path "$ROOT_DIR/target")"
	[[ "$CARGO_TARGET_ABS" == "$REPO_TARGET_ABS" ]] ||
		die "CARGO_TARGET_DIR must be unset or $ROOT_DIR/target (app/build.gradle.kts reads that path)"
fi
[[ -z "${CARGO_BUILD_TARGET:-}" ]] ||
	die "CARGO_BUILD_TARGET is not supported here: it changes target/<triple> layout expected by Gradle"

# The Gradle buildscript and app dependency locks are committed inputs.
ROOT_LOCK="$ANDROID_DIR/buildscript-gradle.lockfile"
APP_LOCK="$APP_DIR/gradle.lockfile"
AGP_CATALOG_VERSION="$(sed -nE 's/^agp[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$ANDROID_DIR/gradle/libs.versions.toml" | head -n 1)"
AGP_BUILDSCRIPT_VERSION="$(sed -nE 's/.*com\.android\.tools\.build:gradle:([0-9][0-9.]*).*/\1/p' "$ANDROID_DIR/build.gradle.kts" | head -n 1)"
[[ -n "$AGP_CATALOG_VERSION" && -n "$AGP_BUILDSCRIPT_VERSION" ]] ||
	die "cannot read the AGP version from libs.versions.toml / build.gradle.kts"
[[ "$AGP_CATALOG_VERSION" == "$AGP_BUILDSCRIPT_VERSION" ]] ||
	die "AGP version drift: libs.versions.toml=$AGP_CATALOG_VERSION buildscript=$AGP_BUILDSCRIPT_VERSION"

log "$JAVA_VERSION_LINE"
log "Gradle wrapper: ${GRADLE_VERSION} ($WRAPPER_PROPS)"
log "Android SDK: $SDK_ROOT ($SDK_PLATFORM, build-tools $BUILD_TOOLS_VERSION)"
log "NDK: $NDK_VERSION  CMake: $CMAKE_VERSION_LINE"
log "Rust targets: ${ABI_TRIPLE[arm64-v8a]}, ${ABI_TRIPLE[x86_64]}"

# ---------------------------------------------------------------------------
# 2. Cross-language shell contracts (unchanged gate extracted from ci.yml).
# ---------------------------------------------------------------------------
step "Android shell contracts (AIDL order, init keys, FFI request shape)"
bash "$ROOT_DIR/scripts/check-android-shell-contracts.sh"

# ---------------------------------------------------------------------------
# 3. Require the committed Gradle lock state before any build.
# ---------------------------------------------------------------------------
step "Gradle dependency lock inputs"

check_lock_file() {
	local file="$1" description="$2" min_modules="$3"
	[[ -f "$file" ]] ||
		die "missing $description lockfile: $file. Generate it with JDK 21 + Android SDK 36 and commit it:
  (cd \"$ANDROID_DIR\" && ./gradlew --write-locks help)
  (cd \"$ANDROID_DIR\" && ./gradlew :app:resolveAndLockAll --write-locks)
This entry refuses to fabricate lock content."
	local modules
	modules="$(grep -Ec '^[^#[:space:]=]+:[^:]+:[^=]+=' "$file" || true)"
	((modules >= min_modules)) ||
		die "$description lockfile looks empty/truncated ($modules modules): $file"
}

check_lock_file "$ROOT_LOCK" "Gradle buildscript" 10
check_lock_file "$APP_LOCK" "Gradle app" 10

grep -Eq "^com\\.android\\.tools\\.build:gradle:${AGP_BUILDSCRIPT_VERSION//./\\.}=" "$ROOT_LOCK" ||
	die "buildscript lockfile does not pin com.android.tools.build:gradle:$AGP_BUILDSCRIPT_VERSION"

# Direct dependencies declared in app/build.gradle.kts (versions may change; the
# module coordinate is the stable part).
for coordinate in \
	"net.java.dev.jna:jna" \
	"dev.rikka.shizuku:api" \
	"dev.rikka.shizuku:provider" \
	"org.lsposed.hiddenapibypass:hiddenapibypass" \
	"androidx.appcompat:appcompat" \
	"com.google.android.material:material" \
	"androidx.constraintlayout:constraintlayout"; do
	grep -Eq "^${coordinate//./\\.}:" "$APP_LOCK" ||
		die "app lockfile is missing direct dependency $coordinate; the lock graph is incomplete"
done
# A transitive module proves the resolved graph (not just declarations) was locked.
grep -Eq "^androidx\\.core:core:" "$APP_LOCK" ||
	die "app lockfile has no transitive androidx.core:core entry; transitive locking is not in effect"
for configuration in debugRuntimeClasspath releaseRuntimeClasspath; do
	grep -Eq "=${configuration}(,|$)|,${configuration}(,|$)|${configuration}=" "$APP_LOCK" ||
		die "app lockfile has no $configuration entries; both debug and release graphs must be locked"
done

log "buildscript lock: $ROOT_LOCK"
log "app lock: $APP_LOCK (debug + release + transitive modules present)"

# CI verifies locks read-only. Record their content before Gradle and fail if a
# build ever rewrites them (e.g. an accidental --write-locks or plugin mutation).
LOCK_ROOT_SHA_BEFORE="$(sha256sum "$ROOT_LOCK" | cut -d' ' -f1)"
LOCK_APP_SHA_BEFORE="$(sha256sum "$APP_LOCK" | cut -d' ' -f1)"

# ---------------------------------------------------------------------------
# 4. Build the two Rust cdylibs for the explicit ABI contract.
# ---------------------------------------------------------------------------
step "Build Rust cdylibs: ${ABI_LIST[*]} (Release, --locked)"

HOST_OS="$(uname -s)"
case "$HOST_OS" in
Linux) NDK_HOST_TAG="linux-x86_64" ;;
Darwin) NDK_HOST_TAG="darwin-x86_64" ;;
*) die "unsupported host OS for NDK toolchain: $HOST_OS (CI uses ubuntu-24.04)" ;;
esac
NDK_TOOLCHAIN="$NDK_DIR/toolchains/llvm/prebuilt/$NDK_HOST_TAG/bin"
[[ -d "$NDK_TOOLCHAIN" ]] || die "NDK LLVM toolchain not found: $NDK_TOOLCHAIN"

export ANDROID_NDK_ROOT="$NDK_DIR"
export ANDROID_NDK_HOME="$NDK_DIR"

# cc-rs (used by ring and any bundled C code) probes `<VAR>_<target>` first and
# `<VAR>_<target_with_underscores>` second; rustc selects the linker through
# `CARGO_TARGET_<TARGET>_LINKER`. The underscore spellings are valid identifiers
# and are exported. The hyphenated spellings matter for cc-rs/ring too, but bash
# cannot export a name containing '-' ("not a valid identifier", hosted run
# 44115863192 failed on exactly that), so they are passed per cargo invocation
# through env(1), which forwards them to the child environment verbatim.
# Sets CC_TARGET_ENV for that purpose.
cargo_env_for_abi() {
	local triple="$1"
	local underscore_triple="${triple//-/_}"
	local upper_triple cc_bin
	upper_triple="$(printf '%s' "$underscore_triple" | tr '[:lower:]' '[:upper:]')"
	cc_bin="$NDK_TOOLCHAIN/${triple}${ANDROID_API_LEVEL}-clang"
	export "CC_${underscore_triple}=$cc_bin"
	export "AR_${underscore_triple}=$NDK_TOOLCHAIN/llvm-ar"
	export "RANLIB_${underscore_triple}=$NDK_TOOLCHAIN/llvm-ranlib"
	export "CARGO_TARGET_${upper_triple}_LINKER=$cc_bin"
	CC_TARGET_ENV=(
		"CC_${triple}=$cc_bin"
		"AR_${triple}=$NDK_TOOLCHAIN/llvm-ar"
		"RANLIB_${triple}=$NDK_TOOLCHAIN/llvm-ranlib"
	)
}

for abi in "${ABI_LIST[@]}"; do
	triple="${ABI_TRIPLE[$abi]}"
	so_path="$ROOT_DIR/target/$triple/release/libcore_ffi_android.so"
	rm -f "$so_path"
	cargo_env_for_abi "$triple"
	log "cargo build -p core-ffi-android --release --locked --target $triple"
	env "${CC_TARGET_ENV[@]}" \
		cargo build -p core-ffi-android --release --locked --target "$triple"
	[[ -f "$so_path" ]] || die "cargo produced no library at $so_path"
	log "  -> $so_path ($(sha256sum "$so_path" | cut -d' ' -f1))"
done

# Recreate jniLibs from scratch so no stale ABI library can survive a failed build.
JNI_LIBS="$APP_DIR/src/main/jniLibs"
safe_rm_rf "$JNI_LIBS" "$APP_DIR/src/main" "jniLibs"
for abi in "${ABI_LIST[@]}"; do
	mkdir -p "$JNI_LIBS/$abi"
	cp "$ROOT_DIR/target/${ABI_TRIPLE[$abi]}/release/libcore_ffi_android.so" "$JNI_LIBS/$abi/libcore_ffi_android.so"
done
log "jniLibs refreshed for: ${ABI_LIST[*]}"

# ---------------------------------------------------------------------------
# 5. Gradle debug build with explicit ABIs.
# ---------------------------------------------------------------------------
step "Gradle :app:assembleDebug (explicit ABIs, locked dependencies)"

APK="$APP_DIR/build/outputs/apk/debug/app-debug.apk"
rm -f "$APK"
[[ -x "$GRADLEW" ]] || chmod +x "$GRADLEW"

EXPLICIT_ABIS="$(IFS=,; printf '%s' "${ABI_LIST[*]}")"
(
	cd "$ANDROID_DIR"
	./gradlew :app:assembleDebug \
		--stacktrace \
		--console=plain \
		-Pclipbridge.android.abis="$EXPLICIT_ABIS"
)
[[ -f "$APK" ]] || die "assembleDebug produced no APK at $APK"

# Lock write protection: a verification run must not update committed locks.
LOCK_ROOT_SHA_AFTER="$(sha256sum "$ROOT_LOCK" | cut -d' ' -f1)"
LOCK_APP_SHA_AFTER="$(sha256sum "$APP_LOCK" | cut -d' ' -f1)"
[[ "$LOCK_ROOT_SHA_AFTER" == "$LOCK_ROOT_SHA_BEFORE" ]] ||
	die "Gradle rewrote $ROOT_LOCK during verification; CI must only verify committed lock state"
[[ "$LOCK_APP_SHA_AFTER" == "$LOCK_APP_SHA_BEFORE" ]] ||
	die "Gradle rewrote $APP_LOCK during verification; CI must only verify committed lock state"

# ---------------------------------------------------------------------------
# 6. Verify the APK really contains both fresh, correctly targeted libraries.
# ---------------------------------------------------------------------------
step "Verify APK native libraries (both ABIs, this build)"

EXPECTED_ARGS=()
for abi in "${ABI_LIST[@]}"; do
	EXPECTED_ARGS+=(--expected "$abi=$ROOT_DIR/target/${ABI_TRIPLE[$abi]}/release/libcore_ffi_android.so")
done
python3 "$ROOT_DIR/scripts/ci/android/verify_apk.py" \
	--apk "$APK" \
	"${EXPECTED_ARGS[@]}" \
	--json-out "$OUT_DIR/apk-check.json"

# ---------------------------------------------------------------------------
# 7. Pinned OSV-Scanner over the resolved Gradle lockfiles.
# ---------------------------------------------------------------------------
step "Dependency vulnerability scan (OSV-Scanner $OSV_VERSION)"

resolve_osv_scanner() {
	if [[ -n "${OSV_SCANNER_BIN:-}" ]]; then
		[[ -x "$OSV_SCANNER_BIN" ]] || die "OSV_SCANNER_BIN is not executable: $OSV_SCANNER_BIN"
		local version_line
		version_line="$("$OSV_SCANNER_BIN" --version 2>&1 | sed -n '1p' || true)"
		[[ "$version_line" == *"$OSV_VERSION"* ]] ||
			die "OSV_SCANNER_BIN must be osv-scanner $OSV_VERSION (got: ${version_line:-unknown})"
		OSV_BIN="$OSV_SCANNER_BIN"
		return 0
	fi
	if [[ "$HOST_OS" != "Linux" ]]; then
		die "set OSV_SCANNER_BIN to a pinned osv-scanner $OSV_VERSION on $HOST_OS"
	fi
	local machine
	machine="$(uname -m)"
	[[ "$machine" == "x86_64" || "$machine" == "amd64" ]] ||
		die "no pinned osv-scanner build for $machine; set OSV_SCANNER_BIN"
	mkdir -p "$OUT_DIR/tools"
	OSV_BIN="$OUT_DIR/tools/osv-scanner_linux_amd64"
	if [[ ! -x "$OSV_BIN" ]]; then
		local download="$OSV_BIN.download"
		rm -f "$download"
		# Bounded retry for transient network failures only; a hash mismatch is final.
		curl -fsSL --retry 2 --retry-delay 3 --connect-timeout 20 -o "$download" "$OSV_LINUX_AMD64_URL" ||
			die "download failed after retries: $OSV_LINUX_AMD64_URL"
		if ! printf '%s  %s\n' "$OSV_LINUX_AMD64_SHA256" "$download" | sha256sum -c - >/dev/null; then
			rm -f "$download"
			die "OSV-Scanner SHA256 mismatch; refusing to run an unpinned scanner"
		fi
		chmod +x "$download"
		mv "$download" "$OSV_BIN"
	fi
	printf '%s  %s\n' "$OSV_LINUX_AMD64_SHA256" "$OSV_BIN" | sha256sum -c - >/dev/null ||
		die "cached OSV-Scanner no longer matches the pinned SHA256: $OSV_BIN"
}

scan_lockfile() {
	local lock="$1" label="$2"
	local report="$OUT_DIR/osv-$label.json"
	local stderr_log="$OUT_DIR/osv-$label.stderr.log"
	local summary="$OUT_DIR/osv-$label.summary.json"
	local rc=0
	log "osv-scanner scan source --lockfile $lock"
	"$OSV_BIN" scan source --lockfile "$lock" --format json --all-packages \
		>"$report" 2>"$stderr_log" || rc=$?
	if [[ $rc -ne 0 && $rc -ne 1 ]]; then
		tail -n 20 "$stderr_log" >&2 || true
		die "OSV-Scanner failed for $label (exit $rc); vulnerability data was not obtained"
	fi
	python3 "$ROOT_DIR/scripts/ci/android/osv_policy.py" \
		--report "$report" \
		--lockfile "$lock" \
		--label "$label" \
		--json-out "$summary"
}

resolve_osv_scanner
OSV_VERSION_LINE="$("$OSV_BIN" --version 2>&1 | sed -n '1p')"
log "OSV-Scanner: $OSV_VERSION_LINE"
scan_lockfile "$APP_LOCK" "app"
scan_lockfile "$ROOT_LOCK" "buildscript"

# ---------------------------------------------------------------------------
# 8. Publish outdir artifacts + evidence.
# ---------------------------------------------------------------------------
step "Write artifacts and evidence to $OUT_DIR"

cp "$APK" "$OUT_DIR/android-debug.apk"
for abi in "${ABI_LIST[@]}"; do
	cp "$ROOT_DIR/target/${ABI_TRIPLE[$abi]}/release/libcore_ffi_android.so" \
		"$OUT_DIR/libcore_ffi_android-$abi.so"
done

{
	echo "platform: android"
	echo "runner: $(uname -s) $(uname -m)"
	echo "date_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
	echo "java: $JAVA_VERSION_LINE"
	echo "java_home: $JAVA_HOME"
	echo "gradle_wrapper: $GRADLE_VERSION"
	echo "android_sdk_root: $SDK_ROOT"
	echo "sdk_platform: $SDK_PLATFORM"
	echo "build_tools: $BUILD_TOOLS_VERSION"
	echo "ndk: $NDK_VERSION"
	echo "cmake: $CMAKE_VERSION_LINE"
	echo "abi_contract: ${ABI_LIST[*]}"
	echo "apk: $APK"
	echo "rustc: $(rustc -V)"
	echo "cargo: $(cargo -V)"
	echo "rust_targets: $(rustup target list --installed | grep -E 'linux-android' | sort | tr '\n' ' ')"
	echo "osv_scanner: $OSV_VERSION_LINE"
	echo "evidence_scope: build + static APK contract only; no device/emulator runtime"
} >"$OUT_DIR/toolchain.txt"

(
	cd "$OUT_DIR"
	sha256sum android-debug.apk libcore_ffi_android-arm64-v8a.so libcore_ffi_android-x86_64.so \
		>"checksums.txt"
)

step "Android CI entry passed"
log "APK: $OUT_DIR/android-debug.apk"
log "Libraries: $OUT_DIR/libcore_ffi_android-arm64-v8a.so, $OUT_DIR/libcore_ffi_android-x86_64.so"
log "OSV reports: $OUT_DIR/osv-app.json, $OUT_DIR/osv-buildscript.json"
log "Note: this proves the debug build/package contract, not device runtime."
