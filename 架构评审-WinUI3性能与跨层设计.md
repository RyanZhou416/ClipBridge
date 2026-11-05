# ClipBridge 架构评审

**评审方式**：通读 `cb_core`(17 模块) / 三个 `core-ffi` / WinUI3 外壳(145 文件) / Android / macOS / CI，未做运行时采样（但每条结论都能被一次 trace 验证，见 §7）。
**所有关键结论我都回到代码里核实过**，带 `文件:行`；标注"待实测"的是建议先测量再动手的部分。

---

## 0. 结论摘要

### 回答你的问题：WinUI3 慢吗？

**WinUI3 有固定开销，但你现在的卡顿有确定的、可消除的代码级原因，而且主战场不在渲染面，在数据面。**

四条最致命的（按对"手感"的贡献排序）：

| # | 问题 | 证据 |
|---|---|---|
| **1** | **遗留的 agent 调试代码：每张卡片往硬编码路径追加 JSON** | `MainViewModel.cs:37-55`，经 `GetItemState`(`:185/199/215/225/234/244`) 被 `OnStateBadgeLoaded`(`MainPage.xaml.cs:965`) **逐卡片**调用 → `File.AppendAllTextAsync("C:\Project\ClipBridge\.cursor\debug.log")` |
| **2** | **窗口在核心初始化完成之后才创建** | `ActivationService.cs:27-35`：先 `await InitializeAsync()`（内含 `await _coreHostService.InitializeAsync()`，`:79`），**然后**才 `new MainWindow()` |
| **3** | **订阅从不解除 → 越用越慢** | `LogsViewModel.cs:527-530` 订阅单例事件，`Dispose()`(`:1325`) **全仓库无人调用**；`MainPage.xaml.cs:131/148/158/196/200` 五处订阅，`OnPageUnloaded`(`:208-224`) 只停 timer；`DevicesViewModel.cs:48` 同样 |
| **4** | **一次复制 = 全量重查 + 全量重建 + 逐卡遍历可视树** | `MainViewModel.cs:412-419` → `:495-532` → `MainPage.xaml.cs:148-155` → `:850-864` |

还有一条**很扎心**的发现：首页那三张图表的渲染是全应用最贵的路径（§2.2 B1：每 100 ms 清空可视树重建、每次重绘 `new BitmapCache()`），但它们读的 `stats_store` **从来没有被写入过**（全仓库无 `stats_store.write` 调用，`api.rs:135` 只 open 不 write）→ `cb_query_cache_stats` 的 `series` **恒为空**（`core-ffi/lib.rs:1217-1228`）。**最贵的渲染在画空气。**

### 但真正该先修的是安全（见 §1）

这一块比性能严重得多，而且都是**已经写进代码的**、不是理论风险：

- TLS 客户端是 `BlindVerifier`（接受任何服务器证书），服务端 `with_no_client_auth()` 不请求客户端证书 → **服务端永远拿不到对端证书，指纹恒为 `"unknown"` 并被 TOFU 钉死**。
- OPAQUE 算出的 `session_key` 被 `let _session_key` **直接丢弃** → PAKE 退化成"证明自己知道密码"，没有绑定信道。
- 私钥"加密"用的密钥由 `device_id + account_uid` 经**固定 salt** 派生，而这两个值由 mDNS TXT **明文广播**。
- 对端可控的 `sha256` / `rel_name` 直接拼进文件路径 → **远程可达的目录穿越/任意文件写**。

### 明确排除的假设（省得你在错的方向花时间）

全仓库无 `.Result`/`.Wait()`/`GetAwaiter().GetResult()`/`Thread.Sleep`（仅 `CoreLogDispatcher.cs:209` 退出时 `Wait(5s)`）；无 WebView；无 `CompositionEffectBrush`；窗口是 `Hide()`/`Show()` 而非销毁重建（这块没问题）；`QuickPasteWindow` **在本仓库完全不存在**（文档 5.5 要求，未实现）。

---

## 1. 安全与信任边界（P0，建议优先于性能）

### 1.1 TLS 盲信 + 服务端不请求客户端证书 → TOFU 钉死 `"unknown"` ★★★★★

```rust
// transport.rs:23-43
/// 自定义 TLS 验证器：接受任何服务器证书 (Blind Trust)
impl ServerCertVerifier for BlindVerifier {
    fn verify_server_cert(...) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())      // ← 任何证书都通过
    }
```
```rust
// transport.rs:109-113（服务端）
TlsServerConfig::builder_with_provider(crypto)
    .with_protocol_versions(&[&TLS13])?
    .with_no_client_auth()                        // ← 不请求客户端证书
    .with_single_cert(cert_chain, priv_key_server)?
```
```rust
// session/actor.rs:168-179（握手后取对端指纹）
let fingerprint = match conn.peer_identity() {
    Some(id) => { ... }
    None => "unknown".to_string(),               // ← 服务端永远走这里
};
```

**影响**：服务端角色（被连方）**永远**把 `"unknown"` 写进 `trusted_peers`（`actor.rs:1513-1534` 的 TOFU 检查），之后再也无法失配 → 服务端侧没有任何证书固定能力。客户端侧虽有 TOFU，但首连窗口完全无认证，且 TLS 只到"任意证书"级别。**局域网内主动 MITM 在首连即可中转全部剪贴板内容。**

**修复**：服务端改 `with_client_auth_cert` / `with_client_cert_verifier`（或改 raw public key + 自签 pin）；`peer_identity` 为 `None` 应视为握手失败，而不是 `"unknown"`；指纹进 `Online` 前强制非空。

### 1.2 OPAQUE 会话密钥被丢弃，PAKE 没绑定信道 ★★★★★
```rust
// session/actor.rs:1506-1508
let _session_key = server_state
    .finish(client_message, ServerLoginParameters::default())?;   // ← 算出来扔掉
```
客户端 `finish_result.message` 同理（`:1460`）。OPAQUE 本应同时提供"认证 + 密钥交换"，这里只当了密码校验；业务帧（`ItemMeta`/`ContentBegin`）**没有任何 nonce/序号/重放窗口**，`proto.rs:35` 的 `client_nonce` 是死字段（`:526` 生成、`:561-566` 接收时 `..` 丢弃）。
另外 `crypto.rs:37-45`：`ServerSetup` 由 `Sha512(password)` + 固定 `ChaCha20Rng::from_seed` 每次重算，标识符硬编码 `b"clipbridge-user"` → OPAQUE 的服务器长期秘密被降级成"大家都知道"。

**修复**：保留并派生 `session_key`（或 `export_key`），用它给所有控制/数据帧做 AEAD/HMAC；`ServerSetup` 首次注册时生成并持久化；nonce 进握手 transcript 并做挑战应答；`msg_id` 加单调序号 + 重放窗口。

