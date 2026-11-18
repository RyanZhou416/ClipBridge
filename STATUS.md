# Project Status

- 单一 Rust 核心 `cb_core`；C ABI 只在 `crates/cb_core_ffi/src/exports.rs` 实现一次，三个平台 crate（Windows / Android / macOS）通过 re-export 复用同一份 34 函数导出面。
- Windows：WinUI/C# 外壳已完成（`CoreHostService`、事件泵、ViewModels、页面与诊断），MSTest 套件在 CI 中执行；构建时由 csproj 自动编译 `core-ffi-windows` 并把 `core_ffi_windows.dll` 复制到输出目录。
- Android：Java/AIDL/JNI 外壳已存在，APK 依赖交叉编译的 `libcore_ffi_android.so`；现有剪贴板读写与启动入口仍依赖 Shizuku。默认交互已决定采用 [通知栏手动发送](<docs/adr/006-android-notification-clipboard-send.md>)（短暂透明 Activity 获焦读取，不进入首页），**尚未实现与真机验收**；Android 功能开发仍暂停，当前仅按授权恢复 CI 实跑与失败修复。
- macOS：Xcode 目标的 `CoreBridge` 通过 `dlopen` 加载 `Native/libcore_ffi_macos.dylib`，是实际实现路径；SwiftPM 包内的同名 `CoreBridge` 仍是 `notImplemented` 桩，`swift build`/`swift run` 只会跑这个桩。
- CI：提交 `96aa612` 的旧 8-job [全量运行](https://github.com/RyanZhou416/ClipBridge/actions/runs/44121503303) 是**历史证据**（当时含 MSI/EXE/ZIP 打包），不代表当前契约。当前日常契约为 `schemaVersion=2` 的 **7 个 job / 5 个固定任务**（`changes`、`ci-config`、`rust`、`windows`、`android`、`macos`、`ci-result`）：普通 push/PR 只编译/测试受影响平台，**不构建 Windows 安装器**；MSI/用户 EXE/便携 ZIP 只在手动发布工作流中构建。新契约的 hosted 验收尚未执行，不以旧运行、`2d7129a` 基线或本地夹具宣称已通过。版本机器人及其工作流/配置/生成清单现已彻底移除，CI 不创建分支或 PR；两个无用分支已删除，日常及版本修改直接推 main。分支清理与历史验收证据见 [CI 实施与托管证据](<docs/ci-architecture.md>)。
- Windows 启动耗时测量脚本位于 `scripts/measure-startup.ps1`，方法学见 `scripts/README.md`。
