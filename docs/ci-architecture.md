# ClipBridge 持续集成架构与验收规范

- 状态：2026-11-18 起，日常 CI 契约为 `schemaVersion=2` 的 7 个 job / 5 个固定任务，普通 push/PR **不构建任何 Windows 安装器**；新契约的 hosted 验收尚未执行，本文不引用未来 run ID 或预写成功结论（见 §10.7）。提交 `96aa612` 的 8/8 全量运行、此前 MSI/EXE/ZIP 打包记录及 `2d7129a` 基线只作为历史证据，见 §10.4、§10.7。历史版本机器人已按最新决定移除，CI 不创建分支/PR，见 §10.6。CI 成功不等于生产签名、正式发布、真实安装或移动端运行已经验收。
- 更新日期：2026-11-18。
- 适用范围：Rust 核心、共享 C ABI、Windows、Android、macOS 的日常构建与测试；Windows 机器级 MSI、用户级 EXE、便携 ZIP 只在人工触发的 Windows 发布中构建和验收（见 [Windows 发布规范](<windows-release.md>)）。
- 现状核对基线：`2d7129a`（引入 `schemaVersion=2` 前的设计基线；此提交上的旧契约运行不代表新契约已验收）。
- 架构决策：脚本驱动的扁平多任务 CI；日常只做受影响平台的构建与测试及轻量配置检查；安装器打包从日常 CI 移出，仅由手动发布入口按选择构建。
- 渠道边界：MSIX、StoreUpload、winget 不进入 GitHub CI 或发布工作流；保留本地脚本和人工渠道流程。这是项目范围决策，不是对 GitHub Actions 技术能力的判断。
- 发布设计：[Windows 手动触发自动发布规范](<windows-release.md>)。工作流与脚本正在据此重构；本地契约测试不代替托管 runner、签名或真实发布验收。

## 1. 目标与范围

持续集成（Continuous Integration，CI）必须回答：本次修改是否通过了应当执行的检查，是否破坏了受影响平台的实际构建与原生集成。

持续交付与发布（Continuous Delivery/Deployment，CD）另行回答：哪个提交、哪个版本和哪一组经过验证的产物可以对外分发。CI 成功不等于发布获准，也不等于应用已通过商店认证。

### 1.1 必须达到的目标

1. 共享核心或 ABI 变化能够传播到平台消费者，不因目录过滤遗漏集成检查。
2. 关键构建、测试和包校验失败时返回失败；缺少原生库或初始化降级不能满足正向集成测试。
3. 验证真正的应用目标，不用占位工程、空测试或单纯语法检查代替实际应用构建。
4. 本地与 CI 共用构建规则；开发者能够在具备相同工具链的对应操作系统上复现命令。
5. 所有自动测试使用独立数据目录和合成数据，不读写真实用户数据库、证书或凭据。
6. 普通 CI 不依赖生产秘密、不使用真实 Store 身份、不发布 Release、不提交商店。
7. 汇总结果说明应运行、实际运行、成功和跳过的任务，能够区分合理跳过与漏检。
8. 保留 main 直推的开发方式，同时支持 Pull Request（PR）检查；不以改换分支工作方式作为重构前提。
9. 日常 CI 只编译/测试应用与原生库，不构建、不签名、不上传 Windows 安装器；MSI/EXE/ZIP 的真实构建、签名与验收只属于人工触发的 Windows 发布入口。普通 push 仍必须编译应用，不能用“没有安装器”描述为“没有构建”。

### 1.2 本次架构不包含的工作

- 在普通 push/PR 后自动公开版本、自动提升产品版本或隐式配置签名凭据。普通 CI 不构建安装器；人工触发后的 MSI/EXE/ZIP 自动发布由独立发布规范定义。
- 在 GitHub Actions 中构建、验证或上传 MSIX/StoreUpload，生成或提交 winget 清单，调用 Partner Center/winget 提交服务。相关本地脚本不删除。
- 每次提交运行 Windows App Certification Kit（WACK）、完整图形界面自动化、跨设备网络测试或性能基准。
- 新建自托管 runner 集群、远程构建缓存服务、自研任务调度框架或跨运行产物晋级系统。
- 强制三端同时发布、统一三端产品版本，或将内部 core/FFI 组件自动变成独立 SDK 发行物。
- 将所有 Windows 格式强制改造成同一次编译的产物：MSIX 的身份、PRI 和布局生成与 unpackaged 发布存在差异。

### 1.3 首期支持与验证矩阵

下表固定首期验证范围，不由 runner 的默认架构、已有二进制或自动生成的 scheme 决定。它是待验收的目标，不是兼容性测试报告。

| 任务 | runner 系列 / CPU | 构建目标与配置 | 首期成功保证 | 不包含的保证 |
|---|---|---|---|---|
| `changes`、`ci-config`、`ci-result` | `ubuntu-24.04` / x64 | 分类、语法、规则测试、汇总 | 调度契约和轻量检查可执行 | 不代表应用运行通过 |
| `rust` | `ubuntu-24.04` / x64 | `x86_64-unknown-linux-gnu`；工作区 fmt/clippy/test，测试使用默认 test profile | host 测试、ABI 静态检查、依赖政策通过 | 不代表存在 Linux GUI 发行物 |
| `windows` | `windows-2022` / x64 | Rust `x86_64-pc-windows-msvc` Release；C# Release / x64 / `win-x64` | 真实 DLL 链接、加载、初始化/关闭，MSTest 与格式检查；不构建安装器 | Windows 10 最低版本、全部 GUI/网络行为尚未实测 |
| `android` | `ubuntu-24.04` / x64 host | Rust `aarch64-linux-android`、`x86_64-linux-android` Release；Gradle `:app:assembleDebug` | APK 包含 `arm64-v8a` 与 `x86_64` 的本次 SO；JNI/Java 构建与静态契约通过 | 无设备/emulator 运行、正式签名 APK/AAB 验收 |
| `macos` | `macos-15` / arm64 | Rust `aarch64-apple-darwin` Release；Xcode target/scheme `ClipBridge`、Release、`ARCHS=arm64` | 真实应用构建、dylib 嵌入、动态加载与 ABI 探测 | Intel/universal、完整 UI、Developer ID、公证和 DMG 分发 |

Android 的 debug APK 与 macOS 的 Xcode `.app` 是各平台集成测试的输出，不属于 Windows 发布安装器；它们的存在不表示日常 CI 仍在打包 MSI/EXE/ZIP，移除日常打包也不表示移除了这些平台构建检查。

工具链边界：

