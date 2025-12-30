# 5) Windows 外壳

## 5.0 Windows 外壳开发计划总览

### 5.0.1 目标

Windows 外壳在 v1 的职责是把 Core 的“网络/同步/历史/日志”能力产品化，提供：

1. **后台常驻 + 系统级交互**：托盘、全局热键、开机自启（可选）、关窗驻留
2. **历史选择粘贴体验**：类似 Win+V 的呼出小窗（Quick Paste）
3. **主窗口管理能力**：历史/设备/设置/日志（调试与支持）
4. **可诊断与可恢复**：统一日志、状态可视化、失败可重试、降级清晰

### 5.0.2 非目标（v1 明确不做）

* 复杂富文本/应用内私有剪贴板格式的完整支持（先聚焦：文本/图片/文件）
* 云端账号/WAN（v1 仅 LAN 或按核心阶段）
* 多端 UI 一致性（Windows 先跑通；移动端后续）

### 5.0.3 总体架构与分层（必须固定，后续按此迭代）

外壳采用“**服务化（Services）+ 数据投影（Stores）+ 视图模型（VM）**”的结构，避免 UI 直接耦合 FFI 与线程细节。

#### A. 进程与边界

* **Shell（WinUI 3 / .NET）**：UI、系统交互（托盘/热键/剪贴板/自启）、策略与体验层、日志展示
* **Core（Rust DLL / FFI）**：权威数据与状态（历史、CAS/缓存、传输、会话、日志库），对外提供 C ABI 与事件回调

#### B. 外壳内部三层

1. **Integration Layer（系统集成层）**

* ClipboardWatcher/ClipboardWriter
* HotKeyService
* TrayService
* StartupService（开机自启）
* AppLifecycle（关窗驻留、单实例等）
2. **Core Bridge Layer（核心桥接层）**

* CoreHost（负责 DLL 加载、cb_init/cb_shutdown、FFI API 封装）
* EventPump（把 Core 事件变成 .NET 可消费流）
* LoggerBridge（Shell ILogger → Core 权威日志库）
3. **Projection + UX Layer（投影与体验层）**

* Stores（HistoryStore/PeerStore/TransferStore/LogStore/StatusStore）
* ViewModels（页面/小窗）
* 策略模块（limits、lazy fetch、超限提示、自动拉取、重试与取消）

---

### 5.0.4 里程碑规划（Shell 与 Core 对齐）

> 说明：每个里程碑都给出“交付物/验收标准/测试点”。
> “已具备/无需开发”仅代表当前代码可复用，但仍需在最终集成时通过对应验收。

#### M5.0 外壳工程骨架与工程化基建（已具备/无需开发）

**交付物**

* WinUI 3 工程结构、导航框架、基础依赖注入（DI）约定
* Settings 持久化骨架（LocalSettingsService）
* 多语言与主题切换框架

**验收**

* 运行与打包路径稳定（Debug/Release）
* 设置可读写、主题/语言切换不崩溃

---

#### M5.1 CoreHost 生命周期与降级策略

**交付物**

* `CoreHost`：封装 DLL 加载、`Init(config_json)`、`Shutdown()`、错误码映射
* “Core 不可用”降级：UI 仍可启动，明确提示不可用原因（路径/版本/加载失败码），并禁用依赖 Core 的功能按钮

**验收**

* DLL 存在：Init/Shutdown 可重复运行（至少 50 次循环无泄漏/无崩溃）
* DLL 缺失：UI 启动、托盘与设置可用；核心相关页面显示降级态

**测试点**

* 单元：错误码映射、JSON config 生成
* 集成：Smoke Test（加载 DLL + init + shutdown）

---

#### M5.2 事件模型 → UI 数据流（EventPump + Stores）

**交付物**

* `EventPump`：CoreEventSink 回调接入 → 线程安全队列 → UI Dispatcher
* Stores 投影：

  * `HistoryStore`：历史 meta 列表投影（增量 upsert、分页/排序）
  * `PeerStore`：设备在线/离线与状态投影
  * `TransferStore`：传输进度/失败/取消投影
  * `StatusStore`：核心状态（Running/Degraded/Offline…）与摘要
* 事件关联机制：按 `item_id` / `transfer_id` / `peer_id` 关联 UI 行为

**验收**

* 高并发事件（例如 1 秒 200 条）不会卡 UI、不丢关键状态
* UI 关闭/后台驻留/再次打开后，Stores 仍能正确刷新（必要时通过查询 API 补齐）

**测试点**

* 压测：EventPump 背压策略（队列上限 + 丢弃策略必须明确：丢弃 debug 日志可接受，但不能丢 CONTENT_CACHED/TRANSFER_FAILED）
* 正确性：同一 item 的 meta 更新顺序一致（最终一致）

---

#### M5.3 ClipboardWatcher + Ingest 策略层（limits/提示/本机仅存）

**交付物**

* ClipboardWatcher：捕获系统剪贴板变化 → 生成 snapshot → 调用 `cb_ingest_local_copy`
* 策略层（入口）：

  * 根据 `limits` 判断是否超限
  * 超限提示：仅本机保存（local_only）/仍然同步（force）/取消
  * 可记住选择（按类型）
* 回环防护：Shell 写回剪贴板不会导致再次 ingest（短期指纹忽略）

**验收**

* 文本复制：能生成历史 meta，UI 列表立刻出现
* 超限：提示流程正确；选择 local_only 后不会向外同步
* 写回剪贴板不会产生重复条目

---

#### M5.4 Quick Paste 小窗（类似 Win+V）与 Lazy Fetch 粘贴链路

**交付物**

* `QuickPasteWindow`：顶层小窗（无边框、置顶、失焦关闭、键盘导航）
* `QuickPasteService`：热键 toggle、定位（鼠标/前台窗口）、焦点恢复
* `QuickPasteViewModel`：

  * 最近 N 条历史展示（来自 HistoryStore，必要时调用 `cb_list_history` 补页）
  * Enter 执行：必要时调用 `cb_ensure_content_cached`，等待 `CONTENT_CACHED` → ClipboardWriter 写入
  * 显示下载进度、允许取消（`cb_cancel_transfer`）
* 体验策略（出口）：

  * 写剪贴板失败（占用）重试（短退避，上限 2 秒）后提示用户重试

**验收**

* 热键呼出/关闭稳定；不进入任务栏；失焦自动隐藏
* 选择条目后剪贴板写入成功；大内容触发 Lazy Fetch 并有进度与取消
* 关闭小窗后尽量回到原前台应用（焦点体验可接受）

---

#### M5.5 设备页与连接状态（可视化与操作）

**交付物**

* Devices 页（或等价入口）：显示局域网设备列表、在线/离线、最后见到时间、信任/配对状态
* 操作：

  * 信任/撤销信任（如果核心阶段已提供）
  * 手动刷新/重连（如核心支持）

**验收**

* 设备状态变化在 UI 端 1 秒内可见
* 对异常状态（Backoff/Offline）有明确文案与建议动作

---

#### M5.6 统一日志系统（Core 权威库 + Shell 统一入口）

**交付物**

* **Core 为权威日志库**：持久化 + range 查询 + after_id tail + 清理 + stats
* Shell 侧：

  * `CoreLoggerProvider`（或 Serilog Sink）：把 `ILogger<T>` 写入 Core 日志库
  * `CoreLogDispatcher`：异步队列写入（避免 UI 阻塞与回调递归）
  * LogsPage：按时间范围查询、关键字过滤、tail、导出、按保留期清理（UI 控制 → 调 Core API）
* 保留期策略：启动时/每天一次执行 `delete_before(now - retention)`

**验收**

