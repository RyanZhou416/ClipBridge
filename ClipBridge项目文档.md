# ClipBridge 项目文档

# 0) 项目简介

- **名称**：ClipBridge（简称 **CB**）
- **定位**：跨平台剪贴板同步工具，先 **局域网 (LAN)**，后期支持 **跨外网 (WAN)**。
- **核心卖点**：**Lazy Fetch**——复制时只广播**元数据**；正文按需拉取并落入本机缓存（CAS）。外壳可在收到元数据后对“小文本”主动发起 `ensure_content_cached` 作为体验优化（v1 默认开启，触发点仍由外壳控制）。
- **目标平台**：Windows（先做外壳 MVP）→ Android（外壳）→ v1 核心（剪贴板历史与同步）→ macOS / Linux（外壳）。

## 0.1 Core FFI 构建命令规范
本节记录 ClipBridge Core 在 **Windows** 与 **Android** 平台上的标准导出（FFI）构建方式，用于外壳（Shell）与核心（Core）之间的 ABI 通信。

### 0.1.1 Windows 平台（Windows Shell 使用）

#### 0.1.1.1 前置条件

* 已安装 Rust（MSVC toolchain）
* 在 Windows 环境下构建（PowerShell / CMD 均可）
* 使用仓库内的 `platforms/windows/core-ffi`

#### 0.1.1.2 构建命令（Release）


`cargo build -p core-ffi-windows --release --target x86_64-pc-windows-msvc`


#### 0.1.1.3 产物位置

Release 构建完成后，DLL 位于：

```
target\release\core_ffi_windows.dll
```

（Rust 会将 crate 名 `core-ffi-windows` 转换为 `core_ffi_windows.dll`）

#### 0.1.1.4 说明

* 该 DLL 导出 **C ABI**（`cb_init / cb_ingest_local_copy / cb_ensure_content_cached / cb_shutdown / cb_free_string` 等）
* Windows Shell 通过 `clipbridge_core.h` + P/Invoke / native binding 调用
* 所有 API 使用 **JSON 字符串作为输入/输出**，事件通过回调函数返回 JSON

---

### 0.1.2 Android 平台

#### 0.1.2.1 前置条件

* 已安装 Rust
* 已安装 Android NDK
* 已配置环境变量（直接运行）：`.\scripts\setup-ndk.ps1`


`cargo install cargo-ndk`

* 已添加 Android Rust targets：

`rustup target add aarch64-linux-android x86_64-linux-android`

---

#### 0.1.2.2 构建 arm64

在**仓库根目录**执行：

```
cargo ndk -t arm64-v8a build -p core-ffi-android --release
```

#### 0.1.2.3 构建 x86_64

```
cargo ndk -t x86_64 build -p core-ffi-android --release
```

---

#### 0.1.2.4 产物位置

构建完成后，会生成如下文件：

arm64:
`
target/aarch64-linux-android/release/libcore_ffi_android.so
`

x86_64:
``

说明：

* crate 名 `core-ffi-android` → `libcore_ffi_android.so`
* Android Java 侧通过：

```java
System.loadLibrary("core_ffi_android");
```

加载该库（不带 `lib` 前缀和 `.so` 后缀）

---

#### 0.1.2.5 说明

* Android 版 core-ffi 与 Windows 版 **使用同一套 C ABI 与 JSON 协议**
* Android Shell 通过 **JNI** 调用这些 C ABI 函数
* Core 事件通过 `on_event(const char* json, void* user_data)` 回调进入 Java 层
* Java 层需立即拷贝 JSON 字符串（回调期间指针有效）

------

# 1) 功能与阶段目标

## v1（Windows MVP + 基础核心）

- 局域网自动发现
- 端到端加密连接
- 同局域网内多设备同账号之间共享剪切板
- **Lazy Fetch**：复制时广播“元数据”；图片/文件默认在需要粘贴时拉取正文；小文本可由外壳在收到元数据后主动调用 `ensure_content_cached` 进行预取以降低粘贴等待（触发点由外壳决定，Core 不自行预取）。
- **剪贴板历史**：本地持久化（SQLite/KV），在各设备同步**历史元数据**；
- 支持类型：**文本**、**图片**、**文件**（先不管应用内复制和特殊格式）
- Windows 外壳：托盘图标、主窗口、呼出小窗
- 全局热键：默认 **Ctrl+Shift+V**（Win+V 为系统保留）

## v2（核心升级 + 历史）

- Android外壳
- 添加云端账号，可以云端同步剪切板
- 定向传输文件

## 未来

- macOS / Linux 外壳
- 同账号内设备分组共享

------

# 2) 功能细节

## 2.1 Core / Shell 分工

### 2.1.1 Rust 内核（Core）——“网络 + 协议 + 数据权威层”

Core 负责所有**跨平台一致**、与 UI 无关的能力，并且是“设备/会话/数据”的唯一权威来源：

* **局域网发现（Discovery）**

    * mDNS 广播与扫描（服务发布、TXT 解析、设备列表维护）
    * 设备在线/离线判定（心跳/超时/去抖）

* **连接与会话（Transport & Session）**

    * 建立/维护局域网连接：优先 QUIC（失败可降级 TCP）
    * 会话生命周期：握手、重连、并发流、超时、取消、流控
    * 传输进度与错误归一化（统一回调给壳）

* **安全与信任（Identity & Trust）**

    * 生成/持有设备身份（device_id + keypair）
    * 配对流程状态机（未配对/待确认/已信任/已撤销）
    * 连接加密（TLS 1.3 / rustls），证书/公钥指纹 pinning
    * 信任库的逻辑管理（具体密钥落地存储可由壳提供）

* **协议与数据同步（Protocol）**

    * 元数据广播/点对点同步（ItemMeta）
    * Lazy Fetch：按需向源设备拉取正文（文本/图片/文件）
    * 请求路由：处理其他设备的内容请求并回传数据流
    * 版本协商：`protocol_version`、向后兼容解析

* **历史/数据库/缓存（Persistence）**

    * SQLite / KV：devices / items / history / cache 索引
    * 内容缓存 CAS（按 sha256 存 blob 文件），DB 仅存引用与状态
    * 清理策略：容量上限 / TTL / LRU / 历史上限
    * 去重（同 hash 内容不重复缓存、不重复广播）

* **对外接口（Core API）**

    * 向壳暴露稳定 API（FFI / callback）
    * 以事件驱动方式通知壳：设备上下线、新元数据、传输进度、错误等

* Core 不会因接收到 ItemMeta 或 UI 状态变化而自动拉取正文内容；
  所有正文获取（文本 / 图片 / 文件）均由 Shell 显式请求触发。

### 2.1.2 平台外壳（Shell）

Shell 负责所有**强依赖平台 API / 权限 / UI 交互**的能力，不直接实现网络协议，只通过 Core API 与内核交互：

* **系统剪贴板集成**

  * 监听系统剪贴板变化（copy），并把“本机新复制内容”提交给 Core（作为新的 item/meta）
  * 将 Core 的“远端条目/历史条目”写入系统剪贴板（paste 之前确保数据可用）

  * **粘贴触发时的数据就绪策略（v1 实现级约束）**
    * v1 默认采用 **Eager Write（选中即写剪贴板）**：用户在 UI（历史面板/小窗）选中某条内容时：
      1) Shell 向 Core 发起 `FetchContent(item_id, mime/part)`（若本地缓存未命中）
      2) Core 拉取并落入本机缓存（CAS），返回“已就绪/本地路径或 bytes 句柄”
      3) Shell 将真实数据写入系统剪贴板（文本/图片 bytes；文件为本地临时落盘路径列表）
    * 用户随后按下 Ctrl+V 时，系统直接从剪贴板读取真实数据，不依赖 CB 在“按键瞬间”参与时序。

  * **Win32 Delayed Rendering（可选优化，不作为 v1 前置条件）**
    * 仅在需要“先把占位写入剪贴板，但数据要等到真正粘贴时才生成/拉取”的场景使用；
      例如：未来做“一键粘贴/自动粘贴”、或需要把超大内容的生成延后到 paste 请求发生的那一刻。
    * 若启用：Shell 可把 DataObject 以延迟渲染形式注册；当目标应用请求某格式数据时，
      Shell 再向 Core 拉取/解码并即时提供。该机制属于 **Windows 平台增强**，不得影响跨平台核心协议。


* **本地内容提供者（Local Content Provider）**

    * 当 Core 需要发送正文时，按需从系统取内容：

        * 文本：读取字符串
        * 图片：导出指定格式 bytes
        * 文件：读取文件 bytes / 打开文件流（受权限限制时由壳处理授权）
    * 对于“无稳定路径”的内容（如临时图片/截图），由壳先落盘到受控目录，再把引用交给 Core

* **系统能力与权限**

    * 系统安全存储：Keychain / Credential Manager / Android Keystore（供 Core 读写密钥/配对信息）
    * 开机启动、后台常驻策略、通知（Android 前台服务等）
    * 网络权限/本地网络权限申请与提示（平台差异由壳处理）

* **UI 与交互**

    * 托盘图标、主窗口、历史面板、小窗、设置页
    * 全局热键（如 Ctrl+Shift+V）、快捷操作
    * 配对确认 UI：展示对端设备信息与指纹，让用户确认/拒绝
    * 错误提示与状态展示（在线设备、传输进度、失败原因）

* **与 Core 的交互原则**

    * Shell 不维护“设备权威列表”和“会话状态”，只消费 Core 事件
    * Shell 不实现协议细节（广播/拉取/重试/流控），只调用 Core API。

* Shell 负责决定正文拉取时机：
  * 接收远端 ItemMeta 时，仅入库与展示，不自动拉取正文；
  * 用户在 UI（主页 / 历史 / 小窗）选中条目时，可根据类型与大小策略触发正文拉取；
  * 在执行粘贴 / 保存 / 导出前，必须确保对应内容已在本机可用。

---

## 2.2 网络与安全（功能细节）

本节从**功能与行为视角**描述 ClipBridge 的网络与安全模型，重点说明“系统如何工作”，而非具体代码实现。

ClipBridge 的网络与安全设计采用**分层模型**，从低到高依次为：

1. **连接层（Connectivity）**
2. **安全传输层（Secure Transport）**
3. **账号证明层（Account Authentication）**
4. **权限与策略层（Authorization & Policy）**

每一层职责单一、边界清晰，上层能力不得绕过下层直接使用。

---

### 2.2.1 连接层（Connectivity / Discovery）

连接层负责解决两个问题：

* **“我能看见哪些设备？”**
* **“我该尝试连接到哪里？”**

#### 局域网发现（Discovery）

* ClipBridge 使用 **mDNS / DNS-SD** 在局域网内进行设备发现。
* 每个设备在登录账号后，广播一个 ClipBridge 服务实例。
* 广播内容包含一个**不可逆的账号标识 `account_tag`**，用于区分不同账号的设备集合。

#### 同账号自动发现

`account_tag` 用于 **发现过滤与快速拒绝**（不是认证、不是授权）。

**派生算法（v1 固定）：**

- 输入：用户口令 `password_utf8`（UTF-8 bytes）
- KDF：Argon2id
  - salt（固定常量，UTF-8）：`"ClipBridge:account_tag:v1"`
  - 参数：`t=3, m=65536 KiB (64 MiB), p=1`
  - 输出：`K` = 32 bytes
- HMAC：`tag_raw = HMAC-SHA256(key=K, data="CB:account_tag:v1")`
- 取前 16 bytes，编码为小写 hex：`account_tag`（长度 32 个 hex 字符）

**使用规则：**
- 同账号设备（同口令）必然拥有相同 `account_tag`；不同账号在同 LAN 中不会互相出现在候选列表中。
- mDNS 监听端只缓存 `account_tag` 匹配的 peer；不匹配直接丢弃。
- `account_tag` 只用于过滤；任何通过 `account_tag` 的连接在 OPAQUE 完成前，安全等级等同于“匿名加密连接”。
- `account_tag` 默认不落盘（可选：为加速启动落盘也允许，但必须视为敏感数据，归入本机安全存储策略）。

#### 发现层的安全边界

* 发现层**不做任何信任判断**：

    * 不验证身份
    * 不验证账号
    * 不授予权限
* mDNS 发现仅提供**连接候选线索**，真实身份与授权必须在后续层级完成。

---

### 2.2.2 安全传输层（Secure Transport）

安全传输层负责解决：

* **“连接是否加密？”**
* **“传输过程中是否能被窃听或篡改？”**
* **“同一台设备是否保持身份连续性（防止中途被替换）？”（v1 采用 TOFU pinning）**

#### 连接方式

* 默认使用 **QUIC（基于 UDP）**
* 在 QUIC 不可用的情况下可降级为 TCP（策略保持一致）

#### 加密与完整性

* 所有连接均通过 **TLS 1.3** 建立加密通道（QUIC 内置 TLS 1.3；TCP 降级同样用 TLS 1.3）。
* v1 默认 **不依赖公网 CA / 域名证书体系**：设备使用自签名证书（或等价 TLS 身份），用于提供加密通道与可 pin 的公钥身份。

#### 传输层与信任的区分（v1 明确结论）

* TLS **保证**：
  * “这条连接是加密的、具有完整性保护”
* TLS **不直接保证**：
  * 对端属于哪个账号（账号证明由 OPAQUE 完成）
* v1 额外增加 **设备连续性防护：TOFU 公钥指纹 pinning**
  * 定义指纹：`tls_peer_fingerprint = SHA256(SPKI)` 的小写 hex（SPKI=证书公钥信息）
  * trust store 记录：`(account_uid, device_id) -> tls_peer_fingerprint`

**TOFU 规则（v1 固定）：**
1. 若本地尚未对该 `device_id` 保存过指纹：允许连接继续跑到 OPAQUE
2. **只有当 OPAQUE 成功并进入 AccountVerified 后**，才把当前 `tls_peer_fingerprint` 写入 trust store（完成首次 pin）
3. 若已存在 pin 且本次握手拿到的指纹不一致：
  - 立即拒绝该连接（不进入 OPAQUE）
  - 上报高风险事件（例如 `PeerIdentityChanged` / `TLS_PIN_MISMATCH`），由外壳提示用户“对端设备身份变化，需要重新配对/确认”

> v1 结论：账号归属靠 OPAQUE；设备连续性靠 TOFU pinning。未 pin 前首次连接仍可能遭遇主动 MITM（这是 v1 接受的首次信任模型）。

---

### 2.2.3 账号证明层（Account Authentication / OPAQUE）

账号证明层解决的问题是：

> **“我们是否属于同一个账号？”**

#### 设计原则

* 不在网络中暴露账号密码
* 不允许离线撞库
* 不依赖中心服务器（LAN 可完全离线）

#### OPAQUE 协议

* ClipBridge 使用 **OPAQUE PAKE** 作为账号证明协议。
* 在安全传输层建立后，双方通过 OPAQUE 完成账号证明：

    * 若双方输入过相同账号密码 → 验证成功
    * 否则失败，连接被降权

#### OPAQUE 的角色分配

* 连接发起方：OPAQUE Client
* 被连接方：OPAQUE Server
* 每个设备本地同时保存 Client / Server 所需的注册记录。

#### 账号验证结果

* **成功**：
    * 会话升级为 *AccountVerified*
    * 允许进入权限判定阶段
* **失败**：
    * 不允许同步任何剪贴板元数据或正文
    * 可记录失败并进入退避状态

说明：
OPAQUE 用于证明账号归属；设备公钥指纹 pinning 用于保证设备连续性。
在同账号前提下，若检测到已信任设备的指纹发生变化，连接应被拒绝并提示潜在风险。

#### 2.2.3.1 账号对象与本地记录

ClipBridge 的“账号”是一个**本地账号域（Account Domain）**，用于将同一套账号密码输入的设备归为一组，并在 LAN 环境下实现离线认证与自动共享。

**账号由三类核心要素组成：**

* **账号主键 `account_uid`（本地唯一）**
  用于在本机数据库中标识一个账号。它是本地权威主键，稳定且不依赖 IP/设备。

* **发现标签 `account_tag`（用于 LAN 发现过滤）**
  由账号密码经 KDF + HMAC 派生，用于 mDNS 发现层筛选同账号设备。
  `account_tag` 只作为“发现过滤”，**不是权限或信任根**。

* **OPAQUE 注册记录（每台设备各自保存）**
  每台设备在“登录/创建账号”后，会在本机保存 OPAQUE 的 Client/Server 注册记录。
  这些记录用于后续 OPAQUE 握手，**不需要也不应跨设备同步**。

**账号切换原则：**
同一时间 Core 只允许存在一个 `ActiveAccount`；切换账号会导致发现过滤、会话认证与权限策略全部切换到新的账号域。
---

### 2.2.4 权限与策略层（Authorization & Policy）

权限层解决的问题是：

> **“这个设备现在能做什么？”**

##### 默认权限模型（同账号）

* 同账号、OPAQUE 验证通过的设备：

    * 默认允许互相发送剪贴板元数据
    * 默认允许 Lazy Fetch 拉取正文
* 用户可在 UI 中手动关闭：

    * 向某设备发送
    * 从某设备接收

##### 临时跨账号授权（可选）

* ClipBridge 支持通过 **临时邀请（Invite）** 的方式：

    * 向其他账号的某一台设备**单向共享剪贴板**
* 临时授权具有：

    * 明确的方向（A → B）
    * 明确的范围（仅元数据 / 允许正文 / 文件大小限制）
    * 明确的时效（TTL）

##### 权限判定原则

* 未通过账号证明的连接：

    * 默认无任何数据访问权限
* 权限判断始终发生在**实际数据发送/拉取之前**

#### 2.2.4.1 策略模型：规则（Rules）+ 临时授权（Grants）

权限与策略层采用“双模型”以同时覆盖「同账号长期共享」与「跨账号临时共享」：

1. **长期规则 PeerRule（Rules）**

* 作用域：`(account_uid, peer_device_id)`
* 用途：控制“同账号设备之间”的默认行为
* 支持：

    * 启用/禁用该设备
    * 方向：双向 / 仅发送 / 仅接收
    * 范围：仅元数据 / 允许正文 / 允许文件
    * 限制：自动拉取大小、文件大小、请求频率、并发上限

2. **临时授权 TemporaryGrant（Grants）**

* 作用域：绑定到某个 `peer_device_id`，带 TTL（过期自动失效）
* 用途：允许“跨账号单向共享/短期共享”
* 特点：

    * 明确方向、范围、限制与过期时间
    * 可撤销（revoked）

**策略判定顺序（固定）：**

* 若会话已通过 OPAQUE 且属于当前 `ActiveAccount` → 使用 PeerRule 判定
* 否则 → 检查 TemporaryGrant（未过期/未撤销）
* 否则 → 拒绝（`PERMISSION_DENIED`）


---
### 2.2.5 连接模型与复用（功能细节）

本节定义 ClipBridge 在网络侧对“连接”的抽象方式，用于支撑 Lazy Fetch 与后续文件传输，同时避免频繁建链带来的延迟与不稳定。

#### 2.2.5.1 核心抽象：Session / Stream

ClipBridge 将“连接”分为两层抽象：

* **Session（逻辑会话）**
  表示“本机与某一台对端设备”的一条长期逻辑会话。Session 的生命周期通常跨越多次复制/粘贴行为，持续数分钟到数小时。

* **Stream / Channel（会话内通道）**
  在同一 Session 内，按用途打开不同的逻辑通道来承载不同类型的消息与数据流，互不阻塞。

在默认使用 QUIC 时，Stream 直接映射为 QUIC streams；在 TCP fallback 时，Stream 由应用层多路复用（或多连接）模拟。

#### 2.2.5.2 通道分类（建议最小集合）

为了让控制消息不被大内容阻塞，至少划分为：

1. **Control 通道（长期）**

    * 连接建立后立即创建并保持
    * 承载：握手、心跳、设备状态、元数据事件、进度通知、错误通知
    * 特点：小包、高优先级、低延迟

2. **Content 通道（按需）**

    * Lazy Fetch 拉取正文时按需创建
    * 承载：`GET item content`（文本/图片/文件片段）
    * 特点：可取消、可超时、并发数量受限

3. **File 通道（按需，后续扩展）**

    * 大文件或定向传输使用
    * 特点：强流控、可分块、可恢复（v2+）

#### 2.2.5.3 为什么必须复用 Session

* **降低粘贴延迟**：粘贴触发时如果还要重新握手/重建连接，用户体感会明显卡顿
* **提高稳定性**：局域网频繁切换（Wi-Fi/有线、睡眠唤醒、DHCP 换 IP）时，持久 Session + 重连策略更可靠
* **隔离大流量影响**：文件/图片拉取不应阻塞设备状态与元数据通知
* **支撑并发**：多台设备同时在线、同时拉取/推送时，需要会话级资源控制

---

### 2.2.6 连接状态机（功能细节）

本节定义对每个对端设备的连接状态模型，用于统一 UI 展示、重连策略与权限控制。

#### 2.2.6.1 状态列表（推荐 v1 最小闭环）

对每个 peer 维护一个状态机（只要实现这些就够）：

* **Discovered**：已通过 mDNS 获得候选地址/端口（仅线索，不代表可连接）
* **Connecting**：正在建立 QUIC/TCP 连接（Transport handshake）
* **TransportReady**：传输层已加密建立（TLS OK），但尚未完成账号证明
* **AccountVerifying**：正在进行 OPAQUE 握手
* **AccountVerified**：账号证明成功（同账号确认），会话已具备进入权限判定的条件，但任何数据发送必须等 Control 通道完全可用（即 Online）
* **Online**：会话稳定在线（Control 通道可用、心跳正常），可进行数据交换。
* **Backoff**：最近连接失败，进入指数退避等待下一次重试
* **Offline**：长时间不可达或明确断开

> 说明：`AccountVerified` 与 `Online` 可合并，但拆开有助于更清楚地区分“验证成功”和“链路健康”。

#### 2.2.6.2 各状态的能力边界（非常重要）

* **Discovered / Connecting**：不允许任何数据同步
* **TransportReady / AccountVerifying**：只允许握手消息，不允许元数据/正文
* **AccountVerified / Online**：进入 Policy 判定，允许/拒绝具体操作，但任何数据发送必须等 Control 通道完全可用（即 Online）
* **Backoff / Offline**：不允许同步，可保留历史与 UI 显示

这样可以避免“还没证明同账号就开始收发元数据”的逻辑漏洞。

#### 2.2.6.3 状态迁移的主要触发事件

* mDNS 发现/更新 → `Discovered`（更新候选地址）
* 连接成功（TLS OK）→ `TransportReady`
* OPAQUE 成功 → `AccountVerified` → `Online`
* 心跳超时/连接断开 → `Backoff` 或 `Offline`
* 指数退避到点 → `Connecting`

---

### 2.2.7 重连与换 IP（功能细节）

局域网环境中，设备离线、睡眠、IP 变化是常态。本节定义 ClipBridge 的“稳定在线”行为原则。

#### 2.2.7.1 身份不依赖网络地址

