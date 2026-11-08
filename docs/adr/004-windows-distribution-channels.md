# ADR-004：Windows 分发渠道与安装形态（v2，评审后修订）

- 状态：**待决策**（已由三路独立评审：MSI/WiX/Burn、MSIX/Store、winget/CI/版本）
- 日期：2026-11-08（v1 同日；v2 为评审后修订）
- 研究：`docs/windows-distribution-practices.md`（真实项目案例与官方文档）
- 关联：`docs/distribution-strategy.md`、`docs/windows-distribution.md`、
  `platforms/windows/ClipBridgeShell_CS/`
- 范围：**仅 Windows 壳**。本文不含实现；实现需在决策确认后另开变更。

## 0. 修订摘要（v1 → v2）

三份评审共发现 **6 个 blocker、20+ 个 high**，其中以下结论直接改变设计：

| 原设计（v1） | 修订（v2） | 依据 |
|---|---|---|
| D2：MSIX 也写包外 `%LOCALAPPDATA%\ClipBridge` | **拆成 D2′/D2″**：MSIX 用 `LocalState` + 首启导入；包外方案需 Store 受限能力，不予采用 | packaged 应用 AppData 写入默认虚拟化：进私有位置、其他进程不可见、卸载即删 |
| D3：WiX 3 手工双用途（`SetDirectory` 条件切换、保留 `InstallScope`） | 正确配方：**删 `InstallScope`**、`ALLUSERS=2` + `MSIINSTALLPERUSER=1`（Secure）、`InstallPrivileges="limited"`、`InstallerVersion="500"`；per-user 下 `ProgramFiles64Folder` **自动重定向**，删 `SetDirectory`；注册表 `HKLM→HKMU` | `perMachine` 会写 `ALLUSERS=1`，重复主键编译失败；MS Installer 5.0 的 per-user 重定向是内建行为 |
| D10：一个 EXE 里选 per-user/per-machine | **EXE 固定 per-user**（per-machine 走 MSI/winget）；要"一个 EXE 切 scope"只能迁 WiX 5 | Burn 在构建期按 MSI 摘要 bit 8 固定 scope/提权；WixStdBA 无 scope UI；`MsiPackage` 无 `DetectCondition` |
| D6：winget 用 singleton 只含 MSI | **必须多文件三件套**（version/installer/locale）；singleton 仅限 verified developers；P1 先 machine-only，双条目作为可选 | winget-pkgs 贡献规则；`Scope` 是安装器筛选而非 msiexec 开关 |
| D8：`Directory.Build.props` 单一版本源 | 版本源上移到 `platforms/windows/ClipBridgeShell_CS/Version.props`（app/Core/Tests 继承，wixproj 显式 Import）；release-please 配置修复；`publish.yml` 重写/删除 | 现有 props 位于 app 项目内，Core/Tests/wixproj 读不到；release-please 从未成功运行 |
| 便携版"已验证" | 仓库**无任何**便携标记代码；数据根 8 处硬编码；需先做 `DataRootResolver` | 代码检索 |

## 1. 背景与目标

Windows 壳已有 WinUI 3 应用、WiX 3.14 MSI、MSTest；目前只有 per-machine MSI，安装器不进 CI。
目标：**一个构建源、四条渠道**（官网 MSI+EXE+便携、Store MSIX、winget 引用 MSI）。

**本轮不做**（D7）：自研更新器（清单/签名校验/差量/回滚）。
**签名方向**（D4，本轮不申请）：MSIX 走 Store 个人开发者（免费、Store 代签）；
MSI/EXE 走 SignPath Foundation（免费 OSS）或备选 Certum 个人版（约 ¥600/年）。

## 2. 决策记录（v2）

