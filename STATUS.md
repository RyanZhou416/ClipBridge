# Project Status

- 单一 Rust 核心 `cb_core`；C ABI 只在 `crates/cb_core_ffi/src/exports.rs` 实现一次，三个平台 crate（Windows / Android / macOS）通过 re-export 复用同一份 34 函数导出面。
- Windows：WinUI/C# 外壳已完成（`CoreHostService`、事件泵、ViewModels、页面与诊断），MSTest 套件在 CI 中执行；构建时由 csproj 自动编译 `core-ffi-windows` 并把 `core_ffi_windows.dll` 复制到输出目录。
- Android：Java/AIDL/JNI 外壳已存在，APK 依赖交叉编译的 `libcore_ffi_android.so`；现有剪贴板读写与启动入口仍依赖 Shizuku。默认交互已决定采用 [通知栏手动发送](<docs/adr/006-android-notification-clipboard-send.md>)（短暂透明 Activity 获焦读取，不进入首页），**尚未实现与真机验收**；Android 功能开发仍暂停，当前仅按授权恢复 CI 实跑与失败修复。
- macOS：Xcode 目标的 `CoreBridge` 通过 `dlopen` 加载 `Native/libcore_ffi_macos.dylib`，是实际实现路径；SwiftPM 包内的同名 `CoreBridge` 仍是 `notImplemented` 桩，`swift build`/`swift run` 只会跑这个桩。
- CI：提交 `96aa612` 的 [GitHub 全量运行](https://github.com/RyanZhou416/ClipBridge/actions/runs/44121503303) **8/8 job 成功**：Linux Rust、Windows 84/84 测试、Android 双 ABI APK、macOS 真实 Xcode 构建、MSI/EXE/ZIP 打包及契约/汇总全部通过。当时 [release-please](https://github.com/RyanZhou416/ClipBridge/actions/runs/44121503043) 创建的版本 PR 已按维护者决定关闭，版本保持 0.1.0；版本辅助改为仅手动触发，普通 push 不再建 PR，日常直接推 main。未合并过期 Windows 分支或自动发布；分支审查与验收证据见 [CI 实施与托管证据](<docs/ci-architecture.md>)。
- Windows 启动耗时测量脚本位于 `scripts/measure-startup.ps1`，方法学见 `scripts/README.md`。
