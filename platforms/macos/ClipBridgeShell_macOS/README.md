# ClipBridgeShell macOS

This is the initial macOS shell scaffold built with SwiftUI, with placeholders for Rust FFI integration.

## Prerequisites

- Xcode 26+
- Swift 6.2+
- Rust stable toolchain
- Installed targets:
  - `aarch64-apple-darwin`
  - `x86_64-apple-darwin` (recommended for universal builds)

## Quick Start

From repository root:

```bash
./scripts/check-macos-shell-env.sh
./scripts/build-macos-ffi.sh
cd platforms/macos/ClipBridgeShell_macOS
swift build
swift run
```

## Current Status

- SwiftUI app scaffold: done
- CoreHost state machine scaffold: done
- Rust FFI binding implementation in `CoreBridge`: pending
- Event pump / stores / menu bar integration: pending