* Shell/核心日志统一出现在同一日志页，可按时间段与关键字检索
* 高日志量下 UI 不冻结
* 删除 N 天前日志可重复执行且统计正确

---

#### M5.7 权限/自启/后台留存（策略与体验落地）

**交付物**

* 后台留存：

  * Close-to-tray：点 X 默认隐藏到托盘（首次提示可关闭）
  * “退出”必须可明确终止进程（托盘菜单）
* 全局热键冲突处理：注册失败提示 + 改键入口
* 自动启动：

  * MSIX 场景使用系统认可的启动机制（StartupTask 或等价方案）
  * 尊重用户禁用（不能强行恢复）
* 统一状态提示：通知权限关闭/自启被禁用/热键占用等都可诊断

**验收**

* 关窗不退出、托盘可控；退出流程完整释放（注销热键、停 watcher、core_shutdown）
* 自启开关状态与系统一致（禁用时 UI 能解释原因）

---

#### M5.8 打包发布与回归测试

**交付物**

* MSIX 打包配置、版本号、升级路径
* 最小回归测试清单（手工 + smoke 自动化）：

  * Core init/shutdown
  * 剪贴板 ingest
  * QuickPaste 粘贴链路
  * 日志查询/清理
  * 托盘/热键/自启

**验收**

* Release 包可在干净系统安装/卸载/升级
* 回归测试全通过

---

## 5.1 系统集成（Integration）

> 目标：把“Core（权威）+ Shell（体验）”在生命周期、目录、配置、异常降级上完全定义清楚。

### 5.1.1 进程生命周期与状态机

定义 Shell 侧状态机（对 UI 与策略层暴露）：

* `CoreState = NotLoaded | Loading | Ready | Degraded | ShuttingDown`
* 进入 Ready 条件：CoreHost.Init 成功 + EventPump 启动 + 基础查询（如 status）通过
* Degraded 条件：

  * DLL 缺失 / 入口函数缺失
  * Init 失败（错误码/异常）
  * 运行中致命错误（例如事件回调异常连续 N 次）

行为约束：

* **所有 UI 功能必须依赖 CoreState** 决定可用性（禁用而不是崩溃）
* Degraded 时仍允许：设置/语言主题/托盘/退出

### 5.1.2 目录与数据位置（必须统一）

统一由 Shell 生成并注入 CoreConfig（你已把 limits 等打包在 config_json，这里继续沿用）：

* `app_data_dir`（Shell 自己的设置与缓存）
* `core_data_dir`（Core DB/CAS/日志根目录）
* `log_dir`（可作为 core_data_dir 子目录，也可显式传入）

约束：

* **Core 只能写 core_data_dir/log_dir**，不允许写当前工作目录
* 删除与清理操作（Prune/Retention）必须只作用于 core_data_dir 内部

### 5.1.3 CoreHost：FFI 封装原则

* Shell 内部禁止直接到处 P/Invoke；统一走 `CoreHost`（或 `CoreApi`）
* `CoreHost` 必须提供：

  * `Init(CoreConfig cfg)`：返回 Result（错误码、错误信息、可诊断字段）
  * `Shutdown()`
  * API 组：History、Transfer、Peers、Logs（按模块分 partial class/接口）
* 所有 FFI 返回的指针内存释放必须在一处集中管理（避免泄漏与重复 free）

### 5.1.4 降级与诊断（“可解释失败”）

在 UI 侧定义统一的诊断模型 `CoreDiagnostics`：

* DLL 路径、版本（如可读）、加载错误码
* 最近一次 init 错误摘要
* 当前 core_data_dir/log_dir
* “复制诊断信息”按钮（便于发 Issue）

---

## 5.2 把 Core 事件模型变成 UI 可用的数据流（EventPump + Stores）

> 目标：UI 不直接消费“回调”，而是消费“稳定的数据投影 + 可订阅的事件流”。

### 5.2.1 事件接入与线程模型

约束必须写死：

* Core 回调线程 ≠ UI 线程
* 回调里禁止做耗时工作（IO/JSON 大解析/调用回 Core 的同步 API）

推荐实现：

* 回调函数只做三件事：

  1. 复制原始 json 字符串（或复制字节）
  2. push 到 `Channel<string>` / lock-free queue
  3. 返回
* 后台 `EventPumpWorker` 负责：

  * JSON 解析（可分级：关键事件优先）
  * 写入 Stores（投影更新）
  * 触发 UI Dispatcher 通知（ObservableCollection/INotifyPropertyChanged）

### 5.2.2 Stores 投影模型（UI 的唯一数据源）

#### HistoryStore

* Key：`item_id`
* 字段：type、preview、source、size、timestamp、pin/lock、cached_state（Ready/NeedsDownload/Downloading/Failed）
* 操作：

  * `UpsertMeta(meta)`
  * `MarkTransferProgress(item_id, …)`
  * `MarkCached(item_id, local_ref)`
  * `QueryPage(sort, filter, offset, limit)`

#### TransferStore

* Key：`transfer_id`
* 字段：item_id、progress、state、error、started_at
* 操作：

  * `Start(transfer_id, item_id)`
  * `UpdateProgress(transfer_id, p)`
  * `Complete(transfer_id)`
  * `Fail(transfer_id, err)`
  * `Cancel(transfer_id)`

#### PeerStore / StatusStore（同理）

* Online/Offline/Backoff、last_seen、display_name、trust_state

> 重要：Stores 是“状态”，EventPump 是“变化”。UI 页面只绑定 Stores；需要一次性事件（弹窗/提示音）时，再订阅 EventPump 的“UI 事件通道”。

### 5.2.3 事件关联与等待模式（用于 Lazy Fetch）

提供一个 `CoreAwaiter`（或 `TransferAwaiter`）帮助 VM 写出可控流程：

* `Task<LocalContentRef> WaitContentCachedAsync(transfer_id, timeout, cancellationToken)`
* 内部监听 `CONTENT_CACHED` 与 `TRANSFER_FAILED`
* 支持取消：用户点取消 → 调用 `cb_cancel_transfer` + 取消等待任务

这样 QuickPaste/详情页的“确保内容可用”就不需要自己写一堆事件匹配逻辑。


-----

## 5.3 剪贴板采集与入口策略层（ClipboardWatcher + Ingest Policy）

### 5.3.1 目标

把系统剪贴板变化转化为 Core 可同步的“本地复制事件”，并在 **进入 Core 之前**完成所有 UX/策略决策（limits、提示、local_only/force、忽略规则、节流）。

### 5.3.2 组件与职责边界

* **ClipboardWatcher（监听）**

  * 监听系统剪贴板变化事件
  * 异步读取剪贴板数据（文本/图片/文件列表）
  * 生成 `ClipboardSnapshot`（含 kind、size、preview、payload 引用）
  * 调用 `IngestPolicy.Decide(snapshot)` 产出 `IngestDecision`
  * 若决策允许：调用 `CoreHost.IngestLocalCopy(decision.snapshot_json)`
* **IngestPolicy（策略层）**

  * 依据 Settings + limits + 上下文（前一次写回、最近重复等）做决策
  * 输出三类结果：Allow（default/force/local_only）、AskUser、Deny
* **ClipboardWriter（写回）**

  * 把已准备好的本地内容写入系统剪贴板
  * 与 Watcher 共享回环防护机制（短期指纹忽略）

### 5.3.3 输入数据模型

`ClipboardSnapshot`（Shell 内部模型，供策略层决策）应至少包含：

* `Kind`: Text | Image | Files
* `EstimatedSizeBytes`
* `PreviewText`（文本截断/文件名摘要/图片尺寸）
* `Fingerprint`（用于去重与回环防护：hash/路径集合 hash 等）
* `CapturedAtUtc`
* `RawPayloadRef`（可选：图片临时编码路径、文件路径列表、文本本体）