* 对端身份由 `device_id`（以及后续的设备密钥指纹）标识
* IP/端口仅是“可达性线索”，可随时变化
* mDNS 提供的地址更新不改变设备身份，只更新“拨号目标”

#### 2.2.7.2 心跳与离线判定（建议策略）

* `Online` 状态维持一个轻量心跳（通过 Control 通道）
* 若连续 N 次心跳失败或超过 `heartbeat_timeout`：

    * 进入 `Backoff`
* 若超过较长时间仍无法恢复：

    * 进入 `Offline`（UI 可显示“离线”）

#### 2.2.7.3 指数退避（Backoff）

连接失败后采用指数退避重试，推荐：

* 1s → 2s → 4s → 8s → … → max 60s
* 成功一次后退避重置

目的：

* 避免网络波动时疯狂重连占用资源
* 避免被恶意设备诱导连接风暴

#### 2.2.7.4 地址变化处理（DHCP / 多网卡）

* mDNS 监听到同一 `device_id` 的新地址：

    * 更新候选地址列表（IPv4/IPv6 都可保留）
* 若已有在线 Session：

    * 不立即断开，仅在连接异常或需要重连时使用最新地址
* 若处于 `Backoff/Offline`：

    * 使用最新地址优先尝试

---


## 2.4 外壳要点

- **Windows**：
  * C#/WinUI 3 外壳；必要处使用 Win32 API
  * v1 默认交互：历史面板/小窗“选中条目立即写入系统剪贴板”（Eager Write），保证用户 Ctrl+V 直接可粘贴
  * Win32 Delayed Rendering：作为可选优化保留（用于未来“一键粘贴/自动粘贴”或超大内容的按需生成），不作为 v1 MVP 前置
  * 快捷键呼起剪贴板历史小窗（类似 Win+V），用户选择要复制的历史内容
  * 任务栏常驻图标：左键打开主窗口，右键为选项菜单

- **Android**（Java 外壳，后继）：
  * 常驻通知显示当前复制内容
  * 后台长期运行
  * 通过 **ContentProvider URI** 提供大内容（粘贴时触发拉取）；
  * JNI 调 Rust FFI `.so`（非必须先做）。

------

# 3) 技术实现

## 3.1 语言/框架

- **核心**：Rust
- **Windows 外壳**：C# + WinUI 3（C#/WinRT），必要处使用 Win32 API
- **Android 外壳**：Java（UI 设计器），JNI 连接 Rust（后做）

## 3.2 Core ↔ Shell 接口（方向）
### 3.2.1 Windows端
- Shell → Core：`cb_init(config) / cb_send_metadata(meta) / cb_request_content(item_id, mime) / cb_pause(bool) / cb_shutdown()`
  
  - 说明（v1 固定语义）：
    - `cb_request_content(item_id, mime)` 为“ensure”语义：仅表示请求 Core 确保该内容在本机可用；
      不保证同步返回正文。实际结果由 Core 通过回调事件 `CONTENT_CACHED { LocalContentRef }` 交付给 Shell。
    - 对 file_list 的单文件拉取，v1 可扩展为 `cb_request_file(item_id, file_id)`（或等价参数形式），语义同上。

- Core → Shell（回调）：`on_device_online(info) / on_device_offline(id) / on_new_metadata(meta) / on_transfer_progress(id, sent, total) / on_error(code,msg)`

## 3.3 网络与安全（技术实现）

本节从**实现视角**描述网络与安全模块在 Core 中的结构、接口与状态机。

---

### 3.3.1 模块划分

Core 内部网络与安全相关模块建议拆分为：

* `AccountManager`
* `DiscoveryService`
* `SessionManager`
* `OpaqueAuth`
* `PolicyEngine`

#### 3.3.1.1 数据权威边界（Single Source of Truth）

为避免 Shell 与 Core 重复维护状态，Core 内部的数据权威边界如下：

* **AccountManager**：账号域权威（`ActiveAccount`、OPAQUE 记录解密态、account_tag）
* **SessionManager**：设备会话权威（peer 列表、连接状态机、重连/退避、streams）
* **PolicyEngine / PolicyStore**：权限权威（PeerRule、TemporaryGrant、判定顺序）

Shell 仅负责展示与用户输入，不维护权威状态；所有“是否在线/是否允许/失败原因”等由 Core 事件上报。

---

### 3.3.2 DiscoveryService（发现实现）

#### 职责

* 发布 mDNS 服务
* 监听并解析局域网内的 ClipBridge 服务实例
* 根据 `account_tag` 过滤候选设备

#### mDNS TXT 字段示例

* `acct=<account_tag>`
* `did=<device_id>`
* `proto=1`
* `cap=txt,img,file`

#### 输出

* `PeerCandidate`：

    * device_id
    * socket addresses
    * capabilities
    * last_seen

---

### 3.3.3 SessionManager（连接与会话）

#### 会话生命周期

  * Discovered
  * Connecting
  * TransportReady
  * AccountVerifying
  * AccountVerified
  * Online
  * Backoff
  * Offline

#### 连接策略

* 控制最大并发连接数
* 对失败连接执行指数退避
* QUIC 连接上复用多条 stream：

    * 控制流
    * 内容流
    * 文件流

---

### 3.3.4 OpaqueAuth（账号证明）

##### 数据存储

* 每个账号本地保存：

    * OPAQUE Client Registration Record
    * OPAQUE Server Registration Record
* 记录通过系统安全存储或加密文件保存。
* SQLite 存（每 account_uid）：

  * `client_record_ciphertext` + `client_record_nonce`
  * `server_record_ciphertext` + `server_record_nonce`
  * `record_version`
  * `key_id`（字符串）
  * `key_version`（int）
* **plaintext data_key 不落盘**
* Core 使用 `data_key(account_uid)` 对 OPAQUE record 做 AEAD（带完整性校验）


##### 握手流程（简化）

1. HELLO（包含 account_tag）
2. OPAQUE Start（Client）
3. OPAQUE Response（Server）
4. OPAQUE Finish（Client）
5. AUTH_OK / AUTH_FAIL

##### 安全属性

* 不暴露密码
* 抗中间人
* 抗离线撞库

#### 3.3.4.1 OPAQUE 记录存储与轮换

本机对每个账号维护两份 OPAQUE 注册记录：

* `OpaqueRecord(role=Client)`
* `OpaqueRecord(role=Server)`

记录以二进制 blob 形式保存，并要求 **静态加密（at-rest encryption）**：

* ciphertext 存 SQLite
* wrap_key / keystore 由 Shell 提供或托管（Windows Credential Manager / macOS Keychain / Android Keystore）

密码更改或安全升级时支持轮换：

* 生成新记录并更新 `record_version`
* 旧记录保留或清理由策略决定（推荐保留一段时间用于回滚）

#### 3.3.4.2 静态加密与 Keystore 契约（v1）

* **每个 `account_uid` 一把 data_key**
* `key_id` 固定命名：`clipbridge/{account_uid}/opaque_data_key/v1`
* 轮换策略：保留最近 **2** 版，保留 **7 天**
* keystore 不可用：报错并禁用账号功能（不降级、不明文）

#### 3.3.4.3 开发阶段过渡策略（Dev Only）

鉴于 OPAQUE 实现复杂度较高，为确保 **M1 里程碑（网络闭环）** 顺利交付，允许在开发阶段使用 **Pre-Shared Key (PSK)** 模式暂时替代：

* **Debug 模式**：若编译时开启 `feature = "unsafe-dev-auth"` 或配置中 `auth_mode="dev_psk"`。
* **替代逻辑**：
  * `OPAQUE_START` / `RESPONSE` / `FINISH` 流程被跳过。
  * HELLO 之后直接验证 `account_tag` 是否匹配（或验证一个静态 PSK）。
  * 验证通过直接发送 `AUTH_OK`。

* **验收约束**：**M2（元数据同步）交付前必须移除此逻辑**，完整上线 OPAQUE。

---

### 3.3.5 PolicyEngine（权限判定）

#### 判定输入

* 连接类型（账号 / 临时授权）
* 对端 device_id
* 请求类型（广播 / 拉取正文 / 文件）

#### 判定输出

* `ALLOW`
* `DENY`
* `LIMITED`（受限大小 / 频率）

#### 判定调用点

* 发送元数据前
* 接收拉取请求前
* 文件流建立前

#### 3.3.5.1 PeerRule / TemporaryGrant 数据模型定型

PolicyEngine 的判定数据来自两类存储对象：

* `PeerRule(account_uid, device_id)`：同账号长期规则（默认自动创建）
* `TemporaryGrant(grant_id, device_id)`：临时授权（跨账号/短期）

PolicyEngine 判定 API（概念）：

* 输入：

    * `active_account_uid`（可空）
    * `device_id`
    * `session_is_account_verified`
    * `request_kind`（SendMeta / ReceiveMeta / FetchContent / SendFile / ReceiveFile）
* 输出：`ALLOW / DENY(code) / LIMITED(limits)`

判定顺序固定：

1. 若 `session_is_account_verified` 且 `active_account_uid` 存在 → 查 `PeerRule`
2. 否则 → 查未过期且未撤销的 `TemporaryGrant`
3. 否则 → `DENY(PERMISSION_DENIED)`

---

### 3.3.6 与 Shell 的交互

* Core 向 Shell 上报：

    * 新设备上线（含账号验证状态）
    * 账号不匹配 / 验证失败
    * 权限拒绝原因（用于 UI 提示）
* Shell 不参与协议与安全判断，仅展示与配置策略。

---

### 3.3.7 向未来 WAN 的兼容性

* LAN 发现（mDNS）可替换为云端 rendezvous
* OPAQUE 协议保持不变
* 权限与策略模型保持不变
* 仅“候选地址来源”发生变化

---

### 3.3.8 Session 抽象与状态机实现（技术实现）

本节给出 Core 内部可落地的实现结构，用于支撑 2.2.5～2.2.7 的行为。

#### 3.3.8.1 关键数据结构（概念级）

* `PeerCandidate`：来自 Discovery 的候选信息

    * `device_id`
    * `addrs: Vec<SocketAddr>`
    * `capabilities`
    * `last_seen`

* `Session`：与某 peer 的会话对象

    * `device_id`
    * `state`
    * `transport`（QUIC/TCP 的统一抽象）
    * `control_channel`
    * `last_heartbeat_at`
    * `backoff`（当前退避计时器）
    * `limits`（并发 stream 上限、速率限制等）

* `SessionState`：对应 2.2.6 定义的枚举状态

#### 3.3.8.2 SessionManager 的职责

* 维护 `HashMap<PeerId, Session>`
* 订阅 Discovery 输出的候选设备变更（增量更新）
* 负责：

    * 连接建立与拆除
    * 握手推进（Transport → App）
    * 心跳与超时
    * backoff 调度
    * stream 并发限制
    * 统一向上抛事件（设备上线/离线、失败原因）

#### 3.3.8.3 状态推进原则（实现规则）

* 状态只能由 SessionManager 推进（单写者），避免竞态
* 所有网络 IO 事件、定时器事件进入同一事件循环（tokio task）
* 状态迁移必须记录原因（用于日志与 UI）：

    * `connect_error`
    * `tls_error`
    * `opaque_fail`
    * `heartbeat_timeout`

#### 3.3.8.4 Session 对象字段定型（权威字段）

每个 `Session(device_id)` 至少包含以下权威字段：

* `device_id`：设备唯一标识，不依赖 IP
* `state`：状态机状态（Discovered/Connecting/.../Online）
* `candidate_addrs`：来自 Discovery 的候选地址池（可频繁更新）
* `selected_addr`：当前拨号使用地址（可为空）
* `transport`：QUIC/TCP 传输句柄（可为空，随状态变化）
* `control_channel`：常驻控制通道（Online 必须存在）
* `limits`：并发、超时、心跳、握手超时等硬限制
* `health`：心跳、rtt、失败计数等健康度指标
* `recent_msg_ids`：短期去重/重放保护（可选但推荐）

不变量：任何剪贴板数据操作必须满足
`state ∈ {AccountVerified, Online}` 且 `PolicyEngine` 判定为允许。

---

### 3.3.9 双阶段握手：Transport Handshake + Application Handshake（技术实现）

#### 3.3.9.1 Transport Handshake（QUIC/TLS）

* 尝试使用 QUIC 建立连接：

  * `Connecting -> TransportReady`
* TLS 1.3 由 QUIC 内置完成；实现必须在握手完成后拿到：
  * `peer_device_id`（来自后续 HELLO，但此处可先缓存握手态）
  * `tls_peer_fingerprint = SHA256(SPKI)`（小写 hex）

**TOFU pinning（v1 必做）：**
- 若 trust store 中已存在 `(account_uid, peer_device_id)` 的 pin：
  - 若 `tls_peer_fingerprint` 不一致：立即拒绝连接，抛 `TLS_PIN_MISMATCH`，并上报事件
- 若不存在 pin：
  - 允许继续进入应用握手（HELLO + OPAQUE）
  - 只有当 OPAQUE 成功进入 `AccountVerified` 后，才把该指纹写入 trust store（完成首次 pin）

> 注：OPAQUE 负责“同账号证明”；pinning 负责“同一台设备的连续性防护”。


#### 3.3.9.2 Application Handshake（HELLO + OPAQUE）

在 `TransportReady` 后，通过 Control 通道执行应用握手：

**(1) HELLO**

* Client → Server：发送 `HELLO`（含 `protocol_version`、`device_id`、`account_tag`、capabilities）
* Server 校验：

    * `account_tag` 是否与本机 active account 一致
    * 不一致：发送 AUTH_FAIL { code: "AUTH_ACCOUNT_TAG_MISMATCH" }，随后 CLOSE（并进入 Backoff）


HELLO_ACK 是 HELLO 的成功确认帧，省略不写不代表不存在，协议定义以 3.3.11 为准。

**(2) OPAQUE（账号证明）**

* Client → Server：`OPAQUE_START (ke1)`
* Server → Client：`OPAQUE_RESPONSE (ke2)`
* Client → Server：`OPAQUE_FINISH (ke3)`
* 成功：进入 `AccountVerified`

**(3) AUTH_OK**

* Server → Client：`AUTH_OK`（可含会话参数、限制、对端能力确认）
* 会话进入 `Online`

#### 3.3.9.3 失败处理与退避

* 任一步失败都不得降级为“匿名在线”
* 失败进入 `Backoff`，并记录失败原因
* 对同一 `(device_id, addr)` 连续失败可拉长 backoff，避免被噪声设备拖垮

---

### 3.3.10 连接复用、并发限制、重连与地址变更（技术实现）

#### 3.3.10.1 连接复用（Streams）

在 `Online` 状态下：

* `control_channel`：保持常驻
* `content_stream`：每次 Lazy Fetch 请求新建一个 stream
* `file_stream`：大文件/定向传输单独 stream（后续扩展）

#### 3.3.10.2 并发与资源上限（建议实现为硬限制）

建议至少实现这些限制字段（可配置）：

* 每 peer 最大同时 content streams：例如 4
* 全局最大同时 content streams：例如 16
* 每 peer 最大同时 file streams：例如 1
* 全局最大同时 file streams：例如 4

目的：

* 防止多设备同时粘贴导致资源爆炸
* 防止对端恶意反复拉取

#### 3.3.10.3 心跳与超时

* Control 通道定期发送 `PING`
* 超时策略：

    * 若超过 `heartbeat_timeout` 未收到对端响应：`Online -> Backoff`
* 重连成功后：

    * 退避重置
    * 重新建立 control_channel 并重新握手（HELLO + OPAQUE）

#### 3.3.10.4 地址变化（DHCP / 多网卡）

DiscoveryService 更新 `PeerCandidate.addrs` 后：

* SessionManager 更新 session 的“拨号地址池”
* 若 session 当前在线：

    * 不立刻迁移，避免无意义抖动
* 若 session 断线重连：

    * 按优先级尝试最新 addr（可用 `last_seen` 时间排序）

#### 3.3.10.5 Backoff 调度（指数退避）

实现要点：

* 每个 session 维护 `backoff_step` 与 `next_retry_at`
* 典型序列：1,2,4,8,…,60（秒）
* 成功一次后将 `backoff_step` 清零

---

### 3.3.11 消息类型与错误码

本节定义 ClipBridge 在 **Control 通道** 与 **Data 通道** 中使用的消息类型与错误码规范，用于保证跨设备通信的一致性、可调试性与可扩展性。

---

#### 3.3.11.1 消息通道划分

所有网络消息均隶属于某个通道（Stream / Channel）：

* **Control 通道（必须）**

    * 长连接、低延迟
    * 承载：握手、状态、事件、心跳、错误
* **Content 通道（按需）**

    * Lazy Fetch 拉取正文
* **File 通道（按需，后续扩展）**

    * 大文件/定向传输

> 原则：
> **所有“决定连接状态、权限、会话生死”的消息必须走 Control 通道。**

---

#### 3.3.11.2 Control 通道消息类型

##### A. 握手与认证类

###### `HELLO`

* **方向**：Client → Server
* **用途**：应用层握手起点，声明协议与账号上下文

字段示例：

```json
{
  "type": "HELLO",
  "protocol_version": 1,
  "device_id": "uuid",
  "account_tag": "hex-string",
  "capabilities": ["text","image","file"],
  "client_nonce": "base64"
}
```

---

###### `HELLO_ACK`

* **方向**：Server → Client
* **用途**：确认 HELLO 可继续，进入 OPAQUE

```json
{
  "type": "HELLO_ACK",
  "server_device_id": "uuid",
  "protocol_version": 1
}
```

---

###### `OPAQUE_START`

* **方向**：Client → Server
* **用途**：OPAQUE 第一步（ke1）

```json
{
  "type": "OPAQUE_START",
  "opaque": "base64-bytes"
}
```

---

###### `OPAQUE_RESPONSE`

* **方向**：Server → Client
* **用途**：OPAQUE 第二步（ke2）

```json
{
  "type": "OPAQUE_RESPONSE",
  "opaque": "base64-bytes"
}
```

---

###### `OPAQUE_FINISH`

* **方向**：Client → Server
* **用途**：OPAQUE 第三步（ke3）

```json
{
  "type": "OPAQUE_FINISH",
  "opaque": "base64-bytes"
}
```

---

###### `AUTH_OK`

* **方向**：Server → Client
* **用途**：账号证明成功，会话升级为 AccountVerified / Online

```json
{
  "type": "AUTH_OK",
  "session_flags": {
    "account_verified": true
  }
}
```

---

###### `AUTH_FAIL`

* **方向**：Server → Client
* **用途**：账号证明失败，连接将进入 Backoff

```json
{
  "type": "AUTH_FAIL",
  "code": "OPAQUE_FAILED"
}
```

---

##### B. 会话与状态类

###### `PING`

* **方向**：双向
* **用途**：心跳与存活检测

```json
{
  "type": "PING",
  "ts": 1700000000
}
```

---

###### `PONG`

* **方向**：双向
* **用途**：心跳响应

```json
{
  "type": "PONG",
  "ts": 1700000000
}
```

---

###### `DEVICE_STATUS`

* **方向**：Server → Client
* **用途**：对端状态变化通知（在线/离线/能力变化）

```json
{
  "type": "DEVICE_STATUS",
  "state": "ONLINE"
}
```

---

##### C. 剪贴板元数据类

###### `ITEM_META`

* **方向**：Server → Client
* **用途**：广播新的剪贴板元数据

```json
{
  "type": "ITEM_META",
  "item": {  }
}
```
> item里放ItemMeta
---

##### D. 错误与控制类

###### `ERROR`

* **方向**：双向
* **用途**：通用错误返回（非致命）

```json
{
  "type": "ERROR",
  "code": "PERMISSION_DENIED",
  "message": "Not allowed to fetch content"
}
```

---

###### `CLOSE`

* **方向**：双向
* **用途**：正常关闭会话（非异常）

```json
{
  "type": "CLOSE",
  "reason": "CLIENT_SHUTDOWN"
}
```

---

#### 3.3.11.3 Content / File 通道消息类型

##### GET_ITEM_CONTENT

* **方向**：Client → Server
* **用途**：Lazy Fetch 请求正文

```json
{
  "type": "GET_ITEM_CONTENT",
  "item_id": "uuid",
  "mime": "text/plain"
}
```

---


##### CANCEL

* **方向**：Client → Server
* **用途**：取消正在进行的内容/文件传输

```json
{
  "type": "CANCEL",
  "reason": "USER_CANCEL"
}
```

---

#### 3.3.11.4 错误码设计（统一规范）

错误码采用 **分层前缀 + 语义枚举** 的方式，便于调试与 UI 显示。

---

##### A. 通用错误（GEN）

| 错误码                     | 含义      |
| ----------------------- | ------- |
| `GEN_INVALID_MESSAGE`   | 消息格式错误  |
| `GEN_PROTOCOL_MISMATCH` | 协议版本不兼容 |
| `GEN_INTERNAL_ERROR`    | 内部错误    |

---

##### B. 连接与传输（CONN）

| 错误码            | 含义      |
| -------------- | ------- |
| `CONN_TIMEOUT` | 连接/响应超时 |
| `CONN_CLOSED`  | 对端关闭连接  |
| `CONN_BACKOFF` | 当前处于退避期 |

---

##### C. 安全传输（TLS）

| 错误码                    | 含义            |
| ---------------------- | ------------- |
| `TLS_HANDSHAKE_FAILED` | TLS/QUIC 握手失败 |
| `TLS_PIN_MISMATCH`     | 设备指纹不匹配       |

---

##### D. 账号证明（AUTH / OPAQUE）

| 错误码                         | 含义          |
| --------------------------- | ----------- |
| `AUTH_ACCOUNT_TAG_MISMATCH` | 账号不一致       |
| `OPAQUE_FAILED`             | OPAQUE 验证失败 |
| `AUTH_REVOKED`              | 账号或设备已被撤销   |

---

##### E. 权限与策略（POLICY）

| 错误码                 | 含义       |
| ------------------- | -------- |
| `PERMISSION_DENIED` | 权限不足     |
| `SHARE_EXPIRED`     | 临时授权已过期  |
| `CONTENT_TOO_LARGE` | 内容超过允许大小 |
| `RATE_LIMITED`      | 触发限流     |

---

##### F. 内容与资源（CONTENT）

| 错误码                   | 含义    |
| --------------------- | ----- |
| `ITEM_NOT_FOUND`      | 条目不存在 |
| `ITEM_EXPIRED`        | 条目已过期 |
| `CONTENT_UNAVAILABLE` | 正文不可用 |
| `TRANSFER_CANCELLED`  | 传输被取消 |

---

#### 3.3.11.5 错误码使用原则（必须遵守）

1. **错误码用于程序逻辑，message 仅用于人类阅读**
2. **权限错误不得自动重试**（避免无意义请求）
3. **OPAQUE / AUTH 错误直接进入 Backoff**
4. **所有错误都必须能映射到 Session 状态变化或 UI 提示**

---

#### 3.3.11.6 与 Session / Policy 的关联点