- Windows 日常使用 .NET 10 SDK、Windows SDK `10.0.26100.0` 与 VS 2022 MSBuild；WiX `3.14.1` 只由人工发布/本地打包入口使用，日常 CI 不安装 WiX、不构建 MSI/EXE/ZIP。C# restore/build/test 的 R2R 属性保持一致，不改为 Debug 来绕过 Release 依赖。
- Android 使用 JDK 21、[Gradle wrapper](<../platforms/android/ClipBridgeShellAndroid/gradle/wrapper/gradle-wrapper.properties>) 的 8.13、[版本目录](<../platforms/android/ClipBridgeShellAndroid/gradle/libs.versions.toml>) 的 AGP 8.13.2，以及 [应用配置](<../platforms/android/ClipBridgeShellAndroid/app/build.gradle.kts>) 对应的 SDK 36、build-tools 36.0.0、NDK 26.3.11579264、CMake 3.22.1；原生 Android API level 为 26。
- macOS 初始选择 Xcode `26.0.1` 及其 macOS SDK 26.0，不使用镜像默认 Xcode；保持 [Xcode 工程](<../platforms/macos/ClipBridgeShell_macOS/ClipBridge/ClipBridge.xcodeproj/project.pbxproj#L440-L470>) 声明的 deployment target 15.7。实施时提交共享 scheme `ClipBridge`，不能依赖个人 Xcode 用户配置。
- Rust 及 .NET 10 的具体补丁版本在实施提交中通过原生工具链配置固定，并由三平台验证记录确认；这里不凭空宣称某个尚未验证的补丁版本可用。GitHub 镜像可用工具以预检为准，缺失时明确安装指定版本或失败，不静默切换目标/工具链。
- runner 系列不是不可变镜像；每次记录实际镜像、CPU、SDK/编译器版本。若指定版本被移除，通过显式修改配置并全量验证迁移，不隐式改用 `latest`。

“支持矩阵通过”只覆盖表中列出的层级。在 Windows Server runner 上运行不证明 Windows 10 最低版本兼容；在 macOS 15 的某个补丁版本上通过也不证明所有部署目标版本兼容。最低系统与未覆盖架构在对应平台发布候选中单独验证。

## 2. 当前基础与已知缺口

以下是基线提交的事实，不是目标架构已经完成的声明。表格保留首次重构前的缺口；其中“日常打包”相关缺口已按 2026-11-18 的决定改为移出日常 CI 并只保留轻量契约检查（见 §10.7），不再以补齐每日包验证为目标。

| 范围 | 已有基础 | 需要修正的缺口 |
|---|---|---|
| 调度与权限 | 基线的 [CI 工作流](<../.github/workflows/ci.yml>) 已支持 PR、main push、只读 token 和取消过时运行 | 基线缺少最终汇总检查和明确超时；变更分类没有完整覆盖依赖链 |
| Rust | Linux 上执行格式、clippy、测试、cargo-deny 和导出名称检查 | Windows job 只有 `cargo check`；原生链接和真实消费者验证不足 |
| Windows | C# 构建、MSTest 和安装器构建入口已经存在 | [项目中的 Rust 构建](<../platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/ClipBridgeShell_CS.csproj>) 失败或 DLL 缺失可只产生警告；restore/build 参数需要一致 |
| Windows 正向冒烟 | [CoreHostSmokeTests](<../platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS.Tests.MSTest/CoreHostSmokeTests.cs>) 执行初始化与关闭 | 断言接受 `Degraded`；临时路径只用于设置，核心仍通过 [CoreHostService](<../platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/Services/CoreHostService.cs>) 取得默认数据根 |
| Android | 两种 ABI 的 Rust SO、APK 构建和包内容检查 | 触发规则漏掉部分公共构建输入；没有实际设备运行保证 |
| macOS | SwiftPM 编译检查、真实 Xcode 工程和原生构建脚本均存在 | CI 仅构建 SwiftPM 桩；[Xcode 嵌入阶段](<../platforms/macos/ClipBridgeShell_macOS/ClipBridge/ClipBridge.xcodeproj/project.pbxproj#L235-L252>) 在缺少 dylib 时可返回成功 |
| Windows 打包 | 各渠道本地脚本已实现；旧版日常 CI 曾包含 `windows-packages` job，并调用 winget 清单生成/校验 | 已按新决定移除日常打包 job：普通 push/PR 不构建 MSI/EXE/ZIP，打包/发布输入只触发 `ci-config` 轻量检查；winget 不在 GitHub 范围；真实包只在手动发布中构建 |
| 发布 | 基线已有 [旧版本自动化](https://github.com/RyanZhou416/ClipBridge/blob/bbfeb4519aa7b98c31accb8cf7c56d5fb4b72115/.github/workflows/release-please.yml) 与旧全 tag 发布入口 | 旧入口已移除；本次由 [手动发布工作流](<../.github/workflows/release-windows.yml>) 承担发行，版本改为人工直推，仍须真实发布验收 |

目标实施时保留有效检查，并解决这些缺口；不以删除检查、接受降级或长期设置 `continue-on-error` 换取成功状态。

## 3. 架构与职责划分

### 3.1 单入口、扁平多任务

[CI 工作流](<../.github/workflows/ci.yml>) 作为统一入口，任务关系直接可见。下列为 schemaVersion=2 的实际任务结构；托管验收状态另见 §10.7。

```text
PR / main push / 手动全量验证
                 │
              changes
                 │
     ┌───────────┼───────────┬───────────┬───────────┐
     │           │           │           │           │
 ci-config      rust       windows     android     macos
     │           │           │           │           │
     └───────────┴───────────┴───────────┴───────────┘
                             │
                         ci-result
```

- `changes`：计算变更范围和必跑任务集合，输出触发原因；不是通用构建系统。
- `ci-config`：执行 §3.4 限定的语法与规则检查；本地文档链接只检查本次新增/修改文档，不启动平台应用，不扩展为全仓库治理。打包/发布输入变化也只在这里做轻量契约检查。
- `rust`、`windows`、`android`、`macos`：各自拥有该平台的构建与验证责任，能够并行运行；Windows 只构建应用/原生库和测试，不构建安装器。
- `ci-result`：依赖上述全部任务，使用 `always()` 汇总结果；上游失败、取消或意外跳过不能被掩盖。

固定 job 集合为 `changes` + 五个验证任务（含 `ci-config`）+ `ci-result`，共 7 个；不存在 `windows-packages` 或其它打包/签名 job。安装器只由独立的 [手动 Windows 发布工作流](<../.github/workflows/release-windows.yml>) 构建、签名和验收。

平台间不为复用一个小文件而建立跨 runner 的复杂依赖。日常平台 job 各自构建所需原生库；发布入口另行构建或复用经 Windows 测试的精确 DLL hash，不把日常缓存命中当作发布候选证明。

### 3.2 构建规则归属

| 层 | 负责 | 不负责 |
|---|---|---|
| 项目配置 | 依赖、编译选项、目标架构、原生库嵌入和包资源 | GitHub 事件、仓库权限、商店发布 |
| 仓库脚本 | 本地可运行的多步骤构建、结构断言、测试编排、明确退出码 | 读取 GitHub 事件以决定发布、自动提权、隐式使用开发机账号 |
| 工作流 | 触发、runner、工具链准备、任务依赖、权限、缓存、日志与制品上传 | 大段内联打包业务逻辑、另一套本地无法复现的构建规则 |
| 发布入口 | 指定提交和版本、受保护的签名/上传步骤、审批、发布记录 | 从未经信任的 PR 自动取得待签名产物 |

日常 CI 复用平台入口 [Windows](<../scripts/ci/windows.ps1>)、[Android](<../scripts/ci/android.sh>)、[macOS](<../scripts/ci/macos.sh>)、[Rust](<../scripts/ci/rust.ps1>) 和 [FFI 一致性脚本](<../scripts/verify-ffi-surface.ps1>)。[Windows 打包主入口](<../scripts/build-windows-installers.ps1>)与 [MSI/EXE/ZIP 全量入口](<../scripts/build-windows-all.ps1>) 只由人工发布或维护者显式本地调用，日常 CI 不调用。需要迁出 YAML 的复杂步骤使用普通 PowerShell/Bash 脚本；单条标准构建命令无需再包一层。MSIX/winget 的独立本地脚本不被任何 GitHub 工作流调用。

本阶段不引入 composite action 或 reusable workflow。只有同一组环境准备或完整 job 在多个有效入口中真实重复时，才提取对应层级；不得仅为目录对称而抽象。保留脚本边界，使将来的提取不改变构建语义。

### 3.3 选择依据与代价

| 决策 | 收益 | 接受的代价 |
|---|---|---|
| 单入口、扁平 job | 调度图直接可读；检查名称稳定；故障定位少跨文件 | 入口文件会随平台任务增长 |
| 构建逻辑放脚本 | 本地与 CI 共用；可以独立验证退出码和负路径 | 需要维护少量脚本帮助与参数契约 |
| 以依赖并集过滤 | 降低无关构建成本，同时覆盖核心到平台的影响 | 规则新增或调整必须有回归用例 |
| push/PR 按影响验证，安装器移出日常 CI | 普通业务代码修改不构建任何安装格式，显著降低 runner 占用与打包输入误触发 | 安装器问题只能在手动发布或维护者显式本地打包时发现；发布入口必须对所选格式自行构建/校验 |
| 独立发布候选构建 | 不依赖普通 CI 制品保留期；发布流程较简单 | 需在发布候选上重新执行验证 |

### 3.4 轻量检查的范围

`ci-config` 每次运行，但检查集合保持有限：

- 使用固定版本的 actionlint 同时检查 `ci.yml` 与 `release-windows.yml` 的语法和表达式，不执行 workflow 内的业务命令。
- 对本次变更的 PowerShell/Bash CI 或构建脚本进行 AST/`bash -n` 等语法检查；只解析，不运行脚本。`scope=full` 时遍历已声明的 CI/构建脚本集合，不包含任意历史辅助脚本。
- 每次执行分类与汇总函数的表驱动测试，覆盖 `schemaVersion=2`、固定 5 个任务、依赖闭包、失败状态和未知路径处理。
- 发布工作流、发布/打包脚本与安装器工程输入（WiX 工程、Burn bundle、打包脚本等）只运行既有的纯契约/夹具、AST、XML 语法及合成表夹具、YAML 结构检查，不构建、不签名、不上传任何安装器；这类变更不启动 `windows` job。
- 检查本次新增/修改文档引用的本地文件、明确的章节或行号目标；不扫描整个文档库，不检查外部网站在线状态，不将历史文档整治混入 CI 重构。
- diff 不可靠时仍对工作流和分类规则做完整检查；文档链接范围无法确定则在摘要标明未检查，不因此启动全仓库文档巡检。平台构建仍按 §4.2 保守全跑。

## 4. 变更分类与验证频率

### 4.1 触发事件

| 事件 | 必须执行的检查 | 发布行为 |
|---|---|---|
| PR | 轻量检查；受影响平台编译/测试；发布/打包输入变化只做 `ci-config` 轻量契约检查，不构建安装器 | 无 |
| push 到 main | 与 PR 相同：受影响平台编译/测试必跑；普通业务代码或测试修改不触发安装器构建，打包脚本/发布输入的修改也只运行轻量检查 | 无 |
| 手动全量验证 | 不使用路径过滤；执行 `ci-config` 与 rust/windows/android/macos 的构建、测试（5 个固定任务），不构建安装器 | 无 |
| 手动发布 Windows | 固定提交和版本；新候选重新检查并构建所选 MSI/EXE/ZIP；已有候选按发布规范核对原证据、现有资产和当前政策，不重打包 | 按选择生成 GitHub 草稿、预发布或正式 Release；见 [发布规范](<windows-release.md>) |

顶层 CI 不通过 `on.paths` 跳过整个 workflow。纯文档修改也产生轻量检查和最终结果，但不占用 Windows/macOS 重型构建资源。当前不创建定时任务。日常 CI 不产出任何安装器；Android debug APK 与 macOS `.app` 只是平台集成测试的制品，不能与 Windows 发布安装器混为一谈。

PR 验证合并结果；main 验证实际 push 的提交。若未来启用 merge queue，再配套增加对应事件；本阶段不预设需要合并队列。

### 4.2 分类原则

- 规则表达的是任务的全部构建输入，而不是文件归属部门；每条路径按 specific-first 只命中首个规则，多个变更路径的影响取并集。规则顺序必须先匹配发布/打包/夹具等特殊路径，再匹配平台目录与共享构建输入，最后才回退未知路径；一条宽泛的 `platforms/**` 或 `scripts/ci/**` 规则不得吞掉更精确的轻量路由。
- 区分平台产品测试与 CI 夹具：修改 Windows 的 MSTest 或其项目依赖仍运行 `windows`；仅修改 `scripts/ci/` 下的报告/发布/平台夹具才只运行恒跑的 `ci-config`。所有路径都不能触发日常安装器构建；具体规则见 §4.3。
- 发布工作流、发布脚本与安装器工程输入只路由到 `ci-config` 轻量检查，不得因位于 Windows 平台目录或 `.github/` 下而重新触发 `windows` 或全量平台构建。
- PR 使用与目标分支的合并基点计算变更；main push 覆盖整个 push 范围，不只最后一个提交。
- 支持新增、删除、重命名。重命名按旧路径和新路径共同计算影响。
- 平台入口/工具链变化只路由到该平台（如 Windows 的 .NET SDK pin 只触发 `windows`）；所有 Rust 消费平台共享的 `rust-toolchain.toml` 路由到 rust/windows/android/macos，不把整个 workflow 标为 full。
- 共享分类/聚合实现或 `ci.yml` 变化触发全量验证，避免修改检查本身却没有运行它。
- diff 无法取得、基点无效或存在未分类路径时，保守执行全部检查，并在摘要说明原因。
- 仅明确列入文档清单且不被构建消费的路径才可免重型检查；应用图标、资源、许可证、安装器素材不能因位于 assets 或文档目录就被排除。
- 手动全量验证与发布候选不依赖 diff 结果。

### 4.3 输入到任务的传播矩阵

下表中的路径表达式是实施时的分类契约，不是现有过滤器的描述。

| 变更输入 | 构建/测试范围 | 轻量检查 / 说明 |
|---|---|---|
| `cb_core/**`、`crates/**`、根 `Cargo.toml`/`Cargo.lock`、`.cargo/**`、`rust-toolchain.toml` | rust + windows + android + macos | `ci-config` 恒跑；不构建安装器；全部 Rust 消费平台 |
| 平台内 Cargo 清单/锁文件（如 `platforms/<p>/**/Cargo.toml`、`Cargo.lock`） | rust + 该平台 | 只扩大该平台的 Rust 构建输入 |
| `platforms/<p>/include/**` 导出头文件 | rust + 该平台 | 共享 ABI 声明一致性与导出面仍由 rust 检查 |
| `crates/cb_core_ffi/**` 共享 C ABI | rust + windows + android + macos | 被各平台嵌入 |
| 单平台 `platforms/<p>/core-ffi/**` | rust + 该平台 | 不扩大到其它平台 |
| Windows 普通 C#/XAML、props/targets、版本、资源、publish profile | windows | 不构建安装器 |
| 仅 Windows 测试代码或测试专用依赖/配置 | windows | 不触发安装器 |
| WiX/Burn 工程、MSI/EXE/ZIP 渠道脚本 | 仅 `ci-config` | AST/XML 表/纯夹具测试；不构建、不签名、不上传包 |
| `.github/workflows/release-windows.yml`、`scripts/release/**` | 仅 `ci-config` | actionlint（同时检查两个工作流）与 release/workflow 夹具；不发布 |
| `scripts/version.ps1` | windows | 版本生成是正常构建输入，不只是打包输入 |
| `.github/workflows/ci.yml`、共享分类/聚合脚本 | 全量 5 个任务 | 修改检查本身必须全跑 |
| `scripts/verify-ffi-surface.ps1` | rust | 由 rust job 执行的共享导出面检查；不扩大到打包 |
| `scripts/ci/windows.ps1`、`windows-report.psm1`、`global.json` | windows | 平台专用 CI 入口/工具链只路由对应平台 |
| `scripts/ci/rust.ps1`、`rust-report.psm1`、`rustfmt.toml`、`deny.toml` | rust | `.editorconfig` 归 `windows` |
| `scripts/ci/android.sh`、`scripts/ci/android/` 生产工具 | android | 仍构建 debug APK 做集成验证 |
| `scripts/ci/macos.sh`、`scripts/ci/macos/` 生产工具 | macos | 仍构建真实 Xcode `.app` |
| `scripts/ci/test-*-report.ps1`、`test-packaging.ps1`、`test-release-*.ps1`、`scripts/ci/android/tests/**`、`scripts/ci/macos/tests/**`、`scripts/README.md` | 仅 `ci-config` | 纯夹具/契约测试或说明文档，不启动平台构建 |
| Android shell/JNI/Gradle/资源/构建脚本 | android | debug APK 是集成测试输出，不是 Windows 安装器 |
| macOS Swift/Xcode/资源/构建脚本 | macos | `.app` 是集成测试输出，不是 Windows 安装器 |
| `.editorconfig` | windows | dotnet format 门禁 |
| 仅 MSIX manifest、MSIX 专用构建/身份配置、winget 生成/包装脚本 | 无平台任务；只跑 `ci-config` 与汇总 | 摘要说明；不调用 MSIX/winget 脚本 |
| 纯说明文档 | 仅 `ci-config` | 不占用 Windows/macOS 重型构建资源 |
| 未分类路径或 diff 不可靠 | 全量 5 个任务 | 保守回退并说明原因 |

日常 CI 不构建 machine MSI、user Burn EXE、portable ZIP 或任何 Windows 安装器；这些只由手动发布工作流按选择构建和验收。Android debug APK 与 macOS `.app` 仍是平台集成测试输出，不因移除日常打包而取消。

本地渠道文件必须显式分类，不能因为未识别路径而间接触发被排除的渠道。若同时修改共用 C# 代码、项目依赖或通用构建脚本，仍按其他规则验证受影响的普通 Windows 构建（但不构建安装器）。普通单测中不需要包身份/部署的迁移逻辑测试可以保留，但不能因此宣称 MSIX 渠道已经在 GitHub 验收。

### 4.4 分类输出与调用契约

分类规则只由一个仓库脚本/模块维护，逻辑为“变更路径与事件 → 固定任务集合”。YAML 中不再维护第二套路径规则，汇总阶段也不重新猜测影响范围。不生成动态 job 图，不引入任务调度框架。

`changes` 只暴露一个结构化输出 `plan`（JSON）；其字段契约如下：

| 字段 | 类型与约束 |
|---|---|
| `schemaVersion` | 整数，当前为 `2`；旧版 `1`（含 `packages`）及不认识的版本失败 |
| `scope` | `affected` 或 `full`；未知路径/diff 回退及手动全量使用 `full` |
| `commit` | 本次实际检出的完整提交 SHA；PR 是被测合并结果的 SHA，不拿 head SHA 冒充 |
| `required` | 固定 5 键的布尔 map：`ci-config`、`rust`、`windows`、`android`、`macos`；不允许缺键或额外任务名；`ci-config` 恒为 true |
| `reasons` | 与 `required` 同键的 map，每个值是非空说明字符串数组；记录触发或不必运行的原因，不含凭据 |

契约不变量：

1. 计划只有 5 个固定任务；不得出现 `windows-packages`、`packages` 字段或 MSIX/StoreUpload/winget 任务名。
2. `schemaVersion=2` 明确移除安装器打包计划；不接受 schemaVersion 1 或带 `packages` 的输入，不做兼容转换。
3. `scope=full` 时 5 个固定任务全部必跑；不因日常不打包而降低任何平台构建/测试要求，也不把安装器加入日常计划。
4. 分类脚本在输出前验证类型、字段、闭包和提交；输出缺失、非法 JSON、未知枚举或不变量破坏直接失败，禁止退回“没有任务”。diff 不可靠可以生成合法全量计划，但脚本自身执行错误不能被当作正常回退。
5. job 的 `if` 从 `plan.required` 取值；`ci-result` 使用同一解析/校验函数与 `needs` 的实际结果对照，不再有第三套规则。安装器格式由手动发布工作流自己的输入选择，不从日常 plan 读取。
6. 新增 job 必须在同一变更中更新固定集合、job 条件、汇总 `needs` 和契约用例；轻量测试核对这些集合一致，避免漏掉新增任务。
7. 原始路径名只作为数据传入，展示前转义；不用文件名拼 shell 或 workflow 命令。分类说明不含本地身份或秘密。

例如，只有 Windows 测试文件变更时，`required` 仅 `ci-config`、`windows` 为 true；只改安装器/Burn 工程或发布/打包脚本时，仅 `ci-config` 为 true；未分类路径则全部 5 个任务为 true。手动全量不接受调用者提供任意任务图。

### 4.5 最终结果检查

`ci-result` 必须读取合法 `plan` 的必跑集合，并逐项核对实际结论；`needs` 必须包含 `changes` 及全部固定任务：

| 情况 | 最终结论 |
|---|---|
| 分类任务失败、输出缺失或无效 | 失败，不退化为空集合 |
| 必跑任务成功 | 该项通过 |
| 必跑任务失败、取消、意外跳过 | 失败 |
| 无需运行的任务被跳过 | 允许，显示跳过原因 |
| 额外执行的任务失败 | 失败 |
| 仅文档或发布/打包输入变化，完成轻量检查、其余按计划跳过 | 成功，并说明本次没有平台构建或安装器构建 |

不得仅检查“没有 failure”而忽略必跑 job 的 skipped。分类规则必须用表驱动用例覆盖公共核心、平台头文件、平台入口/工具链、发布/打包/夹具路径、`ci.yml` 与共享分类脚本、删除/重命名、纯文档和未知路径。

未来若配置 required checks，使用一个固定、跨 workflow 不重名的汇总名称。当前 main 直推模式下，汇总是提交后的质量信号；它不能阻止已经发生的 push。分支保护或 ruleset 的变更需要单独授权，不随文档或 YAML 修改隐式启用。

### 4.6 成功状态的含义

- **本次检查通过**：当前提交的计划任务全部按要求成功；摘要同时显示 `affected/full` 和具体覆盖范围。
- **全量验证通过**：同一提交以 `scope=full` 完成 §1.3 的全部平台构建/测试。它仍不构建安装器，也不包含表中明确排除的设备、最低 OS 或发布认证。
- 上一次 Windows 失败后，下一次文档检查通过不代表 Windows 已修复。必须用修复后的受影响验证或手动全量运行提供新证据，不能用“最近一次绿色图标”覆盖失败事实。
- 不引入跨提交继承绿色状态的数据库；每次结果绑定自身 SHA。架构重构验收必须执行仓库全量验证；Windows 新候选执行完整共享核心/Windows 发布检查，复用既有候选须有原验证证据并核验当前政策和同一份资产，不冒充三端全量验收，也不以最近一次部分 CI 成功代替。
- 整个 workflow 被取消、runner 失联或汇总未执行时，结果为取消/未完成，不能声称 `always()` 保证任何情况下都会生成最终结果，更不能推定成功。

## 5. 各任务的成功标准

### 5.1 Rust 与共享 ABI

- 执行 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked` 和既有 `cargo deny` 检查。
- Linux host 测试不替代 Windows/macOS/Android 的目标构建；各平台 job 必须实际链接对应动态库。
- 保留现有三平台导出名称检查，但不能将名称集合一致描述为完整 ABI 兼容性证明。
- 首期 Windows 通过真实 C# 消费者验证生命周期与 ABI；macOS 验证应用嵌入库的动态加载与 ABI；Android 首期只承诺 JNI 编译链接及包内容，不宣称设备生命周期已运行。头文件声明、参数和调用约定检查使用编译器/测试补齐，不自研通用 C/Rust 解析器；全量 ABI 行为覆盖不在本次承诺内。
- 缺失库、错误架构、错误 ABI、非法输入等是独立负向测试；其预期失败不能放宽正向测试的成功条件。
- 扩充全部导出的行为测试是持续测试工作，不把一次 CI 架构调整扩大成重写整个 FFI。

### 5.2 Windows 原生集成与 C#

- 在 Windows runner 显式构建当前提交的 x64 Rust DLL，构建失败立即失败，不回退到旧 DLL。
- 对具体 C# 项目进行 restore/build/test，不通过包含 WiX 工程的目录隐式选择构建目标。
- restore、build、test 使用一致的 `Configuration=Release`、`Platform=x64`、RID 和 R2R 属性；修复配置不一致，而不是在某一步临时关闭优化来掩盖问题。
- 格式检查为阻断项；实施时清理已有违规并统一行尾/编码策略，不永久保留非阻断格式步骤。
- 正向 native smoke 必须达到 `Ready`，ABI 与消费者期望兼容，无 DLL 加载错误，并完成初始化/关闭。`Degraded` 应在专门的降级测试中验证。
- 核心数据根、缓存、日志、TLS、设置和凭据全部可注入测试路径；仅给设置服务临时路径不满足隔离要求。
- 测试禁用外部发现/网络连接，使用合成账号，不读取真实 PasswordVault；进程和文件清理只针对本次测试拥有的资源。
- 缺 DLL 或 Rust 构建失败的 CI 严格模式应失败；不因此取消产品运行时面向用户的降级处理。

### 5.3 Windows 安装器（不在日常 CI 构建）

- 日常 CI 不调用 [安装器入口](<../scripts/build-windows-installers.ps1>) 或 MSI/EXE/ZIP 包装脚本，不构建、签名或上传任何安装器。这些脚本只在手动发布或维护者显式本地调用时执行。
- 与打包相关的日常门禁仅是 `ci-config` 的纯契约/夹具测试：格式子集与 `-Only` 映射、非法格式/arm64 fail-fast、MSI/Burn 表条件、`package-summary.json` 结构、签名顺序和路径边界。它们不执行真实打包路径，也不产生安装器。
- 真实构建由手动发布入口通过同一 [安装器入口](<../scripts/build-windows-installers.ps1>) 及包装脚本完成，不在 YAML 复制参数和裁剪列表；检查版本、架构、必要原生库、SQLite 依赖、资源、MSI scope、Burn 内部载荷、便携标记与目录布局。
- 发布构建使用版本源的实际版本，不自动提升版本；日常未签名构建与正式发布签名政策分开。
- 普通 C# 构建显式禁用自动生成 MSIX 包；不得因项目启用 MSIX tooling 而附带执行打包、签名、注册或 StoreUpload 目标。
- GitHub 工作流不调用 MSIX/winget 脚本，不生成、验证或上传其包/清单，也不使用真实或测试 Store 身份。
- 顺序执行共享中间目录的构建，或明确隔离中间输出；多个包复用同一份可信 stage，而不是复用开发机残留库。
- 真实安装/升级/卸载在隔离环境和手动发布候选验收，未经验证不得列为日常保证。MSIX 系统级验证由本地渠道流程负责。
- 发布所选包前必须重新运行检查、验证最终资产并计算 hash，具体流程见 [Windows 发布规范](<windows-release.md>)。

### 5.4 Android

- 显式准备兼容的 JDK、Gradle wrapper、SDK、NDK、CMake 与 Rust targets，版本与项目配置一致。
- 先构建 `arm64-v8a`、`x86_64` 两份 Rust SO，再运行 Gradle，避免由“磁盘上碰巧存在什么库”决定本轮验证的 ABI。
- 保留 [Android 契约检查](<../scripts/check-android-shell-contracts.sh>)、debug APK 构建和两种 ABI 的原生库内容断言。
- 使用有业务意义的 JVM 测试作为增量保障；模板算术测试不算 JNI 或设备行为验收。
- debug APK 仅作为测试制品，不宣称等价于正式签名 APK/AAB，也不宣称后台行为、权限或跨设备同步已通过仪器测试。

### 5.5 macOS

- 基于 [原生构建入口](<../scripts/build-macos-ffi.sh>) 构建当前提交的 dylib，使用 [Xcode 工程](<../platforms/macos/ClipBridgeShell_macOS/ClipBridge/ClipBridge.xcodeproj/project.pbxproj>) 的真实应用 target。
- 使用 §1.3 固定的 `ClipBridge` 共享 scheme、Release、arm64、`aarch64-apple-darwin` 与 Xcode/SDK；缺少共享 scheme 是需补齐的实施项，不退回 SwiftPM 桩或自动选择其他 target。
- 检查 dylib 已嵌入应用，架构与应用一致，并做动态加载/ABI 版本探测。缺库必须失败，不能沿用仅打印 warning 的行为。
- CI 不使用仓库预置 dylib 作为权威输入；必须由本次源码生成。需要临时 ad-hoc 签名时由 runner 生成，不读取 Developer ID 私钥。
- SwiftPM 桩可保留为补充检查，但不能代替 Xcode 应用验收。
- 不把开发构建称为已签名、公证或可分发的 DMG；这些属于发布候选与 macOS 发布流程。

### 5.6 测试发现、执行与结果契约

- 测试进程退出码成功只是必要条件；声明执行测试的阶段必须产生可解析的结果报告，记录发现、执行、失败与跳过数量。
- Rust 测试阶段及 Windows MSTest 阶段至少实际执行一项测试；报告缺失、发现数为零、执行数为零或全部跳过均失败。空的单个 Rust crate/doc-test 分组允许存在，但不能用它代替整个工作区测试套件执行。
- Windows 原生生命周期、数据隔离和迁移回归等必需套件必须发现并执行；用套件/测试标识及结果验证，不将总数永久写死为某次的“73 项”。移除必需套件需同步修改契约并说明覆盖替代。
- Rust 使用工具支持的测试输出，Windows 使用 TRX；其他运行时测试使用对应原生报告或最小探测结果。报告核验是小型脚本职责，不自研统一测试平台。
- Android 首期仅声明构建/包契约；不得用模板 JUnit 通过宣称设备集成成功。增加业务 JVM/仪器测试时同时增加其发现与执行检查。
- 发布构建阶段输出计划中每个格式的检查结果及本次产物路径/hash；任一计划格式未产出、少跑断言或读取上次残留文件均失败，不能仅凭脚本 exit 0 判定全部格式通过。日常 CI 不执行该阶段。

### 5.7 数据生命周期与清理风险的检查责任

下列回归属于 Windows 验证与发布候选验证的重点。`ci-config` 只做逻辑、失败恢复和包内条件的纯夹具检查；真正的系统卸载效果仍在隔离环境和 RC 验收，不把静态条件检查等同于系统执行证据。

| 风险与不变量 | 日常检查的责任任务 | 隔离环境 / RC 的补充验证 |
|---|---|---|
| 迁移失败、取消或中断不得覆盖源数据或留下被当成成功的半成品；重试幂等 | `windows`：仅普通 C# 单测中的合成数据库、WAL、TLS、设置迁移逻辑；不注册或构建 MSIX | MSIX 注册启动、重启恢复和再次导入仅在 GitHub 外的本地渠道验证 |
| 明确保留用户数据时不删除；最终卸载默认删除只作用于授权的数据根 | `windows`：相关清理逻辑测试；`ci-config`：MSI 属性、UI 选项、Burn 变量传递和产物条件的纯夹具断言（不构建安装器） | 手动发布候选/隔离环境执行 MSI/EXE 默认删除与显式保留的卸载场景 |
| 升级不得触发最终卸载的数据删除或错误清除自启 | `ci-config`：升级排除条件和变量传递的纯夹具断言；缺守卫必须失败 | 跨版本升级后比较用户数据、设置与自启 |
| 凭据损坏应失败关闭，不能当首次运行并覆盖数据 | `windows`：DPAPI/Vault 抽象的损坏、缺失、写失败和保留文件回归 | 在测试用户中验证恢复/重登，不读取生产凭据 |
| 测试清理不得删除其他目录或另一个测试的数据 | `windows` 及各平台测试脚本：独占临时根、边界/符号链接检查、目录外哨兵文件不变 | 所有安装测试仅在可丢弃用户或 VM 上执行 |

每项只有在相应实现和回归实际运行后才能记为已覆盖。发现业务逻辑不满足这些不变量时修复缺陷，不降低测试要求或手工清理残留后再宣称通过。

## 6. 工具链、缓存、耗时与制品

### 6.1 工具链与可复现性

- 工具链版本集中记录，工作流读取或遵循同一份配置；runner 标签、实际 SDK/编译器版本写入摘要。
- 对第三方 Actions 使用经核实的完整提交 SHA，并保留版本注释；下载工具固定版本且校验摘要。
- Rust 构建统一使用锁文件；NuGet/Gradle 使用可审查的依赖版本与锁定策略，避免 CI 无意更新依赖。
- 浮动 runner 镜像并非逐字节可复现环境。目标是输入受控、来源可追溯、缓存失效后可重建，不作未经验证的逐字节相同承诺。

### 6.2 缓存

- 缓存第三方依赖、包下载与受工具链约束的编译中间结果；按操作系统、架构、工具链和锁文件区分。
- 不缓存用户数据库、凭据、证书私钥、真实身份配置或可直接作为发布依据的最终安装包。
- 工作区原生代码必须按当前源码构建；不得因为找到了旧 DLL/SO/dylib 就跳过验证。
- NuGet/Gradle 缓存优先补齐；大型 SDK/NDK 缓存先测量收益与存储成本，避免新增另一套难维护的缓存服务。
- 缓存损坏不得被当作业务成功；干净缓存构建是实施验收的一部分。

### 6.3 并发与超时

- PR 新提交可以取消同 PR 的过时验证；取消状态不代表通过。
- main 默认不主动取消正在执行的验证；若 GitHub 队列或后续策略替换了等待中的运行，必须标为未验证，不能声称每个中间提交都已通过。
- 发布候选使用独立并发组，不被日常 CI 取消；后续发布必须绑定具体提交，不读取“最近一次绿色”的模糊状态。
- 所有 job 必须配置超时。日常可先采用轻量检查 10 分钟、Rust/Windows/Android/macOS 30 分钟的上限；发布工作流按构建/候选/发布 job 单独设置，再依据 hosted runner 的冷缓存、热缓存记录调整。
- 这些是初始终止上限，不是耗时承诺；成本分析同时考虑 runner 占用、排队、缓存和制品存储，不用本机测量代替 CI 数据。

### 6.4 结果与制品

- 成功运行输出提交 SHA、事件、变更分类原因、工具链版本、测试统计和实际覆盖范围。
- 失败仍上传有用的日志与测试报告；必要输出缺失必须失败，而非上传空 artifact。
- 日常 CI 的 Android debug APK、macOS `.app` 集成制品默认保留 7 天，日志/测试报告默认保留 14 天；发布候选另行保留，不依赖普通 CI 的短期制品。
- 制品名称带平台、架构和提交标识，明确标记非发布用途。上传采用白名单，不递归上传整个工作目录。
- 日常 CI 不上传任何 Windows 安装器、内部 user MSI 或 `package-summary.json`；仅上传 Rust/Windows/Android/macOS 的日志、测试报告与平台集成测试制品。
- 安装器制品只由手动发布工作流生成并核验。后续签名或重打包后必须重新验证并计算最终 hash；发布候选不自动从日常 artifact 晋级。

### 6.5 失败分类、重试与临时例外

| 失败类型 | 处理规则 |
|---|---|
| 编译、格式、业务断言、包结构、测试发现/执行不符合契约 | 直接失败，不自动重跑测试或完整构建直到变绿 |
| 下载超时、限流、服务端临时错误 | 仅重试可识别且幂等的网络操作，最多重试 2 次（共 3 次尝试），有退避/总超时且逐次留日志；认证错误、hash 不匹配不按临时故障重试 |
| runner 故障、整体取消或超时 | 结果为失败/取消/未完成；人工可重跑同一 SHA，保留原运行，不沿用部分成功作为全量成功 |
| 不稳定测试 | 默认仍阻断；先修复确定性、隔离或时序问题，不以整套测试 `continue-on-error` 处理 |
| 日志/报告上传失败 | 原始编译/测试失败必须保留；成功路径缺少必需报告则失败；仅可选诊断附件上传失败可警告，不能覆盖原结论 |

临时例外只能按具体测试或具体漏洞记录：标识、影响范围、原因、维护责任人、关联问题、到期日和恢复条件；必须经过明确审查。默认有效期不超过 30 天，到期后自动恢复阻断。不得使用永久通配符忽略、空原因或“暂时跑不过”。关键数据安全、测试零执行、原生库缺失和正向初始化失败不得通过例外放行。

对已经隔离的非关键不稳定测试，摘要必须显式标注覆盖减少，发布候选前恢复或单独批准发布风险。人工重跑成功应保留可追溯记录，不能宣称原失败从未发生。

## 7. 权限与安全边界

- 普通 CI 默认 `contents: read`。PR 文件分类若使用 API，仅对分类 job 授予必需的 `pull-requests: read`；发布写权限不向全部 job 扩散。
- fork PR 不获得生产秘密；不使用 `pull_request_target` 执行不可信 PR 代码；不通过 `workflow_run` 将未经验证的 PR 代码或产物带入特权执行环境。
- job 使用临时托管 runner，不把开发者工作站作为可执行外部 PR 的自托管 runner。
- 外部事件值通过参数或环境变量传入，不直接拼成可执行 shell 源码。
- Store 身份只用于 GitHub 之外的本地渠道配置；GitHub CI/发布不读取或创建其身份文件，不配置 Store/winget 提交凭据。身份本身不是私钥，但仍按项目约定不写入源码、文档、日志或上传制品。
- `.gitignore` 不是秘密保护机制。日志、artifact、缓存也需要白名单；不得上传 PFX、密码、客户端密钥、生产测试账号或真实用户数据。
- 如后续自动化发布需要访问云签名服务，优先考虑服务支持的短期凭据；不为当前只读 CI 预先配置长期 PAT。

### 7.1 依赖安全检查的责任与阻断规则

在现有平台任务内完成依赖检查，不另建重复扫描平台。首次接入要验证输入确实包含解析后的传递依赖；扫描零个包、缺少锁文件或无法取得漏洞数据不能等价于安全通过。

| 依赖范围 | 检查方式与任务 | 执行时机 |
|---|---|---|
| Rust | `rust` 执行现有 cargo-deny（漏洞、许可证、来源、禁止项），遵循 [依赖政策](<../deny.toml>) | Rust/锁文件/政策变化触发的验证，以及全量/RC |
| NuGet | `windows` 在一致的 restore 配置后执行 NuGet 的漏洞清单查询，包含传递依赖并输出 JSON；脚本核验严重度，不仅检查查询命令退出码 | Windows 验证、全量/RC；生产与测试项目均列入输入 |
| Gradle/Maven | `android` 对解析并锁定的应用及 buildscript 依赖执行固定版本 OSV-Scanner，核验锁定图与当前 Gradle 配置一致 | Android 验证、全量/RC；首期覆盖所构建 debug 和发布候选会用到的 release 依赖配置 |
| Actions 与下载工具 | `ci-config` 检查 Actions SHA pin、下载工具版本/hash 与未授权来源变化；按官方通告更新 | 工作流/工具链配置变化及全量/RC；SHA pin 不等于无漏洞证明 |

- Gradle 使用模块及 buildscript 锁定文件作为扫描输入；仅扫描版本目录或依赖声明不足以覆盖传递依赖。首期不要求上传依赖图或给予普通 CI 仓库写权限。
- Rust 保持既有 cargo-deny 的更严格政策；NuGet/Gradle 首期阻断已知 High/Critical 漏洞。低/中严重度生成报告并跟踪；已确认受影响但严重度未知的发现按阻断处理，需审查后定级，不能靠缺评分自动通过。
- 误报、不可达代码或暂无法修复的非关键发现仅按 §6.5 的具体、限期例外处理；修复版本存在不意味着可以自动修改依赖或锁文件。
- 扫描服务暂不可用按网络失败处理，有限重试后失败。报告注明扫描时间、工具版本和数据来源；对上次报告的复用不能冒充新的扫描成功。
- 纯文档运行不会重新扫描全部依赖，因此不声明“所有依赖当前安全”。手动全量和 RC 必须重新扫描；本次不新增定时任务。
- macOS 首期外部共享依赖由 Rust 检查覆盖，Apple SDK 由工具链更新政策管理；未来引入 SwiftPM/CocoaPods 外部库时必须在同一变更中补锁定与扫描责任，不能宣称未配置的生态已被覆盖。
- 首期 Rust 之外尚不承诺完整的自动许可证合规审计；漏洞扫描和来源固定也不等同于完整软件供应链审计。

## 8. 版本管理与手动发布边界

### 8.1 main 直推与人工版本管理

版本和变更日志由维护者明确修改后直接提交 main，公开发行仍只由独立手动入口授权：

- 日常开发直接提交并推送 main，不要求 PR；普通 push 只触发只读 CI，不创建版本分支或 PR，也不自动改版本。
- release-please 工作流、配置与生成清单全部移除，不保留手动入口。当前只保留日常 CI 与手动 Windows 发布两条工作流；不能在其中另装机器人恢复分支/PR 创建。
- 版本源必须真正更新：Windows 使用现有 [Version.props](<../platforms/windows/ClipBridgeShell_CS/Version.props>)，Rust 使用对应 Cargo package version；变更时保持相关锁定信息一致。不新增生成版本清单，不因清理旧分支而隐式采纳 0.2.0 建议。
- `clipbridge-windows-*` tag 的自动创建权只属于手动发布入口。该入口可以按授权创建发布 tag/Release，但不能创建任何 heads 分支或 PR；普通 push/版本修改不生成发布 tag。
- 手动发布核对所选提交、版本源与 `clipbridge-windows-<version>` tag 一致，不在构建中偷偷修改版本文件。
- 不依赖 `GITHUB_TOKEN` 创建 tag/release 再触发另一流水线；采用一个 `workflow_dispatch` 入口完成所需动作，不为递归触发预先要求长期 PAT。
- Actions 默认 token 权限保持 read，普通 CI 不申请写权限；已撤回为旧版本机器人开启的 create/approve PR 开关。手动发布的最小写权限只用于其发布任务，不作为分支自动化入口。
- 旧全 tag 发布和 release-please 入口均已移除，由单一 [手动 Windows 发布工作流](<../.github/workflows/release-windows.yml>) 承担发行；日常 CI 成功不代表真实发布验收完成。

### 8.2 人工选择、自动构建与发布

详细输入、权限、重试和验收见 [Windows 手动触发自动发布规范](<windows-release.md>)。目标体验是：维护者在 GitHub Actions 选择目标提交、预期版本、MSI/EXE/ZIP 和草稿/预发布/正式发布，启动后由工作流自动完成后续步骤。

```text
人工启动（选提交、版本、MSI/EXE/ZIP、发布方式）
    → 固定 SHA 并预检当前 Release 状态
    → 新候选：检查、构建、签名/封装；已有候选：核对原来源与当前政策，不重打包
    → stage：准备/核对完整候选，输出不可变摘要
    → 草稿：结束；预发布：按政策公开；正式：候选验收和环境审批后公开/晋级
```

- 日常 CI 不发布、也不构建安装器。手动选择公开发布并运行表达发布意图；正式发布还必须在候选准备完成后经过 `windows-release-stable` 环境审批，审批绑定本次运行的 SHA/hash 与系统验收记录，不增加手工打包上传步骤。
- 新候选的发布检查不因路径过滤跳过；复用候选核对原构建/Windows 测试证据并重新执行当前政策校验，不重新生成待发布包。两者均不要求为 Windows 单端发行重新构建 Android/macOS，也不声称因此三端全量通过。
- 候选复用、草稿恢复、预发布转正式及禁止公开状态倒退的条件以 [发布状态规范](<windows-release.md>) 为准；不能只因版本/hash 相同就把请求的状态转换当作已完成。
- 签名顺序遵循载荷、MSI、Burn 封装关系，最终签名后计算 hash。公开的已验证资产不静默覆盖，失败仅恢复同一份候选。
- GitHub 仅上传所选 machine MSI、user EXE、portable ZIP，以及校验和、发布记录；内部 user MSI 不作为公开资产。
- MSIX/StoreUpload、winget 没有输入选项、任务、上传资产或隐式依赖。相关本地构建、WACK、商店及清单提交仍在 GitHub 之外进行；本地渠道可另行下载 GitHub 已发布的 MSI/EXE，但不由此触发 GitHub 工作流。
- 旧本机安装记录不自动算作新候选的安装证据；后续正式发布前的系统级验收要求由发布规范明确。

本阶段不设计跨运行普通 CI 产物晋级。复用的应是构建脚本和明确的发布候选，不是最近一次 PR 上传的任意文件。

## 9. 实施顺序与验收清单

下列项目是最终验收标准；实现及本地契约测试已有进展，但不得根据脚本存在或本文描述自动标记为 hosted 全量通过。当前阻断与证据应单独记录。

### 9.1 实施顺序

1. 按 §1.3 固定目标与工具链，建立单一变更分类输出、job 条件和最终结果契约，加入分类/汇总回归用例。
2. 修复 Windows 原生失败处理、正向 smoke 成功条件、完整测试数据隔离和 restore/build 参数一致性；补测试发现及报告核验。
3. 提取必要的平台多步骤脚本，接入真实 macOS 构建；保留 Android 双 ABI 内容验证。
4. 将安装器打包从日常 CI 移除：删除 `windows-packages` job 与 `packages` 计划字段，打包/发布输入只保留 `ci-config` 轻量契约检查；真实 MSI/EXE/ZIP 只在手动发布中构建。保留 Android 双 ABI APK 与 macOS `.app` 的集成检查，不把它们误删为“打包”。
5. 恢复格式门禁，接入依赖安全检查、有限重试与例外政策，落实超时、缓存、日志和权限约束。
6. 修正版本自动化配置，停用旧自动发布入口；按 [Windows 发布规范](<windows-release.md>) 实施单一手动入口。仓库环境/签名权限单独配置，不由文档变更隐式启用。
7. 执行 §9.3 的故障注入验收，以及同一 SHA 的 hosted runner 全量验证；保留失败与恢复证据，按实际结果更新实现状态。

### 9.2 验收清单

- [ ] §1.3 的 runner/CPU/target/configuration/scheme 与实际日志一致；工具链缺失明确失败，不静默换目标。
- [ ] 公共核心、平台 FFI、平台内头文件/清单、共享锁文件、构建配置和门禁脚本的变化触发正确任务。
- [ ] `plan` 是 `schemaVersion=2`、固定 5 个任务且无 `packages` 字段；schemaVersion 1、带 `packages`、非法字段、未知枚举、缺失 output 和依赖不闭合均失败。
- [ ] 普通 push/PR 不构建 MSI/EXE/ZIP；仅修改发布工作流/发布脚本/安装器工程/打包夹具时，`required` 仅 `ci-config=true`，不启动 `windows` 或其它平台构建。
- [ ] 平台 CI 入口/工具链只路由对应平台；平台内 Cargo 清单与平台头文件只扩大到该平台；共享 `Cargo.lock` 路由全部四个 Rust 消费平台；共享分类/汇总或 `ci.yml` 才 `scope=full`。
- [ ] specific-first 规则顺序有表驱动用例：宽泛 `platforms/**` 或 `scripts/ci/**` 不得把发布/打包/夹具的轻量路由重新扩大为平台构建。
- [ ] 删除、重命名、未知路径和 diff 失败均有分类用例；未知情况不会静默跳过。
- [ ] 纯文档或仅打包/发布输入修改只运行限定的轻量检查与最终结果；仅 Windows 测试变化不触发安装器构建，混合修改正确取并集。
- [ ] 任一必跑任务失败、取消或意外 skipped，都使最终结果失败；分类失败不能生成成功结果。
- [ ] Windows 真实 DLL 链接和 ABI 探测成功；正向初始化达到 Ready；缺库负路径能可靠失败。
- [ ] Windows smoke 的核心数据、缓存、设置、TLS 和日志全部在临时目录，不触碰真实用户数据根。
- [ ] Android debug APK 同时包含两种目标 ABI 的本次构建库；缺少任一 ABI 会失败；它是集成测试输出，不是 Windows 安装器。
- [ ] macOS 真实 Xcode 应用构建并嵌入本次 dylib，架构/加载探测通过；SwiftPM 桩不替代此结果；`.app` 是集成测试输出，不是 Windows 安装器。
- [ ] Windows 的 restore/build/test 使用一致配置，格式检查恢复阻断。
- [ ] 日常 CI 不生成任何安装器；打包契约夹具仍断言版本、架构、原生载荷、scope、便携标记、签名顺序与 `package-summary` 结构。真实包由手动发布按 [Windows 发布规范](<windows-release.md>) 构建并验证最终资产。
- [ ] 仅 MSIX/winget 本地渠道文件修改被正确识别；任何普通/全量/发布 GitHub 运行均不调用相关脚本，不生成或上传其包、清单、身份配置。
- [ ] 清空缓存后可以重建；有冷/热缓存 hosted runner 的耗时和工具链记录。
- [ ] 失败日志与测试报告可获取；日常制品白名单仅包含日志、报告、Android debug APK 与 macOS `.app`，不包含安装器、内部 user MSI 或便携 ZIP；排除私钥、身份配置和用户数据。
- [ ] 普通 PR/main 验证不使用生产秘密、不发布、不申请 UAC、不运行 WACK。
- [ ] 版本自动化更新正确文件且不自动公开 Release；旧全 tag 发布入口停用，MSI/EXE/ZIP 只由手动选择发布。
- [ ] 测试报告缺失、零发现、零执行、必需套件未执行和全部跳过均能被拦住；不硬编码历史总测试数。
- [ ] 数据迁移、升级保护、最终卸载默认删除/显式保留、凭据损坏和清理边界都有 §5.7 对应的测试、纯夹具或发布候选证据。
- [ ] Rust/NuGet/Gradle 的依赖扫描输入包含真实解析依赖；严重漏洞、扫描不可用和过期例外按政策失败。
- [ ] 编译/断言失败不被自动重试掩盖；网络重试次数与原始失败记录可查；日志上传不覆盖原结论。
- [ ] §9.3 故障注入全部产生预期失败，恢复后在同一待验收 SHA 完成干净的全量验证。
- [ ] 摘要明确区分本次部分检查与全量验证；文档运行成功不宣称此前平台失败已修复，也不把“无安装器”表述成“无构建”。
- [ ] 实施记录明确区分编译、原生加载、包结构、安装运行、真实渠道发布等验证层级。

### 9.3 检查有效性的故障注入验收

重构验收不仅要求正常输入通过，还要证明检查能够拦截错误。下列用例在独占临时目录、测试夹具或独立测试进程中执行；不得破坏真实用户数据、向 main 提交故意损坏的产品，或把生产秘密用于负路径。

| 故障输入 | 验证对象 | 必须观察到的结果 |
|---|---|---|
| DLL 不存在、架构不匹配或 ABI 版本不兼容 | 原生加载/ABI 检查与正向 smoke | 非零退出，不能回退旧库或仅 warning |
| 正向初始化返回 `Degraded` | Windows 集成断言 | 测试失败，独立降级测试不改变该判定 |
| 报告没有生成、测试过滤匹配零项或必需套件全部跳过 | 结果核验器 | 即使测试命令返回 0 仍失败 |
| `windows` 必跑但实际 skipped，或上游 failure 导致依赖任务 skipped | 汇总器 | 最终失败并列明任务，不把 skipped 普遍视为通过 |
| `plan` 缺字段、坏 JSON、未知任务、schemaVersion 1 或带 `packages` 字段 | 分类/契约校验 | 明确失败，不转换为空任务集 |
| 发布工作流/发布脚本/安装器工程改动被错误路由为 `windows` 或全量，或宽泛平台规则吞掉轻量路由 | 分类/契约校验 | 计划仅 `ci-config=true`；错误扩大的规则用例失败 |
| 删除打包夹具中的必要原生库/PRI/便携标记，或留下旧版产物冒充本次输出 | 打包契约夹具/发布校验器 | 夹具或发布校验失败；日常 CI 不构建安装器，但静态断言仍须拦截 |
| 移除升级数据删除守卫，或错误传递保留开关 | MSI/Burn 条件夹具校验（`ci-config`） | 检查失败；不需要在开发机执行破坏性卸载 |
| 导入发布阶段失败/重启，或清理路径越过独占临时根 | 数据生命周期测试 | 源数据/外部哨兵不变，目标无伪成功状态，重试符合约定 |
| 漏洞报告含 High/Critical、扫描命令失败、例外过期 | 安全报告/例外判定器 | 阻断并记录原因；不依赖访问真实漏洞包来构造用例 |
| 模拟一次临时下载失败后恢复、持续失败、摘要不匹配 | 网络包装与退出码处理 | 有界重试，超过上限失败；摘要不匹配不重试掩盖 |

这些故障场景作为重构验收证据，相关小型规则/报告判定测试可持续运行；不要求每次提交重新编译一套故意损坏的完整应用。证据应保存用例、预期/实际结论、日志和恢复后的正常验证结果，不计入产品测试的“全部通过”数字。

## 10. 实施与本地审查记录（2026-11-17，历史证据）

下表保留首次提交前的本地审查证据与当时的未验证范围，不将 Mock 当作真实发布证据。实现随后以 `76b42a0` 提交并推送，首次托管全量验证结果见 §10.2；旧的本地通过记录不能覆盖托管失败。

**历史范围说明**：§10.1–§10.4 记录旧 8-job 契约（含 `windows-packages` 日常打包与 `packages` 计划字段）的本地/托管证据，仅用于保留当时事实；它们不描述当前 7-job / 5-task 契约，也不能作为新契约已经 hosted 验收的证明。当前决定与待验收状态见 §10.7。下文历史安装器构建记录同样不构成当前日常 CI 的承诺。

| 检查 | 本地证据 | 尚未证明 |
|---|---|---|
| 分类/汇总 | 158 项契约测试通过；父审查修复了资源 Markdown、嵌套 Cargo manifest 和过宽本地渠道匹配造成的漏检，并加入固定报告/发布套件缺失的检查 | hosted 事件、fork PR 与多次 push 队列行为 |
| Rust | 本机 Windows 上 fmt/clippy/test/FFI/固定 cargo-deny 入口 exit 0；211 项执行通过、20 项既有网络/性能测试 ignored | Linux host 和跨平台实际运行；ignored 项不计作通过 |
| Windows | 父审查最终重跑入口 exit 0；84 项 MSTest 通过（含新增暂存日志注入根回归），格式阻断检查通过，80 个唯一 NuGet 包未发现 High/Critical/unknown；正向原生测试核对本次 DLL hash | hosted Windows runner、真实用户 UI/网络场景 |
| MSI/EXE/ZIP | subagent 实际构建三个单项、All 组合及旧 wrapper；父审查另构建 user EXE + ZIP 子集；最终用 `-SkipRust -ExpectedCoreDllSha256` 复用刚测试的同一 DLL，验证 input/stage hash 一致及内部 MSI 角色；47 项打包契约测试通过，未运行安装器 | 生产证书签名、时间戳信任链、本次真实安装/升级/卸载 |
| 发布规则 | 父审查最终复跑 model 124、API Mock 29、候选夹具 8、workflow/config 16 项及 actionlint 通过；修复了真实报告字段不兼容、审批重试及已测 DLL 溯源缺口 | GitHub 草稿写入、签名、环境审批、真正公开发行；全部未执行 |
| Android/macOS | Android 22、macOS 29 项离线脚本/夹具检查；Gradle 实际生成并锁定 app/buildscript 图，显式缺 ABI 能失败 | Android Linux 完整交叉构建/APK 与 macOS Xcode 实机 runner |
| Android 依赖安全 | 实际 OSV 数据经父审查独立重算：应用 66 个包、0 漏洞；AGP buildscript 127 个包、49 条记录，其中 3 critical + 21 high 阻断 | 当时构建工具图尚未修复；该问题随后已在 §10.4 修复并通过，本条不代表当前状态 |

### 10.1 保留的阻断与配置前置

- **Android 构建供应链**：原始构建工具图的高危/严重发现通过 buildscript 依赖约束修复，不新增忽略项或放宽门禁；本地证据见 §10.3，托管 Linux 构建及扫描也已在 §10.4 通过。应用依赖锁保持不变。仍有 2 条中危发现；约束 AGP 内部依赖不是官方支持组合，后续 AGP/Gradle 更新必须重新验证并优先移除不再必要的约束。
- **托管 runner（旧契约）**：第三轮同一提交全量通过，见 §10.4；该证据属于旧 8-job 契约。fork PR、取消/重跑等其他事件场景和故障注入清单应按实际证据分别验收，不从一次 main push 成功推断所有场景已覆盖。新 7-job 契约的 hosted 验收尚未执行。
- **发布前置**：未配置生产签名或 `windows-release-stable` 环境。正式发布必须拒绝缺失配置。预检为读取草稿使用了 contents: write，但 API context 强制 GET-only；能否收窄 token 权限仍需托管验证。
- **版本 PR 权限（历史与当前）**：前两轮曾因仓库禁止 Actions 创建 PR 而失败，经用户授权短暂开启用于验证。最新决定移除版本机器人，因此该 create/approve PR 开关已撤回；默认 token 权限仍为 read。当前不再保留 release-please 的 XML updater 或生成清单，Windows 仍以 Version.props 为唯一版本源；生产签名/发布环境未配置。
- **已有工具告警**：stable rustfmt 对 nightly-only 配置发出警告；WiX 有既有 ICE/双用途 MSI 告警。它们不被描述为零告警构建，也不作为安装/升级行为的证明。
- **格式改动范围**：恢复格式门禁使约 90 个 C# 文件产生 BOM/行尾/using/style 调整；核心业务修改集中于数据根注入、原生加载验证及对应测试，需与格式变更分开审阅。

本地复现入口（执行对应平台命令前须满足工具链与数据隔离要求）：

```powershell
pwsh -NoProfile -File scripts/ci/test-plan.ps1
pwsh -NoProfile -File scripts/ci/test-windows-report.ps1
pwsh -NoProfile -File scripts/ci/test-rust-report.ps1
pwsh -NoProfile -File scripts/ci/test-packaging.ps1
pwsh -NoProfile -File scripts/ci/windows.ps1 -OutDir dist/ci-review/windows
pwsh -NoProfile -File scripts/ci/rust.ps1 -OutDir dist/ci-review/rust
```

### 10.2 首轮 GitHub 全量验证（旧 8-job 契约，历史）

- 提交：[`76b42a0`](https://github.com/RyanZhou416/ClipBridge/commit/76b42a05fc7561d7ee0743aa5a3dd657e816679a)。
- CI：[运行 44107130372](https://github.com/RyanZhou416/ClipBridge/actions/runs/44107130372)，push 触发，计划 scope=full；2026-11-17 完成，总结论 **failure**。

| 任务 | 托管结果 | 证据 / 直接失败原因 |
|---|---|---|
| changes | 通过 | 六项检查均 required=true，三个允许格式均被选中 |
| windows | 通过 | 84/84 MSTest、零跳过；三个项目格式通过；NuGet 扫描 80 个唯一包，无漏洞发现 |
| windows-packages | 通过 | machine MSI、user EXE、portable ZIP 及内部 user MSI 生成；全部未签名，仅 CI 产物，不是公开发行 |
| macos | 通过 | macOS arm64 / Xcode 26.0.1 真实应用构建、dylib 嵌入及 dlopen 探测，FFI 版本 1.0；不含 UI 实测、Developer ID 或公证 |
| ci-config | 失败 | actionlint 调用 runner 上的 ShellCheck，连续写 GITHUB_OUTPUT 触发 SC2129；后续契约套件未执行 |
| rust | 失败 | Linux Clippy 命中 Unix file_id 的 unnecessary_wraps 及 Windows 专用测试变量在 Unix 上未使用；测试与 cargo-deny 尚未执行 |
| android | 失败 | setup-android 默认尝试安装已不可用的 tools 包；尚未进入项目构建或 OSV 检查，不能将这次失败归因于已知漏洞 |
| ci-result | 正确判失败 | 明确列出上述三项必跑失败，不把平台部分通过当作全量成功 |

修复原则：修正 shell 写法与 Android setup 输入，保持失败即阻断；保留跨平台文件标识的正确语义，修正 Unix 编译与测试；不关闭 ShellCheck/Clippy，也不新增漏洞豁免。Android setup 显式使用 `packages: ''`，避免其旧默认值请求已移除的 tools，固定 SDK/NDK/CMake 安装步骤保留。Unix file_id 为与 Windows 可失败实现共享 Option 接口，仅对该函数使用附理由的 `expect(clippy::unnecessary_wraps)`，不改变身份未知时拒绝驱逐的行为；磁盘守卫测试补上非 Windows 返回 None 的断言。重新验收必须以修复后的提交和新运行记录为准。

本地 actionlint 若未安装 ShellCheck，会少一层检查；再次本地复现时须把 ShellCheck 放到 PATH（本轮使用官方 SHA256 校验的 0.11.0 便携版），不能将 `-shellcheck=` 的结果称为完整托管检查。

随 push 触发的 [release-please 44107130209](https://github.com/RyanZhou416/ClipBridge/actions/runs/44107130209) 已更新其版本准备分支，但创建 PR 被仓库权限拒绝：`GitHub Actions is not permitted to create or approve pull requests`。首轮未修改仓库权限、未触发手动 Windows 发布；后续授权变化见 §10.1。

### 10.3 第二轮托管结果与后续修复（旧 8-job 契约，历史）

[`38b8872`](https://github.com/RyanZhou416/ClipBridge/commit/38b8872bd32d101c7d10b9454c71a4734bc30c7a) 的 [运行 44115863192](https://github.com/RyanZhou416/ClipBridge/actions/runs/44115863192) 仍为 **failure**，但上轮三个直接修复已生效：Rust 全入口通过，ShellCheck 通过，Android SDK 安装通过；Windows、macOS、三种 Windows 包继续通过。

继续执行后暴露的实现问题及修复：

- **Android 交叉编译环境**：Bash 不接受 `export CC_aarch64-linux-android=...` 这类带连字符的标识符。保留合法下划线变量与 Cargo linker 变量，带连字符的 cc-rs 变量经 `env` 传给 cargo；新增真实 shell 子进程回归，覆盖两种 ABI，而非仅做 `bash -n`。
- **Linux 测试临时目录**：发布夹具依赖 Windows 的 TEMP 环境变量，且一项路径越界负例使用 Windows 分隔符。改用已有跨平台临时目录辅助函数与 Path.Combine，补充临时环境变量全部缺失的子进程回归。父审查复跑 model 125、API 29、candidate 8、workflow 16 项及打包 47 项通过；生产发布策略未因此修改。
- 这些失败体现的是实现与验收覆盖不足，不是允许跳过平台检查的理由。第二轮 Android 在首次 cargo 之前退出，尚不能用它声称 Android 构建或依赖门禁已通过。

Android 构建工具依赖另外采用以下受控安全修复，保持 AGP 8.13.2 / Gradle 8.13 / JDK 21 与 SDK pins：

| buildscript 依赖 | 固定修复版本 |
|---|---|
| Guava（JRE） | 33.7.2-jre |
| Netty 已解析模块统一版本 | 4.1.137.Final |
| BouncyCastle bcprov / bcpkix / bcutil | 1.85 |
| jdom2 | 2.0.6.1 |
| jose4j | 0.9.6 |

使用 Gradle dependency constraints，而非 force 或漏洞忽略。checker-qual 保持基线 3.43.0，避免新依赖图将其降级；Guava 伴随依赖按真实解析结果锁定。仅调整 [构建脚本](<../platforms/android/ClipBridgeShellAndroid/build.gradle.kts>) 和 [buildscript 锁文件](<../platforms/android/ClipBridgeShellAndroid/buildscript-gradle.lockfile>)；[应用锁文件](<../platforms/android/ClipBridgeShellAndroid/app/gradle.lockfile>) 字节不变。

本地证据：

- OSV-Scanner 2.7.1 实际扫描覆盖 128/128 个构建包，父审查用当前策略与锁文件独立重算：**0 high / critical / unknown，剩余 2 条 medium（commons-compress 1.21）**；这不是“零漏洞”，也不是下调严重度门槛。
- buildscript 锁 SHA256：`3243fc09203b83a6d34c7e00be1d8b69dddbbeca48d2685b6f643e038e6707c1`；应用锁仍为 `1a56ebf52f95a50e279ad861df9e1506501fb807e0bf2ff0593f809c29af4ee4`。
- Windows 主机上真实执行 help/tasks/buildEnvironment/signingReport、debug/release APK、release AAB、testDebugUnitTest、assembleDebugAndroidTest 均 exit 0；双 ABI Rust SO、AIDL 与 CMake/JNI 构建通过。任务 exit 0 不替代测试数量或真机运行证明。
- debug APK 的 apksigner 与 zipalign 检查通过；release APK 未签名是预期，不作为生产发行。父审查独立核对 debug APK 两种 ELF 架构及包内 Rust SO hash 与本次构建完全一致。
- 这些构建使用本机 warm cache，不代表 Linux 冷构建或设备运行已验证。BouncyCastle 跨多个次版本且 AGP 内部图覆盖不获 Google 支持，保留实际签名工具验证；后续依赖更新必须再次验收。

后续以新提交的 GitHub 全量结果继续验收，不用以上本地证据覆盖失败任务，不自动发布。

### 10.4 第三轮同一提交全量通过（旧 8-job 契约，历史）

- 验证提交：[`96aa612eef3fafa2f83d446670ae08946c31190c`](https://github.com/RyanZhou416/ClipBridge/commit/96aa612eef3fafa2f83d446670ae08946c31190c)。
- [CI 44121503303](https://github.com/RyanZhou416/ClipBridge/actions/runs/44121503303)：push 触发，全量范围，**8/8 job success，没有通过跳过必跑任务取得绿灯**。
- [release-please 44121503043](https://github.com/RyanZhou416/ClipBridge/actions/runs/44121503043)：success；当时由 push 创建 [版本 PR #1](https://github.com/RyanZhou416/ClipBridge/pull/1)，未由代理批准或合并，未触发正式发布。后续决定停止普通 push 自动建版本 PR，见 §10.5；本条保留为当时的运行证据。

父审查已下载并核对本次托管报告：

| 任务 | 实际结果与范围 |
|---|---|
| changes / ci-config / ci-result | 分类、工作流与契约检查、最终汇总全部成功 |
| rust | fmt、Clippy、FFI surface、cargo-deny 全部 exit 0；Linux 210 项执行通过，18 项既有测试 ignored，0 failed / filtered；ignored 不计作通过 |
| windows | 84 项发现并执行，84 passed、0 failed / skipped / inconclusive；必需套件全部执行，格式及 NuGet 门禁通过 |
| android | 双 ABI Rust + JNI + debug APK 构建及包内 hash/ELF 验证通过；应用依赖 66/66 扫描、无发现；构建依赖 128/128 扫描、0 blocking、2 medium，无缺失扫描包 |
| macos | 真实 arm64 Xcode 应用构建、嵌入和 dlopen 通过，FFI=1.0；应用测试产物已上传 |
| windows-packages | 三种所选格式全部生成并上传；内部 user MSI 保持内部角色；None/unsigned/trusted=false/timestamped=false，不能当作正式签名发行 |

旧契约的普通 CI 打包在独立 `windows-packages` job 中重新构建原生库，因此报告 `sourceReusedVerified=false`；这不是复用已测试 DLL 的发布证明。该 job 已按 §10.7 移除。手动发布仍须执行自己的 Windows 验证、精确 DLL 复用、候选绑定、生产签名及审批契约，本轮没有触发该工作流。

Android 通知栏手动发送尚未实现/真机验收，应用运行、真实安装/升级/卸载、正式发行与厂商渠道的既有边界不因 CI 全绿而改变。后续纯文档提交若只运行 affected 检查，应继续引用本节固定 SHA 的历史全量证据，不宣称文档运行重做了全平台构建，也不宣称该历史运行覆盖新的 7-job 契约。

### 10.5 main 直推与旧分支处置（2026-11-17）

- 首次处置时将 release-please 从 push 改为仅在 main 手动触发（提交 `fa1a752`），其 [CI 44144903981](https://github.com/RyanZhou416/ClipBridge/actions/runs/44144903981) 已全量通过，且该 push 未启动版本辅助。此为过渡状态；最新决定是连手动入口一并移除，见 §10.6。
- 自动版本分支 `release-please--branches--main` 只准备三个组件 0.1.0 → 0.2.0 与变更日志，不是待合入的功能代码。维护者明确选择保持 0.1.0、关闭 PR #1，不合入版本建议；当时暂留分支，后续按明确授权删除，见 §10.6。
- 旧开发分支 `ClipBridgeShell_CS` 的 tip 为 `77b99a0`，共同基点 `6e8aaeb`；本次核对时 main 领先 229 个提交，旧分支仅 1 个独有提交。其标题虽为目录树更新，实际改动 13 个路径，包含早期语言切换原型。
- 该分支引用未提交的 ILocalizationService/LocalizationService，SettingsViewModel 的主题命令也未完成初始化。main 的语言热切换已由 [`5955b89`](https://github.com/RyanZhou416/ClipBridge/commit/5955b898c835a8439dce355453a6ba01a126a290) 等后续提交实现；现有 [SettingsViewModel](<../platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/ViewModels/SettingsViewModel.cs#L379-L416>) 使用 WinUI3Localizer 并保存语言选择，不需要旧分支的刷新页面接口。
- merge-tree 只读评估显示 11 处冲突；旧接口自动合并还会引入当前实现未实现的成员。目录树文件已被 main 删除，其他非功能格式/注释改动不值得恢复。因此未执行无意义的冲突合并，也不以 ours-only 合并伪称吸收了功能；首次暂留分支，随后按维护者明确指令删除，见 §10.6。
- 分支处置不修改版本、Cargo.lock、发布标签或 Release；正式发布仍只由独立手动入口授权。

### 10.6 移除分支创建自动化并清理旧分支（2026-11-17）

维护者进一步明确：CI 不得创建分支，release-please 的手动辅助也不保留。执行结果：

- GitHub 上的旧 release-please 注册工作流先禁用；仓库中的工作流、配置、生成版本清单全部删除。默认 token 仍只读，Actions create/approve PR 开关撤回。
- 当前工作流清单只允许日常 CI 和手动 Windows 发布。契约测试验证没有版本机器人、PR 写权限或创建分支/PR 的命令；保留手动发布 tag/Release 的既有权限边界，不误删发行能力。
- 删除前再次核对：版本分支 tip 仍为 `57500742bc6240841871c1e54c4bb9d84ca84bc4`，无额外新提交；旧 Windows 分支仍为 `77b99a03a416590cabf081e8a543f9d956c6ccf8`。以精确 tip 的 lease 保护远端删除，避免覆盖并发新增工作。
- 两个远端分支 `release-please--branches--main`、`ClipBridgeShell_CS` 已删除；本地旧 Windows 分支及相应 remote-tracking refs 也已清除。当前只剩 main（origin/HEAD 是 main 的符号引用，不是额外开发分支）。
- PR #1 保持 closed/unmerged，版本仍为 0.1.0，未改 package version/锁文件、未合入过期代码、未创建 tag/Release。删除的是分支引用，没有重写 main 历史。

今后版本和变更日志由维护者直接提交 main；新功能开发也直接推 main，不用 CI 创建临时分支或通过自动合并 PR 绕行。

### 10.7 收紧日常 CI 影响范围并移出安装器打包（2026-11-18）

维护者明确决定；本节取代 §10.1–§10.4 中的旧 8-job 契约描述，历史章节仅保留事实记录。

1. 日常 CI 固定 **7 个 workflow job**：`changes`、`ci-config`、`rust`、`windows`、`android`、`macos`、`ci-result`。删除 `windows-packages` job；日常 CI 不构建、不签名、不上传任何 Windows 安装器。
2. `plan` 升级为 **`schemaVersion=2`**，顶层字段仅为 `schemaVersion`、`scope`、`commit`、`required`、`reasons`。`required` 固定 5 键：`ci-config`（恒为 true）、`rust`、`windows`、`android`、`macos`。不存在 `packages` 字段；schemaVersion 1、带 `packages` 或额外任务名的输入直接失败，不做兼容降级。
3. 完整日常/手动全量验证 = 上述 5 个固定任务的平台构建与测试，**不包含** machine MSI、Burn user EXE、portable ZIP。手动发布入口保持独立：仅 `workflow_dispatch`，按格式布尔输入选择，复用经 Windows 测试的精确 DLL/hash 构建并按政策签名真实包，候选校验及必要审批后才公开（见 [发布规范](<windows-release.md>)）。
4. 影响范围收紧：平台专用 CI 入口/工具链只路由对应平台；平台内 Cargo 清单/锁文件与 `include` 头文件只路由 rust+该平台；根/共享 `Cargo.lock`、共享 core/ABI 与共享 Rust 工具链才路由全部平台；发布工作流、发布脚本、安装器工程与打包夹具只走 `ci-config` 的 AST/`bash -n`、XML 表、纯夹具与 YAML 结构/actionlint 检查；共享分类/聚合实现、`ci.yml`、未知路径或 diff 不可用才走 `scope=full`。规则 specific-first，宽泛的 `platforms/**` 或 `scripts/ci/**` 不得重新扩大轻量路由。
5. 日常平台检查未被删除：Windows 仍构建 DLL/应用并运行 MSTest；Android 仍构建 debug APK 验证 JNI/双 ABI；macOS 仍构建真实 Xcode `.app`。它们是集成测试输出，不是 Windows 发布安装器；不得把“没有安装器”说成“没有构建”，也不得据此删除上述检查。
6. 本地打包脚本继续保留，但仅在维护者明确要求时手工调用；普通 push、PR、文档或打包脚本修改都不自动产生安装器。
7. 验收状态：本决定对应的工作流、分类和契约测试实现完成后，仍须以同一 SHA 取得新的 hosted 验收记录。截至本文更新，该运行尚未发生；不得把 `96aa612` 的旧 8-job 证据、`2d7129a` 基线或本地夹具通过写成新契约成功，也不得预填未来 run ID 或结论。`96aa612`（[run 44121503303](https://github.com/RyanZhou416/ClipBridge/actions/runs/44121503303)，8/8）及此前 MSI/EXE/ZIP 构建只作为历史证据。

## 11. 参考与适用范围

采用的是职责分离和验证边界，而不是复制其他项目的工作流数量或所有矩阵。

- [Files CI（固定源码版本）](https://github.com/files-community/Files/blob/87dc46e22e91ac2af4fd7e0b5c3fca3d543867f6/.github/workflows/ci.yml)：WinUI/MSIX 可以持续打包，并将系统级测试与构建区分。
- [Files Store CD（固定源码版本）](https://github.com/files-community/Files/blob/87dc46e22e91ac2af4fd7e0b5c3fca3d543867f6/.github/workflows/cd-store-preview.yml)：Store 出包与人工触发、部署环境相结合；不等同于每次 CI 自动提交商店。
- [Tauri 多目标测试（固定源码版本）](https://github.com/tauri-apps/tauri/blob/4acdc0a2230ced11567a90962b07eedfa53dc3d2/.github/workflows/test-core.yml)：Rust 跨平台验证需要明确的目标矩阵，而不仅是 Linux host 成功。
- [GitHub：复用工作流配置](https://docs.github.com/en/actions/concepts/workflows-and-actions/avoiding-duplication)：步骤复用与 job 复用的不同边界。
- [GitHub：required checks 与跳过行为](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks)：整个 workflow 跳过、条件 job 跳过和上游失败的不同结果。
- [GitHub：安全使用 Actions](https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions)：最小权限、秘密管理、不可信代码和第三方 Actions 的风险。
- [Microsoft：WinUI 持续集成](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/ci-for-winui3)：MSIX/StoreUpload 构建与提交商店是不同操作。
- [GitHub 托管镜像及架构标签](https://github.com/actions/runner-images)、[macOS 15 arm64 工具清单](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-arm64-Readme.md)：支持矩阵的 runner/Xcode 选择依据；镜像仍需在实际运行中预检。
- [OSV-Scanner 支持的锁文件](https://google.github.io/osv-scanner/supported-languages-and-lockfiles/)：Gradle 扫描依赖已解析的锁定输入，不能用版本目录代替完整依赖图。

现有渠道命令参见 [Windows 构建文档](<windows-build.md>)；渠道实现记录参见 [ADR-005](<adr/005-windows-distribution-implementation.md>)。本文定义未来 CI 的保证，不将渠道的本机历史验收直接视为 GitHub CI 已覆盖。
