# ClipBridge

跨平台剪贴板同步项目；当前实现状态见 [`STATUS.md`](STATUS.md)。

## 本地构建前置

### Windows

- 需要 .NET 10 SDK（目标框架 `net10.0-windows10.0.26100.0`）和 Rust 工具链。
- `ClipBridgeShell_CS.csproj` 的 `BuildCoreFfi` 目标会在构建前执行 `cargo build -p core-ffi-windows --release --target x86_64-pc-windows-msvc`，并把 `core_ffi_windows.dll` 复制到输出目录。
- `cargo` 不可用时 C# 构建只会告警并继续；Release 缺少 DLL 也会告警，但应用运行时会因找不到 DLL 失败。

### Android

> 剪贴板交互决策见 [ADR-006：通知栏手动发送](<docs/adr/006-android-notification-clipboard-send.md>)：默认采用通知按钮触发短暂透明 Activity，获焦后读取并关闭，再后台发送；无需进入首页或手工粘贴，不要求 Shizuku/Root。**这是已确定但尚未实现、未真机验收的目标；不承诺通知栏始终展开或零焦点切换，现有代码仍依赖 Shizuku。**

- 使用 `platforms/android/ClipBridgeShellAndroid/gradlew`，JDK 21。Gradle 8.13 无法解析 JDK 25 的版本号，`JAVA_HOME` 指向 JDK 25 时构建会在配置阶段直接失败。
- NDK 版本由 `app/build.gradle.kts` 固定为 `26.3.11579264`。
- 先安装 Rust Android 目标并交叉编译 `libcore_ffi_android.so`：

  ```powershell
  rustup target add aarch64-linux-android x86_64-linux-android
  cargo build -p core-ffi-android --release --target aarch64-linux-android
  cargo build -p core-ffi-android --release --target x86_64-linux-android
  ```

- Gradle 会把 `target/<triple>/release/libcore_ffi_android.so` 复制到 `jniLibs`；缺少 `.so` 时 JNI 链接会失败（`unable to find library -lcore_ffi_android`），APK 无法组装。

### macOS

- 先运行 `./scripts/build-macos-ffi.sh` 生成 `Native/libcore_ffi_macos.dylib`，再用 Xcode 打开 `platforms/macos/ClipBridgeShell_macOS/ClipBridge/ClipBridge.xcodeproj`。
- `swift build` / `swift run` 构建的是 SwiftPM 包内的桩，不会加载 dylib；详见 `platforms/macos/ClipBridgeShell_macOS/README.md`。

### Windows 启动耗时测量

- 使用 `scripts/measure-startup.ps1`；参数、A/B 方法与测量注意事项见 `scripts/README.md`。
