#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "Building core-ffi-macos (debug)..."
cargo build -p core-ffi-macos

LIB_PATH="$ROOT_DIR/target/debug/libcore_ffi_macos.dylib"
if [[ ! -f "$LIB_PATH" ]]; then
  echo "Expected dylib not found: $LIB_PATH" >&2
  exit 1
fi

OUT_DIR="$ROOT_DIR/platforms/macos/ClipBridgeShell_macOS/Native"
mkdir -p "$OUT_DIR"
cp "$LIB_PATH" "$OUT_DIR/"

echo "Copied dylib to: $OUT_DIR/libcore_ffi_macos.dylib"
echo "Done."