### 5.3.4 策略输出模型

`IngestDecision`：

* `Action`: Allow | AskUser | Deny
* `ShareMode`: default | local_only | force（仅当 Allow/AskUser 选择后）
* `Reason`: OverLimit | Duplicate | SelfWriteback | Unsupported | UserDenied
* `RememberKey`（用于“记住选择”按类型/大小档位）

### 5.3.5 limits 与超限提示策略（必须落地）

#### 策略规则（默认值可调）

* Text：若 `size > TextSoftLimitBytes` → AskUser
* Image：若 `size > ImageSoftLimitBytes` → AskUser
* Files：若 `sum(size) > FileSoftLimitBytes` 或单文件超限 → AskUser

#### AskUser 的 UI 交互（最小可用）

提示必须包含：

* 内容类型与大小、预览摘要
* 三个动作：

  * “仅本机保存”（local_only）
  * “仍然同步”（force）
  * “取消”（Deny，不 ingest）
* 可选勾选：

  * “记住对此类型的选择”（写入 Settings）

> 约束：提示不阻塞消息线程；Watcher 只发起 UI 请求，决策通过异步回调返回。

### 5.3.6 去重与节流（必须写死的规则）

* **去重窗口**：同 fingerprint 在 `DedupWindowMs` 内重复出现 → Deny（Reason=Duplicate）
* **节流**：剪贴板变化过于频繁时（如 1 秒 > N 次）：

  * 先只保留最后一次事件
  * 或对 progress/频繁类型进行降采样
* **回环防护**（与 Writer 协作）

  * Writer 写回后记录 `lastWriteFingerprint + lastWriteAt`
  * Watcher 在短窗口内若命中 fingerprint → Deny（Reason=SelfWriteback）

### 5.3.7 Unsupported 类型的处理

当剪贴板内容不属于 v1 支持类型：

* Deny（Reason=Unsupported）
* 可选：写一条 Warn 日志（统一进入 Core 权威日志库）

### 5.3.8 验收标准与测试

* 文本复制：在 UI 历史中出现条目（以 meta 事件驱动为准）
* 超限：必定出现提示；选 local_only 不触发对外同步（以 Core 状态/事件为准）
* 写回剪贴板：不产生重复条目（回环防护有效）
* 连续复制压测：UI 不冻结；事件队列不爆；关键事件不丢

---

## 5.4 Quick Paste 呼出小窗

### 5.4.1 目标

提供一个可通过全局热键呼出的顶层小窗（类似 Win+V 的历史选择与粘贴），支持“键盘优先”的历史选择与粘贴；对大内容按 Lazy Fetch 拉取并显示进度，可取消。

### 5.4.2 交互规格（必须严格定义）

#### 呼出/关闭

* 热键：用户可配置；默认提供一个不与系统冲突的组合键
* 行为：Toggle（显示则隐藏，隐藏则显示）
* 关闭触发：

  * Esc
  * 失去焦点（Deactivated）
  * 点击窗外
  * 粘贴成功后可配置：自动关闭（默认关闭）

#### 键盘导航

* ↑/↓：选择条目
* Enter：执行“复制到系统剪贴板”（必要时先 EnsureCached）
* Ctrl+F：聚焦搜索框（可选）
* Tab：搜索框与列表切换焦点（可选）

### 5.4.3 窗口行为与样式约束

* 顶层窗口、无标题栏、圆角、阴影（符合现代 Windows）
* 不出现在任务栏，不进入 Alt-Tab（ToolWindow 行为）
* 显示时置顶（Topmost），隐藏时释放置顶
* 自动定位策略：以鼠标位置为锚点，避免遮挡光标

* 焦点恢复：

  * 显示前记录前台 hwnd
  * 关闭后尝试恢复前台（允许失败，但必须“尽力而为”）

### 5.4.4 数据源策略（快开窗 + 可搜索）

* 打开瞬间：使用 `HistoryStore` 的“最近 N 条”立即渲染（避免阻塞）
* 后台刷新：调用 `cb_list_history` 拉取补齐（可分页）
* 搜索：

  * 输入变化后 debounce（例如 200ms）
  * 调用 `cb_list_history(query_json)` 进行过滤
  * 搜索结果与 Store 结果合并规则必须明确（建议：搜索态只显示查询结果）

### 5.4.5 粘贴链路（EnsureCached → 写剪贴板）

QuickPaste 的执行链路不得单独实现，必须调用 `ApplySelectedToClipboard(item_id)`（见 5.7.8.2）。
行为约束：

* Enter：对当前选中条目调用 `ApplySelectedToClipboard(item_id)`
* NeedsDownload：显示下载进度与可取消（若核心支持 cancel）
* 成功写入剪贴板后：按设置决定是否自动关闭小窗；关闭后尽力恢复前台焦点
* 写入失败（剪贴板占用）：按统一命令的重试与提示规则执行，不得额外弹窗阻塞用户输入

#### 状态机（每条 item 的可见状态）

* Ready：可直接写剪贴板
* NeedsDownload：需要 EnsureCached
* Downloading：显示进度与取消
* Failed：显示错误摘要 + “重试”

#### 执行流程（Enter）

1. 若 Ready：ClipboardWriter 写入 → 成功后（可选）关闭小窗
2. 若 NeedsDownload：

* 调用 `cb_ensure_content_cached(req_json)` 得到 transfer_id
* 进入 Downloading，订阅 TransferStore 的进度
* 等待 `CONTENT_CACHED(transfer_id)`：

  * 成功：ClipboardWriter 写入
  * 失败：Failed（显示原因）
3. 取消：

* 调用 `cb_cancel_transfer(transfer_id)`
* UI 状态回到 NeedsDownload 或标记 Cancelled（v1 可并入 Failed/NeedsDownload）

#### 写剪贴板失败处理（占用/冲突）

* 自动重试（短退避，上限 2 秒）
* 仍失败：显示“剪贴板被占用”并提供“重试”按钮
* 必须保证：失败不触发重新下载（只重试写入）

### 5.4.6 UI 结构占位（留出设计空间）

QuickPasteWindow 建议拆为三块区域：

* 顶部：SearchBox + 小型状态指示（在线设备数/暂停监听）
* 中部：历史列表（每项：类型图标、预览、来源设备、时间、状态徽标）
* 底部：提示行（快捷键帮助、错误提示、下载进度条）

> 视觉与交互细节可迭代，但窗口行为/状态机/快捷键必须先固定。

### 5.4.7 验收标准与测试

* 热键稳定呼出/隐藏；失焦自动隐藏
* 搜索可用；列表滚动不卡顿（至少 500 条）
* Lazy Fetch 可显示进度并可取消
* 写回剪贴板不产生重复 ingest（回环防护验证）
* 多显示器定位正常（至少主副屏）

---

## 5.5 统一日志系统（Core 权威库 + Shell 统一入口）

### 5.5.1 目标

* Core 作为日志权威库：持久化、查询、tail、清理、统计
* Shell 统一使用 `ILogger` 写日志，最终落入 Core 日志库
* Logs 页面提供“排障级”管理能力：时间段检索、关键字过滤、导出、保留期清理

### 5.5.2 统一日志事件模型（Schema）

最少字段：

* `id`（单调递增）
* `ts_utc`
* `level`（Trace/Debug/Info/Warn/Error/Critical）
* `component`（Core/Shell）
* `category`（稳定前缀：Shell.Tray、Shell.Overlay、Core.Net…）
* `message`
* `exception`（可空）
* `props_json`（可空：transfer_id、item_id、peer_id、窗口状态等）