### 1.3 私钥"加密"的密钥来自公开信息 ★★★★
```rust
// transport/cert.rs:21,30
let salt = b"ClipBridge:cert_key:v1";                 // 固定 salt
let password = format!("{device_id}{account_uid}");    // 两个值都是公开的
```
```rust
// discovery.rs:113-114（mDNS TXT 明文广播）
("acct", cfg.account_uid.as_str()),
("did",  cfg.device_id.as_str()),
```

`key.encrypted` 的机密性完全依赖"攻击者拿不到文件"；一旦拿到（同步盘、备份、同机其他用户），密钥材料全部可推导。Argon2id 64MiB 只是拖慢而非阻止。

**修复**：改用 OS keystore（Windows DPAPI/CNG、macOS Keychain、Android Keystore）持 KEK；退一步至少用 CSPRNG 生成随机 salt + 随机 KEK 存 `tls/kek`（0600），不要从公开值派生；`clear_local_cert` 一并清理。

### 1.4 对端可控的路径片段直接拼接 → 目录穿越 ★★★★★（远程可达）
```rust
// cas.rs:27-29
pub fn blob_path(&self, sha256_hex: &str) -> PathBuf {
    let prefix = &sha256_hex[0..2];                       // 未校验；非 ASCII/短串会 panic
    self.blobs_dir.join(prefix).join(sha256_hex)
}
// cas.rs:198-201
let download_dir = self.cache_dir.join("downloads").join(transfer_id);
let target_path = download_dir.join(filename);            // filename = 对端的 rel_name
```
`sha256_hex` 来自 `ContentBegin.sha256`、`filename` 来自对端 `CtrlMsg::ItemMeta` → `FileMeta.rel_name`（`actor.rs:930-943`）。`rel_name = "../../../.."` 即可在任意可写路径落文件（内容由攻击者控制）；`sha256 = "../x"` 让 `rename` 逃出 `blobs/`。

**修复**：`cas.rs` 入口统一 `validate_hash()`（必须 64 位小写 hex）；拼完路径后 `canonicalize` 并校验 `starts_with(cache_dir)`；`rel_name` 用 `Path::file_name()` 白名单化，禁 `..`/分隔符/绝对路径。

### 1.5 账号密码出现在 `Debug` 结构体里
`api.rs:20-29`：`CoreConfig` 带 `account_password: String` 且 `#[derive(Debug)]`；外壳把明文密码放进 `cfg_json` 交给 core（`CoreHostService.cs:121-141`）。任何一次 `{:?}` 打日志就泄露。

---

## 2. WinUI3 性能：分层归因

### 2.1 A 类 — 数据流放大（主因）

#### A1 遗留调试代码写盘 ★★★★★
见 §0-1。`LogAsync`（`MainViewModel.cs:36-56`）用 `JsonSerializer.Serialize` + `File.AppendAllTextAsync` 写 `C:\Project\ClipBridge\.cursor\debug.log`，6 个调用点全在 `GetItemState` 内，而它是**逐卡片**调用的。
后果：滚一下卡片条 = 几十次磁盘 I/O + 几十个 fire-and-forget Task；同文件并发 `AppendAllTextAsync` 互相抛 `IOException`（被 `catch{}` 吞掉）。用户机器上该目录不存在 = 每次都是异常路径（异常构造比成功路径更贵）。
**这是唯一"删掉就立刻变快"的一条。** 顺带 `:190-207` 还有 2 次 `Transfers.FirstOrDefault`，每卡 O(transfers)。

#### A2 历史三条真源 + 每次新增全量重查 ★★★★★
`MainViewModel.cs:412-419`：`HistoryStore.Items.CollectionChanged` → `LoadRecentItemsFromCoreAsync()` → FFI 全量查询 + `_recentItems.Clear()` + 逐项 `Add`（`:495-532`）。
历史数据同时存在于三处：`HistoryStore.Items`（事件镜像）、`HistoryIncrementalCollection`（FFI 游标分页）、`MainViewModel._recentItems`（自己再查）。并发场景下旧结果还会覆盖新结果（竞态）。
`HistoryViewModel.cs:140-170` 是同一个病的另一面：每个新条目 → `ApplyFilters()` → `Source.Refresh()` → `Clear()` + 重置游标 → 重新拉第一页（`HistoryIncrementalCollection.cs:86-104`）。**剪贴板连拍 = 列表被反复推平重建**，用户看到跳顶/闪烁/滚动丢失。

#### A3 日志管道风暴 ★★★★★
链路：`CoreLogDispatcher.cs:22`(`BatchSize=5`) → `:193-202`（**C# 侧伪造 `LOG_WRITTEN` 塞进 core 事件流**）→ `EventPumpService.cs:256-262` → `LogsViewModel.cs:838`(`Task.Run(TickOnce)`) → `:889`(FFI 查 500 行) → `:905`(`_allItems.Any` 逐行 O(n) = O(n²)) → `:249`(每批重建 `HashSet`) → `:266`(`FormattedTextContent = _formattedText.ToString()`) → `:940-949`(逐项 `Add`)。

三个补充事实：
- `FormattedTextContent`/`_formattedText` 在 `LogsPage.xaml`（全文 124 行）里**没有任何绑定** —— 纯死路径，但每批日志都要 `O(总字符数)` 重建一次。
- `OnLogWritten`(`:825-835`) 用 `_isTickRunning` 忙则**直接丢弃**，无补偿 → 日志会漏。
- `CoreInterop.cs:305-417` 五个查询包装都做**双重 JSON 解析**：`Deserialize<JsonElement>` → `data.GetRawText()`（整段复制成新字符串）→ 再反序列化。

#### A4 无界队列 / 重复集合 / 无上限增长 ★★★★
`EventPumpService.cs:32` `Channel.CreateUnbounded<string>()`；`:73` 在**事件泵线程**上同步广播原始 JSON，任一订阅者慢就堵死整条管道；`DevicesViewModel.cs:70-92` 对**每个**事件都 `JsonDocument.Parse` 只为读 `type`。
`LogsViewModel.cs:34`+`:213` 两份同内容集合；`HistoryStore.cs:23` `Insert(0)` 无上限；`TransferStore` 完成项永不清理。

#### A5 `{Binding}` 到无 INPC 的模型 → 用 10 秒轮询兜底 ★★★★
`PeerMetaPayload`/`ItemMetaPayload` 无 `INotifyPropertyChanged`，而 `DevicesPage.xaml:79-109` 用 `{Binding IsOnline}` / `{Binding ShareToPeer, Mode=TwoWay}` → **UI 永不更新**。于是 `DevicesViewModel.cs:51-62` 每 10 秒 `Devices.Clear()` + 逐项 `Add`（`:94-136`）。设备页开着时列表每 10 秒被推平重填。