* 握手类错误：

    * 影响 Session 状态迁移（Connecting → Backoff）
* 权限类错误：

    * 不影响 Session 在线状态
    * 仅影响具体请求
* 内容类错误：

    * 影响单个 Stream，不影响 Session

---


### 3.3.12 Wire Format（实现规格：分帧 / bytes 流 / 关联 ID）

本节定义“消息如何在网络上按字节传输”，用于把 **3.3.11 的消息语义**落到可实现的编码规则上。
协议语义以 3.3.11 为准；本节只规定：
- JSON 消息如何分帧（Frame）
- Content/File 如何以 bytes 流传输（不使用 base64 JSON 分块）
- 如何用 msg_id / transfer_id 关联请求与响应，保证可调试性与可扩展性

---

#### 3.3.12.1 总体编码规则（必须遵守）

- JSON 编码：UTF-8
- JSON 字段要求：
  - 未识别字段必须忽略（向后兼容）
  - 需要校验的关键字段缺失 → `GEN_INVALID_MESSAGE`
- v1 不做压缩（后续可在 header 中声明 `content_encoding` 扩展）

---

#### 3.3.12.2 JSON 分帧（CBFrame）

所有 JSON 消息都使用统一分帧：`u32be_len + json_bytes`

- `u32be_len`：4 字节 **大端**无符号整数，表示后续 JSON 字节长度 N
- `json_bytes`：长度为 N 的 UTF-8 JSON

限制（v1 默认）：
- Control 通道单帧最大：1 MiB（超过 → `GEN_INVALID_MESSAGE`）
- Content/File 的“头 JSON 帧”最大：256 KiB（够用且避免滥用）

---

#### 3.3.12.3 通道与连接映射（QUIC 优先，TCP fallback）

##### A. QUIC（推荐）
- **Control 通道**：QUIC 连接建立后，由 Client 打开的**第一个双向 stream**作为 Control。
  - 该 stream 上只传 JSON CBFrame（例如 `HELLO / OPAQUE_* / ITEM_META / ERROR / PING`）
- **Content / File 通道**：每次 Lazy Fetch / 文件拉取都新建一个双向 stream
  - 一个 stream 对应一个传输任务（transfer）
  - stream 内部的传输结构见 3.3.12.5

##### B. TCP fallback（实现简化策略）
- Control：一个 TCP 长连接，使用 CBFrame 传 JSON
- Content/File：每个传输任务单独建立一个 TCP 连接（避免在 TCP 上实现多路复用）
  - 连接建立后第一个 CBFrame 必须是请求头（如 `GET_ITEM_CONTENT`）

> 这样 QUIC 与 TCP 的应用层编码一致：JSON 都用 CBFrame；差异仅在“多路复用由谁负责”。

---

#### 3.3.12.4 关联 ID（msg_id / reply_to / transfer_id）

为保证可调试性与可重试性，v1 统一引入三个 ID（字段缺失按规则处理）：

- `msg_id`：请求消息的唯一 ID（UUID 字符串），由发起方生成
- `reply_to`：响应消息引用对应请求的 `msg_id`
- `transfer_id`：传输任务 ID（UUID 字符串）
  - 对于 Content/File stream，`transfer_id` 必须出现在请求头与响应头中

规则：
- Control 通道：所有**需要响应**的请求都必须带 `msg_id`，响应必须带 `reply_to`
- Content/File：请求头必须带 `msg_id` 与 `transfer_id`，响应头必须带 `reply_to` 与 `transfer_id`

---

#### 3.3.12.5 Content / File bytes 流传输（Wire=B 的核心）

> 重要：3.3.11.3 里出现的 `ITEM_CONTENT_DATA`（base64 分块）属于“语义层/示例表达”。  
> 在 v1 的 Wire=B 中，**实际线上不发送 base64 JSON 分块**，而是使用“头 JSON + 原始 bytes”。

一个 Content/File stream 的标准结构如下：

1) Client → Server：请求头（CBFrame JSON）
2) Server → Client：响应头（CBFrame JSON）
3) Server → Client：原始 bytes（长度由响应头声明）
4) Server：发送完毕后关闭发送方向（FIN）；可选发送尾帧（CBFrame JSON）

---

##### A. 请求头（示例：GET_ITEM_CONTENT）

```json
{
  "type": "GET_ITEM_CONTENT",
  "msg_id": "uuid",
  "transfer_id": "uuid",
  "item_id": "uuid",
  "part": "content",
  "mime": "text/plain"
}
```

约束：

* `part`：v1 取值建议为 `content`（预留后续扩展：thumb / meta-only / file-entry）
* v1 不支持 offset/resume（若出现 offset 字段 → `GEN_INVALID_MESSAGE` 或忽略并视为 0）

##### B. 响应头（成功：CONTENT_BEGIN）

```json
{
  "type": "CONTENT_BEGIN",
  "reply_to": "uuid",
  "transfer_id": "uuid",
  "item_id": "uuid",
  "mime": "text/plain",
  "total_bytes": 12345,
  "sha256": "hex",
  "content_encoding": "identity"
}
```

规则：

* `total_bytes` 必须存在且 >0（允许 0 作为空文本/空文件的特例）
* `sha256` 用于接收端校验与 CAS 去重；校验失败 → `CONTENT_UNAVAILABLE` 或 `GEN_INVALID_MESSAGE`

##### C. bytes 段（Raw Bytes）

* 响应头发出后，服务端立即发送 **`total_bytes` 个原始字节**
* 接收端读取累计字节数达到 `total_bytes` 即视为正文结束
* 不需要逐块 JSON 分帧；进度由“已读 bytes”计算

##### D. 可选尾帧（CONTENT_END）

为增强可调试性，允许在 bytes 段结束后再发送 1 个 CBFrame：

```json
{
  "type": "CONTENT_END",
  "transfer_id": "uuid",
  "ok": true
}
```

v1 中该尾帧为**可选**：实现可以只用 FIN 作为结束标志。

---

#### 3.3.12.6 文件（File）传输与文件列表（v1 规则）v1 采用“**单文件一个 transfer**”的最简规则，避免一次 stream 内多文件分段导致复杂度激增。

* **处理策略**：
* 如果剪贴板包含多个文件或文件夹：
* `ITEM_META` 中必须包含展平后的文件条目列表（`files[]`）。
* **文件夹处理**：v1 Core **不负责** 文件夹的递归遍历或压缩打包。Shell 需将文件夹内的文件展平为文件列表，或 v1 阶段仅支持单文件/文件列表，暂不支持文件夹结构。


* 粘贴时，Shell 需按需对每个 `file_id` 分别发起一次 File transfer（建议 Shell 侧维护一个串行下载队列，避免并发爆炸）。


* **File 请求头示例**：
```json
{
  "type": "GET_FILE",
  "msg_id": "uuid",
  "transfer_id": "uuid",
  "item_id": "uuid",
  "file_id": "uuid"
}

```


* **File 响应头示例**：
```json
{
  "type": "FILE_BEGIN",
  "reply_to": "uuid",
  "transfer_id": "uuid",
  "item_id": "uuid",
  "file_id": "uuid",
  "rel_name": "Photos/a.png",
  "total_bytes": 987654,
  "sha256": "hex",
  "content_encoding": "identity"
}

```


* 随后同样发送 `total_bytes` 原始 bytes。

---

#### 3.3.12.7 取消（CANCEL）与错误返回（ERROR）

* 取消方式（推荐实现）：

  * Client 在同一 Content/File stream 上发送一个 CBFrame：

    ```json
    {
      "type": "CANCEL",
      "transfer_id": "uuid",
      "reason": "USER_CANCEL"
    }
    ```
  * 发送后 Client 关闭本端发送方向；必要时可直接 Reset stream（QUIC）/关闭连接（TCP）

* 服务端处理：

  * 收到 CANCEL 后必须尽快停止发送 bytes
  * 可返回 Control 通道 `ERROR { code: "TRANSFER_CANCELLED" }` 或在该 stream 上返回 `ERROR` 头帧后关闭

错误约束（与 3.3.11.5 原则一致）：

* 权限错误（PERMISSION_DENIED / SHARE_EXPIRED）不得自动重试
* 内容错误（ITEM_NOT_FOUND / CONTENT_UNAVAILABLE）只影响该 transfer，不影响 Session

## 3.4 协议与通信流程（Protocol & Communication）

本章描述 ClipBridge 在设备间进行通信时的**协议语义与交互流程**。
本章不重复底层实现细节，所有消息格式、错误码与通道定义以 **3.3.11** 为准。

---

### 3.4.1 协议设计原则

ClipBridge 的通信协议遵循以下原则：

1. **分层清晰**

  * 发现、连接、安全、权限、数据传输各司其职
2. **长会话 + 多路复用**

  * 单个 Session 承载多种逻辑流（Control / Content / File）
3. **默认最小权限**

  * 未完成账号证明或权限判定前，不允许任何剪贴板数据交换
4. **Lazy Fetch**

  * 默认仅同步元数据，正文按需拉取
5. **LAN / WAN 可演进**

  * 协议不依赖具体网络拓扑，仅依赖“可达连接”

---

### 3.4.2 设备发现与连接建立

#### 3.4.2.1 局域网发现（Discovery）

ClipBridge 使用 **mDNS / DNS-SD** 在局域网内广播服务实例。

* **服务类型**

  * `_clipbridge._udp.local`（默认，QUIC）
  * `_clipbridge._tcp.local`（可选 fallback）

* **TXT 记录最小字段**

  * `acct=<account_tag>`
  * `did=<device_id>`
  * `proto=1`
  * `cap=txt,img,file`

**行为说明**

* 设备只处理 `acct` 与当前账号匹配的服务实例
* 不匹配的实例直接忽略
* mDNS 仅用于提供地址与能力线索，不参与任何安全判断

---

#### 3.4.2.2 连接建立（Session Dial）

当发现可用对端后：

1. 本机创建一个 **Session**
2. 尝试使用候选地址建立 QUIC 连接
3. 成功后进入安全传输层握手

连接失败将进入 **Backoff** 状态，并按指数退避重试。

---

### 3.4.3 传输层握手（Transport Handshake）

* 使用 **QUIC + TLS 1.3**
* 握手成功后：

  * 通道加密
  * 防窃听、防篡改
* 此阶段仅保证“通道安全”，不保证账号归属

握手完成后，会话状态进入 `TransportReady`。

---

### 3.4.4 应用层握手与账号证明（Application Handshake）

#### 3.4.4.1 HELLO 阶段

* Client 通过 **Control 通道**发送 `HELLO`
* Server 校验：

  * 协议版本
  * `account_tag` 是否一致

不一致直接返回 `AUTH_ACCOUNT_TAG_MISMATCH` 并关闭会话。

---

#### 3.4.4.2 OPAQUE 账号证明

在 `HELLO` 成功后，双方通过 OPAQUE 协议完成账号证明：

1. `OPAQUE_START`
2. `OPAQUE_RESPONSE`
3. `OPAQUE_FINISH`

成功后：

* 会话升级为 `AccountVerified`
* 进入权限判定阶段

失败后：

* 返回 `AUTH_FAIL / OPAQUE_FAILED`
* 会话进入 `Backoff`

---

### 3.4.5 会话在线与心跳

#### 3.4.5.1 Online 状态

当满足以下条件时，会话进入 `Online`：

* 传输层已加密
* 账号证明成功
* Control 通道可用

#### 3.4.5.2 心跳机制

* Control 通道定期交换 `PING / PONG`
* 连续超时将导致：

  * `Online → Backoff`
  * 后续自动重连

---

### 3.4.6 剪贴板元数据同步

#### 3.4.6.1 元数据广播

当本机剪贴板发生变化：

* 生成 `ITEM_META`
* 通过 Control 通道发送给所有 `Online` 且权限允许的会话

元数据包含：

* item_id
* 类型（文本/图片/文件）
* 大小、摘要、时间戳等

---

#### 3.4.6.2 权限检查

在发送元数据前，必须通过 Policy 判定：

* 是否允许向该设备发送
* 是否为临时授权 / 单向授权

失败返回 `PERMISSION_DENIED`。

---

### 3.4.7 正文拉取（Lazy Fetch）

#### 3.4.7.1 拉取流程

1. 用户在目标设备触发粘贴
2. 客户端通过 **Content 通道**发送 `GET_ITEM_CONTENT`
3. 服务端执行：

  * 权限检查
  * 大小限制检查
4. Server 在 Content 通道先发 CONTENT_BEGIN（CBFrame），随后发送 raw bytes（长度=total_bytes，可分多段），最后发 CONTENT_END（CBFrame）。

---

#### 3.4.7.2 失败情况

* `ITEM_NOT_FOUND`：条目不存在或已过期
* `PERMISSION_DENIED`：权限不足
* `CONTENT_TOO_LARGE`：超出限制
* `TRANSFER_CANCELLED`：被中止

失败仅影响当前 Content 通道，不影响 Session 在线状态。

#### 3.4.7.3 自动预取（Auto Prefetch，仅 text）

为减少“粘贴等待”，当设备收到 `ITEM_META` 后：
- 若 `kind=text` 且 `size_bytes <= text_auto_prefetch_bytes`（默认 1MB），Core **可自动触发一次** `ensure_content_cached(item_id)` 把正文预取到本地 CAS
- 该预取本质仍走 Lazy Fetch 的 `GET_ITEM_CONTENT` 流程，只是由 Core 自动触发，而非用户粘贴触发
- 图片/文件默认不预取（避免无意带宽/磁盘消耗）

---

### 3.4.8 临时跨账号共享（可选）

ClipBridge 支持通过临时邀请机制实现跨账号单向共享：

* 邀请生成方创建临时授权（TTL + scope）
* 接收方使用邀请码建立临时会话
* 临时会话不参与账号发现与 OPAQUE
* TemporaryGrant 会话不参与 账号归属 OPAQUE，但仍必须建立加密传输层，并通过邀请凭证完成等效的一次性认证。

权限由 **Temporary Grant** 决定，过期后自动失效。

---

### 3.4.9 会话关闭与异常处理

#### 3.4.9.1 正常关闭

* 任一方发送 `CLOSE`
* 会话进入 `Offline`
* 不触发退避

#### 3.4.9.2 异常断开

* 连接中断、超时、错误码触发
* 会话进入 `Backoff`
* 按策略重连

---

### 3.4.10 协议一致性说明

* 所有消息类型与错误码定义以 **3.3.11** 为唯一权威
* 本章仅描述**行为与语义**
* 协议扩展应：

  * 保持向后兼容
  * 不破坏现有状态机

---

------

# 4) Rust核心

## 4.0 Core 文档权威与约定



## 4.1 设计原则

* **共享内核**：所有跨平台逻辑尽量放 Core（发现/会话、协议、缓存、历史、Lazy Fetch）。
* **按需取用**（Lazy Fetch）：复制只广播**元数据**；粘贴/点击时才拉**正文**。
* **事件驱动**：Core 通过回调把设备上下线/新元数据/传输进度推给壳。
* **可演进协议**：带 `protocol_version`；新增字段向后兼容。
* **低耦合**：壳只做系统集成（UI、剪贴板、托盘、热键、系统秘钥存取）。

---

## 4.2 Core v1 范围与里程碑（实现可交付定义）

本节用于把“v1”从愿景变成**可实现、可验收**的交付定义。  
只要 v1 达到本节的验收条件，Core 就已经具备“可写代码并能稳定迭代”的基础。

> 注意：v1 支持 **文本 / 图片 / 文件** 三类内容，但仍遵循 **Lazy Fetch**：默认仅同步元数据，正文在粘贴/点击时按需拉取。

---

### 4.2.1 v1 范围（必须做）

#### A. 网络范围
- **仅 LAN（局域网）**：mDNS 发现 + QUIC（或 TCP fallback）建立连接。
- 不包含：云端账号 / 跨 WAN 打洞 / Relay（这些属于 v2+）。

#### B. 会话与安全
- Session 状态机需完整落地（Connecting → TransportReady → AccountVerified → Online → Backoff/Offline）。
- 账号证明与权限判定必须存在（具体实现细节在后续章节定义，但这里要求“行为上可用”）：
  - 未完成账号证明：不得交换剪贴板数据
  - 未通过 Policy：不得发送/响应内容拉取

#### C. 三类内容（Text / Image / File）
- **Text**：元数据可同步；正文可按需拉取；可缓存；可再次被分享。
- **Image**：元数据可同步；正文按需拉取；以 bytes 流方式传输；缓存为 CAS。
- **File（文件/文件列表）**：
  - v1 的 File 属于“剪贴板语义的文件”（Lazy Fetch 触发拉取），不是“主动点对点传文件任务”。
  - 文件传输必须是**流式**（分块），不能要求一次性把整个文件装入内存。

---

### 4.2.2 v1 非目标（明确不做，避免实现爆炸）

#### A. 可靠性增强（v2 再做）
- 不做断点续传（失败后重传，由上层提示/自动重试策略决定）
- 不做多路径聚合（多网卡同时传输）
- 不做跨网络段漫游的稳定会话保持（地址变化仅作为重连目标更新）

#### B. 产品化功能（v2 再做）
- 不做“定向传输文件任务”（队列、进度、暂停/恢复、后台传输策略等）
- 不做云端历史同步

---

### 4.2.3 默认限制（可调）

本节定义 **v1 的“软限制（soft limit）”**：用于默认行为（是否同步/是否自动预取/是否提示）。  
同时 Core 还需要有 **硬上限（hard cap）** 做 DoS 防护：超过硬上限直接拒绝（无论是否强制）。

> 软限制由外壳（Shell）提供 UI 让用户配置，并在 `cb_init(config_json)` 时传入 Core（见 4.8.8）。

#### 4.2.3.1 默认软限制

软限制适用于尝试拉取时超出限制外壳会弹窗确认

| 类型 |  默认软限制 | 默认传输策略 | 超过软限制时（外壳行为） |
|---|-------:|---|---|
| text |   1 MB | **元数据到达后自动预取**（等价于对该 item 自动触发一次 `ensure_content_cached`） | 弹窗提示“超出同步限制，是否仍同步？”：否→不同步；是→以 **force** 模式继续（但不再自动预取） |
| image |  30 MB | 仅广播元数据；**粘贴/用户显式拉取时** 才传输正文 | 同上 |
| file_list（按总大小） | 300 MB | 仅广播元数据；**粘贴/用户显式拉取时** 才传输文件内容 | 同上 |

说明：
- “自动预取”只针对 **text 且 size ≤ text_auto_prefetch_bytes**；text_auto_prefetch_bytes默认是256KB；图片/文件默认不做预取（避免无意消耗带宽/磁盘）。
- 软限制是“默认策略阈值”，不是绝对禁止；用户可通过 force 同步（仍受硬上限约束）。

#### 4.2.3.2 默认硬上限（Core 防护）

硬限制适用于复制时超出了限制就不广播元数据

建议 v1 默认硬上限：
- hard_text_bytes：16 MB
- hard_image_bytes：256 MB
- hard_file_total_bytes：2 GB

超过硬上限：必须返回 `ITEM_TOO_LARGE`（或等价错误），不得继续传输/落盘。


---

### 4.2.4 v1 交付里程碑（M0~M3）与验收标准

#### M0：纯本地闭环（不联网）
**目标**：Core 已具备“数据权威 + 存储 + 历史查询”的最小可用能力。
- [验收] Shell 提供 `ClipboardSnapshot`（text/img/file）给 Core 后：
  - Core 能生成 `ItemMeta` 并落库
  - Core 能维护本地历史顺序（sort_ts）
  - Core 能进行去重（同 sha256 不重复写 CAS）
  - Core 能执行最小清理策略（历史上限 / CAS 容量上限）

#### M1：网络闭环（发现 + 会话上线，但不要求传正文）
**目标**：两台设备在同 LAN 能互相看到并稳定进入 Online。
- [验收] mDNS 能发现对端，Core 发出设备上线/离线事件
- [验收] Session 能跑通至 Online，并具备 Backoff 重连
- [验收] PolicyEngine 至少能做“默认拒绝/默认允许”两种策略（细则后续章节定义）

#### M2：元数据同步闭环（只同步 ItemMeta）
**目标**：复制发生在 A，B 端可见历史条目（不拉正文）。
- [验收] A 本机 copy → Core 广播 `ITEM_META` → B 入库并通过回调通知 Shell
- [验收] B 的历史列表中能看到条目：类型、来源设备、时间戳、预览（Text preview / Image thumb placeholder / File list placeholder）

#### M3：Lazy Fetch 正文闭环（Text + Image + File）
**目标**：B 端选择/粘贴条目时，能按需从 A 拉取正文并落地到本地可用形式。
- [验收] Text：
  - B 发起拉取 → A 返回 bytes → B 写入 CAS/DB → Shell 可写入系统剪贴板
- [验收] Image：
  - B 发起拉取 → A 流式返回 bytes → B 写入 CAS → Shell 可写入系统剪贴板（以平台可接受的格式）
- [验收] File：
  - B 发起拉取 → A 流式返回文件内容（分块）→ B 落盘到受控目录（或 CAS 映射目录）
  - Shell 获得可粘贴的本地引用（路径/句柄/URI）
- [验收] 取消与失败：
  - 传输可取消；失败会产生统一错误事件；Session 不因此崩溃

---

### 4.2.5 v1 与 v2“定向传输文件”的边界（避免概念冲突）

- v1 File：属于剪贴板同步的一部分，触发方式是 **Lazy Fetch**（粘贴/点击时拉取），目标是“能粘贴”。
- v2 定向传输文件：属于独立的传输任务（可选目标设备、队列、后台、断点续传、传输策略），目标是“像 AirDrop/SendTo 一样可靠传文件”。

因此：v1 不要求 UI 具备传输队列；只要求 Shell 能在需要时向 Core 请求并接收“文件内容落地结果”。



## 4.3 Core 模块与文件结构

本节把 Core（Rust）与 Windows FFI（Rust）的**文件结构与模块边界**固定下来，目的是：
- 任何人打开仓库都知道“该把代码写到哪”
- 控制耦合：Core 保持跨平台，FFI 只做边界适配（路线 A：C ABI）
- 为后续章节（4.4 并发/4.5 API/4.6 数据模型/4.9 存储/4.10 测试）提供落点

> 设计约束回顾：跨平台逻辑尽量放 Core，壳只做系统集成；Core 事件驱动推送状态/元数据；复制默认只广播元数据，正文按需拉取（Lazy Fetch）【。  
> Wire=B 的 bytes 流传输必须落到实现（头 JSON + raw bytes），不能走 base64 JSON 分块【。

---

### 4.3.1 仓库顶层结构（建议且作为 v1 权威）

> 路径命名与 CI Path Filter、版本管理表保持一致：Core=cb_core；Windows FFI=platforms/windows/core-ffi【。