### 5.5.3 Shell 写入路径（ILogger → Core）

* 提供 `CoreLoggerProvider`（或 Serilog Sink）作为唯一入口
* Provider 内部必须：

  * 物化消息与异常（避免对象生命周期问题）
  * 入队到 `CoreLogDispatcher`（有界队列，丢弃策略仅允许丢 Trace/Debug）
  * 后台批量调用 `cb_logs_write`
* Core 不可用时的策略（v1 必须定死其一）：

  * 方案 A：丢弃 Shell 日志但记录计数并提示（简单）
  * 方案 B（推荐）：写一个 Shell fallback rolling file，仅用于 Core 未加载阶段排障

### 5.5.4 Logs 页面能力（管理功能）

* 时间范围查询：start/end + 分页
* Tail：after_id 增量追加
* 过滤：

  * 最小级别 level_min
  * 关键字 like（message/category/exception 统一语义）
* 管理：

  * 删除 N 天前日志（调用 `delete_before`）
  * 导出 CSV（对当前查询结果）
  * Stats：总条数、磁盘占用（如核心提供）

### 5.5.5 保留期策略（Retention）

* 设置项：`LogRetentionDays`
* 执行策略：

  * 应用启动后执行一次清理
  * 之后每日执行一次（或每次启动足够，v1 可选）
* 约束：清理必须只作用于 core_log_dir，不影响其它数据

### 5.5.6 验收

* Shell 与 Core 日志同屏可检索
* Tail 不卡 UI，持续 10 分钟无内存膨胀（设定最大缓存条数）
* 清理操作可重复执行且 stats 正确

-----

## 5.6 权限、自动启动与后台留存（Policy + UX）

### 5.6.1 目标

在不引入“不可控后台行为”的前提下，实现可诊断、可配置、可关闭的系统级运行能力：

* 后台留存（Close-to-tray）
* 全局热键（可配置、可冲突处理）
* 自动启动（尊重用户与系统禁用）
* 通知与提示（可降级）

### 5.6.2 权限与系统能力边界（v1 固定约束）

#### 剪贴板读写

* 不做运行时权限弹窗假设；以“失败可恢复”为策略核心：

  * 写入失败（占用）→ 重试 + 退避 + 明确提示
  * 读取失败（瞬时）→ 记录日志 + 延迟重试

#### 全局热键

* 必须处理“注册失败/被占用”：

  * UI 提示原因（可能被其它应用占用）
  * 提供“修改快捷键”入口
  * 不得静默失败

#### 通知（Toast/AppNotification）

* 通知可被用户在系统设置中关闭：

  * 检测不到/发送失败时，降级为应用内提示（InfoBar/状态条）
  * 不将通知作为关键流程的唯一通道（例如失败必须在 UI 可见）

### 5.6.3 后台留存（Close-to-tray）状态机

#### 用户可配置策略（Settings）

* `CloseBehavior`：

  * `MinimizeToTray`（默认）
  * `ExitApp`
* `ShowFirstCloseHint`：首次触发 MinimizeToTray 时是否提示

#### 行为规范

* 点击窗口关闭（X）：

  * 若 `MinimizeToTray`：取消关闭 → Hide 主窗口 → 保持进程存活与托盘
  * 若 `ExitApp`：走统一退出流程（见 5.6.6）
* 托盘行为：

  * 单击：可配置为显示/隐藏主窗口（默认显示）
  * 双击：显示主窗口（可选）

#### 体验要求

* 首次 Close-to-tray 必须提示用户发生了什么，并提供“下次直接退出”的设置入口
* 用户必须能明确退出（托盘菜单“退出”始终可见）

### 5.6.4 自动启动（Startup）策略

#### Settings

* `StartupEnabled`：用户开关
* `StartupState`：只读状态（Enabled/Disabled/DisabledByUser/NotSupported/Unknown）

#### 启用流程

* 用户打开开关时：

  * Shell 调用系统认可的启动机制请求启用
  * 根据返回状态更新 UI
  * 若状态为 DisabledByUser：提示“系统已禁用，请到启动应用/任务管理器启用”

#### 禁用流程

* 用户关闭开关时：

  * Shell 触发禁用（或标记为 Disabled）
  * UI 立即反映状态

#### 约束

* 必须尊重用户禁用（DisabledByUser 不得自动恢复）
* 不支持的环境（未打包/无 AppIdentity）必须给出明确状态与解释

### 5.6.5 热键管理（Hotkey Management）

#### Settings

* `HotkeyToggleQuickPaste`：快捷键定义（默认值产品化后确定）
* `HotkeyEnabled`：总开关

#### 注册与冲突处理

* 应用启动/设置变更时尝试注册
* 注册失败：

  * 状态条给出提示
  * 快捷键设置页显示冲突并要求用户修改
  * 不应频繁重试造成系统负担（指数退避或仅用户触发重试）

#### 生命周期

* 进入后台留存：热键保持有效
* 退出流程：注销热键，防止残留

### 5.6.6 统一退出流程（必须唯一）

退出触发来源：

* 托盘菜单“退出”
* 主窗口“退出”按钮（可选）
* CloseBehavior=ExitApp 时点 X

退出流程顺序（固定）：

1. 关闭 QuickPasteWindow（若打开）
2. 停止 ClipboardWatcher（避免退出时仍 ingest）
3. 注销 HotKey
4. Flush 并停止日志写入队列（CoreLogDispatcher 停止接收）
5. `CoreHost.Shutdown()`（确保写盘收尾）
6. 释放托盘资源并退出进程

验收：

* 退出后无后台残留进程
* 重启应用不出现“上次未正常关闭导致状态异常”（若出现必须可自恢复）

### 5.6.7 诊断与可见性（用户能理解）

* Settings 页面应显示：

  * CoreState（Ready/Degraded）
  * StartupState（Enabled/Disabled/DisabledByUser…）
  * HotkeyState（Registered/Conflict/Disabled）
  * 当前 data_dir/log_dir（只读）
* 提供“复制诊断信息”按钮：

  * 包含版本、路径、状态、最近错误摘要

### 5.6.8 验收与测试

* Close-to-tray：点 X 后仍可通过托盘恢复窗口，功能持续工作
* Exit：托盘退出后进程完全结束、资源释放
* 自启：开启后重启系统能自启动；用户禁用后应用不能强行恢复
* 热键：冲突必提示；改键后可恢复注册
* 通知禁用：关键提示仍可通过 UI 看见

---

## 5.7 UI 规划与页面规格

### 5.7.1 目标

将 v1 的 UI 以“可实现、可测试、可迭代设计”的方式规格化，统一信息架构与交互规则，并与 Stores 数据投影对齐。

### 5.7.2 信息架构（IA）

主窗口以左侧导航栏（NavigationView）组织：

* Home（主页）
* History（历史）
* Devices（设备）
* Logs（日志）
* Settings（设置）

QuickPasteWindow 独立于主窗口导航（顶层小窗），但共享 Stores 与命令。

### 5.7.3 主页页面规格（Home / Dashboard）

#### 5.7.3.1 目标

主页作为“剪贴板共享控制台（Dashboard）”，提供三类能力，并严格避免与 History/Devices/Settings 页面功能重复：

1. **核心交互优先**：以“最近元数据横向卡片条”为第一视觉焦点，支持“选中即写剪贴板”、自动跟随新元数据、锁定/删除等高频操作。
2. **可操作的文字摘要**：以文字指标呈现共享策略、缓存/传输/开关状态，并提供一键动作（暂停采集/暂停分享/清空缓存等）。
3. **图表化态势感知**：展示核心提供的缓存与网络占用随时间变化（轻量图表，便于判断系统是否正常工作）。