#### A6 增量加载在后台线程改绑定的集合 ★★★★
`HistoryIncrementalCollection.cs:31-84`：`AsyncInfo.Run(async c => { ... var page = await ...; foreach (var item in page.Items) Add(item); })` —— `await` 之后续体在线程池线程，**从非 UI 线程 `ObservableCollection.Add`**。轻则列表不刷新，重则 `RPC_E_WRONG_THREAD`。这解释了为什么作者在别处补了 `DispatcherQueue.TryEnqueue`（`HistoryViewModel.cs:111`）却漏了这里。

### 2.2 B 类 — 渲染/布局（次要但真实）

| # | 问题 | 证据 | 要点 |
|---|---|---|---|
| B1 ★★★★★ | 图表 `Children.Clear()` + 100 ms 重绘 + `new BitmapCache()`，**而且画的是空数据** | `MainPage.xaml.cs:1188-1266`、`:119-129`、`:1266` | 每次重绘 = 清空子树 + 十几个 `UIElement` 分配 + 全量 measure/arrange + 销毁并重建位图缓存表面。数据源 `stats_store` 从不写入（§0）→ **三次全量重建画了个空图**。10 Hz 定时器不管脏不脏都唤醒。 |
| B2 ★★★★★ | `OpacityMaskView` 离屏遮罩 + 三层 `BitmapCache` | `OpacityMaskView.xaml.cs:56-92`、`MainPage.xaml:101-124` | 每实例建 2 个 `CompositionVisualSurface` + `MaskBrush` + redirect `SpriteVisual`，并把原视觉 `Opacity=0`。600px 全宽 Hero 每帧额外离屏合成 + 逐像素 mask；外层 Grid 与自身各叠一层 `BitmapCache`。要"顶部渐隐"用 `Image.OpacityMask` + 渐变就够。 |
| B3 ★★★★ | 用 Win2D `CanvasControl` 画一行标题 | `MainPage.xaml:134-147`、`MainPage.xaml.cs:465-636` | 为了"颜色不受主题影响"，藏一个 `Opacity=0` 的 `TextBlock` 测量、`CreateResources` 重试、`DispatcherQueuePriority.High` 强刷。`:289-372` 还为了判断标题黑白把整张背景图 `BitmapDecoder` 解码取 200×100 采样，且**每次启动触发两次**。 |
| B4 ★★★★ | 9 处元素级亚克力 + 根 Mica + 2 处 `ThemeShadow` | `MainWindow.xaml:13-15`、`MainPage.xaml:290/304/324/342/351/373/382/391/400`、`:453`、`HistoryPage.xaml:151` | `AcrylicBackgroundFillColorDefaultBrush` 是 **HostBackdrop**：每个元素每次合成都要对桌面背后做一次模糊采样。9~10 个 = 每帧 9~10 次。而根 Mica 又被不透明背景（`MainPage.xaml:112`）完全盖住 → 纯成本无收益。 |
| B5 ★★★ | 整页 `ScrollViewer > StackPanel` + 视差每帧改动大容器 | `MainPage.xaml:96-97`、`:169-172`、`MainPage.xaml.cs:667-680` | `StackPanel` 让内部 `ListView` 拿到无限高 → 虚拟化失效（`DevicesPage.xaml:62-66`、`MainPage.xaml:403` 同样）。视差在 `ViewChanged` 里改包含全部卡片的容器的 `TranslateTransform.Y` → 每帧重合成整页。 |
| B6 ★★★ | 日志行模板逐项成本 | `LogsPage.xaml:19-46`、`LogLevelToColorConverter.cs:29`、`LogModels.cs:145-148` | 每行 9 个 `Run` + `IsTextSelectionEnabled="True"`（每行独立选区基础设施）+ 2 次转换器（**每次 `new SolidColorBrush`**）+ `Formatted*` getter 每次 `Char` 循环 + `new string(' ', n)`。滚动时行被回收重实现，全跑一遍。 |
| B7 ★★★ | 卡片状态靠命令式遍历可视树 | `MainPage.xaml.cs:850-864/866-877/879-938/957-978`、`CardSelectionBorderBrushConverter.cs:33` | 每次变更 `for i in RecentItems → TryGetElement(i) → FindVisualChildByName` 递归；`OnCardLoaded` 每卡 3~4 次递归 + 1 次 `App.GetService<MainViewModel>()`（转换器里做 DI）+ `PeerStore` 线性查找 + 5 条 `Debug.WriteLine` 插值。 |
| B8 ★★ | `CacheMode="BitmapCache"` 用错地方 | `MainPage.xaml:101/105/122/376-394` | 对会随滚动移动的大容器和 `Canvas` 加位图缓存 = 大块显存 + 失效即重栅格化，文字还会发虚。 |

### 2.3 C 类 — UI 线程上的同步 P/Invoke（"假 async"）

没有 sync-over-async，但有一堆**看起来异步、其实同步**的方法：

- `LogsViewModel.cs:732-739`：`Stats = CoreInterop.LogsStats(handle); await Task.CompletedTask;`
- `LogsViewModel.cs:1071` `LogsQueryRange`（500 行 JSON 反序列化，按钮命令在 UI 线程触发）、`:559`、`:1179`、`:1210`、`:1238`
- `DevicesViewModel.cs:144-151` `SetPeerPolicy`：由 `ToggleSwitch.Toggled` 触发，同步 FFI + JSON 往返
- `CoreHostService.cs:213-219` `LogsWrite` 在 `InitializeAsync` 的 UI 线程续体上
- `StashLogManager.cs:39-59`：**每条日志**一次 `File.AppendAllText`（全局 `lock` 内做 I/O）；`:64-110` 读回时逐行反序列化 + **O(n²) 去重**
- `CoreLoggerProvider.cs:161-213`：每条日志 `JsonSerializer.Serialize(state)` + `lock` 自增 ID

修复：所有 `CoreInterop.*` 收敛到 `CoreHostService` 里的 `Task.Run` 模式（或一个串行执行器保证同一 handle 不并发）；删掉所有 `await Task.CompletedTask`；`StashLogManager` 改单写者 `Channel` + 定期 flush；`Id` 用 `Interlocked.Increment`。

### 2.4 D 类 — 框架/工程固有成本