```

repo-root/
├── cb_core/                         # Rust Core（跨平台内核，权威数据层）
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── prelude.rs
│       ├── api/                     # 4.5 Core 公共 API 的唯一出口（稳定）
│       ├── runtime/                 # 4.4：tokio runtime + task 调度 + shutdown
│       ├── discovery/               # mDNS / LAN 发现（仅“线索”，不做安全）
│       ├── transport/               # QUIC/TCP 适配（仅承载 bytes 流/streams）
│       ├── session/                 # 会话状态机 + backoff + 连接生命周期
│       ├── proto/                   # 3.3.11/3.3.12：消息类型 + CBFrame 编解码
│       ├── policy/                  # 权限/策略判断（默认拒绝/允许等）
│       ├── clipboard/               # ingest_local_copy、元数据生成、去重、预览
│       ├── transfer/                # Lazy Fetch：content/file 拉取、取消、进度
│       ├── store/                   # 4.9：SQLite + migrations + query
│       ├── cas/                     # 4.9：内容寻址存储（sha256）+ 落盘策略
│       ├── cache/                   # 内存缓存（peer 列表、meta LRU 等）
│       ├── model/                   # 4.6：Domain/DTO/DbRow（命名规则见 4.6）
│       ├── util/                    # 小工具（时间、uuid、大小限制、hex 等）
│       └── testsupport/             # 测试辅助：fake transport、inproc peers
│
├── platforms/
│   └── windows/
│       ├── core-ffi/                # Windows FFI（Rust）：C ABI 边界层
│       │   ├── Cargo.toml           # package 名可为 core-ffi-windows（与编译命令一致）
│       │   ├── include/
│       │   │   └── clipbridge_core.h# 4.8：对外 C 头文件（壳侧直接包含）
│       │   └── src/
│       │       ├── lib.rs           # #[no_mangle] extern "C" 导出
│       │       ├── bridge.rs        # JSON 入参/出参、回调封送、内存释放策略
│       │       └── error.rs         # FFI 错误码映射（对齐 4.7）
│       │
│       └── ClipBridgeShell_CS/      # Windows Shell（C#）
│
└── docs/ (可选)                     # 额外设计图/序列图（不强制）

```

> 说明：你在项目简介里写的 `cargo build -p core-ffi-windows ...` 依旧成立【；这里我们把“路径”固定为 `platforms/windows/core-ffi/`，把“Cargo package 名”固定为 `core-ffi-windows`，两者不冲突。

---

### 4.3.2 Crate 边界与依赖规则（必须遵守）

#### A. cb_core（跨平台权威层）
- **允许依赖**：纯 Rust 跨平台库（tokio、serde、quinn/rustls、sqlite driver 等）
- **禁止依赖**：任何 OS UI/剪贴板/窗口系统相关库（Win32/WinUI/Android SDK 之类）
- **职责**：
  - 协议语义（3.3.11）与线上编码（3.3.12）在这里落地
  - 负责“设备/会话/数据”的权威状态
  - 通过事件回调把变化推给壳（壳负责 UI 展示与系统集成）【

#### B. platforms/windows/core-ffi（路线 A：C ABI 边界层）
- **只做三件事**（不掺业务）：
  1) 把 C ABI 入参转成 cb_core::api 需要的 Rust 类型（建议统一 JSON）
  2) 把 cb_core 事件回调封送回 C 回调（同样建议 JSON）
  3) 负责跨语言内存释放（cb_free 等）
- **禁止**：
  - 把“业务逻辑”写进 FFI（比如会话状态机、入库、去重、重试策略）
  - 在 FFI 里自行维护一份“Core 状态”（避免双权威）

---

### 4.3.3 模块可见性与稳定性（给后续 API/DTO 做地基）

- `cb_core::api`：**唯一稳定出口**
  - 壳 / FFI 只能依赖这一层（以及明确声明为 public 的 model DTO）
  - 任何跨模块调用尽量通过 api 组织，避免横向耦合
- `cb_core::proto`：协议实现细节
  - 内部可拆成 `types/codec/limits` 等，但对外只暴露必要的类型
- `cb_core::model`：三类模型强制分层（细节在 4.6 完整定义）
  - Domain：领域对象（业务语义）
  - DTO：跨 FFI/网络边界的数据对象
  - DbRow：数据库行镜像（与 schema 对齐）
- `transport/discovery`：只提供“连接能力与线索”，不做安全结论
  - 安全/权限结论必须落到 session/policy 中

---

### 4.3.4 “放哪儿”的快速决策表（写代码时照抄）

- 我监听到系统剪贴板变化，要告诉 Core：
  - 壳侧组装快照 → 调 `cb_core::api::ingest_local_copy(...)`（实现落 cb_core/clipboard + store）
- 我需要把元数据广播给对端：
  - session 触发 → proto 编码（CBFrame）→ transport 发出（实现落 cb_core/session + proto + transport）
- 我点击历史条目要粘贴正文（Lazy Fetch）：
  - 调 `cb_core::api::ensure_content_cached(...)` → transfer 拉流 → cas/store 落地（实现落 cb_core/transfer + cas + store）
- 我要新增一个协议消息类型：
  - 先改 3.3.11 语义，再改 cb_core/proto/types + codec（必须保持未知字段可忽略、版本可演进）
- 我需要新增一个 Windows 专属能力：
  - 先问：能不能放壳？能放就放壳。
  - 不能放壳才考虑“FFI 加一条薄接口”，但逻辑仍在 cb_core。

---

### 4.3.5 命名与文件组织约定（避免后期重构地狱）

- 一个目录一个职责：`session/` 里不要出现 `sqlite`、`win32`、`ui`
- “协议类型”统一在 `proto/`；“业务模型”统一在 `model/`
- 所有跨边界（网络/FFI）对象都必须可序列化（serde），并且字段向后兼容（未知字段忽略）
- 大块流程写在 `api/` 组织，底层能力写在对应模块里（transport/store/cas 等）


## 4.4 运行时与并发模型（v1 实现规范）

本节把 Core 的“线程 / tokio / 任务模型 / 回调线程”写成实现级规范，避免后续出现：
- 多个 tokio runtime 互相打架
- DB/文件 IO 阻塞拖死网络
- 回调线程不明确导致壳侧崩溃
- shutdown 顺序不固定导致死锁/丢数据

> 你原文里已提出：Core 单实例 tokio、阻塞用 spawn_blocking、回调由 Core 线程直接调用、壳需线程安全。本节把这些口径扩展成可直接编码的规则。

---

### 4.4.1 线程与 runtime 所有权（必须一致）

#### A. Core 实例 = 1 个 runtime
- 每个 `CoreHandle`（或 cb_init 返回的 handle）内部必须拥有 **且仅拥有 1 个 tokio runtime**。
- runtime 的创建与销毁完全由 Core 管理；FFI/壳不能创建第二个 runtime 来“接管 Core 任务”。

#### B. “API 调用线程”与“Core 执行线程”解耦
- 任何壳/FFI 调用 Core API 的线程都称为 **Caller Thread**。
- Core 内部运行 tokio 的线程（通常 1 条专用线程）称为 **Core Runtime Thread**。
- 规则：
  - Caller Thread 永远不直接执行网络/DB/文件 IO
  - Caller Thread 只负责把“命令”投递给 Core，并同步等待“短返回”或拿到“异步任务 id”

---

### 4.4.2 Actor / 单写者原则（防止状态机撕裂）

Core 内部采用“单写者”原则组织并发：

#### A. CoreSupervisor（全局单写者）
- 维护全局对象的权威状态：
  - peer 列表、配置快照、限流器、全局传输计数
- 处理来自 API 的命令（Command），并产生事件（Event）推送给壳

#### B. SessionActor（每个 peer 一个单写者）
- 每个 peer 的 Session 状态机（Connecting/Backoff/Online 等）由对应 SessionActor **单线程推进**
- 任何会影响 Session 的输入（发现更新、连接成功/失败、Control 消息、心跳超时）都必须进入该 actor 的队列顺序处理
- 心跳与退避建议策略沿用你 2.2.7 的定义（Online 心跳、N 次失败进入 Backoff、指数退避 1→2→4→…→60s）

#### C. TransferActor（每个 transfer 一个单写者）
- 每次 Content/File 拉取创建一个 transfer 对象（以 `transfer_id` 唯一标识）
- transfer 的状态（Init/Begin/Streaming/Done/Failed/Cancelled）由 TransferActor 顺序推进
- Cancel（来自壳或网络）必须可随时到达并生效（见 3.3.12.7）

---

### 4.4.3 任务拓扑（v1 必须具备的常驻任务）

Core runtime 启动后至少应常驻以下任务（概念任务 → 可映射到 tokio task）：

1) **DiscoveryManager**
- mDNS 广播与监听
- 输出：PeerCandidate 更新事件（仅“线索”，不做安全结论）

2) **SessionSupervisor**
- 维护 `device_id -> SessionActor` 映射
- 根据发现结果/手工配置触发 dial / 更新候选地址
- 负责全局 backoff 限流（避免连接风暴）

3) **StoreExecutor**
- 所有 SQLite 访问统一入口（避免多连接乱序）
- 任何阻塞 DB 操作必须进入 `spawn_blocking` 或专用阻塞线程池

4) **CasExecutor**
- 负责 CAS 落盘/读取、容量统计、LRU/清理
- 大文件写入必须流式落盘；不得把整个文件读到内存

5) **EventPump（回调泵）**
- 从内部事件队列读取 Event，调用 FFI 回调推给壳
- 回调线程规则见 4.4.5

> 备注：你在“流程概览”里列了复制/拉取的关键链路（ingest_local_copy / ensure_content_cached 等），这些链路在实现上就是 CoreSupervisor + Store/CAS/Transfer 的组合。

---

### 4.4.4 Command / Event 队列（接口与背压）

#### A. CommandQueue（Caller → Core）
- API 调用统一转换成 `Command`
- 建议实现为：`tokio::mpsc::Sender<Command>`
- Command 分类：
  - 立即型：如 list_history、get_peers（快速读）
  - 触发型：如 ingest_local_copy、ensure_content_cached（会产生后续事件）

规则：
- CommandQueue 必须有容量上限（默认 1024），满了就返回 `CORE_BUSY`（或等价错误）
- 任何需要阻塞等待结果的命令，必须使用 oneshot 返回（Caller Thread 最多等待一个短超时，例如 2s；超时返回 `TIMEOUT`，但命令可继续在 Core 内执行）

#### B. EventQueue（Core → Shell）
- Core 内部事件统一进入 EventQueue，再由 EventPump 触发回调
- Event 需要覆盖（最小集合）：
  - PeerOnline / PeerOffline / SessionStateChanged
  - ItemMeta（新元数据）
  - TransferProgress / TransferDone / TransferFailed
  - Error（含 scope=Session/Transfer/Core）

背压策略（v1 必须写死一种，避免 UI 卡死）：
- EventQueue 有容量上限（默认 4096）
- 若队列满：
  - 进度类事件（TransferProgress）允许合并/丢弃（保留最新）
  - 状态变更/错误/完成事件不得丢弃（必要时优先踢掉旧进度事件）

---

### 4.4.5 回调线程模型（FFI/壳必须遵守）

你已声明“回调由 Core 内部线程直接调用，壳需线程安全”。v1 进一步规定：

- 回调发生在 **Core Runtime Thread**（或 EventPump 专用线程），**绝不保证是 UI 线程**
- 壳侧必须把回调内容 marshal 到自己的 UI 线程（WinUI Dispatcher 等）
- 回调必须是 **非阻塞**：
  - 回调中不允许进行长时间 IO 或等待网络
  - 如果壳需要做重操作，应把工作放到壳自己的后台线程/队列

---

### 4.4.6 取消、超时与资源限制（v1 默认值）

- 心跳：
  - Online 状态通过 Control 通道发送心跳，超时/连续失败进入 Backoff（策略见 2.2.7）
- Transfer：
  - Content/File 传输必须支持取消（用户取消、UI 切换、Session 掉线触发）
  - v1 不做断点续传：失败后由上层决定是否重拉（Step 1 已定义）
- 限流（与 Step 1 的默认值对齐）：
  - 单 peer 同时 transfer 上限 2
  - 全局同时 transfer 上限 4
  - 超过上限返回 `RATE_LIMITED`（或等价错误）并产生 Error 事件

---

### 4.4.7 Shutdown 顺序（必须固定，避免丢数据/死锁）

当调用 `cb_shutdown(handle)`（或等价 API）时：

1) 标记 Core 进入 `ShuttingDown`（拒绝新 Command）
2) 触发全局 cancel：
  - 取消所有 transfer（通知对端并停止本地写入）
  - 关闭所有 Session（优雅关闭 Control stream）
3) 停止 Discovery 广播与监听
4) flush：
  - 等待 StoreExecutor 完成已入队的关键写入（有超时上限）
  - CAS 落盘收尾（关闭文件句柄）
5) 停止 EventPump：
  - 发出 `CoreStopped`（可选）事件后停止回调
6) join runtime thread，释放 handle 相关资源

规则：
- shutdown 必须是幂等（多次调用只生效一次）
- shutdown 允许超时“硬退出”（防止卡死），但必须保证不会再调用壳回调


## 4.5 Core 公共 API（v1：实现级规范）

本节定义 **Core 对外的“唯一可调用入口”**（Rust API + C ABI FFI 的等价映射）。
目标是：Shell 只要按这章写，就能把 v1 跑通（M1~M3）。

> 约束回顾（必须一致）：
> - Core 事件驱动：通过回调把“设备上线/离线、元数据到达、传输完成/失败”等推给壳
> - Lazy Fetch：默认只同步 ItemMeta，正文/文件在粘贴或点击时拉取
> - Wire=B：网络传输为 “头 JSON（CBFrame）+ raw bytes”，不走 base64 JSON 分块（见 3.3.12.5）

---

### 4.5.1 API 形态（v1 选型：Command + Event）

v1 采用两类 API：

1) **Command（命令）**：壳发起动作，Core 立即返回“是否成功入队/参数是否合法”，后续结果通过 Event 回来
  - 适用：网络连接、广播元数据、Lazy Fetch、取消传输等（可能耗时）
2) **Query（查询）**：壳查询本地状态/历史，Core 可同步返回 JSON（建议壳在后台线程调用）
  - 适用：分页拉历史、读配置、读状态快照

这样做的原因：
- 避免 UI 线程被网络/磁盘阻塞
- 能统一“进度/失败/取消”的表达（全走事件）

---

### 4.5.2 线程与回调契约（必须遵守）

- 所有回调（Event）由 Core 的 EventPump 线程触发（见 4.4 并发模型）
- **壳侧必须线程安全**：回调可能不在 UI 线程
- 回调必须“快”：禁止在回调里做阻塞 IO/长计算；需要的话把数据转发到壳自己的队列再处理
- Core 禁止在 `shutdown` 完成后继续调用任何回调（幂等 + 无悬挂调用）

---

### 4.5.3 数据编码契约（v1 统一 JSON）

跨边界（FFI/回调）负载统一为 UTF-8 JSON 字符串，避免结构体对齐、版本兼容问题。

约定：
- 所有 ID：UUID 字符串（`"xxxxxxxx-xxxx-...."`）
- 时间戳：`ts_ms`（Unix epoch 毫秒，i64）
- 未识别字段：必须忽略（向后兼容）
- 必填字段缺失：视为参数错误（返回 `GEN_INVALID_MESSAGE` 或等价错误）

---

### 4.5.4 Rust 侧 API（权威接口，FFI 只是镜像）

> 下面是“能力面”的清单；具体函数名你可以按 Rust 风格落地，但语义必须一致。

#### A) 生命周期
- `Core::init(config) -> CoreHandle`
- `Core::shutdown(handle)`

#### B) 本机复制注入（本机 -> Core）
- `ingest_local_copy(snapshot: ClipboardSnapshot) -> item_id`
  - 做的事：入库（meta + content 指针/摘要）+ 广播 ITEM_META（给在线对端）

#### C) 历史与元数据查询（本地 DB）
* `list_history(query) -> HistoryPage`
  * 分页（必填）：`cursor` (上次的最后一条 sort_ts_ms，第一页为 null)，`limit` (v1 建议最大 50，默认 20)。**严禁一次性拉取全量历史**。
  * 过滤：`kind` / `device_id` / `time_range`
  * 返回：包含 `items: Vec<ItemMeta>` 和 `next_cursor`。
* `get_item_meta(item_id) -> ItemMeta`

#### D) Lazy Fetch（拉正文/文件）
- `ensure_content_cached(req) -> transfer_id`
  - req 包含：`item_id`、`part`（content/file-entry）、可选 mime
  - 结果通过事件回传：成功返回 `LocalContentRef`（文本/图片/文件路径或 URI）

- `cancel_transfer(transfer_id)`

#### E) 设备与策略（v1 最小集）
- `list_peers() -> PeerList`（本机已知对端 + 在线状态）
- `set_global_policy(policy)`（默认允许/默认拒绝的最低能力）
- `set_peer_rule(device_id, rule)`（可选：以后扩展）

#### F) 诊断
- `get_status() -> CoreStatus`（在线 peer 数、队列长度、版本等）
- `dump_state()`（可选：用于调试）

---

### 4.5.5 C ABI（FFI）映射原则（路线 A：C ABI）

C ABI 只做“薄镜像”：
- Rust API 的能力点在 C 层必须有等价入口
- 参数/返回尽量是：`const char* json_in` / `char* json_out`
- 需要异步结果的命令：返回 `transfer_id` 或 `request_id`，并通过事件回调通知完成

#### A) 最小必备函数清单（v1）

- `cb_init(config_json, callbacks, user_data) -> cb_handle`
- `cb_shutdown(handle)`

- `cb_ingest_local_copy(handle, snapshot_json, out_item_id_json?)`
- `cb_list_history(handle, query_json) -> result_json`
- `cb_get_item_meta(handle, item_id_json) -> meta_json`

- `cb_ensure_content_cached(handle, req_json) -> transfer_id_json`
- `cb_cancel_transfer(handle, transfer_id_json)`

- `cb_list_peers(handle) -> peers_json`
- `cb_get_status(handle) -> status_json`

- `cb_set_global_policy(handle, policy_json)`（v1 必须：用于“停止/继续分享”、allowlist 等策略下发）

- `cb_delete_local(handle, delete_req_json) -> ok_json`（v1 必须：本机隐藏/删除元数据）
- `cb_cache_control(handle, cache_req_json) -> ok_json`（v1 必须：清空/修剪缓存）
- `cb_query_cache_stats(handle, stats_query_json) -> stats_json`（v1 必须：主页 Cache 图表）
- `cb_query_net_stats(handle, stats_query_json) -> stats_json`（v1 必须：主页 Network 图表）

- `cb_free(ptr)`（释放 Core 分配的字符串）

> 说明：哪些函数同步/异步，以“不阻塞 UI”为准；
> - `list_history/get_item_meta/list_peers/get_status/query_*` 可同步返回（Shell 建议后台线程调用）
> - `ensure_content_cached` 必须异步（传输 + 落盘 + 进度）
> - `cache_control/delete_local/set_global_policy` 可同步返回，但可能触发后台任务（例如 GC），Shell 应按“触发后轮询 stats 刷新”实现 UI。


---

### 4.5.6 事件（Event）规范（壳必须实现）

所有事件走一个统一回调入口，例如：
- `on_event(json_event)`

事件 JSON 通用字段：
- `type`：事件类型（字符串）
- `ts_ms`
- `payload`：事件负载对象
- 可选：`severity` / `code`（错误码）/ `message`

v1 必备事件类型（覆盖 M1~M3）：

1) Peer / Session
- `PEER_ONLINE { device_id, name?, addr? }`
- `PEER_OFFLINE { device_id, reason? }`

2) Meta
* `ITEM_META_ADDED { meta, policy? }`

  * `meta`：ItemMeta（必填）
  * `policy`：可选，仅用于 UI 决策提示（不影响协议正确性）

    * `needs_user_confirm: bool`
    * `strategy: "MetaOnlyLazy" | "MetaPlusAutoPrefetch"`

3) Transfer / Lazy Fetch
- `TRANSFER_PROGRESS { transfer_id, bytes_done, bytes_total? }`
- `CONTENT_CACHED { transfer_id, item_id, local_ref }`
- `TRANSFER_FAILED { transfer_id, code, message? }`
- `TRANSFER_CANCELLED { transfer_id }`

> 约束：主页图表（Cache/Network）不通过事件推送；Shell 通过 `cb_query_cache_stats/cb_query_net_stats` 周期拉取。Core 可在后台更新统计快照，但不要求额外事件类型。

4) Error / Diagnostic（可选但强烈建议）
- `CORE_ERROR { code, message?, context? }`
- `CORE_LOG { level, message, fields? }`

---

### 4.5.7 v1 验收对应关系（把 API 对齐到 M1~M3）

- M1（网络闭环）：
  - 依赖：`cb_init` + `PEER_ONLINE/OFFLINE` + `set_global_policy`
- M2（元数据同步）：
  - 依赖：`ingest_local_copy` 触发对端 `ITEM_META_ADDED`
- M3（Lazy Fetch 正文闭环）：
  - 依赖：`ensure_content_cached` + `CONTENT_CACHED/FAILED/PROGRESS` + 本地落盘引用（local_ref）


## 4.6 数据模型（Core DTO / Domain / DbRow）

本节把 Core 的数据模型写成“字段级规格”，用于：
- Rust 内部实现（Domain / DbRow）
- 跨边界（FFI JSON / 协议 JSON）的 DTO 一致性
- DB schema、CAS 落盘、Lazy Fetch 传输都能对齐

命名分层原则沿用你文档里已有口径（避免 DTO/Entity 混乱）：
- `AccountProfile` / `PeerRule` / `TemporaryGrant`：**领域模型（Domain）**
- `DbAccountRow` / `DbPeerRuleRow`：**数据库行（DB Row）**
- `AccountDto` / `PeerDto`：**跨 FFI 的 DTO**（serde + JSON）

---

### 4.6.1 通用约定（所有模型通用）

- `*_id`：UUID 字符串
- `ts_ms`：Unix epoch 毫秒（i64）
- `sha256`：hex 字符串（小写，64 字符）
- `mime`：IANA MIME（例如 `text/plain`、`image/png`）
- JSON 兼容：
  - 未识别字段必须忽略（向后兼容）
  - 必填字段缺失 → 参数错误（`GEN_INVALID_MESSAGE` 或等价）

---

### 4.6.2 ClipboardSnapshot（壳 → Core：本机复制输入）

> 这是壳监听系统剪贴板后，送入 Core 的“原始快照”。Core 负责生成 item_id、写库、广播元数据。