| 编号 | 决策 | 选择 |
|---|---|---|
| D1 | 产物关系 | 一次 `publish` 派生四种产物；但 MSIX 走 build 布局（见 §4），不是 publish 目录 |
| D2′ | MSI/便携数据根 | `%LOCALAPPDATA%\ClipBridge`（与 MSIX 解耦，可独立实施） |
| D2″ | MSIX 数据根 | `ApplicationData.Current.LocalFolder`（LocalState）+ 首启从真实 `%LOCALAPPDATA%` **只读导入** + 显式导出；**不做**包外共享 |
| D3 | MSI 范围 | 双用途：per-user 默认（免 UAC）/ per-machine 可选；按 §5.2 正确配方（**研究后建议改为两个安装器，见 §11 / D16**） |
| D4 | 签名 | 本轮不买证书；Store 代签 + SignPath（后续） |
| D5 | MSIX 定位 | 仅 Store 渠道；不做自签名侧载分发 |
| D6 | winget | **多文件三件套**；P1 先 machine-only（可选：双条目支持双用途） |
| D7 | 更新责任 | 本轮先不做自研更新器；Store/winget/手动 |
| D8 | 版本与发布 | `Version.props` 单一来源 + 修复 release-please + 重写 Release 流水线 |
| D9 | 共存 | 跨格式单实例互斥（命名互斥量 + 命名管道 + 冲突提示） |
| D10 | EXE | Burn，**固定 per-user**；per-machine 走 MSI/winget（**研究后建议：user EXE + machine MSI，见 §11 / D17**） |
| D11 | MSI 工具链 | **待确认**：WiX 3.14 手工双用途（推荐）vs 迁 WiX 5 |
| D12 | 设置存储 | **待确认**：统一到数据根 `settings.json`（含迁移） |
| D13 | 便携默认 | 数据随 exe（标记）；不可写回退 LocalAppData 并提示 |
| D14 | MSIX 包外数据 | 不采用（需受限能力）；若 V1 探针意外 PASS 再议 |
| D15 | EXE scope | 固定 per-user；如坚持一个 EXE 切 scope → 触发 D11=WiX 5 |

## 3. 渠道与产物矩阵（v2）

