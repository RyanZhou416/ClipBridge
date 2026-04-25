# ClipBridge 项目 macOS 外壳文档

本文档是 `ClipBridge项目文档.md` 中「7) macOS 外壳（SwiftUI + AppKit）」章节的详细实施版，目标是把架构决策直接落成可执行的工程计划。

---

## 0) 文档定位

### 0.1 目标

定义 macOS Shell 的：

* 具体技术方案（而不是选型讨论）
* 代码分层和模块职责
* 与 Rust Core 的 ABI/线程契约
* 分阶段实现计划（里程碑、交付物、验收标准）

### 0.2 适用范围

* 平台：macOS（不含 iOS）
* 版本目标：v1（与当前 Windows v1 能力对齐）
* 核心前提：Core 仍是唯一权威层，Shell 不实现协议与会话状态机

### 0.3 非目标

* 不在 Shell 中复制 Core 的网络/安全逻辑
* 不追求与 Windows 的 UI 完全一致
* 不引入 Web 技术栈（Tauri/Electron）作为主实现

---

## 1) 方案定稿

### 1.1 技术栈

* UI：SwiftUI
* 系统集成：AppKit
* 并发：Swift Concurrency（`async/await`）
* 事件发布：`AsyncStream` 或 Combine（按团队习惯二选一）
* Core 互操作：Rust `cdylib` + C ABI + `clipbridge_core.h`
* 日志：`OSLog`（Shell）+ Core 权威日志库
* 凭据：macOS Keychain
* 构建：Xcode + `cargo`（必要时脚本桥接）
* 发布：codesign + notarization（`.app`，后续可扩展 `.dmg`）

### 1.2 关键决策

1. 采用 SwiftUI + AppKit 混合，而不是纯 SwiftUI。  
原因：菜单栏、全局热键、剪贴板、Panel 行为需要 AppKit 级控制。

2. 复用现有 C ABI 契约，而不是重新定义 Swift 专用接口。  
原因：保持三端一致，避免协议分叉。

3. 采用事件泵（EventPump）而不是回调直更 UI。  
原因：FFI 回调线程不可控，必须隔离解析和 UI 更新。

---

## 2) 总体架构

### 2.1 进程边界

* Shell（Swift）：

  * UI 与用户交互
  * 系统能力调用（菜单栏/热键/剪贴板/启动项）
  * FFI 封装与配置组装
* Core（Rust）：

  * 网络、会话、策略、安全、历史、缓存、日志权威

### 2.2 Shell 分层

#### A. Integration Layer

* `MenuBarService`
* `HotKeyService`
* `PasteboardWatcher`
* `PasteboardWriter`
* `LaunchAtLoginService`
* `KeychainService`

#### B. Core Bridge Layer

* `CoreBridge`：原始 C 函数映射、指针释放、JSON envelope 统一解码
* `CoreHostService`：生命周期管理、降级策略、配置注入
* `EventPumpService`：回调入队、解析、分发

#### C. Projection + UX Layer

* `HistoryStore`
* `PeerStore`
* `TransferStore`
* `StatusStore`
* `LogStore`
* 对应 ViewModel + SwiftUI Views

### 2.3 架构约束（必须遵守）

* UI 不直接调用 C 函数，必须通过 `CoreHostService`
* 回调里不做耗时工作，不做同步 Core 调用
* Stores 是 UI 唯一状态源
* 所有网络/会话权威状态以 Core 事件和查询结果为准

---

## 3) 项目结构建议

建议新增目录：

* `platforms/macos/core-ffi/`
* `platforms/macos/include/clipbridge_core.h`
* `platforms/macos/ClipBridgeShell_macOS/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/App/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/Interop/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/Services/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/Stores/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/ViewModels/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/Views/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/Models/`
* `platforms/macos/ClipBridgeShell_macOS/ClipBridgeShell/Resources/`

建议新增脚本：

* `scripts/build-macos-ffi.sh`
* `scripts/copy-macos-ffi.sh`
* `scripts/run-macos-shell.sh`

---

## 4) Core FFI 设计