```json
{
  "type": "ClipboardSnapshot",
  "ts_ms": 0,
  "source_app": "optional-string",
  "kind": "text|image|file_list",

  "share_mode": "default|local_only|force",

  "text": {
    "mime": "text/plain",
    "utf8": "..."
  },
  "image": {
    "mime": "image/png",
    "bytes_b64": "..."
  },
  "files": [
    {
      "rel_name": "Photos/a.png",
      "size_bytes": 123,
      "sha256": "hex-optional",
      "abs_path": "C:\\Users\\...\\report.pdf"
    }
  ]
}
````

规则：

* `kind=text`：必须有 `text.utf8`
* `kind=image`：v1 允许壳直接传 `bytes_b64`（本机），Core 落 CAS；网络传输不走 b64（见 3.3.12）
* `kind=file_list`：
  * `files[]` 必须非空
  * `rel_name` 必须为“相对路径/展示名提示”，不得为绝对路径；不得包含 `..`
  * 分隔符允许为 `/` 或 `\`（Core 会归一化）；不允许控制字符
  * `sha256` 在 v1 可选（可后算）；但若壳已能拿到哈希则建议填写（利于去重与校验）
* v1 限制（默认值见 4.2.3）：
  * `share_mode=default`：遵循软限制；若超限 Core 返回 `ITEM_TOO_LARGE`（建议在错误 context 里带上 soft_limit_bytes/actual_bytes），外壳弹窗决定是否重试
  * `share_mode=local_only`：只写本机历史，不广播元数据
  * `share_mode=force`：无视软限制继续同步，但仍必须遵守硬上限；且 text 超限时不做自动预取
* role ↔ Client/Server
* record_version
* cipher_alg（固定也行，比如 xchacha20poly1305）
* kek_id / key_id（本地 keystore 的 key 标识）
* nonce
* ciphertext
* created_at_ms/rotated_at_ms
---

### 4.6.3 ItemMeta（网络广播/本地历史的“权威元数据”）

> Lazy Fetch 的核心：复制时只同步 meta；正文/文件按需拉取。

```json
{
  "type": "ItemMeta",
  "item_id": "uuid",
  "kind": "text|image|file_list",
  "created_ts_ms": 0,
  "source_device_id": "uuid",
  "source_device_name": "optional",
  "size_bytes": 0,

  "preview": {
    "text": "optional-short",
    "image_hint": { "w": 0, "h": 0 },
    "file_count": 0
  },

  "content": {
    "mime": "text/plain|image/png|application/octet-stream",
    "sha256": "hex",
    "total_bytes": 0
  },

  "files": [
    {
      "file_id": "uuid",
      "rel_name": "Photos/a.png",
      "size_bytes": 123,
      "sha256": "hex-optional"
    }
  ],

  "expires_ts_ms": 0
}
```

字段规则：

* `item_id`：由源设备生成（全局唯一）
* `source_device_id`：源设备在本账号下的 device_id
* `preview.text`：

  * 文本：建议截断到 200~500 字符
  * 图片/文件：可不填
* `content.sha256` / `content.total_bytes`：

  * text/image：必须有（用于 CAS 与校验）
  * file_list：`content` 可用于“列表 JSON 本身”的哈希（可选），真正文件哈希在 `files[]`
* `expires_ts_ms`：v1 可默认 `created + 7d`（或配置），用于清理策略

---

### 4.6.4 LocalContentRef（Core → Shell：本地可用引用）

> `ensure_content_cached` 成功后，Core 给 Shell 的结果必须是“平台可消费”的引用。

```json
{
  "type": "LocalContentRef",
  "kind": "text|image|file",
  "item_id": "uuid",
  "mime": "text/plain|image/png",
  "text_utf8": "optional",
  "local_path": "optional",
  "uri": "optional",
  "sha256": "hex",
  "total_bytes": 0
}
```

**规则：**

* `kind=text`：优先返回 `text_utf8`（Shell 直接写系统剪贴板）。
* `kind=image`：
  * 返回 `local_path`（指向 Core 管理目录中的图片文件）。
  * Shell 职责：读取该路径图片数据，转换为平台剪贴板支持的格式（如 Windows `CF_DIB` / `CF_BITMAP`）写入。
  * 约束说明：
    * image 类型对应的 local_path 必须指向 Core 管理的 CAS（或等价受管存储）中的文件；
    * Core 不得将外部路径、临时下载路径或源应用路径直接暴露给 Shell；
    * image 正文与 file 正文在存储一致性上遵循相同的 CAS 规则。

* `kind=file`：
  * 返回 `local_path`（落盘结果）。
  * 若是文件列表：Core 对每个 file_id 分别拉取，Shell 最终拿到多个 path。
  * **Shell 职责（Windows）**：Shell 必须将这些 `local_path` 封装为 **`CF_HDROP` (DROPFILES 结构)** 写入系统剪贴板，以支持文件粘贴到资源管理器。

---

### 4.6.5 Peer / Session（状态快照模型）

```json
{
  "type": "PeerDto",
  "device_id": "uuid",
  "device_name": "optional",
  "account_tag": "hex",
  "capabilities": ["text","image","file"],
  "state": "Offline|Connecting|TransportReady|AccountVerified|Online|Backoff",
  "last_seen_ts_ms": 0,
  "addrs": ["ip:port", "ip:port"]
}
```

说明：

* `addrs` 来源于 discovery（仅线索，不作安全结论）
* `state` 与你 Session 状态机一致（2.2.6/2.2.7）

---

### 4.6.6 Transfer（Lazy Fetch 传输任务模型）

```json
{
  "type": "TransferDto",
  "transfer_id": "uuid",
  "kind": "content|file",
  "item_id": "uuid",
  "file_id": "optional-uuid",
  "device_id": "uuid",
  "state": "Init|Begin|Streaming|Done|Failed|Cancelled",
  "bytes_done": 0,
  "bytes_total": 0,
  "started_ts_ms": 0
}
```

规则：

* `transfer_id` 必须与 3.3.12 的 `transfer_id` 一致（用于 stream 关联）
* `bytes_total` 来自 `CONTENT_BEGIN/FILE_BEGIN.total_bytes`

---

### 4.6.7 Policy（v1 最小可实现）

```json
{
  "type": "PolicyDto",
  "default": "allow|deny",
  "peer_rules": [
    { "device_id": "uuid", "action": "allow|deny" }
  ],
  "share_ttl_ms": 604800000
}
```

规则：

* v1 允许最小策略：默认 allow/deny + 单 peer 覆盖
* `share_ttl_ms` 影响 `expires_ts_ms` 的默认生成与清理

---

### 4.6.8 DbRow（数据库行镜像：与 schema 一一对应）

> 这里定义“字段级”Row，目的是让 migrations 与代码映射稳定。

建议最小表集合（与 v1 能力对齐）：

* `peers`
* `items`
* `files`（用于 file_list 的条目）
* `history`
* `content_cache`（CAS 索引）
* `transfers`（可选：用于崩溃后诊断；v1 可不持久化）

DbRow 示例（结构示意）：

* `DbPeerRow`：`device_id` `device_name` `account_tag` `last_seen_ts_ms` `capabilities_json`
* `DbItemRow`：`item_id` `kind` `created_ts_ms` `source_device_id` `size_bytes` `mime` `sha256` `total_bytes` `expires_ts_ms` `preview_json`
* `DbFileRow`：`file_id` `item_id` `rel_name` `size_bytes` `sha256`
* `DbHistoryRow`：`row_id`(auto) `item_id` `sort_ts_ms` `source_device_id`
* `DbContentCacheRow`：`sha256` `path` `size_bytes` `present` `last_access_ts_ms`

约束：

* `items.sha256` 必须可作为 CAS key（text/image）
* `content_cache` 以 sha256 为主键，支持去重与存在性检查
* `history` 只负责排序与过滤，不重复存大字段（大字段在 items/preview_json）

---

### 4.6.9 DTO 与协议字段对齐清单（防止“同名不同义”）

* `item_id`：协议（3.3.11）/Wire（3.3.12）/DB（items）/FFI（DTO）必须一致
* `transfer_id`：仅用于 Content/File stream 的传输任务（3.3.12.4/5）
* `sha256 + total_bytes + mime`：

  * 对端响应头（CONTENT_BEGIN/FILE_BEGIN）与本地 CAS/DB 必须一致
* `capabilities`：

  * discovery TXT `cap=...`（线索）
  * HELLO `capabilities`（权威声明）
  * PeerDto.capabilities（本地合并结果）





## 4.7 错误模型与版本策略（v1：实现级规范）

本节定义：
- Core 内部统一错误对象（含：错误码、是否可重试、影响范围）
- 错误如何在：协议 / Core API / FFI / Event 中传播
- 协议版本与存储版本如何做兼容与升级

协议错误码“枚举与含义”以 3.3.11.4 为准（GEN/CONN/TLS/AUTH/POLICY/CONTENT）；
错误使用原则与“是否影响 Session”以 3.3.11.5~3.3.11.6 为准。

---

### 4.7.1 统一错误对象（CoreError）

Core 内部所有失败必须归一到同一种结构（便于日志、UI、重试）：

```json
{
  "type": "CoreError",
  "code": "GEN_INVALID_MESSAGE",
  "message": "human readable, optional",
  "scope": "Core|Session|Transfer|Store|Cas|Ffi",
  "retryable": false,
  "affects_session": false,
  "device_id": "optional-uuid",
  "transfer_id": "optional-uuid",
  "request_id": "optional-uuid",
  "detail": { }
}
````

字段规则：

* `code`：必须来自 3.3.11.4 的错误码集合（或未来新增的同前缀枚举）
* `message`：仅用于人类阅读；程序逻辑只看 `code`（遵循 3.3.11.5）
* `retryable` 与 `affects_session` 由本章 4.7.2 规则决定（不允许“随便填”）
* `detail`：可放调试字段（例如：期望版本/实际版本、size limit、sqlite 扩展信息等）

---

### 4.7.2 错误分层与行为规则（必须写死）

本节把 3.3.11.5/3.3.11.6 的原则落到 Core 行为上：哪些错误会让 Session 退避、哪些只影响单个请求/单个 Stream。

#### A) Handshake / Auth 类（TLS / AUTH / OPAQUE）

* 典型码：`TLS_HANDSHAKE_FAILED`、`TLS_PIN_MISMATCH`、`AUTH_ACCOUNT_TAG_MISMATCH`、`OPAQUE_FAILED`、`AUTH_REVOKED`
* 规则：

  * `affects_session = true`
  * SessionActor 必须进入 Backoff（Connecting → Backoff），并按退避策略重连或等待人工处理（符合 3.3.11.5 第 3 条）
  * `retryable`：

    * 握手失败/超时（TLS/CONN）可能可重试：true（但受 Backoff 控制）
    * 账号撤销/OPAQUE 失败：默认 false（除非用户重新配对/刷新凭据）

#### B) Permission / Policy 类（POLICY）

* 典型码：`PERMISSION_DENIED`、`SHARE_EXPIRED`、`CONTENT_TOO_LARGE`、`RATE_LIMITED`
* 规则（必须符合 3.3.11.6）：

  * `affects_session = false`（不影响在线状态）
  * `retryable = false`（权限错误不得自动重试，避免无意义请求）
  * 对应动作：

    * 直接让该请求失败（TransferFailed / CoreError 事件），但 Session 保持 Online

#### C) Content / Resource 类（CONTENT）

* 典型码：`ITEM_NOT_FOUND`、`ITEM_EXPIRED`、`CONTENT_UNAVAILABLE`、`TRANSFER_CANCELLED`
* 规则（必须符合 3.3.11.6）：

  * `affects_session = false`（只影响单个 Stream/Transfer）
  * `retryable`：

    * `TRANSFER_CANCELLED`：false
    * `CONTENT_UNAVAILABLE`：可按产品策略为 true/false（v1 建议 false，交给用户再次触发）

#### D) Generic / Internal（GEN）

* 典型码：`GEN_INVALID_MESSAGE`、`GEN_PROTOCOL_MISMATCH`、`GEN_INTERNAL_ERROR`
* 规则：

  * `GEN_INVALID_MESSAGE`：

    * `affects_session` 视发生位置：如果是握手/Control 关键帧 → true（可视为协议不一致/对端异常）；如果是某个请求体 → false
  * `GEN_PROTOCOL_MISMATCH`：`affects_session=true`，直接断开并进入 Backoff（版本不兼容）
  * `GEN_INTERNAL_ERROR`：默认 `affects_session=false`，但必须发 `CORE_ERROR` 事件；若连续发生可触发自我保护（可选）

---

### 4.7.3 错误传播路径（协议 / API / FFI / Event）

#### A) 协议层（Control 通道）

* 当某个 Control 请求失败：

  * 返回 `ERROR` 消息（或等价结构），包含：`reply_to`、`code`、`message?`、`scope?`
* 当 Content/File 传输失败（Wire=B）：

  * 可在该 stream 上返回 `ERROR` 头帧后关闭；或在 Control 通道发 `ERROR`（推荐做二者其一，v1 建议：Control 通道统一报错更好排查）

#### B) Core API 同步返回

* 对于 Query 类 API（list_history/get_item_meta/list_peers 等），允许同步返回：

  * `{"ok":true,"data":...}` 或 `{"ok":false,"error":CoreError}`
* 对于 Command 类 API（ensure_content_cached/ingest_local_copy 等），同步阶段只返回：

  * “参数/队列是否接受”的错误（例如：`CORE_BUSY`、`GEN_INVALID_MESSAGE`、`CONTENT_TOO_LARGE`）
  * 真正执行阶段的错误必须通过 Event 返回（见 4.5 事件规范）

#### C) FFI（C ABI）返回与事件

* FFI 同步返回值只表达两类失败：

  1. 入参 JSON 不合法 / 缺字段 → `GEN_INVALID_MESSAGE`
  2. Core 当前不可用（队列满/已 shutdown）→ `CORE_BUSY` / `CORE_SHUTTING_DOWN`
* 网络/传输/权限/内容类失败统一走事件：

  * `TRANSFER_FAILED { transfer_id, code, message? }`
  * `CORE_ERROR { code, message?, context? }`
    （这些事件类型你已在 4.5 的事件规范里定义过，保持一致即可）

---

### 4.7.4 版本策略（v1）

v1 至少需要管理 3 类版本：

1. **协议语义版本**（proto）
2. **Wire 编码版本**（wire：CBFrame + bytes 流规则）
3. **存储 schema 版本**（sqlite migrations + cas layout）

#### A) proto 版本（语义层）

* 采用 `proto_major.proto_minor`（例如 1.0）
* 兼容规则：

  * major 不同：不兼容 → `GEN_PROTOCOL_MISMATCH`，断开会话
  * major 相同 minor 不同：兼容

    * 未识别字段忽略（向后兼容）
    * 新增 message type：旧端收到应忽略并不崩溃（仍保持 Online）

#### B) wire 版本（编码层）

* v1 wire 固定为：`CBFrame(u32be_len + json)` + `CONTENT_BEGIN/FILE_BEGIN + raw bytes`（3.3.12）
* wire 变更（例如压缩、分块校验）必须通过：

  * Control 握手阶段声明 `wire_rev`
  * 若不支持 → `GEN_PROTOCOL_MISMATCH`

#### C) 存储 schema 版本（sqlite）

* schema 采用单调递增 `schema_version`（整数）
* 启动时：

  * 若 db 版本 < 当前版本：自动 migrations（WAL 模式）
  * 若 db 版本 > 当前版本：拒绝启动或只读模式（v1 建议：拒绝启动并报 `GEN_PROTOCOL_MISMATCH` 或专用 `STORE_SCHEMA_TOO_NEW`）

> 你说的“4.6 数据库与缓存”会在 Step 9 写 4.9 时合并进去，届时 schema_version、migrations、CAS layout 会在 4.9 作为权威来源。

---

### 4.7.5 可观测性（必须能定位问题）

* 所有 CoreError 必须写入日志（至少：code/scope/device_id/transfer_id）
* 建议对外提供：

  * `get_status()` 返回最近错误计数/最后错误摘要（你在 4.5 里已经列了诊断接口）
* 发生影响 Session 的错误（affects_session=true）时必须同时：

  * 触发 SessionStateChanged（Online→Backoff）
  * 发出一个可见错误事件（CORE_ERROR 或 PEER_OFFLINE 附 reason）



## 4.8 FFI（C ABI）合约与头文件（v1：实现级规范）

本节定义 Windows Shell（C#）调用 Rust Core 的 **C ABI 边界**：
- 采用“全 JSON 入参/出参 + 单一事件回调”的薄封装（路线 A）
- 明确：字符串所有权、回调线程、错误返回、版本兼容、资源释放
- 头文件路径为：`platforms/windows/core-ffi/include/clipbridge_core.h`

> 设计原则：FFI 只做封送，不写业务逻辑；业务逻辑必须留在 `cb_core`（见 4.3）。

---

### 4.8.1 ABI 版本与兼容策略

- `CB_FFI_ABI_MAJOR` / `CB_FFI_ABI_MINOR`：FFI ABI 版本
- 兼容规则：
  - major 不同：不兼容（初始化失败，返回错误）
  - major 相同 minor 不同：兼容（新增函数/字段必须向后兼容）

Shell 在 `cb_init` 后可调用 `cb_get_ffi_version()` 进行记录与诊断。

---

### 4.8.2 线程模型（再次强调，Shell 必须适配）

- Core 的事件回调 **不会在 UI 线程**；回调来自 Core 的 EventPump 线程（见 4.4/4.5）
- Shell 必须把事件 marshal 回 UI 线程（WinUI Dispatcher）
- 回调不得阻塞：回调内不允许做长时间 IO/等待，否则会拖慢 Core 的网络与存储

---

### 4.8.3 字符串与内存所有权（必须严格遵守）

FFI 所有字符串都用 UTF-8 `char*`。

**入参：**
- 由调用方（Shell）分配与管理，Core 只读，不缓存指针
- 入参为 JSON 字符串（UTF-8）

**出参：**
- 由 Core 分配返回 `char*`（UTF-8）
- 调用方必须使用 `cb_free_string(ptr)` 释放
- 除事件回调参数外，任何 `cb_*` 返回的 `const char*` 在调用 `cb_free_string` 前保持有效。

**事件回调参数（cb_on_event_fn 的 json）**
- `json` 是临时指针，仅在回调期间有效；壳侧必须立刻拷贝后再异步处理。

**禁止：**
- 用 `free()` / `CoTaskMemFree()` 释放 Core 字符串
- Shell 把入参指针长期保存给 Core（Core 绝不依赖调用方指针的生命周期）

---

### 4.8.4 句柄（handle）与生命周期

- `cb_handle_t`：不透明指针（Core 内部实例）
- 规则：
  - 只能通过 `cb_init` 创建
  - 必须通过 `cb_shutdown` 销毁（幂等：重复调用只生效一次）
  - `cb_shutdown` 完成后，Core 不得再触发任何回调

**Handle 的 FFI 表示（v1 实现约定）**

* `cb_init(...)` 同步返回统一 JSON envelope：`{"ok": true, "data": {"handle": <u64>}}`，其中 `handle` 是 Core 内部不透明指针地址按“指针宽度整数”导出（实现为 `usize`）。
* 壳侧必须将该整数当作 **进程内指针** 使用：在后续调用中把它作为 `cb_handle*` 传回。
* 该 handle **仅在创建它的同一进程内有效**：不得持久化、不得跨进程/跨机器传递。
* `cb_shutdown(handle)` 会释放该指针指向的实例；释放后再次使用该 handle 属于未定义行为（壳侧需自行置空）。
---

### 4.8.5 回调接口（事件驱动：单入口）

v1 使用 **单一事件回调**：
- `on_event(const char* event_json, void* user_data)`

事件 JSON 的字段与类型以 4.5.6 为准（`type/ts_ms/payload/...`）。

---

### 4.8.6 错误返回规则（同步返回 vs 异步事件）

#### A) 同步返回（函数立即返回的错误）
只用于：
- JSON 语法错误 / 缺字段 → `GEN_INVALID_MESSAGE`
- Core 不可用（队列满/已 shutdown）→ `CORE_BUSY` / `CORE_SHUTTING_DOWN`
- 参数超限（例如本机 snapshot 超过 v1 限制）→ `CONTENT_TOO_LARGE`（或等价）

同步返回的 JSON 统一格式：
```json
{ "ok": false, "error": { "code": "...", "message": "...", "scope": "Ffi", "retryable": false } }
````

#### B) 异步错误（执行过程中失败）

* 一律通过事件回调发出（`TRANSFER_FAILED` / `CORE_ERROR`）
* 错误码与 4.7 规则一致（affects_session / retryable 等不可乱填）

---

### 4.8.7 头文件规范：clipbridge_core.h（v1）

> 这是“规范级”头文件，实际代码可以按此落地（函数名/字段名尽量一致）。

```c
#pragma once
#include <stdint.h>
#include <stddef.h>

#ifdef _WIN32
  #define CB_CALL __cdecl
  #ifdef CLIPBRIDGE_CORE_EXPORTS
    #define CB_API __declspec(dllexport)
  #else
    #define CB_API __declspec(dllimport)
  #endif
#else
  #define CB_CALL
  #define CB_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

// -------- ABI version --------
#define CB_FFI_ABI_MAJOR 1
#define CB_FFI_ABI_MINOR 0

// -------- Opaque handle --------
typedef struct cb_handle_t cb_handle_t;

// -------- Callback --------
typedef void (CB_CALL *cb_on_event_fn)(const char* event_json_utf8, void* user_data);

// 未来如果你要加更多回调，可扩展这个 struct；但 v1 先保持最小
typedef struct cb_callbacks_t {
  cb_on_event_fn on_event;   // required
} cb_callbacks_t;

// -------- Version / diag --------
CB_API void CB_CALL cb_get_ffi_version(uint32_t* out_major, uint32_t* out_minor);

// -------- Lifecycle --------
// config_json: JSON（包含 data_dir、log_level、limits、device_name 等）
// callbacks: 事件回调
// user_data: 原样回传给回调
CB_API const char* CB_CALL cb_init(
  const char* config_json_utf8,
  const cb_callbacks_t* callbacks,
  void* user_data,
  cb_handle_t** out_handle
);

// 幂等；调用后不再触发回调
CB_API void CB_CALL cb_shutdown(cb_handle_t* handle);

// -------- Memory --------
CB_API void CB_CALL cb_free(const char* ptr);

// -------- Commands / Queries (JSON in / JSON out) --------

// 快照注入：本机剪贴板 -> Core
CB_API const char* CB_CALL cb_ingest_local_copy(
  cb_handle_t* handle,
  const char* snapshot_json_utf8
);

// 历史分页查询
CB_API const char* CB_CALL cb_list_history(
  cb_handle_t* handle,
  const char* query_json_utf8
);

// 查询单条 ItemMeta
CB_API const char* CB_CALL cb_get_item_meta(
  cb_handle_t* handle,
  const char* item_id_json_utf8
);

// Lazy Fetch：确保 content/file 已缓存到本地；返回 transfer_id（或错误）
CB_API const char* CB_CALL cb_ensure_content_cached(
  cb_handle_t* handle,
  const char* req_json_utf8
);

// 取消传输
CB_API const char* CB_CALL cb_cancel_transfer(
  cb_handle_t* handle,
  const char* transfer_id_json_utf8
);

// peers / policy / status
CB_API const char* CB_CALL cb_list_peers(cb_handle_t* handle);
CB_API const char* CB_CALL cb_set_global_policy(cb_handle_t* handle, const char* policy_json_utf8);
CB_API const char* CB_CALL cb_get_status(cb_handle_t* handle);

