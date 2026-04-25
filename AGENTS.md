# AGENTS.md

## Overview
ClipBridge is a multi-language, cross-platform clipboard sync workspace.

Primary components:
- `cb_core`: Rust core library for clipboard ingest, storage, policy, networking, discovery, crypto, sessions, and runtime.
- `platforms/windows/core-ffi`: Rust CDylib exposing the core through a Windows FFI layer.
- `platforms/android/core-ffi`: Rust CDylib exposing the core through an Android FFI layer.
- `platforms/windows/ClipBridgeShell_CS`: WinUI/C# shell and MSTest test project.
- `platforms/android/ClipBridgeShellAndroid`: Android shell project.

Current status from `STATUS.md`:
- Core v1 M0 is complete.
- Windows FFI is complete.
- The next planned step is the C# P/Invoke wrapper plus event dispatch.

## Repository Layout
- `Cargo.toml`: Rust workspace root.
- `cb_core/src`: Main Rust implementation.
- `cb_core/src/session/tests.rs`: Session-focused Rust tests.
- `cb_core/src/bin/demo.rs`: Demo binary.
- `platforms/windows/core-ffi/src`: Windows Rust FFI bridge.
- `platforms/android/core-ffi/src`: Android Rust FFI bridge.
- `platforms/windows/include` and `platforms/android/include`: Exported C headers.
- `platforms/windows/ClipBridgeShell_CS`: Windows app, installer, and MSTest projects.
- `platforms/android/ClipBridgeShellAndroid`: Android Gradle project.
- `.github/workflows/ci.yml`: Canonical CI validation steps.

## Preferred Workflow
1. Read the smallest set of files needed before making changes.
2. Keep changes scoped to the layer you are modifying.
3. Do not rewrite generated or template-derived files unless the task requires it.
4. Preserve existing architecture boundaries between Rust core, Rust FFI, and platform shells.
5. Update docs when behavior or developer workflow changes materially.

## Validation
For Rust changes, prefer the same sequence used in CI:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

For dependency and policy checks when relevant:
- `cargo deny check advisories bans licenses sources`

For Windows C# shell changes:
- `dotnet restore platforms/windows/ClipBridgeShell_CS`
- `dotnet format platforms/windows/ClipBridgeShell_CS --verify-no-changes`
- `dotnet build platforms/windows/ClipBridgeShell_CS -c Release --no-restore`

For Android shell changes, use the Gradle wrapper from `platforms/android/ClipBridgeShellAndroid` and keep Rust/NDK integration changes isolated.

## Rust Conventions
- Treat `cb_core` as the source of truth for shared behavior.
- Keep FFI crates thin: translate types, map errors, and delegate logic to `cb_core`.
- Avoid unnecessary public API expansion in FFI crates.
- When changing protocol, storage, crypto, or session behavior, check for impacts across core and both platform bridges.
- Favor tests near the affected Rust module or existing session/testsupport infrastructure.

## Platform Notes
### Windows
- The WinUI shell lives under `platforms/windows/ClipBridgeShell_CS`.
- The Rust FFI crate and the C# shell are separate layers; avoid coupling UI logic into Rust.
- Existing MSTest coverage lives in `ClipBridgeShell_CS.Tests.MSTest`.

### Android
- The Android app shell lives under `platforms/android/ClipBridgeShellAndroid`.
- The Android Rust FFI crate is `platforms/android/core-ffi`.
- Be careful with changes that affect JNI/FFI boundaries or NDK setup.

## CI Notes
CI currently gates:
- Rust formatting, clippy, tests, and `cargo-deny` on Rust-related paths.
- `dotnet restore`, `dotnet format`, and `dotnet build` on Windows C# paths.

When possible, run the validations that match the files you changed before finishing.

## Documentation Notes
There are several large Chinese project documents in the repository root. Do not bulk-edit them unless the task explicitly calls for documentation work there. Prefer targeted updates.

## Agent Guidelines
- Prefer focused, minimal diffs.
- Check for related headers or shell consumers when changing FFI signatures.
- Do not assume the Windows shell and Android shell expose identical capabilities.
- Use CI workflow commands as the default source of truth for verification.
- If a change affects public interfaces, mention follow-up work needed in the platform shells.
