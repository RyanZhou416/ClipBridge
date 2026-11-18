# AGENTS.md

## Overview
ClipBridge is a multi-language, cross-platform clipboard sync workspace.

Primary components:
- `cb_core`: Rust core library for clipboard ingest, storage, policy, networking, discovery, crypto, sessions, and runtime.
- `platforms/windows/core-ffi`: Rust CDylib exposing the core through a Windows FFI layer.
- `platforms/android/core-ffi`: Rust CDylib exposing the core through an Android FFI layer.
- `platforms/macos/core-ffi`: Rust CDylib exposing the core through a macOS FFI layer.
- `platforms/windows/ClipBridgeShell_CS`: WinUI/C# shell and MSTest test project.
- `platforms/android/ClipBridgeShellAndroid`: Android shell project.
- `platforms/macos/ClipBridgeShell_macOS`: macOS SwiftUI/Xcode shell; the Xcode target contains the `dlopen` bridge, while the SwiftPM package is a stub.

Current status from `STATUS.md`:
- One Rust core (`cb_core`) plus one shared C ABI implementation (`crates/cb_core_ffi`); the Windows, Android, and macOS `core-ffi` crates re-export the same 34-function surface.
- The Windows shell is complete and its MSTest suite runs in CI.
- The Android APK needs a cross-compiled `libcore_ffi_android.so`; the macOS Xcode target uses the `dlopen` bridge while the SwiftPM package remains a stub.

## Repository Layout
- `Cargo.toml`: Rust workspace root.
- `cb_core/src`: Main Rust implementation.
- `cb_core/src/session/tests.rs`: Session-focused Rust tests.
- `cb_core/src/bin/demo.rs`: Demo binary.
- `platforms/windows/core-ffi/src`: Windows Rust FFI bridge.
- `platforms/android/core-ffi/src`: Android Rust FFI bridge.
- `platforms/macos/core-ffi/src`: macOS Rust FFI bridge.
- `platforms/windows/include`, `platforms/android/include`, and `platforms/macos/include`: Exported C headers.
- `platforms/windows/ClipBridgeShell_CS`: Windows app, installer, and MSTest projects.
- `platforms/android/ClipBridgeShellAndroid`: Android Gradle project.
- `platforms/macos/ClipBridgeShell_macOS`: macOS Xcode app and SwiftPM scaffold.
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
- `pwsh -NoProfile -File scripts/ci/windows.ps1 -OutDir dist/ci/windows`
- This entry builds the current Rust DLL, then restores/formats/builds/tests the concrete C# projects with consistent Release/x64/win-x64/R2R settings and isolated test data.
- Do not restore/build the whole Windows solution as a C# check: it also contains WiX projects. Do not run native smoke tests against the real user data root or accept Degraded as a positive result.

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
The refactored CI uses a single `schemaVersion=2` change plan, platform entry scripts, and an always-run result check. Daily CI has 7 jobs (`changes`, `ci-config`, `rust`, `windows`, `android`, `macos`, `ci-result`) and 5 fixed plan tasks (`ci-config`, `rust`, `windows`, `android`, `macos`); there is no `packages` field or daily packaging job. Implementation and unverified/blocked items are recorded in [CI architecture](<docs/ci-architecture.md>).
- Shared Rust/ABI inputs propagate to Windows, Android, and macOS; a skipped required task is not success.
- Windows runs strict native integration and MSTest. macOS targets the real Xcode app, not the SwiftPM stub. Android builds a debug APK for JNI/ABI integration.
- Daily CI does not build Windows installers (MSI, user EXE, portable ZIP); ordinary pushes still compile the affected apps and run their tests. Installer/packaging and release-workflow changes are routed only to the lightweight `ci-config` checks, not to platform builds.
- Windows packaging/release is restricted to MSI, user EXE, and portable ZIP, built only by the explicit manual release workflow. Local package scripts remain usable only when a maintainer explicitly invokes them. MSIX/StoreUpload and winget remain local-only; do not add them or daily packaging to CI.
- Develop by committing/pushing directly to main; CI must not create branches or PRs. release-please and its metadata were removed by explicit decision; do not reintroduce them, even as a manual helper. Version/changelog edits are maintainer-authored commits.
- Public Windows releases require the explicit manual workflow. Never dispatch a release or change signing/environment settings merely to test CI.
- Preserve security gates when they reveal existing dependency vulnerabilities; do not turn them into continue-on-error to claim a green run.

When possible, run the validations that match the files you changed before finishing.

## Documentation Notes
There are several large Chinese project documents in the repository root. Do not bulk-edit them unless the task explicitly calls for documentation work there. Prefer targeted updates.

## Agent Guidelines
- Prefer focused, minimal diffs.
- Check for related headers or shell consumers when changing FFI signatures.
- Do not assume the Windows shell and Android shell expose identical capabilities.
- Use CI workflow commands as the default source of truth for verification.
- If a change affects public interfaces, mention follow-up work needed in the platform shells.