// --- Policy / Status ---
CB_API const char* CB_CALL cb_get_status(
  cb_handle_t* handle
);

CB_API const char* CB_CALL cb_set_global_policy(
  cb_handle_t* handle,
  const char* policy_json_utf8
);

// --- Local delete (v1) ---
CB_API const char* CB_CALL cb_delete_local(
  cb_handle_t* handle,
  const char* delete_req_json_utf8
);

// --- Cache control (v1) ---
CB_API const char* CB_CALL cb_cache_control(
  cb_handle_t* handle,
  const char* cache_req_json_utf8
);

// --- Stats query (v1) ---
CB_API const char* CB_CALL cb_query_cache_stats(
  cb_handle_t* handle,
  const char* stats_query_json_utf8
);

CB_API const char* CB_CALL cb_query_net_stats(
  cb_handle_t* handle,
  const char* stats_query_json_utf8
);

#ifdef __cplusplus
} // extern "C"
#endif
```

---

### 4.8.8 FFI JSON 负载约定（v1 最小集）

#### 4.8.8.1 cb_init(config_json)

建议字段（缺省可由 Core 填默认值）：

```json
{
  "type": "CoreConfig",
  "data_dir": "C:/Users/.../AppData/Local/ClipBridge",
  "cache_dir": "C:/Users/.../AppData/Local/ClipBridge/cache",
  "log_dir": "C:/Users/.../AppData/Local/ClipBridge/logs",
  "device_name": "Ryan-PC",
  "log_level": "info",
  "limits": {
    "soft_text_bytes": 1048576,
    "soft_image_bytes": 31457280,
    "soft_file_total_bytes": 209715200,

    "text_auto_prefetch_bytes": 262144,

    "hard_text_bytes": 16777216,
    "hard_image_bytes": 268435456,
    "hard_file_total_bytes": 2147483648
  },
  "active_account_uid": "",
  "keystore_mode": "shell",
  "account_keys": [
    { "account_uid": "", "key_id": "", "key_version": "", "data_key_b64": "" }
  ]
}

```
规则：
- `text_auto_prefetch_bytes` 仅用于 text；图片/文件默认不预取
- `hard_*` 是 Core 安全底线：无论 `share_mode` 如何都不能突破

#### 4.8.8.2 返回 JSON（统一 envelope）

所有返回字符串统一为：

* 成功：`{ "ok": true, "data": ... }`
* 失败：`{ "ok": false, "error": CoreError }`（CoreError 结构见 4.7.1）

例如 `cb_ensure_content_cached` 成功返回：

```json
{ "ok": true, "data": { "transfer_id": "uuid" } }
```

#### 4.8.8.3 GlobalPolicy（v1：停止/继续分享 + allowlist 下发）

`cb_set_global_policy(handle, policy_json) -> ok_json`

入参（示例）：
```json
{
  "sharing": {
    "enabled": true,
    "inbound_enabled": true,
    "outbound_enabled": true,

    "inbound_allow_peers": ["peer_id_A", "peer_id_B"],
    "outbound_allow_peers": ["peer_id_A", "peer_id_C"],

    "mode": "peer_allowlist",
    "circles": [] 
  }
}
````

语义（v1 必须实现）：

* `sharing.enabled=false`：等价于 inbound/outbound 全关（不接收、不广播）
* `inbound_enabled=false`：Core 忽略/丢弃来自对端的元数据（不落库、不产生事件）
* `outbound_enabled=false`：Core 不向外广播本机元数据（但本机仍可 ingest）
* allowlist 用于过滤目标：

  * inbound：仅 allow 的 peer 才能进入系统（其余直接丢弃）
  * outbound：仅 allow 的 peer 作为广播目标（其余不发送）

vNext 伏笔（不实现，仅定义）：

* `mode="circles"` 与 `circles` 字段用于未来 Sharing Circles；v1 固定为 `peer_allowlist`。

返回（示例）：

```json
{ "ok": true }
```

---

#### 4.8.8.4 DeleteLocal（v1：本机隐藏/删除元数据）

`cb_delete_local(handle, delete_req_json) -> ok_json`

入参（示例）：

```json
{
  "item_id": "it_..."
}
```

语义：

* v1 仅做本机删除（不广播），建议实现为 DB 软删/隐藏标记，以保证 list_history 默认不返回已删除条目。
* 若该 item 正在 transfer：允许继续完成落盘，但 UI 侧不可见（可选：实现时可取消）。

返回：

```json
{ "ok": true }
```

---

#### 4.8.8.5 CacheControl（v1：清空/修剪缓存）

`cb_cache_control(handle, cache_req_json) -> ok_json`

入参（示例）：

```json
{
  "action": "CLEAR_ALL"
}
```

或：

```json
{
  "action": "PRUNE_TO_LIMITS"
}
```

语义：

* `CLEAR_ALL`：清空本机 CAS/content_cache（不影响历史元数据）
* `PRUNE_TO_LIMITS`：按 4.9.7 的 LRU/容量策略执行一次 GC

返回：

```json
{ "ok": true }
```

---

#### 4.8.8.6 StatsQuery：Cache / Network（v1：主页图表）

`cb_query_cache_stats(handle, stats_query_json) -> stats_json`
`cb_query_net_stats(handle, stats_query_json) -> stats_json`

统一入参（示例）：

```json
{
  "start_ts_ms": 0,
  "end_ts_ms": 0,
  "bucket_sec": 10
}
```

约定：

* `start_ts_ms=0,end_ts_ms=0` 表示 Core 选择默认窗口（建议：最近 60 分钟）
* `bucket_sec` 为时间桶大小（建议支持 5/10/30/60）

Cache 返回（示例）：

```json
{
  "window": { "start_ts_ms": 123, "end_ts_ms": 456, "bucket_sec": 10 },
  "series": [
    { "ts_ms": 123, "cache_bytes": 1048576 },
    { "ts_ms": 133, "cache_bytes": 2097152 }
  ],
  "current_cache_bytes": 2097152
}
```

Network 返回（示例）：

```json
{
  "window": { "start_ts_ms": 123, "end_ts_ms": 456, "bucket_sec": 10 },
  "series": [
    { "ts_ms": 123, "bytes_sent": 12000, "bytes_recv": 8000 },
    { "ts_ms": 133, "bytes_sent": 0, "bytes_recv": 64000 }
  ]
}
```



---

### 4.8.9 Shell 侧最小使用流程（按这个写就能跑通）

> 本节是“可落地的实现级流程”。关键口径：
> - Core 负责：入库、去重、CAS、网络会话与传输、以及“缓存是否已就绪”的权威判断
> - Shell 负责：UI/交互触发点、系统剪贴板写入、以及在合适时机调用 Core 拉取正文

1. 调 `cb_init`（传 config + on_event 回调）
  - 注意：回调可能不在 UI 线程；Shell 必须自行 marshal 到 UI 线程（见 4.5.2/并发模型）。

2. on_event 收到：
  - `PEER_ONLINE/OFFLINE`：更新 UI
  - `ITEM_META_ADDED`：把 meta 插入历史列表/主页 feed（本机与远端都统一处理）

3. 本机复制变化（系统剪贴板 → Core）：
  - Shell 组 `ClipboardSnapshot` JSON（4.6.2）
  - 根据 UI/设置决定 `share_mode=default|local_only|force`
  - 调 `cb_ingest_local_copy`

4. 远端元数据到达（对端 `ITEM_META_ADDED`）时的 Shell 行为：
  - **只更新 UI，不自动写系统剪贴板**
  - 如需“收件箱式体验”（可选能力），由 Shell 决定是否对“文本且较小”的条目立即发起拉取：
    - 条件建议：`kind=text` 且 `size_bytes <= text_auto_prefetch_bytes`
    - 行为：Shell 调 `cb_ensure_content_cached(item_id)`，等待 `CONTENT_CACHED`
  - 若不启用该体验：保持纯 Lazy Fetch，直到用户选中/粘贴时才拉取

5. 用户在 UI 上“选中某条元数据并准备粘贴/复制到系统剪贴板”（核心触发点）：
  - 触发来源：主页条目点击、历史页条目点击、QuickPaste 小窗条目点击、或快捷键“粘贴上一条/选中条目”等
  - Shell 调用 `cb_ensure_content_cached(item_id, ...) -> transfer_id`
  - Shell 等待事件：
    - 成功：`CONTENT_CACHED { transfer_id, local_ref: LocalContentRef }`
    - 失败：`TRANSFER_FAILED { transfer_id, error }`（或等价错误事件）
  - Shell 拿到 `LocalContentRef` 后写入系统剪贴板（规则见 4.6.4 与 §5.4）

6. 写入系统剪贴板成功后（可选）：
  - Shell 可将该 item 标为“最近使用/置顶候选”
  - v1 不要求自动模拟 Ctrl+V（未来能力可在 5.4/5.5 扩展）


---

### 4.8.10 约束（为了 v1 可控）

* v1 不要求把二进制跨 FFI 直接回传给 Shell：图片/文件都以 `local_path` 交付（更稳）
* v1 不做“跨语言 async await”：一律事件回调驱动（避免 C# 等待 Rust task）
* v1 不暴露内部锁/线程：Shell 不允许假设 Core 的线程数量与调度策略




## 4.9 存储与缓存（SQLite + CAS）

本章定义 Core 的**本地持久化与缓存**：SQLite（权威元数据 / 历史 / 策略）+ CAS（正文/文件内容）。
目标：做到 M0/M2 能闭环——写库、查历史、Lazy Fetch 命中/回源、并且可清理、可迁移、可并发。

> 口径继承自 4.6：
> - SQLite 使用 WAL；正文缓存为文件 CAS：`<cache>/blobs/sha256/xx/sha256`，DB 只记录 sha256、大小、present、last_access；清理由容量/TTL/历史上限触发
> - 约束：`items.sha256` 必须可作为 CAS key；`content_cache` 以 sha256 为主键；`history` 不存大字段
> - `UNIQUE(account_uid, item_id) ON CONFLICT IGNORE`
> 语义：同一账号下，同一个 `item_id` 重复到达时只保留一条 history 记录（避免重放/重连导致历史膨胀）。
> 如需“同 item 多次出现于历史”这种 UX，应该由 **生成新 item_id**（代表新的复制事件）实现，而不是复用老 item_id。
---

### 4.9.1 存储分层与“权威性”边界

- **SQLite = 权威层（Source of Truth）**
  - ItemMeta / 历史排序 / 设备资料 / 账号与认证记录 / 权限与策略。
  - “是否存在某条历史 / 某条 meta / 某条规则”以 DB 为准。
- **CAS = 缓存层（可丢弃、可重建）**
  - 仅存“可由 sha256 定位”的正文 bytes（text/image/file payload）。
  - CAS 丢了不影响历史列表；只会导致 Lazy Fetch 时需要回源拉取。

---

### 4.9.2 磁盘目录布局（跨平台统一口径）

Core 只需要 2 个根目录：`data_dir`（持久）与 `cache_dir`（可清空）。

推荐（实现上用 `directories` crate 获取）：
- `data_dir/`
  - `core.db`（SQLite 主库，WAL）
  - `core.db-wal` / `core.db-shm`（WAL 运行时文件）
- `cache_dir/`
  - `blobs/sha256/xx/<sha256>`（CAS：正文/文件内容）
  - `tmp/`（下载/写入 CAS 的临时文件，成功后 rename）
  - `download/`（可选：对“文件列表”提供落地目录，给 shell 选择最终保存位置前的暂存）

约定：
- CAS 写入必须“先写 tmp，再原子 rename”以保证 crash-safe。
- 任何时候都允许用户清空 cache_dir；Core 需要自愈（DB 仍可用，present=0 触发回源）。

---

### 4.9.3 SQLite 全局约定

- PRAGMA：
  - `journal_mode=WAL`
  - `synchronous=NORMAL`（v1 默认；后续可配置）
  - `foreign_keys=ON`
- 版本与迁移：
  - 使用 `PRAGMA user_version = <schema_version>` 管理迁移。
  - 迁移以“递增脚本”形式执行：`v1 -> v2 -> v3 ...`，每步必须幂等或可检测已执行。
- 时间戳：
  - 所有 `*_ts_ms` 为 Unix epoch 毫秒（i64）。
- ID：
  - `item_id / peer_device_id / account_uid`：v1 统一用 UUID 字符串（或 BLOB(16) 也行，但必须全库一致；不要混用）。

---

### 4.9.4 最小表结构（v1 可落地）

> 这里给出“实现 M0/M2/M3 必须”的最小字段集合。后续字段扩展必须向后兼容（新增列可为空/有默认）。

#### A) devices（设备资料）
- `peer_device_id` TEXT PRIMARY KEY
- `display_name` TEXT
- `last_seen_ts_ms` INTEGER NOT NULL

索引：
- `idx_devices_last_seen` on (`last_seen_ts_ms`)

#### B) items（权威元数据：ItemMeta 的落库）
- `item_id` TEXT PRIMARY KEY
- `kind` TEXT NOT NULL            -- text|image|file_list
- `owner_device_id` TEXT NOT NULL -- 来源设备（peer_device_id）
- `created_ts_ms` INTEGER NOT NULL
- `size_bytes` INTEGER NOT NULL
- `mime` TEXT NOT NULL
- `sha256_hex` TEXT NOT NULL      -- 64 hex lower
- `preview_json` TEXT             -- 小预览（可选；严禁放大字段）
- `expires_ts_ms` INTEGER         -- 可选：TTL

索引：
- `idx_items_created` on (`created_ts_ms`)
- `idx_items_sha256` on (`sha256_hex`)
- `idx_items_owner_created` on (`owner_device_id`, `created_ts_ms`)

约束建议：
- `sha256_hex` 必须可作为 CAS key

#### C) history（历史排序 + 过滤，不存大字段）
- `history_id` INTEGER PRIMARY KEY AUTOINCREMENT
- `account_uid` TEXT NOT NULL
- `item_id` TEXT NOT NULL
- `sort_ts_ms` INTEGER NOT NULL   -- v1 固定：用于 UI 排序，一律取 ItemMeta.created_ts_ms（本机复制则取 snapshot.ts_ms；若极端缺失才 fallback=接收时刻）
- `source_device_id` TEXT           -- 可选：来源 peer（用于显示“来自谁”）
- `is_deleted` INTEGER NOT NULL DEFAULT 0  -- 软删除（清理/隐藏）

索引：
- `idx_history_account_sort` on (`account_uid`, `sort_ts_ms` DESC)
- `idx_history_item` on (`item_id`)

排序规则（v1 固定）：
- UI 查询历史时排序：`ORDER BY sort_ts_ms DESC, history_id DESC`
  - `history_id` 仅用于同一毫秒内的稳定 tie-break（不参与跨设备一致性）
  约束建议：
- `history` 只负责排序与过滤，不重复存大字段

#### D) content_cache（CAS 存在性 + 访问时间）
- `sha256_hex` TEXT PRIMARY KEY
- `total_bytes` INTEGER NOT NULL
- `present` INTEGER NOT NULL      -- 0/1：CAS 文件是否“应当存在且已验证”
- `last_access_ts_ms` INTEGER NOT NULL
- `created_ts_ms` INTEGER NOT NULL

索引：
- `idx_cache_lru` on (`present`, `last_access_ts_ms`)
- `idx_cache_size` on (`present`, `total_bytes`)

约束建议：
- `content_cache` 以 sha256 为主键，支持去重与存在性检查

#### E) （v1 若要做账号/策略闭环）accounts / opaque_records / peer_rules / temporary_grants
这些表字段以“4.6.2 SQLite ↔ Core 结构对照表”为准（不在此重复写 SQL），本章主要约束它们的：
- 主键与作用域必须稳定（account_uid / peer_device_id / grant_id）
- 更新必须在事务内完成（尤其是 rule/grant 的增删改）

* aad = "cb:opaque:v1" + account_uid + role + record_version（不落库，按规则生成）
* Seal(kek_id, nonce=random, aad, plaintext=opaque_record_bytes) -> ciphertext
* 解密失败统一报 KEYSTORE_CRYPTO_FAIL / STORE_CORRUPTED（你已有错误模型的话就对齐到你那套）

---

### 4.9.5 关键写入流程（必须按事务实现）

#### 4.9.5.1 本机复制写入（Shell -> Core：ClipboardSnapshot）
目标：生成 item_id、落 items、落 history、落 content_cache、落 CAS（如需要），并对“重复内容”去重。

事务边界（建议：一个写事务）：
1. 解析 ClipboardSnapshot，生成 `item_id`、`created_ts_ms`。
2. 计算 sha256（text/image/file chunk）。
3. `INSERT OR IGNORE content_cache(sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)`
4. items：
  - 若允许“同 sha256 多条不同 item_id”（推荐允许）：直接插入新 item。
5. history：
  - 插入一条 history（sort_ts_ms = created_ts_ms 或 now）。
6. CAS：
  - 若 `content_cache.present=0` 或本地文件不存在：写入 `cache_dir/tmp/<uuid>`，完成后 rename 到 `blobs/sha256/xx/<sha256>`，再把 present=1。

失败回滚：
- DB 事务回滚即可；CAS 写 tmp 不 rename 就不会污染。
- 若 CAS rename 成功但 DB 失败：启动时做一次 “CAS 扫描修复/或 present 校验”。

#### 4.9.5.2 收到远端 ItemMeta（网络 -> Core：ITEM_META）
目标：入库 items + history，但**不要求有正文**（Lazy Fetch）。

步骤：
1. upsert items（以 item_id 为主键；若已存在则忽略/更新非破坏字段）
2. 写 history（account_uid + sort_ts_ms）
3. upsert content_cache（present=0，last_access=now）——表示“我知道这个 sha256，但本地未必有 CAS”

---

### 4.9.6 Lazy Fetch：命中逻辑（读路径）

当 Shell 针对某条 ItemMeta 显式请求正文内容时：
1. 查 items 得到 `sha256_hex + mime + total_bytes`
2. 查 content_cache：
  - `present=1` 且 CAS 文件存在：直接读本地 bytes（命中）
  - 否则：走网络 `GET_ITEM_CONTENT / GET_FILE` 拉取（回源）
3. 回源成功后：
  - 写入 CAS（tmp -> rename）
  - `UPDATE content_cache SET present=1, last_access_ts_ms=now`
  - 
#### Lazy Fetch 的结果交付约束

Lazy Fetch 的执行结果，Core 仅通过事件形式向 Shell 交付 `LocalContentRef`。

* Core 不通过同步返回值直接暴露正文数据；
* Shell 必须等待对应的 `CONTENT_CACHED`（或等价）事件；
* Shell 后续所有剪贴板写入、文件导出行为，均以 `LocalContentRef` 为输入。

---

### 4.9.7 清理策略（v1 最小实现）

触发条件（任一满足即可触发一次清理任务）：
- 历史条数超过上限（M0 必须）
- cache_dir 下 CAS 总大小超过上限（M0 必须）
- items/history TTL 到期（可选，v1 可以先不做 expires_ts_ms）

#### 4.9.7.1 历史清理（History GC）
策略：
- 按 `history.account_uid + sort_ts_ms` 从旧到新删除（或软删除 is_deleted=1）
- 删除 history 不一定删除 items（因为 items 可能被多个 history 引用；v1 可简单：history 删除后，如果没有任何 history 引用该 item_id，则删除 items）

#### 4.9.7.2 CAS 清理（Cache GC）
策略（LRU）：
- 从 `content_cache` 中挑选 `present=1` 且 `last_access_ts_ms` 最旧的条目开始删文件，直到低于容量上限。
- 每删除一个 CAS 文件：`UPDATE content_cache SET present=0`
- 注意：即使 items/history 仍引用该 sha256，也允许删（因为 CAS 是缓存层，可回源）。

> 约束：`cb_cache_control(PRUNE_TO_LIMITS/CLEAR_ALL)` 必须复用本节清理实现；`cb_query_cache_stats` 的 `current_cache_bytes` 与时间序列必须与实际 CAS/content_cache 一致，作为 Shell 主页展示的权威数据源。

---

### 4.9.8 并发模型（Rust 实现建议）

- SQLite 连接：
  - 1 个“写连接”（串行写入，所有写操作通过队列发送给存储线程）
  - N 个“读连接”（只读查询可并发）
- 好处：
  - 避免多线程争用写锁导致抖动
  - 事务边界清晰（上面的写入流程不会被拆散）
- CAS I/O：
  - 允许在 async task 中写 tmp 文件
  - rename + DB 更新仍回到“写线程事务”保证一致性

---

### 4.9.9 与 DTO/协议的一致性检查点（防止同名不同义）

（本节与 4.6.9 保持一致，作为实现自检清单）
- `item_id`：协议 / Wire / DB(items) / FFI(DTO) 必须一致
- `sha256 + total_bytes + mime`：回源响应头与本地 CAS/DB 必须一致

### 4.9.10 存储与缓存内部接口契约（Repo / CAS / GC）

本节定义 Core 内部的“存储与缓存接口边界”，用于：
- 让 `clipboard/`、`transfer/`、`session/` 只依赖接口，不直接写 SQL/文件路径
- 限制耦合：DB schema 改动时，影响集中在 `store/` 与 `cas/`
- 为测试提供可替换实现（内存 store / 临时目录 cas）

> 本节是内部契约（非对外 API）。对外 API 仍以 4.5 为准。

---

#### 4.9.10.1 依赖方向（必须遵守）

- `clipboard/`、`transfer/`、`session/` → 只能依赖：
  - `store::repos::*`（Repo trait）
  - `cas::Cas`（CAS trait）
  - `policy::PolicyEngine`
- `store/` 可以依赖 `model::DbRow` 与 SQLite driver
- `cas/` 只能依赖文件系统与 `model` 中的轻量 DTO（sha256/size）

禁止：
- 业务模块（clipboard/transfer）直接引用 SQLite 连接/SQL 字符串
- FFI 直接访问 store/cas（FFI 只能调 4.5 的 api 入口）

---

#### 4.9.10.2 Repo trait（SQLite 的抽象）

> 下面是 v1 最小集合。实现可以拆分为多个 repo，也可以一个 struct 实现多个 trait，但接口必须覆盖这些语义。

##### A) ItemRepo（items 表）
- `upsert_item(meta: ItemMeta) -> ()`
- `get_item(item_id) -> Option<ItemMeta>`
- `get_item_by_sha(sha256) -> Option<ItemMeta>`（可选：用于去重/诊断）

##### B) HistoryRepo（history 表）
- `append_history(account_uid, item_id, sort_ts_ms, source_device_id?) -> ()`
- `list_history(account_uid, cursor?, limit, filters?) -> HistoryPage`
- `soft_delete_history(account_uid, item_id) -> ()`（可选）
- `prune_history(account_uid, keep_latest_n) -> PruneResult`