| 产物 | 生成方式 | 安装位置 | 数据 | 卸载 | 签名 | 更新 |
|---|---|---|---|---|---|---|
| MSI（双用途） | WiX 3.14 | per-user：`%LOCALAPPDATA%\Programs\ClipBridge`（系统自动重定向）；per-machine：`C:\Program Files\ClipBridge` | `%LOCALAPPDATA%\ClipBridge` | 保留数据（可选删除） | SignPath（后续） | 手动/winget |
| EXE 引导器 | WiX Burn，包 MSI | 同 per-user MSI | 同上 | 同 MSI | SignPath（后续） | 手动 |
| 便携 ZIP | 压缩 publish 目录 + 标记 + 顶层目录 | 用户选择 | exe 旁 `data\`（可回退） | 手动 | 无 | 手动 |
| MSIX | `dotnet build` 布局 + `StoreUpload` 出包 | Store 管理（`WindowsApps`） | `LocalState` + 首启导入 | 卸载包；包内数据随包删 | Store 代签 | Store 自动 |
| winget 清单 | 脚本生成三文件 | 由 MSI 决定 | 同 MSI | 同 MSI | 不需要（SHA256） | `winget upgrade` |

## 4. 构建与产物派生（修订）

- **canonical**：`dotnet publish`（Release、win-x64、self-contained、R2R、瘦身）。
- **便携 ZIP**：publish 目录 + `ClipBridge.portable` 标记，**包一层顶层目录**（`ClipBridge-<ver>-win-x64/`），
  过滤 `*.pdb`/`*.xml`。
- **MSI**：WiX `heat` harvest publish 目录（过滤 PDB），`candle/light`。
- **EXE**：Burn 包 MSI（固定 per-user），先签 MSI、再打 bundle、再签 EXE。
- **MSIX**：**不走 publish**——`dotnet build -p:GenerateAppxPackageOnBuild=true
  -p:UapAppxPackageBuildMode=StoreUpload -p:AppxPackageDir=<out>`，由 MSBuild 处理
  `$targetnametoken$`/`x-generate`/PRI；载荷裁剪必须在打包前对**构建布局**生效（不能复用
  publish 的裁剪钩子），裁剪后需重生成/校验 PRI。
- 瘦身规则：移除未使用的 WindowsAppSDK AI/ML；只保留 zh-CN/zh-TW/en-us。
  量级参考（v1 临时代码）：publish 287→241MB；MSI 87.9MB；MSIX 109.4→89.9MB；ZIP 91.7MB。

## 5. 详细设计

### 5.1 数据模型（D2′/D2″，评审后重写）

**统一解析器**：先引入 `DataRootResolver`（唯一判定点），把现有 8 处硬编码/分叉收敛：
`CoreConfigBuilder.cs:30-38`、`CoreHostService.cs:141-157,372-379`、`App.xaml.cs:705-716`、
`StashLogManager.cs:22-29`、`LocalSettingsService.cs:56-92`、`SettingsPage.xaml.cs:169-179,651-664`、
`StartupTrace.cs:36-40`、`FrameTrace.cs:2010-2015`。

- **MSI / 便携**：`%LOCALAPPDATA%\ClipBridge`；便携标记（exe 旁 `ClipBridge.portable`）→ `<exeDir>\data`；
  不可写回退并**明确提示**（避免半数据分裂）。
- **MSIX**：`LocalFolder`（LocalState）；`LocalCacheFolder` 作 cache；设置统一到 `<LocalState>\settings.json`（D12）。
  - **为什么不用包外**：packaged 应用的 AppData 写入默认虚拟化（新文件进私有位置、其他进程不可见、卸载即删）；
    要写真实 AppData 需 `rescap:unvirtualizedResources`（Store 审批，个人剪贴板工具概率低）。
  - **换渠道不丢数据**：首启检测 LocalState 无 `core.db`、而真实 `%LOCALAPPDATA%\ClipBridge\core.db` 存在 →
    复制 DB + `tls\` + 设置 + `device_id` 到 LocalState，打 sentinel，弹一次性提示；读真实 AppData 会回退（可读）。
  - **绝对禁止**两个渠道同时指向同一 DB：真实 `core.db` + 私有 `-wal/-shm` 会产生混合损坏态（V2 验证）。
- **设置/设备身份**：`device_id` 与 `tls\` 必须随数据根迁移（否则换渠道 = 新设备 + TOFU 重固定）。
  设置现状在 `%LOCALAPPDATA%\ClipBridgeShell_CS\ApplicationData`，MSIX 在 LocalSettings → D12 统一。
- **WAL 风险**：便携数据在 U 盘/网络盘时 SQLite WAL 可能损坏；需"可移动/网络卷禁用 WAL 或警告"策略。
- **卸载语义**：MSI 询问删数据（默认保留）；MSIX 卸载删 LocalState（数据随包）；便携手动。
  注意 MSI 的"删数据"会删 `%LOCALAPPDATA%\ClipBridge`，若 Store 版用 LocalState 则互不影响（D2″ 的好处）。
- **诊断/日志**：packaged 下 EventLog 与 `%LOCALAPPDATA%` 日志不可见 → 改写 LocalState + 设置页"导出诊断"；
  `Diagnostics` 显示的路径必须是**实际传入**的路径（现状错：取 BuildPaths 而非 LocalState）。

### 5.2 MSI 双用途（D3，正确配方）

**授权（已核实 WiX 3.14）**：`Package/@InstallScope` 只有 `perMachine|perUser`；`perMachine` 会写
`ALLUSERS=1`，因此**必须删掉 `InstallScope`**，手工授权：

```xml
<Package InstallerVersion="500" Compressed="yes" InstallPrivileges="limited" ... />  <!-- 不写 InstallScope -->
<Property Id="ALLUSERS" Value="2" Secure="yes" />
<Property Id="MSIINSTALLPERUSER" Value="1" Secure="yes" />
```

- per-machine：命令行显式 `MSIINSTALLPERUSER=""`（依赖空串覆盖 Property 表默认值，**需实机验证**；
  不行则改 `SetProperty Before="InstallValidate"`）。
- **目录**：删除 `Product.wxs:29` 的 `SetDirectory`；保留 `ProgramFiles64Folder → INSTALLFOLDER` 树。
  per-user 时 Windows Installer 5.0 自动把 `ProgramFiles64Folder` 重定向到 `%LOCALAPPDATA%\Programs`。
  **不要**用条件 `SetDirectory`（维护/卸载拿不到 scope 会错解析；且 WiX 3 的 `SetDirectory` 无 `Condition` 属性）。
- **注册表**：`HKLM` → `HKMU`（per-user 写 HKCU，per-machine 写 HKLM）；快捷方式 keypath 的 `HKCU` → `HKMU`。
  现有 `RegistryEntries` 组件（`Product.wxs:138-145`）仓库无读取方，可直接删除而非条件化。
- **RemoveFolderEx**：`Property="INSTALLFOLDER"` 不可靠（属性须在 CostInitialize 前解析，且不接受 Directory id）→
  用 Remember Property：安装时写 `HKMU\...\InstallPath`（已有），卸载时 `RegistrySearch` 读入属性再清理；或删除该组件。
- **跨 scope 阻断**：同一 UpgradeCode 下 per-user ↔ per-machine **不能升级**，必须"先卸载再装"。
  加双向 `RegistrySearch` + `LaunchCondition`，文案要求先卸载；数据在包外，卸载不丢数据。
- **UI**：`WixUI_InstallDir` 没有 scope 选择且目录页可指向 Program Files → 增加自定义 scope 对话框
  （RadioButton 写 `MSIINSTALLPERUSER=1` / `""`，必须在 CostFinalize 前），per-user 时禁用/校验目录页。
- **ICE**：双用途必须过 **ICE105**；现有 `SuppressIces ICE03;ICE30;ICE82;ICE90` 需复核，至少不在双用途期间抑制关键项。
- **自启**：现有只写 `HKCU\...\Run`（`StartupService.cs:128-166`）→ per-machine 安装只对安装者生效；
  产品决策：per-machine 写 HKLM Run（并清理）或明确文档限制。
- **卸载清理 CA**：`REMOVEUSERDATA`/`ALLUSERS`/`MSIINSTALLPERUSER` 全部 `Secure="yes"`；
  现有 `CleanupStartupEntryCA` 缺 `NOT UPGRADINGPRODUCTCODE`，**major upgrade 会误删自启**（现有真 bug，应修）。
- **测试矩阵**：per-user/per-machine × install / major-upgrade / repair / uninstall；跨 scope 阻断；ICE105。

### 5.3 EXE 引导器（D10 = 固定 per-user）

- WiX Burn bundle（`WixBalExtension`）包 MSI；`MsiPackage Visible="no"`（MSI ARP 隐藏，bundle 为唯一 ARP 入口）。
- **构建期固定 per-user**（Burn 按 MSI 摘要 bit 8 固定 scope/提权，WixStdBA 无 scope UI，
  `MsiPackage` 无 `DetectCondition`；另一 scope 的已装产品发现不了）。
- `/quiet`、`/passive` 由 Burn 处理；MSI 的 `MsiProperty` 可传 `MSIINSTALLPERUSER=1`。
- Bundle 自身：独立 `UpgradeCode`（永久不变）、`Version` 来自 Version.props、图标/许可/本地化；
  签名顺序：签 MSI → 打 bundle → 签 EXE。
- 若必须"一个 EXE 切 scope"：只能迁 **WiX 5**（D11），并重测全部矩阵。

### 5.4 MSIX 与 Store（D2″/D5）

- **身份**：仓库 manifest 保留开发身份；另存模板，构建期注入 Partner Center 的
  `Identity/@Name`、`@Publisher`（`CN=<PublisherGuid>`）、`@Version`、`Properties/@DisplayName`、
  `PublisherDisplayName`、`mp:PhoneIdentity`。开发/上架 manifest 必须分离；Publisher 必须等于重签证书主体。
  不要把 Store 身份提交进仓库（它不是秘密，但会漂移）。
- **能力**：`runFullTrust`（必需）；`internetClient`/`privateNetworkClientServer` 对 mediumIL 是**惰性**的
  （声明无害，不解决防火墙/组播）；不要指望能力解决网络问题。防火墙规则按程序路径记忆，升级后路径变化需验证。
- **TargetDeviceFamily**：只留 `Windows.Desktop`；`MinVersion` 按 AppSDK 2.0/.NET 10 实测最低 OS；
  `MaxVersionTested` 提到当前 OS（现状 10.0.19041 与 TFM 26100 不符）。
- **startupTask**：`desktop:StartupTask` **无 Arguments** → `--startup-to-tray` 失效；
  改用 `AppInstance.GetActivatedEventArgs().Kind == ExtendedActivationKind.StartupTask`（需 V4 验证）；
  MSIX 下**隐藏**"注册表 Run"自启方式（否则写 `WindowsApps\<ver>`，更新即失效）。
- **通知**：toast COM 结构正确，但（a）self-contained WinAppSDK 的 AppNotification 依赖 Singleton 包，
  manifest 无 `PackageDependency` → 需 V5 验证/补依赖；（b）重定向激活丢参数 → `Activated` 按 Kind 分发；
  （c）`AppNotificationService` 终结器里 `Unregister` 应移除。
- **Strings**：packaged 下 resw 可能被 PRI 消费，`ms-appx` 松散文件读取会失败 → 验证包内 `Strings\...` 存在，
  否则改用 PRI/ResourceLoader。
- **数据**：LocalState + 首启导入（见 §5.1）；禁止与 MSI 共享同一 DB。
- **版本**：Store 规则 Major≠0、Revision=0、永久单调递增；建议首发 `1.0.0.0`，由 Version.props 映射。
- **出包**：`UapAppxPackageBuildMode=StoreUpload` 生成 `.msixupload`（不是裸 `.msix`）；
  载荷裁剪对构建布局生效并重生成 PRI；未签名包仅本地调试（`Add-AppxPackage -Register`，开发者模式）。
- **Store 准入**：隐私政策、年龄分级、截图、`runFullTrust` 说明；`unvirtualizedResources` 不申请。

### 5.5 winget（D6 修订）

- 提交形态：**多文件三件套**（`<Id>.yaml` version、`<Id>.installer.yaml`、`<Id>.locale.<lang>.yaml`）；
  singleton 仅 verified developers，**不能作为提交形态**（本地可作字段自检）。
- P1 先 **machine-only**（单条目 `Scope: machine`）；若要支持 per-user，加第二条：
  `Scope: user` + `InstallerSwitches: Silent: "/qn ALLUSERS=2 MSIINSTALLPERUSER=1"`（URL/SHA 相同）。
  `Scope` 是安装器筛选，**不是**传给 msiexec 的开关；单条目 + `--scope user` 会"无适用安装器"。
- 清单：`InstallerLocale: zh-CN`（MSI 仅 2052）；显示名与 MSI ARP 统一（`ClipBridge` vs `ClipBridge Shell` 需定）；
  不写死 ProductCode；`MinimumOSVersion` 按实际。
- 流程：build →（sign）→ **hash** → upload（冻结资产）→ 生成清单 → `winget validate` → 提 PR。
  Hash 必须对签名后的最终字节计算；资产上传后不得替换。
- 升级：`winget upgrade` 会按新清单调 MSI；跨 scope 静默迁移是已知坑（user 装过 → winget 提权按 machine 安装），
  需在 VM 实测并写 FAQ。winget 版本在 CI 中 pin；`publish.yml` 的 .NET 9 问题一并修。
- 便携 ZIP 不进 winget（`portable` 语义会复制到 WinGet 目录，改变"用户自己放哪"的定义）。

### 5.6 便携版（评审后修正）

- **代码现状**：仓库**没有**便携标记逻辑；数据根 8 处硬编码。必须先做 `DataRootResolver` 再谈 ZIP。
- ZIP 布局：顶层目录 `ClipBridge-<ver>-win-x64/`，内含 exe/依赖 + `ClipBridge.portable` + `version.txt`/`README`。
- 数据：默认 exe 旁 `data\`；不可写（Program Files/只读盘/UNC）回退 LocalAppData 并**明确提示**；
  可移动/网络盘上 WAL 有损坏风险 → 禁用 WAL 或警告。
- **不写注册表**的说法不成立：开机会自启仍写 `HKCU\...\Run`（值 `ClipBridge`），删除目录会留死项 →
  文档写"先关闭自启再删除"；便携与已安装版共存会抢同一 Run 键并双托盘（需互斥/提示）。
- 无卸载项、无开始菜单、手动更新；未签名会触发 SmartScreen/MOTW 提示。

### 5.7 共存、单实例与激活（D9 修订）

- `AppInstance.FindOrRegisterForKey` 按 app identity 作用域，**MSI 与 MSIX 之间不互通**；
  失败分支 `catch → return true` 会让两个都跑。必须加：
  1. 跨格式命名互斥量 `Local\ClipBridge-<UserSID>`（登录时未被占用才启动）；
  2. 转发通道：命名管道 `\\.\pipe\ClipBridge-<SID>`（或 RegisterWindowMessage）；
  3. 冲突 UX：提示"另一渠道的 ClipBridge 正在运行，请先退出"，而不是静默退出。
- 激活：`Activated` 事件按 `ExtendedActivationKind` 分发（通知/启动任务/协议），不要只 `ActivateMainWindow`；
  窗口未建好时排队。
- 启动项：MSIX 用 StartupTask，非打包用 Run 键；**同一渠道内**二选一，跨渠道各自注册会双启 → 检测到互斥时提示。

### 5.8 版本与 CI（D8 修订）

**版本源**：
- `platforms/windows/ClipBridgeShell_CS/Version.props`（app/Core/Tests 由父级 `Directory.Build.props` 继承；
  wixproj 显式 `<Import>`）。现有 app 项目内的 `Directory.Build.props` 只覆盖 app，**Core/Tests/wixproj 读不到**。
- 删除 `AssemblyVersion`/`FileVersion` 字面量（由 `$(Version)` 派生）。
- `Product.wxs`：`Version="$(var.ProductVersion)"`、ARP 注册表 `[ProductVersion]`（不再写死 1.0.9.0）。
- MSIX：无一等版本属性 → 构建期 XmlPoke/生成 `AppxManifest.xml`，4 段（Major≥1、Revision=0）。
- `scripts/version.ps1`：由 semver 派生 `MsiVersion`（3 段，x/y≤255、z≤65535）与 `AppxVersion`（4 段），
  越界/预发布不支持时**构建失败**；构建后回读三产物版本并断言一致。

**release-please（当前坏了）**：
- workflow 指向 `.release-please-manifest.json`，实际文件是 `release-please-manifest.json`；
- extra-file 路径少一层（指向 `platforms/windows/ClipBridgeShell_CS/Directory.Build.props`，实际在 `.../ClipBridgeShell_CS/ClipBridgeShell_CS/`）；
- 缺 `type: xml` + xpath；仓库零 tag → 从未成功。
- **tag 方案**：release-please 配置是组件前缀、无 `v`（`include-component-in-tag: true`、`include-v-in-tag: false`），
  发布流水线按 `platforms/windows/ClipBridgeShell_CS-x.y.z` 过滤并从 tag 解析版本；与 `Version.props` 交叉断言。

**CI**：
- 新增独立 `windows-installer` job（不塞进 `csharp` job）：下载固定版 `wix314-binaries.zip` →
  `-WixInstallPath` → 跑脚本 → 断言 MSI 内无 `.pdb`；PR 先 `continue-on-error`，tag 时阻塞。
- `publish.yml` 是不可用桩（.NET 9、restore 目录、artifact 名含 `/`、无 Rust/MSI/MSIX）→ 删除/重写。
- Release：tag 触发 → 三件套（ZIP/MSI/EXE）+ `SHA256SUMS`；MSIX 暂只作 CI 工件（Store 未接入）；
  上传后回读校验 SHA256；`paths-filter` 纳入 `scripts/**`、`.github/workflows/**`。
- 架构：本轮只 x64；arm64 后续增量（清单/Store 包/CI 矩阵一致）。

## 6. 签名与信任路线（D4，不变）

| 阶段 | MSI/EXE | MSIX | 成本 |
|---|---|---|---|
| 现在 | 未签名（SmartScreen 提示） | 不发布（本地松散注册调试） | 0 |
| 近期 | SignPath Foundation（MIT 符合；需复核依赖许可/System Libraries） | Store 个人开发者（免费、代签） | 0 |
| 备选 | Certum 个人版 ~¥600/年；Azure Artifact Signing ~$10/月 | 同上 | 低 |

签名 ≠ 无 SmartScreen（信誉积累期数周）；Store 渠道天然可信。

## 7. 验证计划（V1–V10，实现前/Store 前必须完成）

| # | 验证 | 判定 | 阻塞 |
|---|---|---|---|
| V1 | **AppData 写虚拟化**：runFullTrust 包写 `%LOCALAPPDATA%\ClipBridgeProbe\probe.txt`，对比真实路径 vs `Packages\<PFN>\LocalCache` | 真实路径出现 = PASS（D2″ 可改回包外）；只在私有目录 = FAIL（维持 D2″） | D2″ |
| V2 | MSI 造真实 DB → 装 MSIX 启动：检查 `-wal/-shm` 落点、对端是否见"新设备"、DB 是否混合 | 无混合 = PASS；混合 = 禁止共享（D2″ 已规避） | D2″/D9 |
| V3 | 托盘：图标、菜单、Explorer 重启后恢复 | 正常 = PASS | MSIX |
| V4 | StartupTask：登录启动 + 能否识别 `ExtendedActivationKind.StartupTask`；`desktop:` vs `uap5:` 写法 | 能启动且能识别 = PASS | 自启修复 |
| V5 | Toast：运行/未运行/重定向后 payload | 三场景可用 = PASS；否则补 Singleton 依赖/修重定向 | 通知 |
| V6 | UDP/mDNS/防火墙：首启、升级一版后、双实例 | 升级后仍发现 = PASS | 网络 |
| V7 | Strings：包内 `Strings\...\resw` 存在，zh/en 正常 | PASS/FAIL 决定 PRI 方案 | 本地化 |
| V8 | 单实例：MSI+MSIX 各启一次，托盘数/重复采集/互斥提示 | 单进程+提示 = PASS | D9 |
| V9 | Store 身份/版本：草稿提交确认报错、版本规则、`runFullTrust` 说明 | 掌握规则 = PASS | P2 |
| V10 | 签名安装形态重跑 V3–V7 | 与松散注册一致 = PASS | Store |

> V1/V2 是唯一能判定 D2″ 的实验；成本约 1–2 人日。**在用户确认前不执行**。

## 8. 阶段计划

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P0** | V1/V2 探针；`Version.props` + `version.ps1`；release-please 修复；`windows-installer` CI job；`publish.yml` 删除/重写；修 `CleanupStartupEntryCA` 的 `NOT UPGRADINGPRODUCTCODE` | 探针结论、版本一致、安装器进 CI |
| **P1** | `DataRootResolver`（8 处收敛）；MSI 双用途（若 D11=WiX 3）；便携 ZIP；Burn EXE（fixed per-user）；winget 多文件（machine-only）+ validate；Release 流水线（未签名，三件套） | 四产物可重复构建；安装/升级/卸载矩阵通过；winget PR 可提交 |
| **P2** | Store 账号 + MSIX 提交；SignPath 申请；V3–V10 全过；签名后重算 hash | Store 通过、签名产物 |
| **P3** | 自研更新器（本轮不做） | 后续 ADR |

## 9. 风险与待验证（v2）

| # | 风险 | 处理 |
|---|---|---|
| R1 | WiX 3 双用途的 `MSIINSTALLPERUSER=""` 空串覆盖、ICE105、维护/卸载路径 | P0 spike + 全矩阵实测 |
| R2 | 跨 scope 升级残留 | LaunchCondition 阻断 + 文档"先卸载" |
| R3 | MSIX AppData 虚拟化（D2″ 前提） | V1/V2 探针 |
| R4 | StartupTask 无参数/激活种类 | V4；必要时改 uap5 写法 |
| R5 | Toast 的 Singleton 依赖 / 重定向丢参 | V5 + PackageDependency + Kind 分发 |
| R6 | 跨格式单实例 | 互斥量 + 管道 + 冲突提示 |
| R7 | winget 多文件校验/提交流程、hash 时机 | CI pin 版本、签名后 hash、冻结资产 |
| R8 | 版本规则（Store Major≥1/Revision=0） | version.ps1 + 断言 |
| R9 | 便携 WAL on 可移动/网络卷 | 禁用 WAL 或警告 |
| R10 | 安装器首次进 CI 就发布 | P0 先加 job（continue-on-error） |
| R11 | 2052 单语言 vs winget/Store 本地化 | `InstallerLocale: zh-CN`；Store 补英文资源 |

## 10. 需要用户重新确认的决策

| 决策 | 选项 | 建议 |
|---|---|---|
| **D2** | 接受 D2′/D2″（LocalState + 导入） / 先跑 V1/V2 再定 | 先跑 V1/V2（1–2 人日），再定稿 |
| **D6** | winget P1 machine-only / 双条目（user 用 `ALLUSERS=2 MSIINSTALLPERUSER=1`） | P1 machine-only，P2 再加 user 条目 |
| **D11** | WiX 3 手工双用途 / 迁 WiX 5 | WiX 3（改动可控），除非 D15 要 EXE 切 scope |
| **D15** | EXE 固定 per-user / 一个 EXE 切 scope（→ WiX 5） | 固定 per-user |
| **D16（研究后新增）** | 安装器形态：单个双用途 MSI / **per-user EXE + per-machine MSI**（VS Code、PowerToys 模式） | 后者：scope 构建期固定，winget 两条 Scope 自然对齐 |
| **D17（研究后新增）** | EXE：单 per-user bundle / user+machine 两个 bundle / 换 Inno·NSIS 实现运行时切 scope | 单 per-user bundle + machine MSI |

| **Store 首发版本** | `1.0.0.0` 起 / 其他 | `1.0.0.0`，与 release-please 建立映射 |
| **探针** | 现在跑 V1/V2 / 留到实现阶段 | 现在跑（唯一能定 D2″ 的实验） |

## 11. 成熟实践研究结论（2026-11-08，待并入决策）

证据与出处见 `docs/windows-distribution-practices.md`。要点：

1. **主流项目不用"单个双用途 MSI"**：VS Code / PowerToys 用 user/machine 两个安装器；
   Git / Python 用一个 EXE 按参数/提权选 scope；7-Zip 的 MSI 只做 per-machine。
   微软官方 Single Package Authoring 确实定义了双用途 MSI（ALLUSERS=2 + MSIINSTALLPERUSER、ICE105、
   ProgramFiles64Folder 自动重定向），但真实项目少见；WiX v3 无原生支持，v4+ 有 perUserOrMachine，
   v7 才有 perMachineOrUser。
2. **MSIX 默认 AppData 写虚拟化**（官方文档明确）：共享真实 AppData 需 unvirtualizedResources +
   关闭写虚拟化；Files（WinUI3、Store 上架）就是这样做的，但它是文件管理器，理由充分。
   CB 默认仍采用 LocalState + 首启导入，把受限能力留作备选。
3. **WinUI3 的 startupTask 应用 uap5:Extension + uap5:StartupTask**（Files 的写法）；
   现有 desktop:Extension + Executable/EntryPoint 会出现"登录启动但窗口不出现"。
4. **单实例**：AppInstance 按应用版本/用户分列表；unpackaged 官方建议命名互斥量——跨格式仍需互斥量 + 管道。
5. **签名**：Store 对 MSIX 免费重签；MSI/EXE 提交 Store 必须自己 Authenticode 签名——与 D4 路线一致。
6. **便携**：VS Code 约定是"ZIP + exe 旁 data 目录"，不支持从安装版切换；CB 的标记方案与之等价。

据此建议把 **D3/D10 改为"per-user EXE + per-machine MSI"**（D16/D17），其余决策不变。
确认后再更新 §3 矩阵、§5.2/§5.3 与实现计划。