#### 5.7.3.2 页面布局（自上而下三段式）

主页采用固定三段式布局（自适应宽度，垂直滚动仅在小屏时启用）：

A) **顶部：Recent Items 横向卡片条（核心功能区）**
B) **中部：文字数据与操作区（状态与控制）**
C) **底部：图表区（Cache + Network）**

> 主页不展示日志摘要；日志相关信息仅在 Logs 页面用于调试与排障。

---

#### 5.7.3.3 顶部：Recent Items 横向卡片条（选中即写剪贴板）

##### 5.7.3.3.1 可见条目与“显示全部历史”

* 默认显示最近 **10** 个历史元数据条目（按时间倒序，最新在最前）。
* 横向列表尾部固定一个“查看更多历史…”卡片/按钮：

  * 点击后跳转到 `History` 页面，并定位到最新条目（顶端）。

##### 5.7.3.3.2 卡片信息结构（统一模板）

每张卡片必须包含以下信息（避免 UI 各处不一致）：

* **主区域（Preview）**

  * Text：显示前 1–2 行（80–120 字截断）
  * Image：显示缩略图占位（可先不做真实缩略图）+ 分辨率（如 1920×1080）
  * Files：显示首个文件名 + “+N”文件数摘要
* **副区域（Meta）**

  * 来源设备名（peer display name / alias）
  * 时间（相对时间，如 “2 min ago”，悬停显示绝对 UTC/本地时间）
* **状态区域（State badge）**

  * Ready / NeedsDownload / Downloading(含进度) / Failed(含短错误摘要)
  * 可选：大小（如 2.3 MB）

##### 5.7.3.3.3 卡片右上角操作按钮（Lock / Delete）

每张卡片右上角提供两个小按钮：

1. **Lock（锁定）**

  * 锁定后进入 `SelectionMode=Locked`：系统剪贴板始终保持锁定条目内容，不受新元数据影响。
  * 再次点击解除锁定：回到 `SelectionMode=FollowNewest`（默认行为，简单且符合直觉）。
  * 锁定状态在卡片上必须有明显视觉标识（锁图标高亮/徽标）。

2. **Delete（删除）**

  * 默认执行 **Local Delete（本机删除/隐藏）**：仅本机 UI 不再显示该条元数据；不影响其它设备。
  * 右键菜单（或二次确认）提供 **Delete everywhere（全局删除 / Tombstone）** 占位（vNext；v1 可不实现，但 UI/文档预留）。
  * 若被删除条目处于 Locked：

    * 自动解除锁定，并切换到最新条目（FollowNewest）并写剪贴板（确保状态一致）。

> 删除语义的 v1 目标：先实现“本机隐藏”，不引入跨设备一致性复杂度；全局删除作为未来里程碑采用 Tombstone + GC（详见后续删除体系章节）。

##### 5.7.3.3.4 选择模式（SelectionMode）与自动跟随规则

主页卡片条必须实现明确的选择模式，以定义“新元数据到达时是否抢占选中与剪贴板内容”。

* `FollowNewest`（默认）

  * 当前选中始终指向最新条目。
  * 新元数据到达：自动切换选中到新条目，并将新条目写入系统剪贴板。
* `Manual`（v1 可选，若不做则省略）

  * 用户显式选择非最新条目后进入。
  * 新元数据到达：列表更新但不抢占选中与剪贴板；可显示“New”提示，允许用户一键回到 FollowNewest。
* `Locked`

  * 用户锁定某条目进入。
  * 新元数据到达：不改变剪贴板内容；列表仍可更新并展示新条目，但选中保持锁定项。

v1 最小落地要求：

* 必须实现 `FollowNewest` 与 `Locked`。
* `Manual` 可作为 v1.1 增强；若暂不实现，则用户任何一次选择都视为 FollowNewest 的“最新提交”，并在新元数据到达时继续自动跟随。

##### 5.7.3.3.5 “选中即写剪贴板”的提交（Commit）规则（防抖与可控性）

为避免用户滑动过程中造成剪贴板高频抖动，规定“选中提交（commit）”时机：

* **触控/拖拽横向滑动**：在 `PointerUp`（松手）后，以“视口居中卡片”为最终选中，触发写剪贴板。
* **鼠标滚轮/触控板滚动**：滚动停止后 debounce 150–250ms，以“最接近居中的卡片”为选中，触发写剪贴板。
* **键盘左右键**：每次按键立即提交选中并写剪贴板。

提交选中的副作用：

* 更新选中视觉效果（Selected 状态）
* 触发“写剪贴板链路”（详见 5.7.3.3.6）

##### 5.7.3.3.6 写剪贴板链路（复用 QuickPaste：EnsureCached → ClipboardWriter）

主页卡片条写剪贴板必须复用 QuickPaste 的同一套命令与等待机制，避免逻辑分叉。

统一命令：`ApplySelectedToClipboard(item_id)`

执行规则：

1. 若条目状态为 `Ready`：直接调用 `ClipboardWriter.Write(item)` 写入系统剪贴板。
2. 若状态为 `NeedsDownload`：

  * 调用 Core：`cb_ensure_content_cached(item_id)` 获取 `transfer_id`
  * UI 状态进入 Downloading，并从 `TransferStore` 订阅进度
  * 等待 `CONTENT_CACHED(transfer_id)`：

    * 成功：写入系统剪贴板，条目状态变为 Ready
    * 失败：条目状态变为 Failed，保留“重试”入口（再次选中即可重试）
3. 若用户在 Downloading 过程中切换选中到另一个条目：

  * 若 Core 支持取消：调用 `cb_cancel_transfer(old_transfer_id)`，再对新条目执行 ensure
  * 若不支持取消：允许旧 transfer 在后台完成，但 UI 仅跟随当前选中条目展示进度（避免混乱）

写入失败处理（剪贴板被占用）：

* 自动重试（短退避，上限 2 秒）
* 仍失败：在中部文字区显示非阻塞提示（如“剪贴板被占用，稍后重试或再次选择”），并记录日志（进入 Core 权威日志库）。

##### 5.7.3.3.7 新元数据到达时的自动写剪贴板规则

当 Core 产生新元数据（新条目进入 HistoryStore）时：

* 若 `SelectionMode=FollowNewest`：自动选中新条目并触发 `ApplySelectedToClipboard(new_item_id)`
* 若 `SelectionMode=Locked`：只更新列表，不触发写剪贴板

---

#### 5.7.3.4 中部：文字数据与操作区（状态与控制）

##### 5.7.3.4.1 必要状态字段（必须可见）

中部区域用于呈现“与控制直接相关”的文字状态，至少包含：

* Sharing（共享策略摘要）

  * 当前启用共享的目标设备数（Outbound）/允许接收的设备数（Inbound）
  * 共享模式说明：v1 为“设备 allowlist（默认圈子 Default 的子集）”；vNext 支持 Sharing Circles（伏笔）
* Local Resources（本机资源）

  * Cache 当前大小（来自 Core stats）
  * Active transfers 数量（来自 TransferStore）
* Toggles（全局开关）

  * Clipboard Capture：ON/OFF（是否读取系统剪贴板并生成元数据）
  * Clipboard Sharing：ON/OFF（是否接受与广播元数据）

> 主页不展示日志统计与最近错误；相关信息仅在 Logs 页面查看。

##### 5.7.3.4.2 必要操作按钮（v1 必做）

主页必须提供以下按钮（按钮可放在对应文字指标旁或统一操作区）：