1. **非打包 + `WindowsAppSDKSelfContained` + `PublishSingleFile` + `EnableCompressionInSingleFile`**（`csproj:17`、`SelfContained-Exe-*.pubxml`）：WinAppSDK 自举 + 单文件解压/映射，冷启动常数项叠了三层。分发优先 MSIX；若必须非打包，去掉 SingleFile 压缩、保留 ReadyToRun。
2. **没有 `PublishTrimmed`/源生成 JSON**：`obj/.../XamlTypeInfo.g.cs` 有 4743 行，热路径 JIT 成本不低。
3. **`TargetPlatformMinVersion=10.0.17763.0` 搭配 `TFM=net10.0-windows10.0.26100.0`**（`csproj:4,6`）：WinUI3 本身不支持 17763，这个组合让版本适配分支不可预测。建议提到 19041 或 22621。
4. **workspace 根 `Cargo.toml` 没有任何 `[profile.release]`**（无 `lto`/`codegen-units=1`/`strip`/`panic=abort`）—— 这是要随包发布的 cdylib。

---

## 3. 跨层架构问题

### 3.1 FFI 三份复制，Windows 与 macOS 字节相同
```
3C671D3E…  platforms/windows/core-ffi/src/lib.rs   ~1401 行
3C671D3E…  platforms/macos/core-ffi/src/lib.rs     ~1401 行   ← 完全相同
48A6FC3E…  windows/core-ffi/src/error.rs = macos/core-ffi/src/error.rs
           windows|macos/include/clipbridge_core.h  同为 4450 字节、内容一致
           tests/ffi.rs、tests/mod.rs 也相同；bridge.rs 仅差 2 行注释
```
**而 Android 已经分叉**：12 个导出 vs 33 个（缺 `cb_set_peer_policy`/`cb_clear_peer_fingerprint`/`cb_clear_local_cert`/`cb_delete_item_*`/全部 `cb_logs_*`/`cb_clear_*`/`cb_query_*`）；Android **有** `catch_unwind`（`android/.../lib.rs:15-40`），Windows/macOS **没有**；Android handle 走字符串、Windows 走数字；错误码前者只有 `"FFI_ERR"`/`"PANIC"`，后者有 `INGEST_FAILED` 等。头文件也缺 `cb_get_ffi_version`。
重复量 ≈ 4100 行。
修复：抽 `cb_core_ffi`（唯一 C ABI + `catch_unwind` + envelope + DTO 解析），平台 crate 各剩 ~30 行，头文件由 `cbindgen` 生成并纳入 CI diff 校验。

### 3.2 事件契约是字符串魔法，且已三套写法并存
- `api.rs:399-407`：`{"type":"ITEM_META_ADDED","meta":...,"policy":...}`（无 `ts_ms`，meta 在顶层）
- `actor.rs:714-718`：`{"type":"ITEM_META_ADDED","ts_ms":...,"payload":{"meta":...}}`（有 `ts_ms`，在 payload 里）
- 大小写混用：`ITEM_META_ADDED`/`PEER_ONLINE`（大写）vs `peer_changed`/`local_cert_cleared`（小写）→ 外壳只能 `type.ToUpperInvariant()` 兜底（`EventPumpService.cs:81`）
- **`LOG_WRITTEN` 在 Rust 里根本不存在**，是 C# 侧 `CoreLogDispatcher.cs:193-202` 伪造后塞进 core 事件流的（事件泵因此无法区分来源）
- `TRANSFER_PROGRESS`/`TRANSFER_CANCELLED` core 会发，外壳**没有对应 case**，`TransferStore.Upsert` **全仓库无人调用** → 首页"活动传输数"恒为 0，`MainViewModel.cs:190-207` 是死代码

**后果已经体现在测试里**：`EventPumpTests.cs` 三个测试全过不了（第三个断言 `TRANSFER_PROGRESS` 无 case；第一个用 `"id": 1001` 而模型要 `item_id` 且 `TryPickMeta` 要求 `item_id`+`kind`）。而 CI **从不跑 `dotnet test`**（§5），所以坏了很久没人知道。

修复：`model.rs` 定义 `#[serde(tag="type")] enum CoreEvent` + `EventEnvelope { ts_ms, seq, payload }`，core 内只允许 `emit(CoreEvent)`；`cbindgen` 生成 C# 枚举，`switch` 由编译器强制穷尽；envelope 带协议版本。

### 3.3 分层在文档里很干净，在代码里被绕过
文档 5.0.3 定了三层（Integration / Core Bridge / Projection+UX，"Stores 是 UI 唯一数据源"）：

| 应当 | 实际 |
|---|---|
| VM 只依赖 `ICoreHostService` | `LogsViewModel` 直接调 **10 处** `CoreInterop.*`（`:559,737,778,889,992,1071,1179,1210,1238`） |
| 视图不含 FFI | `SettingsPage.xaml.cs:273,347,421` 直接 `ClearCoreDb/ClearCache/ClearStatsDb` |
| 统计走 Store | `MainViewModel.cs:595,610,621` 直接 `Query*Stats` |
| Store 唯一真源 | 历史有**三个**来源（§A2） |

更糟的是 `api.rs:836-845` 的 `Inner` 字段全是 `pub`，FFI 直接 `handle.core.inner.store.lock()`（`core-ffi/lib.rs:694,1111,1149,1217,1231`）—— 桥接层绕过 `Core` 门面直接拿数据库连接，任何 `Inner` 改名都是跨 crate 破坏性变更。

### 3.4 `_coreHandle` 裸奔：use-after-free 竞态 ★★★★★
`CoreHostService` 把 Rust `Box<cb_handle>` 的指针存成裸 `IntPtr`，`GetHandle()`(`:376-379`) 无保护地交出去，调用方**判空后直接调 FFI**：
```csharp
var handle = _coreHost.GetHandle();
if (handle == IntPtr.Zero) return;
var batch = CoreInterop.LogsQueryAfterId(handle, ...);   // ← 这期间 core 可能已被 shutdown
```
`ShutdownAsync` 在另一线程 `Box::from_raw` 释放（`core-ffi/lib.rs:222`）再置零，**判空和调用之间没有同步** → 账号切换/退出/清库时可能踩已释放内存（访问冲突，不是异常，直接崩）。而 Rust 侧每个入口 `unsafe { &mut *h }`，C# 侧又确实多线程并发调（`CoreHostService.cs:506/529/630/847` 全是 `Task.Run`）→ **同时存在两个 `&mut cb_handle`，别名 UB**。
修复：句柄改 `struct Handle { core: Mutex<Option<Core>>, alive: AtomicBool }`，入口先查 `alive` + `try_lock`，`shutdown` 置 `None` 而非 `Box::from_raw`；C# 侧用 `SafeHandle` 或读写锁包住"取句柄+调用"整段，并把 `GetHandle()` 从公共接口删掉。