##### C) CacheIndexRepo（content_cache 表）
- `touch_cache(sha256, now_ms) -> ()`
- `get_cache_entry(sha256) -> Option<CacheEntry>`
- `mark_present(sha256, total_bytes, now_ms) -> ()`
- `mark_missing(sha256, now_ms) -> ()`
- `select_lru_candidates(bytes_to_free) -> Vec<CacheEntry>`

##### D) PeerRepo（devices 表等）
- `upsert_peer(peer: PeerDto) -> ()`
- `list_peers() -> Vec<PeerDto>`
- `update_last_seen(device_id, ts_ms) -> ()`

##### E) PolicyRepo（可选：若策略要持久化）
- `load_policy(account_uid) -> PolicyDto`
- `save_policy(account_uid, policy) -> ()`

---

#### 4.9.10.3 CAS trait（文件缓存的抽象）

CAS 以 sha256 为 key，提供“存在性、读取、写入、删除”的最小能力。

- `cas_path(sha256) -> PathBuf`（用于诊断/返回给 shell 的 local_path）
- `exists(sha256) -> bool`
- `open_read(sha256) -> ReadHandle`（可选：流式读取）
- `put_stream(sha256, total_bytes, reader) -> PutResult`
  - 必须实现：写入 tmp -> fsync（可选）-> rename 原子落地
- `remove(sha256) -> ()`
- `verify(sha256, expected_size?) -> bool`（可选：启动时/命中时校验）

约束：
- `put_stream` 不得把全部内容读入内存（尤其是 file）
- 失败必须不污染最终路径（只留 tmp）

---

#### 4.9.10.4 事务边界与一致性约定（实现者必须遵守）

v1 要求做到以下“足够一致”，避免历史和缓存状态对不上：

##### A) ingest_local_copy 的一致性
- DB：items/history/content_cache 的写入必须在同一个“写序列”中完成（建议一个事务）
- CAS：允许先写 CAS 再落 DB，但最终必须保证：
  - DB 若记录 present=1，则 CAS 文件必须存在
  - 若 CAS 写成功但 DB 事务失败：启动时应能自愈（present 校验/重建索引）

##### B) Lazy Fetch 的一致性
- 回源成功后必须按顺序：
  1) CAS 落地成功
  2) DB `mark_present` + `touch_cache`
  3) 才能发 `CONTENT_CACHED` 事件给 shell
- 回源失败不得写 present=1

---

#### 4.9.10.5 GC（清理）接口（可定时或按触发调用）

定义一个统一的清理入口，避免多个地方各写各的清理：

- `run_gc(reason: "HistoryLimit"|"CacheLimit"|"Startup"|"Manual") -> GcReport`

GcReport 建议包含：
- 释放的 bytes
- 删除的 history 条数
- 置 missing 的 sha256 数
- 耗时 ms

触发点（v1 最小）：
- ingest_local_copy 之后（若超过上限）
- Lazy Fetch 成功之后（若超过上限）
- Core 启动后（可选：轻量校验 present 与文件存在性）

---

#### 4.9.10.6 测试替身（为了 4.10 验收用例）

为了让 4.10 的用例可以“无网络/无真实磁盘”跑通，建议提供：
- `InMemoryStore`（实现上述 Repo trait，但用 HashMap）
- `TempDirCas`（写到临时目录）
- 这样 M0 的测试可以完全在 CI 里跑，不依赖真实环境


## 4.10 测试与验收用例（v1）

本章把 v1 的 M0~M3 里程碑转成“可验证的测试与验收用例”，用于：
- Core 开发期间作为回归标准（CI 可跑）
- Shell 集成时作为黑盒验收（只看事件与返回 JSON）
- 防止协议/Wire/存储演进导致功能悄悄退化

测试分三层：
1) 单元测试（proto 编解码、CAS 路径、错误码映射）
2) 组件测试（store/cas repo、GC、history 查询）
3) 集成测试（inproc peers：两端 Core + fake transport + 事件序列断言）

> 建议：所有测试都以“事件序列”作为核心断言（见 4.5.6），并且对 DB/CAS 做额外断言。

---

### 4.10.1 通用测试夹具（Test Harness）

建议提供统一的测试工具（位于 `cb_core/testsupport`）：
- `TestCore`：包一层 CoreHandle + 事件收集器（收集 on_event JSON）
- `FakeTransport`：inproc 连接（可模拟断线、延迟、丢包）
- `TempDirCas`：临时目录 CAS
- `InMemoryStore` 或 `TempDbStore`：内存/临时 SQLite

通用断言工具：
- `assert_event(type, predicate)`：按顺序匹配事件类型与 payload
- `assert_no_event(type, within_ms)`：用于“不应该发生”的场景
- `assert_db(query, expected)`：用于 schema 断言
- `assert_cas_present(sha256)` / `assert_cas_missing(sha256)`

---

### 4.10.2 M0：纯本地闭环（不联网）

#### 用例 M0-1：Text 快照写入 + 历史可查
**输入**
- 调 `ingest_local_copy(ClipboardSnapshot kind=text)`（4.6.2）

**期望**
- 返回 ok，得到 `item_id`
- 事件：`ITEM_META_ADDED`（本机也可以发，便于 UI 统一）
- DB 断言：
  - `items` 存在 `item_id`
  - `history` 新增一条（sort_ts_ms 合理）
  - `content_cache` 对应 sha256 present=1
- CAS 断言：
  - `<cache>/blobs/.../<sha256>` 文件存在，大小符合

#### 用例 M0-2：Image 快照写入 + CAS 去重
**输入**
- 连续两次注入同一张图片（bytes 相同）

**期望**
- DB：
  - `items` 有两条 item（item_id 不同）或允许你实现“去重到同 item”（二选一，但必须在文档中固定；v1 推荐：item_id 不同但 sha256 相同）
  - `content_cache` 只有 1 条 sha256（主键去重）
- CAS：
  - blob 文件只有 1 份（同 sha256）

#### 用例 M0-3：GC（缓存容量触发）
**输入**
- 设置极小 cache 上限（例如 1MB）
- 写入多条大内容，触发 `run_gc`

**期望**
- CAS：最旧 blob 被删除（LRU）
- DB：对应 `content_cache.present=0`
- 历史仍可列出（meta 不丢）

---

### 4.10.3 M1：网络闭环（发现 + 会话上线）

#### 用例 M1-1：Peer 上线/下线事件
**输入**
- 启动 CoreA/CoreB（同 account_tag）
- FakeTransport 让双方可连接

**期望**
- A 收到：`PEER_ONLINE(B)`
- B 收到：`PEER_ONLINE(A)`
- 断开 transport 后：
  - 双方收到 `PEER_OFFLINE` 或 `SessionStateChanged Online->Backoff`
- 断言：不会因为一次断线导致 Core 崩溃；会进入 backoff 并可重连（2.2.7 策略）

#### 用例 M1-2：账号不匹配（AUTH_ACCOUNT_TAG_MISMATCH）
**输入**
- CoreA 与 CoreB 使用不同 account_tag

**期望**
- 会话失败，返回/事件错误码：`AUTH_ACCOUNT_TAG_MISMATCH`
- `affects_session=true`：进入 Backoff
- 不得进入 Online，不得交换 meta

---

### 4.10.4 M2：元数据同步闭环（ITEM_META）

#### 用例 M2-1：A 复制 text，B 收到 ItemMeta 并入库
**输入**
- A：ingest_local_copy(text)
- A 在线广播 meta（内部自动做或显式触发）

**期望**
- B 收到事件：`ITEM_META_ADDED`（payload 包含 item_id/kind/sha256/preview）
- B 的 DB：
  - `items` 存在该 item_id（present=0 合理）
  - `history` 有记录
  - `content_cache` 存在该 sha256 且 present=0

#### 用例 M2-2：重复 meta 不应导致历史爆炸
**输入**
- 重放同一条 ITEM_META（相同 item_id）

**期望**
- DB：
  - items 不重复（主键约束）
  - history 行为必须固定：
    - 方案 A：允许再次 append（会导致重复历史）【不推荐】
    - 方案 B：按 (account_uid,item_id) 去重（推荐）
- v1 推荐在文档固定为：方案 B（避免 UI 重复）

---

### 4.10.5 M3：Lazy Fetch 正文闭环（Wire=B）

#### 用例 M3-1：Text Lazy Fetch 命中/回源
**准备**
- A/B 在线
- A 已有 text item，B 仅有 meta（present=0）

**输入**
- B：`ensure_content_cached(item_id, part=content)`

**期望事件序列（B）**
1) `TRANSFER_PROGRESS`（可选）
2) `CONTENT_CACHED { local_ref }`（必须）
  - local_ref.kind=text 且 text_utf8 存在（或 local_path 指向文本文件，二选一但要固定）
3) DB/CAS 断言：
  - `content_cache.present=1`
  - blob 文件存在且 sha256 匹配

#### 用例 M3-2：Image Lazy Fetch（raw bytes）
**输入**
- B：ensure_content_cached(image item)

**期望**
- transfer 以 raw bytes 写入 CAS（不走 base64）
- local_ref.kind=image 且 local_path 指向图片文件
- 校验 total_bytes + sha256 一致（不一致必须失败）

#### 用例 M3-3：File Lazy Fetch（单文件一个 transfer）
**准备**
- A 发布 file_list meta（包含 files[]）

**输入**
- B 对其中一个 file_id 发起 `ensure_file_cached(item_id, file_id)`

**期望**
- B 接收 FILE_BEGIN + raw bytes，落盘到受控目录
- local_ref.kind=file 且 local_path 可被 shell 读取
- 取消/失败不会污染最终路径（只留 tmp）

#### 用例 M3-4：取消传输（CANCEL）
**输入**
- B 发起下载大文件，进行中调用 `cancel_transfer(transfer_id)`

**期望**
- 事件：`TRANSFER_CANCELLED`
- DB：content_cache.present 仍为 0（或保持原值）
- FS：最终路径不存在；tmp 可被清理

---

### 4.10.6 错误与鲁棒性用例（必须覆盖）

#### 用例 E-1：权限拒绝不影响在线（PERMISSION_DENIED）
**输入**
- 配置 policy 让 B 请求 A 的内容被拒绝

**期望**
- B 收到 `TRANSFER_FAILED code=PERMISSION_DENIED`
- Session 仍保持 Online（affects_session=false）

#### 用例 E-2：内容不存在（ITEM_NOT_FOUND）
**输入**
- B 请求一个不存在的 item_id

**期望**
- `TRANSFER_FAILED code=ITEM_NOT_FOUND`
- 不影响 Session

#### 用例 E-3：协议/版本不兼容（GEN_PROTOCOL_MISMATCH）
**输入**
- 让对端声明不支持 wire_rev / proto_major

**期望**
- 断开并进入 Backoff
- 报错可见（CORE_ERROR 或 reason）

---

### 4.10.7 CI 运行建议（非强制）

- 单元/组件测试：每次 push 必跑
- 集成测试（inproc peers）：至少在 PR 必跑
- Windows 平台：可加一个最小 smoke test（加载 DLL，cb_init/cb_shutdown 能跑通）



---






------
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
## 5.4 读取内容与写入系统剪贴板

本节定义 **平台外壳（Shell）如何基于 `ItemMeta` 获取正文内容，并将其写入系统剪贴板**。
该流程是所有 UI 行为（主页、历史列表、Quick Paste 小窗）的底层依赖。

### 5.4.1 设计目标与约束

* Shell 不直接处理网络、去重或缓存一致性；
* Shell 不解析或拼装远端协议数据；
* Shell 只通过 Core 提供的接口与事件获取内容；
* Shell 写入系统剪贴板的输入 **必须是 `LocalContentRef`**。

---

### 5.4.2 Shell 侧内容状态模型（实现级）

对于每一条 `ItemMeta`，Shell 在本地维护如下状态之一：

* **MetaOnly**

  * 仅持有 `ItemMeta`；
  * 尚未向 Core 请求正文内容。

* **Fetching**

  * 已向 Core 发起正文/文件请求；
  * 等待 `LocalContentRef` 事件返回。

* **Ready**

  * 已收到 `LocalContentRef`；
  * 对应内容已在本机可用，可写入系统剪贴板。

该状态模型仅存在于 Shell 内部，不向 Core 暴露。

---

### 5.4.3 触发正文读取的时机

Shell 在以下情况下可以或必须触发正文读取：

1. **用户在 UI 中选中某条内容**

* 主页当前条目
* 历史列表点击
* Quick Paste 小窗中选中
2. **用户即将执行粘贴 / 保存 / 导出**

* 若状态非 Ready，必须先拉取正文
3. **Shell 收到远端 ItemMeta**

* 仅进入 MetaOnly 状态
* 不自动拉取正文

---

### 5.4.4 正文读取流程（Shell → Core）

当 Shell 决定读取正文时，执行以下流程：

1. 根据 `ItemMeta.kind` 判断类型：

* `text / image`：调用
  `ensure_content_cached(item_id, mime, mode)`
* `file`：对 `files[]` 中的每个 `file_id` 调用
  `ensure_file_cached(item_id, file_id, mode)`

2. Shell 将该条目状态置为 **Fetching**；

3. Shell 不阻塞 UI，不轮询，不假定同步返回；

4. Shell 等待 Core 通过事件回调返回 `LocalContentRef`。

---

### 5.4.5 接收 LocalContentRef 与状态更新

当 Shell 收到 Core 发出的 `CONTENT_CACHED`（或等价）事件时：

1. 校验 `item_id / file_id` 与当前请求是否匹配；
2. 将对应条目状态置为 **Ready**；
3. 缓存 `LocalContentRef` 供后续使用。

---

### 5.4.6 写入系统剪贴板

当条目状态为 **Ready** 时，Shell 按类型写入系统剪贴板：

* **text**

  * 使用 `LocalContentRef.text_utf8`
* **image**

  * 从 `LocalContentRef.local_path` 读取图片数据
  * 写入系统剪贴板图像格式
* **file**

  * 将 `LocalContentRef.local_path`（或路径列表）
    写入系统剪贴板文件列表格式

Shell **不得**：

* 在未 Ready 时写入占位数据；
* 使用非 Core 管理的外部路径；
* 直接复用远端来源路径。

---

### 5.4.7 与 Quick Paste 的关系

* Quick Paste 仅负责 **UI 选择与触发**
* 实际内容读取与剪贴板写入逻辑，**全部复用本节流程**
* Quick Paste 不拥有独立的数据流或缓存逻辑

---
## 5.5 Quick Paste 呼出小窗

### 5.5.1 目标

提供一个可通过全局热键呼出的顶层小窗（类似 Win+V 的历史选择与粘贴），支持“键盘优先”的历史选择与粘贴；对大内容按 Lazy Fetch 拉取并显示进度，可取消。

### 5.5.2 交互规格（必须严格定义）

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

### 5.5.3 窗口行为与样式约束

* 顶层窗口、无标题栏、圆角、阴影（符合现代 Windows）
* 不出现在任务栏，不进入 Alt-Tab（ToolWindow 行为）
* 显示时置顶（Topmost），隐藏时释放置顶
* 自动定位策略：以鼠标位置为锚点，避免遮挡光标

* 焦点恢复：

  * 显示前记录前台 hwnd
  * 关闭后尝试恢复前台（允许失败，但必须“尽力而为”）

### 5.5.4 数据源策略（快开窗 + 可搜索）

* 打开瞬间：使用 `HistoryStore` 的“最近 N 条”立即渲染（避免阻塞）
* 后台刷新：调用 `cb_list_history` 拉取补齐（可分页）
* 搜索：

  * 输入变化后 debounce（例如 200ms）
  * 调用 `cb_list_history(query_json)` 进行过滤
  * 搜索结果与 Store 结果合并规则必须明确（建议：搜索态只显示查询结果）

### 5.5.5 粘贴链路（EnsureCached → 写剪贴板）

QuickPaste 的执行链路不得单独实现，和历史页面一样（见 5.4）。
行为约束：

* NeedsDownload：显示下载进度与可取消（若核心支持 cancel）
* 成功写入剪贴板后：按设置决定是否自动关闭小窗；关闭后尽力恢复前台焦点
* 写入失败（剪贴板占用）：按统一命令的重试与提示规则执行，不得额外弹窗阻塞用户输入


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

### 5.5.6 UI 结构占位（留出设计空间）

QuickPasteWindow 建议拆为三块区域：

* 顶部：SearchBox + 小型状态指示（在线设备数/暂停监听）
* 中部：历史列表（每项：类型图标、预览、来源设备、时间、状态徽标）
* 底部：提示行（快捷键帮助、错误提示、下载进度条）

> 视觉与交互细节可迭代，但窗口行为/状态机/快捷键必须先固定。

### 5.5.7 验收标准与测试

* 热键稳定呼出/隐藏；失焦自动隐藏
* 搜索可用；列表滚动不卡顿（至少 500 条）
* Lazy Fetch 可显示进度并可取消
* 写回剪贴板不产生重复 ingest（回环防护验证）
* 多显示器定位正常（至少主副屏）

---

## 5.6 统一日志系统（Core 权威库 + Shell 统一入口）

### 5.6.1 目标

* Core 作为日志权威库：持久化、查询、tail、清理、统计
* Shell 统一使用 `ILogger` 写日志，最终落入 Core 日志库
* Logs 页面提供“排障级”管理能力：时间段检索、关键字过滤、导出、保留期清理

### 5.6.2 统一日志事件模型（Schema）

最少字段：

* `id`（单调递增）
* `ts_utc`
* `level`（Trace/Debug/Info/Warn/Error/Critical）
* `component`（Core/Shell）
* `category`（稳定前缀：Shell.Tray、Shell.Overlay、Core.Net…）
* `message`
* `exception`（可空）
* `props_json`（可空：transfer_id、item_id、peer_id、窗口状态等）

### 5.6.3 Shell 写入路径（ILogger → Core）

* 提供 `CoreLoggerProvider`（或 Serilog Sink）作为唯一入口
* Provider 内部必须：

  * 物化消息与异常（避免对象生命周期问题）
  * 入队到 `CoreLogDispatcher`（有界队列，丢弃策略仅允许丢 Trace/Debug）
  * 后台批量调用 `cb_logs_write`
* Core 不可用时的策略（v1 必须定死其一）：

  * 方案 A：丢弃 Shell 日志但记录计数并提示（简单）
  * 方案 B（推荐）：写一个 Shell fallback rolling file，仅用于 Core 未加载阶段排障

### 5.6.4 Logs 页面能力（管理功能）

* 时间范围查询：start/end + 分页
* Tail：after_id 增量追加
* 过滤：

  * 最小级别 level_min
  * 关键字 like（message/category/exception 统一语义）
* 管理：

  * 删除 N 天前日志（调用 `delete_before`）
  * 导出 CSV（对当前查询结果）
  * Stats：总条数、磁盘占用（如核心提供）

### 5.6.5 保留期策略（Retention）

* 设置项：`LogRetentionDays`
* 执行策略：

  * 应用启动后执行一次清理
  * 之后每日执行一次（或每次启动足够，v1 可选）
* 约束：清理必须只作用于 core_log_dir，不影响其它数据

### 5.6.6 验收

* Shell 与 Core 日志同屏可检索
* Tail 不卡 UI，持续 10 分钟无内存膨胀（设定最大缓存条数）
* 清理操作可重复执行且 stats 正确

-----

## 5.7 权限、自动启动与后台留存（Policy + UX）

### 5.7.1 目标

在不引入“不可控后台行为”的前提下，实现可诊断、可配置、可关闭的系统级运行能力：

* 后台留存（Close-to-tray）
* 全局热键（可配置、可冲突处理）
* 自动启动（尊重用户与系统禁用）
* 通知与提示（可降级）

### 5.7.2 权限与系统能力边界（v1 固定约束）

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

### 5.7.3 后台留存（Close-to-tray）状态机

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

### 5.7.4 自动启动（Startup）策略

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

### 5.7.5 热键管理（Hotkey Management）

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

### 5.7.6 统一退出流程（必须唯一）

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

### 5.7.7 诊断与可见性（用户能理解）

* Settings 页面应显示：

  * CoreState（Ready/Degraded）
  * StartupState（Enabled/Disabled/DisabledByUser…）
  * HotkeyState（Registered/Conflict/Disabled）
  * 当前 data_dir/log_dir（只读）
* 提供“复制诊断信息”按钮：

  * 包含版本、路径、状态、最近错误摘要

### 5.7.8 验收与测试

* Close-to-tray：点 X 后仍可通过托盘恢复窗口，功能持续工作
* Exit：托盘退出后进程完全结束、资源释放
* 自启：开启后重启系统能自启动；用户禁用后应用不能强行恢复
* 热键：冲突必提示；改键后可恢复注册
* 通知禁用：关键提示仍可通过 UI 看见

---

## 5.8 UI 规划与页面规格

### 5.8.1 目标

将 v1 的 UI 以“可实现、可测试、可迭代设计”的方式规格化，统一信息架构与交互规则，并与 Stores 数据投影对齐。

### 5.8.2 信息架构（IA）

主窗口以左侧导航栏（NavigationView）组织：

* Home（主页）
* History（历史）
* Devices（设备）
* Logs（日志）
* Settings（设置）

QuickPasteWindow 独立于主窗口导航（顶层小窗），但共享 Stores 与命令。

### 5.8.3 主页页面规格（Home / Dashboard）

#### 5.8.3.1 目标

主页作为“剪贴板共享控制台（Dashboard）”，提供三类能力，并严格避免与 History/Devices/Settings 页面功能重复：

1. **核心交互优先**：以“最近元数据横向卡片条”为第一视觉焦点，支持“选中即写剪贴板”、自动跟随新元数据、锁定/删除等高频操作。
2. **可操作的文字摘要**：以文字指标呈现共享策略、缓存/传输/开关状态，并提供一键动作（暂停采集/暂停分享/清空缓存等）。
3. **图表化态势感知**：展示核心提供的缓存与网络占用随时间变化（轻量图表，便于判断系统是否正常工作）。

#### 5.8.3.2 页面布局（自上而下三段式）

主页采用固定三段式布局（自适应宽度，垂直滚动仅在小屏时启用）：

A) **顶部：Recent Items 横向卡片条（核心功能区）**
B) **中部：文字数据与操作区（状态与控制）**
C) **底部：图表区（Cache + Network）**

> 主页不展示日志摘要；日志相关信息仅在 Logs 页面用于调试与排障。

---

#### 5.8.3.3 顶部：Recent Items 横向卡片条（选中即写剪贴板）

##### 5.8.3.3.1 可见条目与“显示全部历史”

* 默认显示最近 **10** 个历史元数据条目（按时间倒序，最新在最前）。
* 横向列表尾部固定一个“查看更多历史…”卡片/按钮：

  * 点击后跳转到 `History` 页面，并定位到最新条目（顶端）。

##### 5.8.3.3.2 卡片信息结构（统一模板）

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

##### 5.8.3.3.3 卡片右上角操作按钮（Lock / Delete）

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

##### 5.8.3.3.4 选择模式（SelectionMode）与自动跟随规则

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

##### 5.8.3.3.5 “选中即写剪贴板”的提交（Commit）规则（防抖与可控性）