1. **Pause/Resume Clipboard Capture（停止/继续管理剪贴板）**

  * OFF：停止 ClipboardWatcher；不再从系统复制生成元数据
  * ON：恢复 ClipboardWatcher

2. **Pause/Resume Clipboard Sharing（停止/继续分享剪贴板）**

  * OFF：

    * 不广播本机新元数据
    * 不接受远端元数据（不写入历史/不写剪贴板）
    * 可保持设备发现（可选）但不建立传输
  * ON：恢复接受与广播

3. **Clear Cache（清空缓存）**

  * 调用 Core cache 清理能力（v1 若仅提供 prune，则执行 prune；若提供 clear，则执行 clear）
  * 操作需二次确认（避免误触）
  * 执行期间展示进度/忙碌态（非阻塞）

4. （可选）Open QuickPaste

  * 打开 QuickPasteWindow（等同热键）

##### 5.7.3.4.3 Share Targets 管理（v1：设备表 In/Out allowlist）

主页 Share Targets 区域仅展示摘要与列表，不提供额外按钮（符合你的要求），管理通过右键菜单进行：

对每个 peer 的右键菜单（v1）：

* Toggle：**Share to this device（Outbound allow）**
* Toggle：**Accept from this device（Inbound allow）**
* Copy device info（peer_id / name / last_seen）

文档伏笔（vNext，不实现）：

* Sharing Circles：Devices 页面提供图形化圈子编辑；v1 allowlist 等价于隐式圈子 `Default` 的成员集合。

---

#### 5.7.3.5 底部：图表区

##### 5.7.3.5.1 图表清单（v1 必做）

底部提供图表，用于“态势感知”，不承担复杂分析：

1. **Cache usage over time**

  * X 轴：时间
  * Y 轴：缓存占用（bytes / MB / GB）
2. **Network throughput (bytes over time)**

  * X 轴：时间
  * Y 轴：数据量（bytes per bucket 或 bytes/sec）

Network 占用图数据必须由 Core 提供权威统计接口（示例：`cb_net_stats_query(start_ts, end_ts, bucket_sec)` 返回按时间桶的 `bytes_sent/bytes_recv`），Shell 仅负责拉取与渲染，不做推断统计。

3. **设备连接列表**

  * 展示目前所找到的所有设备，并展示设备状况（是否接受分享，是否向其分享）
  * 右键对应的设备会弹出菜单
    * Toggle: Share to this device
    * Toggle: Accept from this device
    * Pause this peer for 1h / 24h
    * Copy device info（peer_id / name）
    * Rename alias（仅本机别名，不影响对方）
4. **全局复制活动**

  * 横轴：时间（从24小时前到现在，后期可以做一个下拉框选择横轴时间）
  * 纵轴：过去 24 小时新增元数据数（三个折线：text/image/files）
##### 5.7.3.5.2 数据来源（必须由 Core 提供）

图表数据必须来自 Core（权威数据源），Shell 不做推断统计，以保证跨平台一致与准确性。

建议 Core 提供接口（示例契约，最终以核心实现为准）：

* `cb_cache_stats_query(start_ts, end_ts, bucket_sec) -> series[]`
* `cb_net_stats_query(start_ts, end_ts, bucket_sec) -> series[]`

  * 每个 bucket 至少含：`bytes_sent`, `bytes_recv`（可选：meta/content 拆分）

Shell 仅负责：

* 周期性拉取（例如每 5–10 秒刷新一次最近 30–60 分钟窗口）
* 绑定到图表控件渲染
* 允许用户切换时间窗口（可选：15m/1h/24h）

---

#### 5.7.3.6 依赖与实现约束（工程落地要求）

* 主页所有业务动作不得直接调用 P/Invoke；必须通过 Service/Command（CoreHost/Stores/Policy）层完成。
* 主页写剪贴板链路必须复用 QuickPaste 同一套命令与 awaiter（EnsureCached/TransferStore/CONTENT_CACHED）。
* 主页卡片条在高频更新（新元数据持续进入）时不允许卡顿：

  * 列表更新必须增量（最多替换前 10 条可见窗口）
  * UI 线程仅做最小 diff 更新；重计算/统计放后台任务

---

#### 5.7.3.7 验收标准（v1）

1. Recent Items 显示最近 10 条；末尾“查看更多历史”跳转正常。
2. 在 `FollowNewest` 下，新元数据到达后自动选中并写入系统剪贴板，选中效果正确。
3. 锁定某条后进入 `Locked`，新元数据到达不改变剪贴板内容；解除锁定后恢复跟随最新。
4. 选中 `NeedsDownload` 条目会触发 EnsureCached，显示下载进度，完成后写剪贴板；切换选中可取消或正确忽略旧进度。
5. 删除（Local）后条目从主页消失；若删除锁定项则自动解除锁定并选中新条目。
6. “停止/继续管理剪贴板”能暂停/恢复本机采集；“停止/继续分享剪贴板”能暂停/恢复接收与广播。
7. Cache 清理按钮可用且有二次确认；执行过程中 UI 有忙碌态反馈。
8. 底部两张图表可展示随时间变化数据，数据来自 Core 接口，刷新不阻塞 UI。

### 5.7.4 历史页面规格（History）

#### 5.7.4.1 目标

History 页面提供“全量历史浏览 + 精准定位 + 管理动作”，是 Home（最近 10 条）与 QuickPaste（临时呼出）之外的主入口。History 必须与 Home/QuickPaste 共享同一套条目状态机与操作命令，避免三套逻辑不一致。

#### 5.7.4.2 列表展示与虚拟化分页（UI Virtualization）

* **列表控件**：使用 WinUI 3 `ListView` 配合 `ItemsStackPanel`，必须开启 UI 虚拟化（默认行为），禁止将 ListView 放入 ScrollViewer 或 StackPanel 等无限高度容器中。
* **数据源实现**：Shell 侧集合必须实现 `ISupportIncrementalLoading` 接口，支持滚动到底部自动触发加载。
* **分页协议（核心约束）**：
* 采用 **游标分页（Cursor-based / Keyset Pagination）**，严禁使用 Offset 页码分页。
* 分页参数：`limit`（建议 50） + `cursor`（上一页最后一条的 `sort_ts_ms`）。
* Core 行为：执行 `WHERE sort_ts_ms < cursor` 查询。这确保了当新数据插入顶部时，底部的分页“锚点”不会偏移，杜绝数据重复或漏读。



#### 5.7.4.3 实时数据流策略（智能静默插入）

针对用户浏览过程中 `ITEM_META_ADDED` 新数据到达的场景，采用 **“智能静默插入（Smart Silent Insertion）”** 策略，以消除列表抖动（Scroll Jitter）：

* **交互逻辑**：
1. **浏览态（User is scrolling）**：当列表滚动条不在顶部（`VerticalOffset > 0`）时，新数据静默插入底层集合，但 **强制保持视口锚定（Scroll Anchoring）**。用户当前看到的条目位置保持像素级不变，不会被新数据“挤”下去。
2. **监控态（User is at top）**：当列表紧贴顶部（`VerticalOffset ≈ 0`）时，新数据插入后自然展示，列表内容下推，让用户实时感知最新动态。


* **技术实现约束**：
* 必须配置 `ItemsStackPanel.ItemsUpdatingScrollMode = KeepItemsInView`。
* 必须在 UI 线程（Dispatcher）将新 Item 插入到集合头部（Index 0），利用 WinUI 的锚定机制处理视觉位置。

#### 5.7.4.4 选中与写剪贴板行为（与主页一致）

History 的“选中写剪贴板”必须提供两种模式（由 Settings 控制）：

* `HistorySelectionWritesClipboard = false`（默认建议）

  * 单击只选中与显示详情，不写剪贴板
  * 通过显式动作写剪贴板（双击/按钮）