### 3.5 剪贴板两条并行 ingest 路径 ★★★★★
```
Clipboard.ContentChanged
 ├─ ClipboardService.OnClipboardContentChanged  → IngestLocalCopyAsync(snapshot)   [ClipboardService.cs:43-83]
 │                                                share_mode 硬编码 "local_only"，不看采集开关
 └─ ContentChanged 事件 → ClipboardWatcher(200ms 防抖) → IngestPolicy.Decide
                                                   → IngestLocalCopy(json, ToJson()) [ClipboardWatcher.cs:87-147]
```
**同一次复制入库两次**，payload 形状和 `share_mode` 都不同。core 的 `build_item_meta` 每次 `Uuid::new_v4()` 生成新 `item_id`（`clipboard.rs:61`），CAS 只按 sha 去重 blob → **两条历史记录指向同一份内容**。
副作用：**"关闭剪贴板采集"不生效**（`ClipboardWatcher.Stop()` 只退订事件，`ClipboardService` 那条路径照旧入库）；外壳 `IngestPolicy`(`:32-57`) 是 `cb_core::policy::decide` 的第二份且不完整实现（`:54` 明写限额检查是 TODO）。
修复：只保留一条 ingest 路径，采集开关在它上面判断；去重/限额回收进 core。

### 3.6 没有单实例保护 ★★★★
全仓库 grep `Mutex|SingleInstance|NamedPipe` = 0。两次启动 = 两个 `cb_init`，同一 `data_dir` 上两个 SQLite 连接、两个 CAS 目录、两套 TLS 证书、两个 mDNS 广播。而 `store.rs:36-45` **没有 `busy_timeout`**（rusqlite 默认无 busy handler → 立即 `SQLITE_BUSY`）。数据损坏级风险。

### 3.7 文档承诺的组件不存在
`QuickPasteWindow` / `QuickPasteService`（热键 toggle、定位、焦点恢复）/ `HotKeyService` / `ClipboardWriter` / `LogStore` / `StatusStore` —— 六个都不存在。
**这意味着 v1 的核心体验（Win+V 式呼出）还没开始做**，而现在所有性能投入都在"仪表盘"上，仪表盘恰恰不是高频界面。建议先明确：高频路径是 QuickPaste，它的预算（热键唤醒 < 100 ms、常驻轻量）不该复用 MainPage 这套视觉结构。

---

## 4. Rust core 工程问题

| 严重度 | 问题 | 证据 |
|---|---|---|
| ★★★★★ | **FFI 无 `catch_unwind`**（Windows/macOS），而 `Core::init` 有 3 处 `panic!` + 1 处 `.expect()`；`ret()` 有 `CString::new(s).unwrap()`。panic 穿过 `extern "C"` = abort/UB。Android 反而有保护 | `api.rs:97,130,153,176`；`core-ffi/lib.rs:62` |
| ★★★★★ | 每入口 `unsafe { &mut *h }` + 壳侧多线程并发 = 别名 UB；`Box::from_raw` 后无失效标记，二次 `cb_shutdown` = double-free | `core-ffi/lib.rs:222,247…` |
| ★★★★ | **事件回调在持锁 + 持 runtime 线程时同步进入 shell**：`api.rs:613-635` 的 `store` guard 存活期间调 `emit`；`actor.rs:1540-1551` 同。回调内若再进任何需要同一把锁的 FFI = 同线程死锁；若在 tokio 线程上触发 `blocking_send` = panic（而 Windows 无 unwind 保护 → abort） | `api.rs:613-635`、`actor.rs:1540-1551` |
| ★★★★ | **没有单一写者**：`Arc<Mutex<Store>>` 被 FFI 线程、网络线程、各会话共享；握手时还额外 `Store::open` 两次新连接（绕过共享连接并发写 `trusted_peers`） | `api.rs:840`、`net.rs:249-258`、`actor.rs:1521-1533`、`store.rs:36-45` |
| ★★★★ | **网络全跑在一个 `new_current_thread` runtime 上**：任一会话在 `reply_rx.await`（`actor.rs:1072`，注释自认会阻塞）或写盘背压里停住，其余会话的 Ping/Pong 全排队 → 触发 6s 心跳超时 → 误判断线 → 退避重连风暴 | `net.rs:82-91`、`actor.rs:1005,1072`、`:34` |
| ★★★★ | **日志表无界增长**：每 64KB 分块、每次心跳（2s/会话）都同步 `INSERT` 且永不清理；`run_gc` 只处理 history 和 CAS。1GB 传输 ≈ 万级 INSERT，每次持全局 `Mutex<LogStore>` | `logs.rs:151-159`、`actor.rs:466-472,1011-1023,1583-1589` |
| ★★★★ | **`stats` 是完整死子系统**：模块 173 行 + `stats.db` + 3 个 FFI + 3 个 C# wrapper + 3 张图表 UI，但**全仓无一次 `stats_store.write`** → 三个端点恒返回空 `series` | `api.rs:135`、`core-ffi/lib.rs:1217-1228` |
| ★★★★ | `receivers: HashMap` **无数量上限、无超时、无回收**；每个 `ContentBegin` 都 spawn 一个 writer task。对端不停发 Begin 不发 End 即可耗尽内存/句柄 | `actor.rs:146,861,989` |
| ★★★★ | **CAS GC 账本错**：只统计 `blobs/`，`materialize_*` 硬链接出去的 `files/`、`downloads/` 永不回收 → 报"已回收"但磁盘不降；且每次 ingest 都全目录 `stat` 扫描 | `cas.rs:79-95,160-215`、`api.rs:426,452-475` |
| ★★★ | `runtime.rs` **是空文件**却 `pub mod`；文档 4.4 整章描述的"线程与 runtime 所有权"没有对应实现 | `cb_core/src/runtime.rs` |
| ★★★ | 重连双拨产生重复会话（`net.rs:398-423` 缺发现路径里的收敛规则）；`Shutdown` 用 `try_send` 满则丢弃 → 退出后线程/endpoint 残留；`CandidateLost` 分支根本没实现 | `net.rs:431-433,398-423`、`api.rs:865`、`net.rs:426-457` |
| ★★★ | 依赖卫生：`tempfile`(只在 test 用)、`format`/`env`(零引用) 写在 `[dependencies]`；`rand 0.8` 与 `0.9`、`rand_chacha 0.3`/`0.9` 并存；`tokio features=["full"]`；无 MSRV；`deny.toml` 的 `bans` 是 `allow/warn` 空转 | `cb_core/Cargo.toml:13-49`、`deny.toml:36-44` |
| ★★★ | `api.rs`(871 行) god-facade + `session/actor.rs`(~1600 行) god-actor（握手/OPAQUE/TOFU/收发文件/心跳/日志全在一个 `match`） | — |
| ★★ | `PeerStatus` 返回假数据：`device_name: None, // TODO`、`last_seen_ts_ms: now, // TODO` → 壳侧永远显示 "Unknown"/"刚刚在线" | `net.rs:293,331` |
| ★★ | 断点续传半实现且校验对象错位：只对本次追加片段算 `Sha256` 却与整文件哈希比；拉取侧永远 `offset: Some(0)` | `actor.rs:859-875,909-913,1370` |
| ★★★ | `soft_delete_history_keep_latest` 用 `changes()` 前后相减得删除行数，但 `before` 取的是**上一条语句**的影响行数 → u64 下溢，debug 构建直接 panic（release 静默回绕）。在 ingest 路径上 | `store.rs:562-578` |
| ★★ | 入站图片每次 ingest 走 base64 解码 + **两次全量 `clone`**（`snapshot_content_bytes` 被 `build_item_meta` 和 `make_ingest_plan` 各调一次） | `bridge.rs:186-188`、`clipboard.rs:49-58,65,170` |
| ★★ | `history_count_for_account` 每次 ingest 全量 `COUNT(*)`；`get_file_meta` 每次解析整段 `files_json` | `store.rs` |

