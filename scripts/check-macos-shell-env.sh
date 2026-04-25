#!/usr/bin/env bash
set -euo pipefail

echo "== macOS =="
sw_vers

echo
echo "== Xcode =="
xcode-select -p
xcodebuild -version
swift --version
xcrun --sdk macosx --show-sdk-path

echo
echo "== Rust =="
rustc -V
cargo -V
rustup show active-toolchain
echo "Installed targets:"
rustup target list --installed

echo
echo "Environment check passed."