* `HistorySelectionWritesClipboard = true`（与你主页一致的体验）

  * 单击选中即触发 `ApplySelectedToClipboard(item_id)`（同主页链路）
  * 为避免误触，可增加 150ms debounce（仅对鼠标滚动/快速点击）

> 主页强制“选中即写”，History 建议默认不写，以避免用户浏览时频繁污染剪贴板；但文档必须允许用户统一成同一体验。

#### 5.7.4.5 条目操作（Lock / Delete / Copy）

每个条目提供以下动作（按钮/右键均可）：

* **Copy to clipboard**

  * 统一调用 `ApplySelectedToClipboard(item_id)`（详见 5.7.8）
* **Lock**

  * 统一进入 `SelectionMode=Locked(item_id)`，并在 Home 顶部卡片条反映锁定态
  * 锁定时剪贴板保持该内容，不受新元数据影响
* **Delete（v1：本机隐藏）**

  * 调用 `DeleteLocal(item_id)`，使条目在本机所有 UI（Home/History/QuickPaste）不可见
  * 若删除的是 Locked 项：自动解除锁定并回到 FollowNewest
* **Delete everywhere（vNext 占位）**

  * 未来采用 tombstone，同步到其它设备并触发 GC（v1 不实现，仅预留）

#### 5.7.4.6 过滤与搜索（v1 最小集）

* 关键字搜索（匹配 preview/文件名摘要/来源设备名）
* 类型过滤（Text/Image/Files）
* 状态过滤（NeedsDownload/Downloading/Failed）
* 时间范围（可选）

#### 5.7.4.7 验收标准（v1）

1. 全量历史可分页加载，滚动不卡顿。
2. Copy/Lock/DeleteLocal 与主页语义一致，且跨页面联动正确（Home 顶部卡片条反映锁定/删除后状态）。
3. NeedsDownload 条目可触发 EnsureCached、展示进度、完成后可写剪贴板。

---

### 5.7.5 设备页面规格（Devices）与分享策略（v1 + vNext 伏笔）

#### 5.7.5.1 目标

Devices 页面负责“设备发现与共享策略管理”。v1 采用“每设备 Inbound/Outbound allowlist”落地；文档预留 vNext 的“分享圈（Sharing Circles）”升级路径。

#### 5.7.5.2 v1 页面结构（列表 + 右键管理）

* 展示所有检测到的设备（包含非本账号设备），并标注：

  * 设备名/别名、在线状态（Online/Offline/Backoff）、最后见到时间、peer_id（可折叠显示）
* 页面顶部显示共享摘要：

  * Outbound allowed count（允许向其分享的设备数量）
  * Inbound allowed count（允许接收其分享的设备数量）

#### 5.7.5.3 分享策略（v1：allowlist；vNext：Sharing Circles 预留）

v1 采用每设备 allowlist 的最小落地模型，对每个 peer 维护两个布尔策略：

* `ShareToPeer`（Outbound allow）：为 false 时，本机不向该 peer 广播元数据/内容
* `AcceptFromPeer`（Inbound allow）：为 false 时，本机不接收/落库/展示来自该 peer 的元数据

管理方式（v1）：

* Devices 页面对每个 peer 提供右键菜单切换 Outbound/Inbound 开关（主页仅展示摘要，不提供按钮）。

vNext 预留：Sharing Circles（分享圈）

* 未来允许用户创建多个“圈子（Circle）”，把设备拖入圈子实现“圈内共享、圈间隔离”；设备可属于多个圈子以桥接共享。
* v1 allowlist 等价于隐式圈子 `Default` 的成员集合与策略子集，保证升级时不推翻现有配置。


#### 5.7.5.4 设备右键菜单（v1 必做）

对每个设备提供右键菜单：

* Toggle：Share to this device（Outbound）
* Toggle：Accept from this device（Inbound）
* Copy device info（peer_id / name / last_seen）
* Set local alias（可选，仅本机显示名）

> Home 的 Share Targets 区域仅展示摘要与列表，不提供按钮；实际管理入口在 Devices（右键/详情面板）。

#### 5.7.5.5 vNext：分享圈（Sharing Circles）伏笔（不实现，仅定义）

为避免未来推翻 v1，文档预定义 Sharing Circles 概念：

* 用户可创建多个圈子（Circle），每个圈子包含若干设备成员。
* **圈子内共享，圈子间隔离**：不同圈子成员默认不互通；设备可属于多个圈子以桥接共享。
* v1 allowlist 等价于隐式圈子 `Default` 的成员集合与策略子集。

建议的数据结构预留（示意）：

```json
"sharing": {
  "mode": "peer_allowlist",
  "circles": [
    { "id": "default", "name": "Default", "members": ["peerA","peerB"] }
  ]
}
```

图形化 UI（设想）：

* 设备图谱 + 拖拽加入圈子（环形/气泡布局）
* 圈子创建/重命名/删除
* 设备可拖入多个圈子

#### 5.7.5.6 验收标准（v1）

1. 设备发现与状态变化可见，右键可切换 In/Out。
2. 切换策略后立即生效：Inbound 关闭后不再接收该 peer 元数据；Outbound 关闭后不再向其发送。
3. Home Share Targets 展示与 Devices 策略一致。

---

### 5.7.6 设置页面规格（Settings）

#### 5.7.6.1 目标

Settings 页面集中管理所有“策略性开关与阈值”，并明确区分：

* 采集（Capture）：是否从系统剪贴板读取并生成元数据
* 分享（Sharing）：是否接收/广播元数据（网络层）
* limits：超限提示与默认动作
* 快捷键/后台留存/自启：系统集成行为

#### 5.7.6.2 必要分组（v1）

Settings 必须提供两个互相独立的全局开关，并与主页中部按钮双向同步（同一状态源）：

1. `Clipboard Capture`（采集开关）

* OFF：停止从系统剪贴板读取并生成元数据（停止 ClipboardWatcher）
* ON：恢复采集

2. `Clipboard Sharing`（分享开关）

* OFF：不广播本机元数据；不接收远端元数据（不落库/不展示/不写剪贴板）；可选保持设备发现
* ON：恢复接收与广播

约束：

* 两个开关必须相互独立（允许“只采集不分享”或“只分享但不采集”——后者通常用于只接收）


* **Limits / Policy**

  * Text/Image/Files soft limits
  * 超限默认动作（Ask / Default local_only / Default force）
* **QuickPaste**

  * 热键设置与启用（冲突提示见 5.6）
* **Home / History 行为**

  * `HomeSelectionMode` 默认 FollowNewest
  * `HistorySelectionWritesClipboard`（建议默认 false，可改 true）
* **Cache**

  * Cache 上限（可选）
  * Prune 策略（保留期/最大占用）
* **Diagnostics（只读）**

  * CoreState、版本号、data_dir/log_dir、复制诊断信息

---

### 5.7.7 日志页面规格（Logs）

> 本节保留为调试用途，不出现在主页摘要中，但必须具备工程排障能力。

#### 5.7.7.1 目标

提供 Core 权威日志库的查询、tail、过滤、导出与保留期清理能力；Shell 通过 ILogger 写入同一库（见 5.6 与日志系统章节）。

#### 5.7.7.2 核心能力（v1）

* 时间范围查询（start/end）
* level_min 过滤
* 关键字 like
* after_id tail（增量追加）
* 删除 N 天前（retention）
* 导出 CSV（当前查询结果）

---

### 5.7.8 统一命令与语义（跨 Home / History / QuickPaste 一致性）

> 这一节非常关键，用于防止三处 UI 行为不一致。建议写在 UI 章节末尾并作为实现约束。