### 4.1 函数集策略

macOS FFI 函数语义与 Windows/Android 保持一致，至少覆盖：

* 生命周期：`cb_init` / `cb_shutdown` / `cb_free_string`
* 本机注入：`cb_plan_local_ingest` / `cb_ingest_local_copy`
* 状态设备：`cb_get_status` / `cb_list_peers` / `cb_set_peer_policy`
* 内容拉取：`cb_ensure_content_cached` / `cb_cancel_transfer`
* 历史查询：`cb_list_history` / `cb_get_item_meta`
* 诊断日志：日志查询与统计接口（与 Windows 语义一致）

### 4.2 Envelope 与错误模型

统一 envelope：

* 成功：`{"ok":true,"data":...}`
* 失败：`{"ok":false,"error":{"code":"...","message":"..."}}`

Shell 内部定义 `CoreResult<T>`：

* `ok=true` 解码为 `T`
* `ok=false` 转为 `CoreError(code, message)`

### 4.2.1 冻结口径（对齐 Windows，2026-04-25）

macOS 端协议冻结为与 Windows 当前实现一致：

* `cb_init(const char* cfg_json, cb_on_event_fn on_event, void* user_data)` 返回  
  `{"ok":true,"data":{"handle":<usize整数>}}`
* 所有 `const char*` 返回 API 使用统一 envelope：
  * 成功：`{"ok":true,"data":...}`
  * 失败：`{"ok":false,"error":{"code":"...","message":"..."}}`
* 事件最小稳定形状：`{"type":"...","payload":{...}}`
  * `payload` 可为空/缺省
  * 必须忽略未知 `type` 与未知字段
* `cb_get_ffi_version(out_major, out_minor)` 作为 ABI 诊断探针，初始化时记录版本。

权威来源以代码为准：

* `platforms/macos/core-ffi/src/lib.rs`
* `platforms/windows/core-ffi/src/lib.rs`
* `platforms/macos/include/clipbridge_core.h`
* `platforms/windows/include/clipbridge_core.h`

### 4.3 内存管理约束

* 所有 C 返回字符串必须由 `cb_free_string` 释放
* `CoreBridge` 统一实现 `ptrToStringAndFree`
* 禁止在其他层直接 `UnsafePointer` 释放

### 4.4 ABI 版本检查

初始化时执行：

1. 加载动态库（`dlopen` 或系统默认加载）
2. 调 `cb_get_ffi_version`（若已提供）
3. 与 Shell 期望版本比较，不匹配则进入 `Degraded`

---

## 5) 线程与事件模型

### 5.1 回调线程规则

Core 回调线程不保证是主线程。回调里只允许：

* 拷贝 JSON 字符串
* 写入线程安全队列
* 立即返回

### 5.2 EventPump 设计

建议结构：

* `EventQueue`：无界队列（v1）+ 监控指标
* `EventParser`：按类型解析（兼容字段漂移）
* `EventDispatcher`：写 Stores + 发 UI 通知

### 5.3 背压策略

v1 先采用无界队列，但必须加监控：

* `queue_depth`
* `events_per_sec`
* `parse_fail_count`

当队列深度超阈值（例如 5000）时：

* 允许丢弃低价值日志事件
* 禁止丢弃 `CONTENT_CACHED` / `TRANSFER_FAILED` / `ITEM_META_ADDED`

---

## 6) 配置与目录规范

### 6.1 目录

建议路径（按用户级）：

* `~/Library/Application Support/ClipBridge`：Core 数据根目录
* `~/Library/Caches/ClipBridge`：缓存目录
* `~/Library/Logs/ClipBridge`：Shell 附加日志（可选）

### 6.2 Core config 生成

Shell 在 `cb_init` 前组装 JSON：

* `device_id`
* `device_name`
* `account_uid`
* `account_password`（若有）
* `data_dir`
* `cache_dir`
* `app_config`（limits/policy/gc）

### 6.3 设备 ID 策略

* 首次启动生成 UUID 并持久化
* 后续稳定复用
* 不使用易变标识（如网卡地址）

---

## 7) 剪贴板设计（macOS）