**测试缺口**（这解释了为什么上面这些都还藏着）：`cas.rs`/`proto.rs`/`store.rs`/`logs.rs`/`stats.rs`/`transport/cert.rs` 零覆盖；`discovery.rs:184-190` 的测试**没有一行 assert**；`session/tests.rs:167` 给 client actor 传的是 **server 的 store**，因此服务端 TOFU 的 `"unknown"` bug 从未被发现；`m1_net.rs` 依赖真实 mDNS（CI 容器里多播不可用）；FFI 负路径（NULL 句柄、非法 JSON、重复 shutdown、并发调用）全部未测；Android 无 tests。

---

## 5. 工程可验证性：CI 现在基本不起作用

| 问题 | 证据 | 后果 |
|---|---|---|
| **C# CI 用 .NET 9，项目 TFM 是 `net10.0`** | `ci.yml:83` vs `csproj:4` | 这条 gate 必然失败/长期被跳过（本机也只有 8.0/10.0 SDK，无 9.0） |
| **从不跑 MSTest** | ci.yml 全文无 `dotnet test` | §3.2 那三个坏测试一直没被发现 |
| **`cargo fmt --check` 实测就不通过**（14 处 diff / 7 文件） | `rustfmt.toml:14-15` 用了 stable 上不生效的 nightly-only 项 | 第一个门禁就不绿，说明 workflow 长期没被执行 |
| Rust CI 跑在 `ubuntu-latest` | `ci.yml:41` | 只验证 Linux 目标；Windows/macOS/Android 的 FFI crate 实际未被构建验证 |
| `paths-filter` 漏 android/macos core-ffi | `ci.yml:23-29` | 改这两个平台的 FFI 不触发任何 Rust 检查 |
| 无 Android CI | — | Gradle/JNI 完全无 gate |
| **TFM 漂移**：csproj `net10.0`，所有 pubxml 写 `net9.0` | `SelfContained-Exe-*.pubxml`、`Installer-x64.pubxml`、`FolderProfile.pubxml` | 用这些 profile 发布 = 输出目录错位或构建失败 |
| 构建依赖本机绝对路径 | `RustTargetDir=C:\Project\ClipBridge\target\...`(`csproj:28`)、`AppInstallerUri=C:\Users\12693\Downloads`(`:26`)、`PrintCoreDllInfo` 硬编码(`App.xaml.cs:534`) | 别人/CI 无法复现；`PrintCoreDllInfo` 每次 Debug 启动还弹通知 |
| `FindCoreDll` 从 BaseDirectory **向上找 5 层**，两份重复实现 | `CoreHostService.cs:481-494`、`App.xaml.cs:621-651` | 可能加载 `target/` 里的旧 DLL 或别人的 DLL；且 DLL 缺失时 `Condition="Exists"` **静默跳过** → 构建成功、运行时 `DllNotFoundException` |
| `AppxAutoIncrementPackageRevision=True`(`csproj:22`) | — | 打包不可复现 |

---

## 6. 建议路线

### P0-a 安全（1–2 天，先做）
1. 服务端请求并校验客户端证书；`peer_identity` 为 `None` 判握手失败，不允许 `"unknown"` 进 `trusted_peers`。
2. `cas.rs` 全部路径入口加 `validate_hash` + `canonicalize`+`starts_with` 校验；`rel_name` 用 `file_name()` 白名单化。
3. 私钥加密改 OS keystore（或随机 salt + 随机 KEK）。
4. `CoreConfig` 去 `Debug`；密码不进 `cfg_json` 明文（走独立参数 + 不出现在任何日志）。

### P0-b 体感（3–5 天）
5. **删掉 `MainViewModel.LogAsync` 与 6 个调用点**（§A1）。
6. **修生命周期**（§0-3）：`LogsPage.OnNavigatedFrom` 调 `VM.Dispose()`；`MainPage.OnPageUnloaded` 成对退订 5 处（并合并两处重的 `PropertyChanged`）；`DevicesViewModel` 退订 `EventReceived`。或把 `LogsViewModel`/`MainPage` 改单例 + `NavigationCacheMode`。
7. **日志页重写**（§A3）：删 `_formattedText`；`Dictionary` 去重；`BatchSize` 提升 + 250ms 节流；`ListView` 替换 `ItemsRepeater`；关掉逐项 `IsTextSelectionEnabled`；配色改静态画刷。
8. **图表改 Win2D**（或先确认它该不该存在——现在画的是空数据）；删 100ms 定时器；统计轮询加可见性守卫、间隔 15–30s。
9. **摘掉 `OpacityMaskView` + 双层 `BitmapCache`**；背景改 `Image` + 渐变覆盖层；去掉视差或改 `ExpressionAnimation`。
10. **标题去掉 Win2D**；背景取色移后台线程、只做一次。
11. **搜索框加 250ms 防抖**；**默认关闭卡片亚克力**；卡片状态改绑定。

### P1 止住持续腐化（1–2 周）
12. **单实例保护**（§3.6）。
13. **句柄生命周期**（§3.4）+ 把 14 处直接 `CoreInterop.*` 收回桥接层（§3.3）+ `Inner` 字段降 `pub(crate)`。
14. **剪贴板单入口**（§3.5）+ 采集开关生效 + 去重回收进 core。
15. **事件契约收敛**（§3.2）+ 修好或删除三个坏 MSTest。
16. **启动顺序**（§0-2）：先出窗口再初始化核心；resw 拷贝加版本戳。
17. **CI 修好**：.NET 版本、`dotnet test`、`-p:Platform=x64`、paths-filter、去硬编码路径、先跑一遍 `cargo fmt`。

