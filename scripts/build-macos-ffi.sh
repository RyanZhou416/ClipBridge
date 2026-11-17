#!/usr/bin/env bash
# Build the macOS core FFI cdylib from the current checkout.
#
#   ./scripts/build-macos-ffi.sh [--release|--debug] [--target <triple>] [--out-dir <dir>]
#
# Defaults preserve the developer workflow documented in
# platforms/macos/ClipBridgeShell_macOS/README.md: debug build for the host,
# copied into Native/ next to the Xcode project so the Xcode embed phase can
# find it.
#
# CI (scripts/ci/macos.sh) passes an explicit Release target and an out-dir
# outside the repository, then hands the fresh path to the Xcode build through
# CLIPBRIDGE_CORE_DYLIB. The checked-in Native/libcore_ffi_macos.dylib is never
# an authoritative build input.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

MODE="debug"
TARGET=""
OUT_DIR="$ROOT_DIR/platforms/macos/ClipBridgeShell_macOS/Native"

usage() {
	cat <<'EOF'
Usage: ./scripts/build-macos-ffi.sh [options]

  --release            build the optimized profile (CI uses this)
  --debug              build the debug profile (default)
  --target <triple>    cargo target triple, e.g. aarch64-apple-darwin
  --out-dir <dir>      copy destination (default: platforms/macos/ClipBridgeShell_macOS/Native)
  -h, --help           show this help

The build always uses --locked. A missing Cargo.lock or a target that is not
installed fails the script instead of silently building something else.
EOF
}

die() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--release)
		MODE="release"
		shift
		;;
	--debug)
		MODE="debug"
		shift
		;;
	--target)
		[[ $# -ge 2 ]] || die "--target requires a triple"
		TARGET="$2"
		shift 2
		;;
	--out-dir)
		[[ $# -ge 2 ]] || die "--out-dir requires a directory"
		OUT_DIR="$2"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		usage >&2
		die "unknown argument: $1"
		;;
	esac
done

CARGO_ARGS=(build -p core-ffi-macos --locked)
if [[ "$MODE" == "release" ]]; then
	CARGO_ARGS+=(--release)
fi
if [[ -n "$TARGET" ]]; then
	CARGO_ARGS+=(--target "$TARGET")
fi

echo "Building core-ffi-macos ($MODE${TARGET:+, target $TARGET})..."
cargo "${CARGO_ARGS[@]}"

LIB_PATH="${CARGO_TARGET_DIR:-$ROOT_DIR/target}${TARGET:+/$TARGET}/$MODE/libcore_ffi_macos.dylib"
if [[ ! -f "$LIB_PATH" ]]; then
	echo "Expected dylib not found: $LIB_PATH" >&2
	exit 1
fi

mkdir -p "$OUT_DIR"
cp -f "$LIB_PATH" "$OUT_DIR/"
COPIED="$OUT_DIR/libcore_ffi_macos.dylib"

if command -v shasum >/dev/null 2>&1; then
	echo "sha256: $(shasum -a 256 "$COPIED" | awk '{print $1}')"
fi
echo "Copied dylib to: $COPIED"
echo "Done."