### 7.1 监听方案

v1 采用 `NSPasteboard.general.changeCount` 轮询：

* 间隔：150~300ms（可配）
* 去抖：200ms（默认）
* 队列化处理：避免重入

### 7.2 Snapshot 生成

v1 最小支持：

* Text（必须）
* Image（第二阶段）
* FileList（第二阶段）

`ClipboardSnapshot` 最低字段：

* `kind/mime`
* `timestamp`
* `data or ref`
* `preview`
* `fingerprint`

### 7.3 IngestPolicy

规则优先级：

1. 空值拒绝
2. 自写回检测（`LastWriteFingerprint`）
3. 去重窗口（例如 1000ms）
4. 限制判定（soft/hard）
5. 最终决策：`default/local_only/force/deny`

### 7.4 回环防护

`PasteboardWriter` 写回时记录：

* `lastWriteFingerprint`
* `lastWriteAt`

Watcher 命中短窗口时直接拒绝 ingest。

---

## 8) Lazy Fetch 与写回剪贴板

### 8.1 状态模型

每个条目在 Shell 中维护：

* `MetaOnly`
* `Fetching`
* `Ready`
* `Failed`

### 8.2 拉取流程

1. 用户选中条目
2. 若未就绪，调用 `cb_ensure_content_cached`
3. 记录 `transfer_id`，进入 `Fetching`
4. 等待 `CONTENT_CACHED` 或 `TRANSFER_FAILED`
5. 成功后进入 `Ready` 并写系统剪贴板

### 8.3 取消与超时

* 用户取消：调用 `cb_cancel_transfer`
* 超时默认：10~15 秒可配
* 超时后状态置 `Failed`，保留重试按钮

---

## 9) Quick Paste 设计

### 9.1 窗口形态

* 使用 `NSPanel`
* 置顶、无常规标题栏、失焦关闭
* 不出现在 Dock/应用切换主列表（按实际行为配置）

### 9.2 交互规范

* 热键 Toggle 显示/隐藏
* 上下方向键选择
* Enter 执行“确保就绪并写剪贴板”
* Esc 关闭

### 9.3 数据策略

* 打开即读 `HistoryStore` 最近 N 条，保证秒开
* 后台拉取分页补齐
* 搜索使用 debounce（200ms）

---

## 10) 菜单栏、热键、启动项

### 10.1 菜单栏

`NSStatusItem` 菜单建议：

* 打开主窗口
* 打开 Quick Paste
* 启用/暂停剪贴板监听
* 打开设置
* 退出

### 10.2 全局热键

* 使用 Carbon HotKey（或成熟封装）
* 冲突时提示并提供改键入口
* 失败不导致应用不可用

### 10.3 开机启动

* 使用 `SMAppService`（符合现代 macOS 机制）
* 设置页提供开关与状态说明
* 系统拒绝时给出诊断文案

---

## 11) 安全与凭据（Keychain）

### 11.1 Keychain 职责

* 账号敏感凭据
* OPAQUE 相关数据密钥（与 Core 契约对应）
* 必要的证书/信任辅助材料（若 Shell 托管）

### 11.2 契约

* `key_id = clipbridge/{account_uid}/opaque_data_key/v1`
* 不可明文落盘
* keystore 不可用时，账号能力降级并明确提示

### 11.3 账号切换策略

* 检测 `account_uid` 变化
* 清理/重建对应证书与信任缓存
* 避免跨账号复用旧身份材料

---

## 12) 日志与诊断

### 12.1 双层日志

* Shell 技术日志：`OSLog`
* 业务权威日志：Core 日志 API（用于查询页）

### 12.2 Logs 页能力

* 时间范围查询
* 关键字过滤
* 增量 tail（after_id）
* 清理（delete_before）
* 统计（stats/source_stats）

### 12.3 诊断模型

`CoreDiagnostics` 最低字段：

* 动态库路径
* ABI 版本
* 最近初始化错误
* 数据目录
* 当前 CoreState

---

## 13) 构建与发布计划

### 13.1 本地构建