### P2 结构性（1–2 月，可与功能并行）
18. **FFI 三合一** + `catch_unwind` + `cbindgen` + ABI 版本校验（§3.1）。
19. **Rust 硬伤**：事件回调改"出队"语义并 `drop(guard)`；`StoreActor` 单写者 + `busy_timeout`；网络 runtime 改 multi-thread；日志环形缓冲 + 保留策略；`receivers` 加超时/上限；CAS GC 改按 `content_cache` 记账。
20. **拆 god-facade / god-actor**（`runtime.rs` 要么实现要么删）。
21. **补 QuickPaste**（§3.7）—— 高频路径单独设计。
22. **依赖清理 + `[profile.release]`**。

---

## 7. 先测量，再优化

P0 我有把握，但**每项占多少毫秒要实测**：

1. **首帧/切页**：`dotnet-trace collect --profile dotnet-sampled-thread-time` 看主线程栈；配合 ETW `Microsoft-Windows-XAML` + `DXGI`。
2. **UI 线程停顿**：WPR 勾 `CPU usage`+`XAML`+`GPU`，WPA 看 `UI Delays`（预判前三名：`GetItemState`→`LogAsync` 的 I/O、`LogsViewModel.ApplyFilters`、`DrawCacheChart`）。
3. **输入延迟**：搜索框连打字同时录 WPR，看 `Input` provider。
4. **掉帧**：WPA `Composition Frame` 通道 —— 亚克力与 `OpacityMaskView` 的离屏 RT 会在这里现形。
5. **确认没走软件渲染**：WinUI3 在部分虚拟机/远程桌面会退到 WARP —— `App.xaml.cs:393-416` 那两段"检查硬件加速"的 Debug 代码说明你已经怀疑过；如果用户里有 RDP 场景，这就是"只有某些机器卡"的解释。
6. **复现卡顿最快的开关**：日志页 `AddTestLogCmd`（`LogsViewModel.cs:613-627`）用 1 秒定时器持续造日志，能把 §A3+§2.3 整条管道串起来；`dotnet-counters monitor System.Runtime` 看 `gen-0 GC count` 与 `ThreadPool Queue Length`。

**成本最低的对照实验**：临时注释掉 `OpacityMaskView`、`CacheMode`、Win2D 标题、图表 Canvas，换纯 `TextBlock` + 静态图形，录一遍"冷启动 → 首页 → History → Logs → 连续滚动"。但我预期**"删调试代码 + 修生命周期"**这两步的收益，会比渲染层的全部改动加起来更明显。

---

## 8. 核实记录（逐条在代码/命令层面确认）

**基线**：`git rev-parse --abbrev-ref HEAD` = `main`；工作区干净（仅本报告未跟踪）。
构建基线：`dotnet build ...\ClipBridgeShell_CS.csproj -c Release -p:Platform=x64` = **0 错误**（4 个来自生成代码的预存在 CS0612 警告）；`cargo check --workspace --all-targets` = **通过**。

### 8.1 已核实 — 本轮修复

| # | 断言 | 核实证据 |
|---|---|---|
| 1 | 遗留调试代码每卡片写盘 | `MainViewModel.cs:52` 硬编码 `C:\Project\ClipBridge\.cursor\debug.log`；`GetItemState` 内 6 处 `_ = LogAsync(...)`（185/199/215/225/234/244）；调用点 `MainPage.xaml.cs:965`（`Loaded="OnStateBadgeLoaded"` 逐卡片） |
| 2 | 窗口在核心初始化后才创建 | `ActivationService.cs:30` `await InitializeAsync()` → `:79` `await _coreHostService.InitializeAsync()`；`:32-35` 才 `new MainWindow()` |
| 3 | `LogsViewModel` 每次进页面泄漏一个 tailer | 类声明 `:28` 未实现 `IDisposable`；`:529` 订阅 `LogWritten`、`:637` 订阅 `StateChanged`；`:1325` 有 `Dispose()` 但全仓库无调用；`LogsPage.xaml.cs` 无导航钩子 |
| 4 | `MainPage` 5 处订阅从不解除（且两处重复） | `:131/:148/:158/:196/:200` 五处 `+=`；`OnPageUnloaded`(`:208-224`) 只停 timer；`MainViewModel` Singleton vs `MainPage` Transient |
| 5 | `DevicesViewModel` 事件订阅泄漏 | `:48` `EventReceived +=`；`:64-68` `OnNavigatedFrom` 只停 timer |
| 6 | 一次复制触发全量历史重查 | `:412-419` CollectionChanged → `LoadRecentItemsFromCoreAsync()`；`:495-532` FFI 查询 + `Clear()` + 逐项 `Add` |
| 7 | 历史页每新项全量重建 | `HistoryViewModel.cs:140-170` → `:223-234` → `HistoryIncrementalCollection.cs:86-104` `Clear()` + 游标重置 |
| 8 | 跨线程改绑定的 `ObservableCollection` | `HistoryIncrementalCollection.cs:31-84`：`AsyncInfo.Run(async c => { ... foreach Add(item); })` |
| 9 | 图表 100ms 全量重建 + `BitmapCache` churn | `MainPage.xaml.cs:119-129`；`:1193-1196/1266` `CacheMode=null`→`new BitmapCache()`；每帧 `Children.Clear()` |
| 10 | `OpacityMaskView` 离屏遮罩 | `OpacityMaskView.xaml.cs:56-92` 两个 `CompositionVisualSurface`+`MaskBrush`+`visual.Opacity=0`；`MainPage.xaml:101-124` |
| 11 | Win2D 只为画标题 | `MainPage.xaml:134-147`；`MainPage.xaml.cs:465-636`；`:289-372` 整图解码取色 |
| 12 | 视差每帧改大容器 | `MainPage.xaml.cs:667-680`；`MainPage.xaml:169-172` |
| 13 | `ListView` 在 `StackPanel` 里虚拟化失效 | `DevicesPage.xaml:62-66`；`MainPage.xaml:403` |
| 14 | 转换器每次新建画刷 | `AcrylicBrushConverter.cs:63-70` |
| 15 | 剪贴板双入口 + 采集开关失效 | `ClipboardService.cs:59` ingest；`ClipboardWatcher.cs:110-147` 再 ingest；`SetCaptureState(false)` 只退订 |
| 16 | 传输事件无人处理 | `EventPumpService.cs:125-267` 无 `TRANSFER_PROGRESS`/`TRANSFER_CANCELLED`；`TransferStore.Upsert` 0 调用点 |
| 17 | 三个 MSTest 契约错误 + 测试跑不起来 | `EventPumpTests.cs` 用 `"id":1001`（`TryPickMeta:97-123` 直接 return）；无 `TRANSFER_PROGRESS` 分支；见 8.3 |
| 18 | CAS 路径穿越 | `cas.rs:27-29` 未校验 `sha256_hex`，且 `[0..2]` 对非 ASCII panic；`cas.rs:198-201` `join(filename)` 取自对端 `rel_name` |
| 19 | TLS 盲信 + 服务端不请求客户端证书 | `transport.rs:33-43`（注释自述 Blind Trust）；`:112` `.with_no_client_auth()` |
| 20 | 服务端 TOFU 钉死 `"unknown"` | `actor.rs:168-179`；`:1513-1534` |
| 21 | 私钥 KEK 由公开值派生 | `cert.rs:21,30`；`discovery.rs:113-114` mDNS TXT 广播 `acct`/`did` |
| 22 | 事件回调持锁时同步进外壳 | `api.rs:613` lock，`:635` `emit`，guard 到函数尾 |
| 23 | 密码在 `#[derive(Debug)]` 里 | `api.rs:20-29` |
| 24 | 无 `busy_timeout` | `store.rs:36-45` |
| 25 | `changes()` 相减可下溢 | `store.rs:554-578` |
| 26 | FFI 无 `catch_unwind` | win/mac `lib.rs` grep = 0；Android `lib.rs:15-40` 有；`api.rs:130/153/176` 有 `panic!` |
| 27 | 句柄 double-free / UAF | `core-ffi/lib.rs:222` `Box::from_raw`；40+ 处 `&mut *h`；C# `CoreHostService.cs:506/529/630/847` 并发调 |
| 28 | `ret()` 会 panic | `core-ffi/lib.rs:61-63` `CString::new(s).unwrap()` |
| 29 | `cb_shutdown` 返回值泄漏 | 头文件 `:22`；`CoreHostService.cs:356` 丢弃 |
| 30 | win/mac FFI 字节相同 | sha256 `3C671D3E…CED` 一致；`error.rs` 一致；头文件同为 4450 字节 |
| 31 | C# CI 用 .NET 9 / 项目 net10.0 | `ci.yml:83` vs `csproj:4`；本机仅 8.0.427 / 10.0.204 SDK |
| 32 | `dotnet build <目录>` 必失败 | 实测 `error : WiX Toolset 未安装。`；改用具体 csproj 即 0 错误 |
| 33 | CI 从不跑测试 | `ci.yml` 无 `dotnet test` |
| 34 | paths-filter 漏 android/macos | `ci.yml:23-29` |
| 35 | Rust CI 只验 Linux | `ci.yml:41` |
| 36 | 构建依赖本机绝对路径 | `csproj:28,26,22` |
| 37 | pubxml TFM 漂移 | 5 个 pubxml 全写 `net9.0-...` |
| 38 | 未用/错位依赖 | `cb_core/Cargo.toml:45`(`tempfile`)、`:23`(`format`)、`:24`(`env`) |
| 39 | 多余 cdylib + 无 release profile | `cb_core/Cargo.toml:11`；根 `Cargo.toml` 无 `[profile.release]` |
| 40 | 无单实例保护 | 全仓库 grep `Mutex\|SingleInstance\|NamedPipe` = 0 |
| 41 | 每次启动覆写 resw | `App.xaml.cs:471-490`，`:477-479` 跳过逻辑被注释 |