为避免用户滑动过程中造成剪贴板高频抖动，规定“选中提交（commit）”时机：

* **触控/拖拽横向滑动**：在 `PointerUp`（松手）后，以“视口居中卡片”为最终选中，触发写剪贴板。
* **鼠标滚轮/触控板滚动**：滚动停止后 debounce 150–250ms，以“最接近居中的卡片”为选中，触发写剪贴板。
* **键盘左右键**：每次按键立即提交选中并写剪贴板。

提交选中的副作用：

* 更新选中视觉效果（Selected 状态）
* 触发“写剪贴板链路”（详见 5.7.3.3.6）

##### 5.8.3.3.6 写剪贴板链路（复用 QuickPaste：EnsureCached → ClipboardWriter）

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

##### 5.8.3.3.7 新元数据到达时的自动写剪贴板规则

当 Core 产生新元数据（新条目进入 HistoryStore）时：

* 若 `SelectionMode=FollowNewest`：自动选中新条目并触发 `ApplySelectedToClipboard(new_item_id)`
* 若 `SelectionMode=Locked`：只更新列表，不触发写剪贴板

---

#### 5.8.3.4 中部：文字数据与操作区（状态与控制）

##### 5.8.3.4.1 必要状态字段（必须可见）

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

##### 5.8.3.4.2 必要操作按钮（v1 必做）

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

##### 5.8.3.4.3 Share Targets 管理（v1：设备表 In/Out allowlist）

主页 Share Targets 区域仅展示摘要与列表，不提供额外按钮（符合你的要求），管理通过右键菜单进行：

对每个 peer 的右键菜单（v1）：

* Toggle：**Share to this device（Outbound allow）**
* Toggle：**Accept from this device（Inbound allow）**
* Copy device info（peer_id / name / last_seen）

文档伏笔（vNext，不实现）：

* Sharing Circles：Devices 页面提供图形化圈子编辑；v1 allowlist 等价于隐式圈子 `Default` 的成员集合。

---

#### 5.8.3.5 底部：图表区

##### 5.8.3.5.1 图表清单（v1 必做）

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
##### 5.8.3.5.2 数据来源（必须由 Core 提供）

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

#### 5.8.3.6 依赖与实现约束（工程落地要求）

* 主页所有业务动作不得直接调用 P/Invoke；必须通过 Service/Command（CoreHost/Stores/Policy）层完成。
* 主页写剪贴板链路必须复用 QuickPaste 同一套命令与 awaiter（EnsureCached/TransferStore/CONTENT_CACHED）。
* 主页卡片条在高频更新（新元数据持续进入）时不允许卡顿：

  * 列表更新必须增量（最多替换前 10 条可见窗口）
  * UI 线程仅做最小 diff 更新；重计算/统计放后台任务

---

#### 5.8.3.7 验收标准（v1）

1. Recent Items 显示最近 10 条；末尾“查看更多历史”跳转正常。
2. 在 `FollowNewest` 下，新元数据到达后自动选中并写入系统剪贴板，选中效果正确。
3. 锁定某条后进入 `Locked`，新元数据到达不改变剪贴板内容；解除锁定后恢复跟随最新。
4. 选中 `NeedsDownload` 条目会触发 EnsureCached，显示下载进度，完成后写剪贴板；切换选中可取消或正确忽略旧进度。
5. 删除（Local）后条目从主页消失；若删除锁定项则自动解除锁定并选中新条目。
6. “停止/继续管理剪贴板”能暂停/恢复本机采集；“停止/继续分享剪贴板”能暂停/恢复接收与广播。
7. Cache 清理按钮可用且有二次确认；执行过程中 UI 有忙碌态反馈。
8. 底部两张图表可展示随时间变化数据，数据来自 Core 接口，刷新不阻塞 UI。

### 5.8.4 历史页面规格（History）

#### 5.8.4.1 目标

History 页面提供“全量历史浏览 + 精准定位 + 管理动作”，是 Home（最近 10 条）与 QuickPaste（临时呼出）之外的主入口。History 必须与 Home/QuickPaste 共享同一套条目状态机与操作命令，避免三套逻辑不一致。

#### 5.8.4.2 列表展示与虚拟化分页（UI Virtualization）

* **列表控件**：使用 WinUI 3 `ListView` 配合 `ItemsStackPanel`，必须开启 UI 虚拟化（默认行为），禁止将 ListView 放入 ScrollViewer 或 StackPanel 等无限高度容器中。
* **数据源实现**：Shell 侧集合必须实现 `ISupportIncrementalLoading` 接口，支持滚动到底部自动触发加载。
* **分页协议（核心约束）**：
* 采用 **游标分页（Cursor-based / Keyset Pagination）**，严禁使用 Offset 页码分页。
* 分页参数：`limit`（建议 50） + `cursor`（上一页最后一条的 `sort_ts_ms`）。
* Core 行为：执行 `WHERE sort_ts_ms < cursor` 查询。这确保了当新数据插入顶部时，底部的分页“锚点”不会偏移，杜绝数据重复或漏读。



#### 5.8.4.3 实时数据流策略（智能静默插入）

针对用户浏览过程中 `ITEM_META_ADDED` 新数据到达的场景，采用 **“智能静默插入（Smart Silent Insertion）”** 策略，以消除列表抖动（Scroll Jitter）：

* **交互逻辑**：
1. **浏览态（User is scrolling）**：当列表滚动条不在顶部（`VerticalOffset > 0`）时，新数据静默插入底层集合，但 **强制保持视口锚定（Scroll Anchoring）**。用户当前看到的条目位置保持像素级不变，不会被新数据“挤”下去。
2. **监控态（User is at top）**：当列表紧贴顶部（`VerticalOffset ≈ 0`）时，新数据插入后自然展示，列表内容下推，让用户实时感知最新动态。


* **技术实现约束**：
* 必须配置 `ItemsStackPanel.ItemsUpdatingScrollMode = KeepItemsInView`。
* 必须在 UI 线程（Dispatcher）将新 Item 插入到集合头部（Index 0），利用 WinUI 的锚定机制处理视觉位置。

#### 5.8.4.4 选中与写剪贴板行为（与主页一致）

History 的“选中写剪贴板”必须提供两种模式（由 Settings 控制）：

* `HistorySelectionWritesClipboard = false`（默认建议）

  * 单击只选中与显示详情，不写剪贴板
  * 通过显式动作写剪贴板（双击/按钮）
* `HistorySelectionWritesClipboard = true`（与你主页一致的体验）

  * 单击选中即触发 `ApplySelectedToClipboard(item_id)`（同主页链路）
  * 为避免误触，可增加 150ms debounce（仅对鼠标滚动/快速点击）

> 主页强制“选中即写”，History 建议默认不写，以避免用户浏览时频繁污染剪贴板；但文档必须允许用户统一成同一体验。

#### 5.8.4.5 条目操作（Lock / Delete / Copy）

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

#### 5.8.4.6 过滤与搜索（v1 最小集）

* 关键字搜索（匹配 preview/文件名摘要/来源设备名）
* 类型过滤（Text/Image/Files）
* 状态过滤（NeedsDownload/Downloading/Failed）
* 时间范围（可选）

#### 5.8.4.7 验收标准（v1）

1. 全量历史可分页加载，滚动不卡顿。
2. Copy/Lock/DeleteLocal 与主页语义一致，且跨页面联动正确（Home 顶部卡片条反映锁定/删除后状态）。
3. NeedsDownload 条目可触发 EnsureCached、展示进度、完成后可写剪贴板。

---

### 5.8.5 设备页面规格（Devices）与分享策略（v1 + vNext 伏笔）

#### 5.8.5.1 目标

Devices 页面负责“设备发现与共享策略管理”。v1 采用“每设备 Inbound/Outbound allowlist”落地；文档预留 vNext 的“分享圈（Sharing Circles）”升级路径。

#### 5.8.5.2 v1 页面结构（列表 + 右键管理）

* 展示所有检测到的设备（包含非本账号设备），并标注：

  * 设备名/别名、在线状态（Online/Offline/Backoff）、最后见到时间、peer_id（可折叠显示）
* 页面顶部显示共享摘要：

  * Outbound allowed count（允许向其分享的设备数量）
  * Inbound allowed count（允许接收其分享的设备数量）

#### 5.8.5.3 分享策略（v1：allowlist；vNext：Sharing Circles 预留）

v1 采用每设备 allowlist 的最小落地模型，对每个 peer 维护两个布尔策略：

* `ShareToPeer`（Outbound allow）：为 false 时，本机不向该 peer 广播元数据/内容
* `AcceptFromPeer`（Inbound allow）：为 false 时，本机不接收/落库/展示来自该 peer 的元数据

管理方式（v1）：

* Devices 页面对每个 peer 提供右键菜单切换 Outbound/Inbound 开关（主页仅展示摘要，不提供按钮）。

vNext 预留：Sharing Circles（分享圈）

* 未来允许用户创建多个“圈子（Circle）”，把设备拖入圈子实现“圈内共享、圈间隔离”；设备可属于多个圈子以桥接共享。
* v1 allowlist 等价于隐式圈子 `Default` 的成员集合与策略子集，保证升级时不推翻现有配置。


#### 5.8.5.4 设备右键菜单（v1 必做）

对每个设备提供右键菜单：

* Toggle：Share to this device（Outbound）
* Toggle：Accept from this device（Inbound）
* Copy device info（peer_id / name / last_seen）
* Set local alias（可选，仅本机显示名）

> Home 的 Share Targets 区域仅展示摘要与列表，不提供按钮；实际管理入口在 Devices（右键/详情面板）。

#### 5.8.5.5 vNext：分享圈（Sharing Circles）伏笔（不实现，仅定义）

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

#### 5.8.5.6 验收标准（v1）

1. 设备发现与状态变化可见，右键可切换 In/Out。
2. 切换策略后立即生效：Inbound 关闭后不再接收该 peer 元数据；Outbound 关闭后不再向其发送。
3. Home Share Targets 展示与 Devices 策略一致。

---

### 5.8.6 设置页面规格（Settings）

#### 5.8.6.1 目标

Settings 页面集中管理所有“策略性开关与阈值”，并明确区分：

* 采集（Capture）：是否从系统剪贴板读取并生成元数据
* 分享（Sharing）：是否接收/广播元数据（网络层）
* limits：超限提示与默认动作
* 快捷键/后台留存/自启：系统集成行为

#### 5.8.6.2 必要分组（v1）

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

### 5.8.7 日志页面规格（Logs）

> 本节保留为调试用途，不出现在主页摘要中，但必须具备工程排障能力。

#### 5.8.7.1 目标

提供 Core 权威日志库的查询、tail、过滤、导出与保留期清理能力；Shell 通过 ILogger 写入同一库（见 5.6 与日志系统章节）。

#### 5.8.7.2 核心能力（v1）

* 时间范围查询（start/end）
* level_min 过滤
* 关键字 like
* after_id tail（增量追加）
* 删除 N 天前（retention）
* 导出 CSV（当前查询结果）

---

### 5.8.8 统一命令与语义（跨 Home / History / QuickPaste 一致性）

> 这一节非常关键，用于防止三处 UI 行为不一致。建议写在 UI 章节末尾并作为实现约束。

#### 5.8.8.1 统一状态机（ItemState）

所有页面对条目状态的显示与行为必须一致：

* `Ready`：可直接写剪贴板
* `NeedsDownload`：需 EnsureCached
* `Downloading(progress)`：显示进度，可取消（若核心支持）
* `Failed(error)`：可重试（再次选中/Copy）

#### 5.8.8.2 统一写剪贴板命令

命令名（建议）：

* `ApplySelectedToClipboard(item_id)`

语义（必须一致）：

1. Ready → ClipboardWriter.Write
2. NeedsDownload → Core.EnsureCached → 等 CONTENT_CACHED → ClipboardWriter.Write
3. 写入失败（剪贴板占用）→ 短退避重试（<=2 秒）→ 非阻塞提示

#### 5.8.8.3 统一锁定语义

* `Lock(item_id)`：进入 `SelectionMode=Locked(item_id)`
* Locked 期间：

  * 新元数据到达不抢占剪贴板
  * 删除 Locked 项会自动解除锁定并回到 FollowNewest

#### 5.8.8.4 统一删除语义（v1）

* `DeleteLocal(item_id)`：本机隐藏，不广播、不影响其它设备
* `DeleteEverywhere(item_id)`：vNext（tombstone + GC），v1 仅占位

---


### 5.8.5 Devices 页面规格

* 列表字段：

  * 设备名、状态（Online/Offline/Backoff）、最后见到
* 详情（可选）：

  * device_id、版本、tag
  * 信任状态与操作（若核心支持）
* 操作：

  * 刷新（若核心提供）
  * 复制设备信息（用于支持）

### 5.8.6 Settings 页面规格（策略集中地）

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

### 5.8.7 Logs 页面规格（统一日志中心）

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

### 5.8.8 统一语义与命令（Home / History / QuickPaste 必须一致）

#### 5.8.8.1 统一条目状态机（ItemState）

所有 UI（主页卡片条、History 列表、QuickPaste 列表）对条目状态显示与行为必须一致，状态集合固定为：

* `Ready`：内容已在本机可用，可直接写入系统剪贴板
* `NeedsDownload`：内容不在本机，需要触发 `EnsureCached`
* `Downloading(progress)`：下载中，显示进度；允许取消（若核心支持）
* `Failed(error)`：失败态，显示短错误摘要；允许重试（再次选中/再次执行 Copy）

约束：

* UI 不得各自引入额外状态名（避免漂移）
* 同一 `item_id` 的状态由 Stores 投影为唯一真相源

#### 5.8.8.2 统一写剪贴板命令（选中即写 / Copy 按钮共用）

统一命令（建议命名）：

* `ApplySelectedToClipboard(item_id)`

语义固定为：

1. 若 `Ready`：调用 `ClipboardWriter.Write(item)` 写入系统剪贴板
2. 若 `NeedsDownload`：调用 Core `cb_ensure_content_cached(item_id)` 得到 `transfer_id`，进入 `Downloading` 并订阅 `TransferStore`；等待 `CONTENT_CACHED(transfer_id)` 后再写入剪贴板
3. 写入失败（剪贴板被占用）：短退避重试（总时长不超过 2 秒），仍失败则给出**非阻塞提示**并允许用户再次触发

约束：

* Home 的“选中即写”、History 的“Copy”、QuickPaste 的“Enter”必须调用同一命令，不得三套实现

#### 5.8.8.3 统一锁定语义（Lock）

* `Lock(item_id)`：进入 `SelectionMode=Locked(item_id)`
* Locked 期间：新元数据到达不抢占剪贴板内容
* 解锁：回到 `SelectionMode=FollowNewest`

约束：

* 删除 Locked 项时必须自动解除锁定并回到 FollowNewest（避免“锁定指向不存在条目”）

#### 5.8.8.4 统一删除语义（v1：本机隐藏；vNext：全局 Tombstone）

* `DeleteLocal(item_id)`：仅本机隐藏，不广播、不影响其它设备
* `DeleteEverywhere(item_id)`：vNext 占位（tombstone + GC），v1 不实现但可在 UI 预留入口（右键/二级确认）


### 5.8.9 QuickPasteWindow UI 规格（与 5.4 对齐）

* 顶部搜索框（可选）
* 列表：最近 N 条 + 状态徽标 + 进度条
* 底部提示：快捷键帮助、错误提示、取消按钮（Downloading 时）

### 5.8.10 验收与测试

* UI 与 Stores 解耦：页面不直接调用 FFI（只能调用 Service/VM 命令）
* 快速打开：History 页面首屏 < 200ms（使用 Store 现有数据）
* Lazy Fetch：进度与取消可用，错误可解释
* 全局状态条信息准确并可联动跳转

---

## 5.9 打包发布、版本管理与回归测试

### 5.9.1 目标

形成可安装、可升级、可回归验证的发布链路，确保系统集成功能（托盘/热键/自启/剪贴板）在 Release 环境稳定。

### 5.9.2 打包形态

* v1 默认：MSIX（获得 AppIdentity，便于通知、自启、升级）
* 调试形态：

  * Debug（不强依赖自启）
  * Release（完整功能验证）

### 5.9.3 版本号与兼容策略

* Shell 版本号与 Core 版本号均应展示在 About/Diagnostics
* 兼容性策略：

  * Shell 检测 Core ABI 版本（或导出符号版本）不匹配时进入 Degraded 并提示升级

### 5.9.4 发布工件

* MSIX 安装包
* 变更日志（Changelog）
* 诊断导出（日志导出 + 配置导出）用于用户反馈

### 5.9.5 回归测试清单（最低集）

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

### 5.9.6 验收标准

* Release 包在干净系统安装后可完成全套回归测试
* 卸载不残留后台进程；数据目录按策略处理（保留/清理规则可后续定义）
* 用户可通过“复制诊断信息 + 导出日志”提交有效问题报告

------

# 6) 开发和工程规范

本章节定义 ClipBridge 在**多组件、多语言、多平台**背景下的工程治理规则，包括：

* 版本管理原则
* CI / CD 自动化机制
* 代码格式与质量检查
* 日常开发与发布的使用方式

这些规则的目标是：

> **让组件可以独立演进、独立发布，同时保持仓库整体一致性与可维护性。**

---

## 6.1 版本管理（Versioning）

### 6.1.1 分组件版本（Component-level Versioning）

ClipBridge 采用 **分组件版本管理**，而不是全仓库单一版本号。

当前组件包括：

| 组件                | 路径                                      | 版本载体                    |
| ----------------- | --------------------------------------- | ----------------------- |
| Core（Rust）        | `cb_core/`                              | `Cargo.toml`            |
| Windows FFI（Rust） | `platforms/windows/core-ffi/`           | `Cargo.toml`            |
| Windows Shell（C#） | `platforms/windows/ClipBridgeShell_CS/` | `Directory.Build.props` |

**原则：**

* 每个组件都有自己独立的语义化版本号（SemVer）
* 组件版本号只存在于其**构建系统权威文件**中
* 不维护额外的 `VERSION` 文件，避免多源不一致

---

### 6.1.2 变更驱动版本（Conventional Commits）

所有提交遵循 **Conventional Commits** 规范，例如：

* `feat(core): add lazy fetch timeout`
* `fix(win-shell): tray icon crash`
* `refactor(win-ffi): simplify export boundary`

**作用：**

* 提交信息本身即是“变更语义”
* 自动决定版本 bump（major / minor / patch）
* 自动生成变更日志（changelog）

---

### 6.1.3 自动发版（release-please）

ClipBridge 使用 **release-please（manifest mode）** 进行自动版本管理：

* 监听 `main` 分支的提交历史
* 按组件路径与 commit scope 生成 **Release PR**
* 在 Release PR 中：

  * 自动更新组件版本号
  * 自动生成 changelog
* 合并 Release PR 后：

  * 自动创建 **组件级 tag**
  * 触发对应组件的发布流程

**示例 tag：**

* `cb_core-0.2.0`
* `platforms/windows/core-ffi-0.1.3`
* `platforms/windows/ClipBridgeShell_CS-0.4.0`

---

## 6.2 CI / CD（持续集成与发布）

### 6.2.1 CI 的职责（Pull Request 阶段）

CI 的目标不是“构建一切”，而是：

> **在合并前，验证改动不会破坏受影响的组件。**

#### 路径感知（Path Filter）

CI 根据 PR 中改动的路径决定执行哪些检查：

* 修改 `cb_core/**` 或 `core-ffi/**` → 执行 Rust CI
* 修改 `ClipBridgeShell_CS/**` → 执行 C# CI
* 仅修改文档 → 不执行重型构建

---

### 6.2.2 Rust CI（Core / FFI）

Rust 相关组件在 CI 中执行以下检查：

* `cargo fmt --check`（格式）
* `cargo clippy -D warnings`（静态分析）
* `cargo test`（单元 / 集成测试）
* `cargo deny`（依赖安全与许可证审计）

---

### 6.2.3 C# CI（Windows Shell）

Windows Shell 在 CI 中执行：

* `dotnet format --verify-no-changes`
* `dotnet build -c Release`

确保：

* 代码格式与仓库规范一致
* Release 配置下可正常构建

---

### 6.2.4 CD（基于 Tag 的发布）

发布不依赖分支，而**只由 tag 触发**：

* release-please 创建 tag
* CI 根据 tag 前缀判断发布哪个组件
* 只构建并上传对应组件的产物

**当前发布策略：**

| 组件            | 发布产物                |
| ------------- | ------------------- |
| Core          | 源码 + 可选构建产物         |
| Windows FFI   | `.dll` / `.pdb`     |
| Windows Shell | `.zip`（后续可升级为 MSIX） |

---

## 6.3 代码格式与质量检查

### 6.3.1 全局格式规范（`.editorconfig`）

* 全仓库统一使用 `.editorconfig`
* **缩进使用 Tab（4）**
* Rust / C# 共享基础规则
* YAML / JSON / Markdown 使用空格，避免生态冲突

---

### 6.3.2 Rust 格式化（`rustfmt.toml`）

* `hard_tabs = true`：缩进使用 Tab
* 对齐允许使用空格（rustfmt 官方行为）
* 与 `cargo fmt --check` 保持一致

---

### 6.3.3 C# 格式化（dotnet format）

* 不使用 clang 系列工具
* 由 `.editorconfig + dotnet format` 统一控制
* Visual Studio / Rider / CI 行为一致

---

### 6.3.4 依赖安全（`deny.toml`）

ClipBridge 使用 **cargo-deny** 进行供应链检查：

* 已知安全漏洞：**CI 失败**
* 许可证问题 / 多版本依赖：**CI 警告**
* 未知 registry：**禁止**

该策略在保证安全底线的同时，避免早期开发被过度阻断。

---

### 6.3.5 已弃用工具说明

以下工具为历史 C++ 方案遗留，**已不再使用**：

* `.clang-format`
* `.clang-tidy`

文档中不再作为现行规范。

---

## 6.4 日常开发与发布流程（How to use）

### 6.4.1 日常开发

1. 在功能分支开发
2. 按 Conventional Commits 提交
3. 提交 PR → CI 自动检查
4. CI 通过后合并到 `main`

---
### 6.4.2 Conventional Commits 怎么用（写提交信息的规则）

你每次 commit message 写成这样就行：

* 改 Core：

  * `feat(core): add lazy fetch timeout`
  * `fix(core): handle offline device`
* 改 win-ffi：

  * `fix(win-ffi): export cb_free`
* 改 win-shell：

  * `feat(win-shell): add tray menu`
* 改 CI：

  * `ci: speed up cargo cache`
* 改文档：

  * `docs: update architecture notes`

**重点：scope 要稳定**（`core / win-ffi / win-shell`），这样 release-please 的 changelog 才清晰。


### 6.4.3 发版流程

1. 合并功能 PR 到 `main`
2. release-please 自动创建 Release PR
3. 审核并合并 Release PR
4. 自动打 tag
5. 自动构建并发布对应组件产物

**开发者不需要手动修改版本号或打 tag。**

---