1. 构建 `core-ffi-macos` 动态库
2. 拷贝到 Xcode 工程可加载目录
3. 启动 Shell，验证 init/shutdown

### 13.2 CI（后续）

* Rust：fmt/clippy/test
* macOS Shell：`xcodebuild build` + 基础 lint
* 可选：UI smoke test

### 13.3 发布

* 代码签名
* notarization
* 产物打包（`.app` 或 `.dmg`）

---

## 14) 实施里程碑（详细版）

### M0（1 周）：工程基建

**交付物**

* `platforms/macos` 目录落地
* Xcode 工程可运行（空页面 + 菜单栏）
* Rust FFI crate 骨架

**验收**

* Shell 启动成功
* FFI 动态库可被加载（哪怕暂未调用业务）

### M1（1 周）：CoreHost 生命周期

**交付物**

* `CoreBridge` + `CoreHostService`
* `cb_init/cb_shutdown/cb_free_string` 跑通
* `CoreState` 与 `CoreDiagnostics`

**验收**

* 连续 50 次 init/shutdown 稳定
* 缺库/版本不匹配进入 `Degraded`

### M2（1~2 周）：事件泵与投影层

**交付物**

* EventQueue + EventPump + Parser
* `HistoryStore/PeerStore/TransferStore/StatusStore`
* 主窗口实时展示基础状态

**验收**

* 高频事件下 UI 可响应
* 关键事件不丢

### M3（1 周）：文本剪贴板闭环

**交付物**

* `PasteboardWatcher`（文本）
* `IngestPolicy`（空值/回环/去重/最小限制）
* `cb_ingest_local_copy` 闭环

**验收**

* 本机复制文本可入历史
* 自写回不重复 ingest

### M4（1~2 周）：Lazy Fetch 与回填

**交付物**

* 条目选中 -> `ensure_content_cached`
* `CONTENT_CACHED/TRANSFER_FAILED` 处理
* `PasteboardWriter` 文本回填

**验收**

* 远端条目可回填系统剪贴板
* 失败可重试、可取消

### M5（1 周）：Quick Paste

**交付物**

* 全局热键
* `NSPanel` 小窗
* 键盘导航与 Enter 粘贴

**验收**

* 呼出/关闭稳定
* 失焦自动关闭

### M6（1 周）：设置、日志、发布准备

**交付物**

* 设置页（监听开关、热键、limits）
* 日志页（查询/清理）
* 启动项开关

**验收**

* 主要设置可持久化
* 日志查询可用

### M7（1 周）：打包与回归

**交付物**

* 签名、公证、发布脚本
* 回归测试清单与结果

**验收**

* 干净环境安装运行通过
* 卸载后无残留后台进程

---

## 15) 风险与缓解

### 15.1 FFI JSON 契约漂移

* 风险：三端字段不一致
* 缓解：维护统一 schema 与 Golden JSON 测试样例

### 15.2 回调并发导致状态错乱

* 风险：UI 与传输状态不一致
* 缓解：单入口 EventPump + Stores 单写者规则

### 15.3 剪贴板监听噪声

* 风险：重复 ingest 或高 CPU
* 缓解：去抖、去重、轮询间隔动态调优

### 15.4 发布链路复杂

* 风险：签名/公证阻塞交付
* 缓解：M6 前建立最小可跑通脚本，持续验证

---

## 16) v1 验收清单（最终）

* Core 可初始化、可降级、可关闭
* 文本复制 ingest 与历史展示闭环
* 选中历史可确保内容并写回剪贴板
* Quick Paste 热键路径可用
* 菜单栏常驻、退出流程完整
* 设置与日志页可用
* 基础打包发布流程跑通

---

## 17) 建议的下一步代码任务

1. 建立 `platforms/macos/core-ffi` crate（先复用 windows/core-ffi 实现框架）。  
2. 建立 Xcode 工程骨架与 `CoreBridge` 空实现。  
3. 优先打通 `cb_init -> cb_get_status -> cb_shutdown` 三步闭环。  
4. 再推进 Watcher/QuickPaste 与 Lazy Fetch。