### 8.2 已核实 — 本轮**不**修（需协议/产品决策或更大重构）

| 断言 | 证据 | 原因 |
|---|---|---|
| OPAQUE `session_key` 被丢弃、PAKE 未绑定信道 | `actor.rs:1506` | 协议改造 + 双方兼容 |
| `client_nonce` 死字段、无重放保护 | `proto.rs:35`；`actor.rs:561-566` | 同上 |
| 单线程 runtime 承载全部会话 | `net.rs:82-91` | 影响全部网络行为 |
| 无单一写者（`Arc<Mutex<Store>>` 三方共享） | `api.rs:840`、`net.rs:249-258` | 需 `StoreActor` |
| CAS GC 只统计 `blobs/` | `cas.rs:79-95,160-215`、`api.rs:426,452-475` | 需重做账本 |
| `logs` 表无保留策略 | `logs.rs:151-159`、`actor.rs:466-472` | 需环形缓冲 |
| `receivers` 无上限/超时 | `actor.rs:146,861,989` | 需超时设计 |
| FFI 三平台合并（≈4100 行） | 见 #30 | 本轮仅让 win/mac 保持同步 |
| `stats_store` 从不写入 → 图表恒空 | `core-ffi/lib.rs:1217-1228` | 功能决策 |
| 全库 `cargo fmt` 不通过 | `rustfmt.toml:14-15` | 会覆盖并行 agent 改动 |
| 句柄 TOCTOU 的 14 处调用点 | `LogsViewModel`×10、`MainViewModel`×4、`SettingsPage.xaml.cs`×3 | 文件分属不同 agent |
| `&mut *h` 别名 UB 的完整改造 | 40+ 处 | 本轮消除触发条件，结构另开 |
| `QuickPaste` 六组件不存在 | 全仓库无匹配 | 功能开发 |
| 根 `MicaBackdrop` 被不透明内容遮盖 | `MainWindow.xaml:13-15` | 视觉决策 |

### 8.3 核实过程中新发现的问题

1. **测试套件根本跑不起来**：`dotnet test` 中止于 `testhost.runtimeconfig.json` 缺失 → 退回 self-contained 后找不到 `hostpolicy.dll`；`-p:SelfContained=false -p:RuntimeIdentifier=` → `NETSDK1094`；`-p:SelfContained=false -p:PublishReadyToRun=false` → 又是缺 runtimeconfig。三种组合全部失败。输出目录出现 `workloads.*.json` + 有 `ClipBridgeShell_CS.runtimeconfig.json` 却**没有**测试程序集自己的 runtimeconfig，说明 `UseWinUI=true` 让 WinAppSDK 的 app-layout targets 接管了测试项目。**这正是那三个坏测试从未被发现的原因。**
2. **`dotnet build <目录>` 不可用**：会连带构建 WiX installer 并因 `WiX Toolset 未安装` 失败 —— CI 与 `AGENTS.md` 里的验证命令本身是坏的。
3. **`MainPage` 挂了两个 `PropertyChanged` 处理器**（`:131` 与 `:158`），同一页面对同一事件重复处理。
4. **只有 `DevicesViewModel` 实现 `INavigationAware`**（`DevicesViewModel.cs:19`）—— `LogsViewModel`/`HistoryViewModel` 的 `OnNavigatedFrom` 永不被调用，disposal 必须在 Page 侧接。
