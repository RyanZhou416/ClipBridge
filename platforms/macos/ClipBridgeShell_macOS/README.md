# ClipBridgeShell macOS

This directory contains two Swift entry points with different status. Only the Xcode target loads the real Rust bridge.

## Working path: Xcode target (dlopen bridge)

- Open `ClipBridge/ClipBridge.xcodeproj` and run the `ClipBridge` scheme.
- `ClipBridge/ClipBridge/CoreBridge.swift` loads `libcore_ffi_macos.dylib` at runtime with `dlopen` and resolves `cb_init`, `cb_get_status`, `cb_shutdown`, `cb_free_string`, and `cb_get_ffi_version`. The dylib handle stays open for the process lifetime.
- The app target's `Embed Core FFI` build phase copies `Native/libcore_ffi_macos.dylib` into the app's `Frameworks` directory and ad-hoc codesigns it. If the dylib is missing, the phase prints a warning and skips the copy.
- At runtime the bridge checks `CLIPBRIDGE_CORE_LIB` first, then paths relative to the current directory and the app bundle; see `dylibCandidates()` for the exact order.

## Stub path: SwiftPM package

- `swift build` and `swift run` from this directory build the `ClipBridgeShell` executable from `Sources/ClipBridgeShell`, not the Xcode app.
- `Sources/ClipBridgeShell/Interop/CoreBridge.swift` is a stub: `initialize` and `shutdown` throw `CoreBridgeError.notImplemented`, and it never loads a dylib.
- This path is a scaffold, not a runnable shell.

## Prerequisites

- Xcode 26+
- Swift 6.2+
- Rust stable toolchain
- Installed targets:
  - `aarch64-apple-darwin`
  - `x86_64-apple-darwin` (recommended for universal builds)

## Build and Run

From repository root:

```bash
./scripts/check-macos-shell-env.sh
./scripts/build-macos-ffi.sh
open platforms/macos/ClipBridgeShell_macOS/ClipBridge/ClipBridge.xcodeproj
```

Then run the `ClipBridge` scheme from Xcode. Before launching, provide account credentials through `clipbridge.account_uid` / `clipbridge.account_password` in UserDefaults, Keychain, or the `CLIPBRIDGE_ACCOUNT_UID` / `CLIPBRIDGE_ACCOUNT_PASSWORD` environment variables; otherwise `CoreHostService` stays `Degraded` with a missing-credentials error.

## Current Status

- Xcode target: SwiftUI app, `CoreHostService`, and the `dlopen` bridge are implemented; status queries, shutdown, and event display are wired.
- Event pump / stores / menu bar integration: pending.
- SwiftPM package: stub only (`notImplemented`); it is not the working path.