#### 5.7.8.1 统一状态机（ItemState）

所有页面对条目状态的显示与行为必须一致：

* `Ready`：可直接写剪贴板
* `NeedsDownload`：需 EnsureCached
* `Downloading(progress)`：显示进度，可取消（若核心支持）
* `Failed(error)`：可重试（再次选中/Copy）

#### 5.7.8.2 统一写剪贴板命令

命令名（建议）：

* `ApplySelectedToClipboard(item_id)`

语义（必须一致）：

1. Ready → ClipboardWriter.Write
2. NeedsDownload → Core.EnsureCached → 等 CONTENT_CACHED → ClipboardWriter.Write
3. 写入失败（剪贴板占用）→ 短退避重试（<=2 秒）→ 非阻塞提示

#### 5.7.8.3 统一锁定语义

* `Lock(item_id)`：进入 `SelectionMode=Locked(item_id)`
* Locked 期间：

  * 新元数据到达不抢占剪贴板
  * 删除 Locked 项会自动解除锁定并回到 FollowNewest

#### 5.7.8.4 统一删除语义（v1）

* `DeleteLocal(item_id)`：本机隐藏，不广播、不影响其它设备
* `DeleteEverywhere(item_id)`：vNext（tombstone + GC），v1 仅占位

---


### 5.7.5 Devices 页面规格

* 列表字段：

  * 设备名、状态（Online/Offline/Backoff）、最后见到
* 详情（可选）：

  * device_id、版本、tag
  * 信任状态与操作（若核心支持）
* 操作：

  * 刷新（若核心提供）
  * 复制设备信息（用于支持）

### 5.7.6 Settings 页面规格（策略集中地）

Settings 分组：

* General

  * CloseBehavior（MinimizeToTray/Exit）
  * StartupEnabled（含状态解释）
  * Hotkey（编辑/启用/冲突提示）
* Sync Policy

  * limits（Text/Image/Files soft limits）
  * 超限默认动作（询问/默认仅本机/默认强制同步）
  * 点击条目行为（自动拉取并复制/仅手动）
* Privacy

  * 暂停监听（Toggle）
  * 仅本机默认（可选：全局开关）
* Logs

  * LogRetentionDays
  * 最小日志级别（可选：仅影响 Shell 侧过滤输出）

### 5.7.7 Logs 页面规格（统一日志中心）

#### 查询

* 时间范围 start/end
* 级别 level_min
* like 关键字
* 分页 offset/limit
* Tail：after_id 增量

#### 操作

* 清理：删除 N 天前
* 导出：CSV（当前查询结果）
* 统计：总条数、磁盘占用（若核心提供）
* 跳转联动：

  * 从状态条/错误提示点击进入 Logs 并预填时间范围（例如最近 10 分钟）

### 5.7.8 统一语义与命令（Home / History / QuickPaste 必须一致）

#### 5.7.8.1 统一条目状态机（ItemState）

所有 UI（主页卡片条、History 列表、QuickPaste 列表）对条目状态显示与行为必须一致，状态集合固定为：

* `Ready`：内容已在本机可用，可直接写入系统剪贴板
* `NeedsDownload`：内容不在本机，需要触发 `EnsureCached`
* `Downloading(progress)`：下载中，显示进度；允许取消（若核心支持）
* `Failed(error)`：失败态，显示短错误摘要；允许重试（再次选中/再次执行 Copy）

约束：

* UI 不得各自引入额外状态名（避免漂移）
* 同一 `item_id` 的状态由 Stores 投影为唯一真相源

#### 5.7.8.2 统一写剪贴板命令（选中即写 / Copy 按钮共用）

统一命令（建议命名）：

* `ApplySelectedToClipboard(item_id)`

语义固定为：

1. 若 `Ready`：调用 `ClipboardWriter.Write(item)` 写入系统剪贴板
2. 若 `NeedsDownload`：调用 Core `cb_ensure_content_cached(item_id)` 得到 `transfer_id`，进入 `Downloading` 并订阅 `TransferStore`；等待 `CONTENT_CACHED(transfer_id)` 后再写入剪贴板
3. 写入失败（剪贴板被占用）：短退避重试（总时长不超过 2 秒），仍失败则给出**非阻塞提示**并允许用户再次触发

约束：

* Home 的“选中即写”、History 的“Copy”、QuickPaste 的“Enter”必须调用同一命令，不得三套实现

#### 5.7.8.3 统一锁定语义（Lock）

* `Lock(item_id)`：进入 `SelectionMode=Locked(item_id)`
* Locked 期间：新元数据到达不抢占剪贴板内容
* 解锁：回到 `SelectionMode=FollowNewest`

约束：

* 删除 Locked 项时必须自动解除锁定并回到 FollowNewest（避免“锁定指向不存在条目”）

#### 5.7.8.4 统一删除语义（v1：本机隐藏；vNext：全局 Tombstone）

* `DeleteLocal(item_id)`：仅本机隐藏，不广播、不影响其它设备
* `DeleteEverywhere(item_id)`：vNext 占位（tombstone + GC），v1 不实现但可在 UI 预留入口（右键/二级确认）


### 5.7.9 QuickPasteWindow UI 规格（与 5.4 对齐）

* 顶部搜索框（可选）
* 列表：最近 N 条 + 状态徽标 + 进度条
* 底部提示：快捷键帮助、错误提示、取消按钮（Downloading 时）

### 5.7.10 验收与测试

* UI 与 Stores 解耦：页面不直接调用 FFI（只能调用 Service/VM 命令）
* 快速打开：History 页面首屏 < 200ms（使用 Store 现有数据）
* Lazy Fetch：进度与取消可用，错误可解释
* 全局状态条信息准确并可联动跳转

---

## 5.8 打包发布、版本管理与回归测试

### 5.8.1 目标

形成可安装、可升级、可回归验证的发布链路，确保系统集成功能（托盘/热键/自启/剪贴板）在 Release 环境稳定。

### 5.8.2 打包形态

* v1 默认：MSIX（获得 AppIdentity，便于通知、自启、升级）
* 调试形态：

  * Debug（不强依赖自启）
  * Release（完整功能验证）

### 5.8.3 版本号与兼容策略

* Shell 版本号与 Core 版本号均应展示在 About/Diagnostics
* 兼容性策略：

  * Shell 检测 Core ABI 版本（或导出符号版本）不匹配时进入 Degraded 并提示升级

### 5.8.4 发布工件

* MSIX 安装包
* 变更日志（Changelog）
* 诊断导出（日志导出 + 配置导出）用于用户反馈

### 5.8.5 回归测试清单（最低集）

#### 生命周期

* 启动 → 初始化 Core → 退出（50 次循环）
* Close-to-tray → 恢复 → 退出

#### 剪贴板

* 文本复制 → 历史出现 → QuickPaste 写回
* 超限提示 → local_only/force/取消
* 写回不回环

#### QuickPaste

* 热键呼出/隐藏
* 选择条目粘贴（Ready 与 NeedsDownload 两类）
* 取消传输
* 写剪贴板被占用时重试与提示

#### 日志

* Shell ILogger 写入 → LogsPage 可检索
* 时间范围查询/关键字过滤
* 清理 N 天前日志 + stats 正确

#### 系统集成

* 热键冲突提示
* 自启启用/禁用状态正确（若环境支持）
* 通知禁用时降级提示仍可见

### 5.8.6 验收标准

* Release 包在干净系统安装后可完成全套回归测试
* 卸载不残留后台进程；数据目录按策略处理（保留/清理规则可后续定义）
* 用户可通过“复制诊断信息 + 导出日志”提交有效问题报告

------
