# ADR-004：Windows 分发渠道与安装形态（v3，定稿待评审）

- 状态：**设计定稿，待并行评审**（无实现）
- 日期：2026-11-08（v1 初稿；v2 三路评审后修订；v3 采纳成熟实践，确定安装器形态）
- 研究：`docs/windows-distribution-practices.md`（真实项目案例与官方文档）
- 关联：`docs/distribution-strategy.md`、`docs/windows-distribution.md`
- 范围：**仅 Windows 壳**；本文不含实现。

## 0. 修订摘要

### v1 → v2（三路评审）
- D2 拆分为 D2′（MSI/便携统一数据根）与 D2″（MSIX = LocalState + 首启导入）。
- D3 的 WiX 3 双用途配方修正（删 `InstallScope`、`ALLUSERS=2`+`MSIINSTALLPERUSER`、`HKMU`、`ProgramFiles64Folder` 自动重定向）。
- D10 的"一个 EXE 切 scope"在 Burn 下不可行。
- D6 winget 不能用 singleton 提交，必须多文件。
- D8 版本源上移、release-please 修复、`publish.yml` 重写。

### v2 → v3（成熟实践研究，**已采纳**）
- **D16 = A**：放弃"单个双用途 MSI"，改为 **per-machine MSI + per-user EXE**（VS Code / PowerToys 模式）。
- **D17**：**单 per-user Burn bundle**（内部包一个隐藏的 per-user MSI）+ **独立的 per-machine MSI**。
- 随之确定：
  - 两个 MSI 变体（machine 独立分发；user 藏在 EXE 里，ARP 隐藏）；
  - 两套 `UpgradeCode`（避免 Windows Installer 把另一个 scope 当"相关产品"误升级/误删）；
  - 安装时**互斥检测**：检测到另一个 scope 已安装则阻断并提示先卸载；
  - winget 两条 Scope：machine MSI + user EXE；
  - MSIX 维持 D2″；startupTask 改 `uap5` 写法；单实例用命名互斥量；
  - WiX 3.14 即可（不再需要双用途/ICE105，也无需迁 WiX 5）。

## 1. 背景与目标

Windows 壳已有 WinUI 3 应用、WiX 3.14 MSI、MSTest；目前只有 per-machine MSI，安装器不进 CI。
目标：**一个构建源、四条渠道**：

| 渠道 | 产物 | 面向 |
|---|---|---|
| 官网 | per-user EXE（双击装，免 UAC）+ per-machine MSI（管理员/企业）+ 便携 ZIP | 普通用户 / 企业 / 绿色版 |
| Microsoft Store | MSIX | 商店用户 |
| winget | 清单（machine MSI + user EXE 两条 Scope） | 命令行用户 |

**本轮不做**（D7）：自研更新器（清单/签名校验/差量/回滚）。更新方式按渠道走：
Store 自动、winget upgrade、MSI/EXE/便携手动。
**签名方向**（D4，本轮不申请）：MSIX 走 Store 个人开发者（免费、代签）；
MSI/EXE 走 SignPath Foundation（免费 OSS）或备选 Certum 个人版（约 ¥600/年）。

## 2. 决策记录（v3）

| 编号 | 决策 | 选择 |
|---|---|---|
| D1 | 产物关系 | 一次构建派生：machine MSI、user MSI→EXE、便携 ZIP；MSIX 走 build 布局单独出包 |
| D2′ | MSI/便携数据根 | `%LOCALAPPDATA%\ClipBridge` |
| D2″ | MSIX 数据根 | `LocalState` + 首启从真实 `%LOCALAPPDATA%` 只读导入 + 显式导出 |
| D3 | **安装器形态** | **per-machine MSI（独立）**；per-user 走 EXE bundle（内含 per-user MSI） |
| D4 | 签名 | 本轮不买证书；Store 代签 MSIX + SignPath 签 MSI/EXE（后续） |
| D5 | MSIX 定位 | 仅 Store 渠道；不做自签名侧载分发 |
| D6 | winget | 多文件清单；两条 Scope：machine MSI、user EXE |
| D7 | 更新责任 | 本轮不做自研更新器 |
| D8 | 版本与发布 | `Version.props` 单一来源 + 修复 release-please + 重写 Release 流水线 |
| D9 | 共存 | 安装期互斥检测 + 运行期命名互斥量/管道 |
| D10 | EXE | Burn，fixed per-user；内部包 per-user MSI（ARP 隐藏） |
| D11 | MSI 工具链 | **WiX 3.14**（不再需要双用途；不迁 WiX 5） |
| D12 | 设置存储 | 统一到数据根（MSIX 为 LocalState）`settings.json`，含迁移 |
| D13 | 便携默认 | 数据随 exe（标记）；不可写回退并提示 |
| D14 | MSIX 包外数据 | 不采用（受限能力仅作跨渠道硬需求时的备选） |
| D15 | EXE scope | 仅 per-user；per-machine 只走 MSI |
| D16 | 安装器形态 | **A：per-user EXE + per-machine MSI**（已确认） |
| D17 | EXE 形态 | **单 per-user bundle + machine MSI**（已确认） |

## 3. 渠道与产物矩阵（v3）

| 产物 | 生成方式 | 安装位置 | 数据 | 卸载 | 签名 | 更新 |
|---|---|---|---|---|---|---|
| per-machine MSI | WiX 3.14，`InstallScope="perMachine"` | `C:\Program Files\ClipBridge` | `%LOCALAPPDATA%\ClipBridge`（按用户） | 保留数据（可选删除） | SignPath（后续） | 手动/winget |
| per-user EXE | WiX Burn 包 per-user MSI（隐藏 ARP） | `%LOCALAPPDATA%\Programs\ClipBridge` | 同上 | 同 MSI | SignPath（后续） | 手动 |
| 便携 ZIP | 压缩 publish 目录 + 标记 + 顶层目录 | 用户选择 | exe 旁 `data\`（可回退） | 手动 | 无 | 手动 |
| MSIX | `dotnet build` 布局 + `StoreUpload` | Store 管理（`WindowsApps`） | `LocalState` + 首启导入 | 卸载包；LocalState 随包删 | Store 代签 | Store 自动 |
| winget 清单 | 三文件，两条 Scope | 由对应安装器决定 | 同安装器 | 同安装器 | 不需要（SHA256） | `winget upgrade` |

## 4. 构建与产物派生（v3）

```
dotnet publish（Release、win-x64、self-contained、R2R、瘦身）
  └─ <publishDir>
       ├─ 便携 ZIP：publishDir + ClipBridge.portable + 顶层目录（过滤 *.pdb/*.xml）
       ├─ machine MSI：heat(publishDir) → candle/light（InstallScope=perMachine）
       ├─ user MSI：heat(publishDir) → candle/light（InstallScope=perUser）
       │    └─ Burn bundle（WixStdBA，fixed per-user）→ ClipBridge-Setup-<ver>-user.exe
       └─ MSIX：dotnet build -p:GenerateAppxPackageOnBuild -p:UapAppxPackageBuildMode=StoreUpload
```

- **版本**：单一来源 `platforms/windows/ClipBridgeShell_CS/Version.props`；三个 MSI/WiX 变量与
  MSIX 4 段版本由它派生（见 §5.8）。
- **两个 MSI 的差异**：`InstallScope`、`UpgradeCode`、`OutputName`、注册表根（HKLM vs HKCU）、
  开始菜单（公共 vs 用户）、ARP（machine 可见；user 由 bundle 隐藏）。
- **瘦身**：移除未使用的 WindowsAppSDK AI/ML；只保留 zh-CN/zh-TW/en-us。量级参考：
  publish 287→241MB；MSI 87.9MB；MSIX 109→90MB；ZIP 91.7MB。
- **MSIX** 不走 publish：由 MSBuild 处理 `$targetnametoken$`/`x-generate`/PRI；
  载荷裁剪对构建布局生效并重生成/校验 PRI。

## 5. 详细设计

### 5.1 数据模型（D2′/D2″）

**统一解析器** `DataRootResolver`（唯一判定点），收敛现有 8 处硬编码：
`CoreConfigBuilder.cs:30-38`、`CoreHostService.cs:141-157,372-379`、`App.xaml.cs:705-716`、
`StashLogManager.cs:22-29`、`LocalSettingsService.cs:56-92`、`SettingsPage.xaml.cs:169-179,651-664`、
`StartupTrace.cs:36-40`、`FrameTrace.cs:2010-2015`。

- **MSI / 便携**：`%LOCALAPPDATA%\ClipBridge`；便携标记（exe 旁 `ClipBridge.portable`）→
  `<exeDir>\data`；不可写回退并**明确提示**。
- **MSIX**：`LocalFolder`（LocalState）+ `LocalCacheFolder`；设置统一到 `<LocalState>\settings.json`（D12）。
  - **依据**：MSIX 默认 AppData 写虚拟化（新建文件进私有位置、跨进程不可见、卸载即删）；
    共享真实 AppData 需 `rescap:unvirtualizedResources` + 关闭写虚拟化（Files 有先例，但需 Store 理由）。
  - **换渠道**：首启检测 LocalState 无 `core.db` 且真实 `%LOCALAPPDATA%\ClipBridge\core.db` 存在 →
    复制 DB + `tls\` + 设置 + `device_id`，打 sentinel，一次性提示。
  - **禁止**两个渠道同时指向同一 DB（真实 `core.db` + 私有 `-wal/-shm` = 混合损坏态）。
- **设置/设备身份**：`device_id` 与 `tls\` 跟随数据根迁移；设置统一（D12）。
- **WAL 风险**：便携数据在 U 盘/网络盘时 WAL 可能损坏 → 禁用 WAL 或明确警告。
- **卸载语义**：machine MSI 询问删数据（默认保留）；user MSI 同；MSIX 卸载删 LocalState；
  便携手动。注意 machine MSI 的"删数据"只覆盖执行卸载的用户（`%LOCALAPPDATA%` 是每用户）。
- **诊断/日志**：packaged 下 EventLog 与 `%LOCALAPPDATA%` 日志不可见 → 改写 LocalState + 导出诊断；
  `Diagnostics` 显示实际传入路径（现状错：取 BuildPaths 而非 LocalState）。

### 5.2 per-machine MSI（D3/D16，v3 重写）

- **WiX 3.14 直接用 `Package InstallScope="perMachine"`**（原生支持）；不需要双用途、不需要
  `ALLUSERS=2`/`MSIINSTALLPERUSER`、不需要 scope 对话框、不需要 ICE105。
- **目录**：`[ProgramFiles64Folder]ClipBridge`；保留现有 `Directory` 树。
- **注册表**：`HKLM\SOFTWARE\ClipBridge\ClipBridgeShell`；快捷方式 keypath 用 `HKLM`（machine）。
- **开始菜单**：`ProgramMenuFolder`（所有用户）；桌面快捷方式按现有 `DesktopFolder`（安装者桌面，
  若希望所有用户桌面则用 `CommonDesktopFolder`——产品决策）。
- **ARP**：machine 范围自动注册；`Product/@Version` 用 `$(var.ProductVersion)`，注册表值 `[ProductVersion]`。
- **卸载清理**：
  - `RemoveFolderEx` 不能直接吃 Directory id：用 **Remember Property**（安装时写 `HKLM\...\InstallPath`，
    卸载时 `RegistrySearch` 读入属性再清理），或删除该组件。
  - `REMOVEUSERDATA` 等属性 `Secure="yes"`；deferred CA 的 `Impersonate`/`%LOCALAPPDATA%` 语义要写清
    （per-machine 卸载只删当前用户数据）。
  - 修正现有 bug：`CleanupStartupEntryCA` 缺 `NOT UPGRADINGPRODUCTCODE`，major upgrade 会误删自启。
- **自启**：应用只写 `HKCU\...\Run`（每个用户各自启用）；machine MSI 卸载只清当前用户。
  文档明确"per-machine 安装后，每个用户需自行开启自启"。
- **升级**：machine 专用 `UpgradeCode`；`MajorUpgrade` 正常。
- **跨 scope 检测**：安装前检查 `HKCU` 的 per-user 安装标记（user MSI/EXE 写入），
  若存在则 `LaunchCondition` 阻断，提示"请先卸载当前用户版"；反之 user MSI 检查 HKLM 标记。
- **ICE**：恢复被抑制的关键 ICE（至少 ICE03/ICE30 复核）；`RemoveFolderEx` 修正后再评估。
- **测试矩阵**：install / major-upgrade / repair / uninstall；与 user 版互斥；数据保留。

### 5.3 per-user EXE（D10/D17，Burn）

- **结构**：Burn bundle（`WixBalExtension`）+ 内部 **per-user MSI**（`InstallScope="perUser"`）。
  - per-user MSI：装到 `%LOCALAPPDATA%\Programs\ClipBridge`（`ProgramFiles64Folder` 在 per-user 上下文
    自动重定向，无需 `SetDirectory`）；注册表 HKCU；用户开始菜单；ARP 由 bundle 隐藏。
  - bundle：`MsiPackage Visible="no"`（MSI 不单独出现在 ARP），bundle 自身为 ARP 入口；
    独立 `Bundle/@UpgradeCode`（永久不变），`Version` 来自 Version.props。
- **scope**：构建期固定 per-user（Burn 按 MSI 摘要 bit 8 固定提权/scope；StdBA 无 scope 选择）。
  `/quiet`、`/passive` 由 Burn 处理；`MsiProperty` 传 `MSIINSTALLPERUSER=1`（per-user MSI 也可用
  `InstallScope="perUser"` 直接声明，二者选一，避免冲突）。
- **ARP/卸载**：bundle 是唯一 ARP 入口；卸载 bundle 时 MSI 一并卸载；清理自启/数据按 §5.2 的 CA 规则。
- **签名顺序**：签 per-user MSI → 打进 bundle → 签 EXE。
- **跨 scope 检测**：安装前检查 `HKLM\SOFTWARE\ClipBridge\ClipBridgeShell` 标记（machine MSI 写入），
  存在则阻断提示先卸载机器版。
- **不做**：一个 EXE 里选 scope（WiX 3 Burn 做不到，D15）；per-machine 由 MSI/winget 提供。

### 5.4 MSIX 与 Store（D2″/D5）

- **身份**：仓库 manifest 保留开发身份；构建期注入 Partner Center 的
  `Identity/@Name`、`@Publisher`（`CN=<PublisherGuid>`）、`@Version`、`Properties/@DisplayName`、
  `PublisherDisplayName`、`mp:PhoneIdentity`。开发/上架 manifest 分离；Publisher 必须等于重签证书主体。
- **能力**：`runFullTrust` 必需；`internetClient`/`privateNetworkClientServer` 对 mediumIL 惰性
  （声明无害，不解决防火墙/组播）。`unvirtualizedResources` 不申请（备选）。
- **TargetDeviceFamily**：只留 `Windows.Desktop`；`MinVersion` 按实测最低 OS；
  `MaxVersionTested` 提到当前 OS。
- **startupTask**：改用 **`uap5:Extension Category="windows.startupTask"` + `uap5:StartupTask`**
  （Files 的写法）；不要 `desktop:Extension` + `Executable/EntryPoint`（WinUI3 下"登录启动但窗口不出现"）。
  通过 `AppInstance.GetActivatedEventArgs().Kind == ExtendedActivationKind.StartupTask` 判断自启启动（V4 验证）。
- **通知**：toast COM 结构保留；self-contained 的 AppNotification 依赖 Singleton 包 → V5 验证，
  必要时补 `PackageDependency`；重定向激活按 `Kind` 分发（不要只 `ActivateMainWindow`）；
  移除 `AppNotificationService` 终结器里的 `Unregister`。
- **Strings**：packaged 下验证 `Strings\...\resw` 是否以松散文件进包；否则改用 PRI/ResourceLoader。
- **数据**：LocalState + 首启导入（§5.1）。
- **版本**：四段、Major≠0、Revision=0、单调递增；建议首发 `1.0.0.0`。
- **出包**：`-p:GenerateAppxPackageOnBuild=true -p:UapAppxPackageBuildMode=StoreUpload`
  → `.msixupload`；载荷裁剪对构建布局生效并重生成 PRI。
- **本地调试**：开发者模式 + `Add-AppxPackage -Register <layout>\AppxManifest.xml`（不要自签名分发给用户）。
- **Store 准入**：隐私政策、年龄分级、截图、`runFullTrust` 说明；提交前跑 WACK。

### 5.5 winget（D6，v3）

- **多文件三件套**（version / installer / locale）；**不用 singleton 提交**（仅 verified developers）。
- **两条 Scope**：
  - `Scope: machine` + `InstallerType: msi` → machine MSI；
  - `Scope: user` + `InstallerType: burn`（或 `exe`）→ per-user EXE，`InstallerSwitches` 静默参数。
- URL 指向 GitHub Release；`InstallerSha256` 在**签名后**计算；资产上传后冻结。
- `InstallerLocale: zh-CN`；显示名与 ARP 统一（`ClipBridge`）；不写死 ProductCode。
- 升级：`winget upgrade` 按各自 scope 调对应安装器；**不要**让 winget 把 user 版升级成 machine 版
  （两条 Scope 各自独立）。
- 提交流程：build → sign → hash → upload（冻结）→ 清单 → `winget validate` → PR。
- 便携 ZIP 不进 winget（`portable` 会把文件复制到 WinGet 目录，语义不同）。

### 5.6 便携版（D13）

- ZIP 顶层目录 `ClipBridge-<ver>-win-x64/`；内含 exe/依赖 + `ClipBridge.portable` + `version.txt`/`README`。
- 数据默认 exe 旁 `data\`；不可写回退 LocalAppData 并提示；可移动/网络盘禁用 WAL 或警告。
- 不写卸载项；开机自启仍写 HKCU Run → 文档"先关自启再删目录"；与安装版共存会抢 Run 键/双托盘 → 互斥提示。
- 无签名 → SmartScreen/MOTW 提示。

### 5.7 共存、单实例与激活（D9）

- **安装期互斥**：两个 MSI 各自写 scope 标记（machine → HKLM，user → HKCU）；安装器用
  `RegistrySearch` + `LaunchCondition` 检测另一 scope 并阻断；错误文案要求先卸载。
- **运行期单实例**：`AppInstance` 按应用版本/用户分列表，**MSI 与 MSIX 之间不互通** →
  加 `Local\ClipBridge-<UserSID>` 命名互斥量 + 命名管道 `\\.\pipe\ClipBridge-<SID>`；
  冲突时提示"另一渠道/版本正在运行"。
- **激活**：`Activated` 按 `ExtendedActivationKind` 分发；窗口未建好时排队。
- **自启**：MSIX 用 StartupTask（uap5），MSI/EXE 用 HKCU Run；同一渠道内二选一。

### 5.8 版本与 CI（D8）

- `platforms/windows/ClipBridgeShell_CS/Version.props`（app/Core/Tests 由父级 `Directory.Build.props` 继承；
  wixproj 显式 Import）。删除 `AssemblyVersion`/`FileVersion` 字面量。
- `Product.wxs`：`Version="$(var.ProductVersion)"`、ARP `[ProductVersion]`；两个 MSI 都从同一来源。
- MSIX：构建期注入 4 段版本（Major≥1、Revision=0）。
- `scripts/version.ps1`：semver → `MsiVersion`（3 段，x/y≤255、z≤65535）与 `AppxVersion`（4 段）；
  越界/预发布不支持时构建失败；构建后回读三产物版本并断言一致。
- **release-please 修复**：`manifest-file: release-please-manifest.json`；extra-file 用
  `{type: xml, path: <真实路径>, xpath: //Project/PropertyGroup/Version}`；tag 方案统一为
  `platforms/windows/ClipBridgeShell_CS-x.y.z`（现有配置：组件前缀、无 v）。
- **CI**：
  - 新增 `windows-installer` job：下载固定版 wix314-binaries.zip → `-WixInstallPath` →
    构建两个 MSI + bundle + ZIP → 断言 MSI 内无 `.pdb`；PR 先 `continue-on-error`。
  - `publish.yml` 删除/重写（现为 .NET 9、restore 目录、artifact 名含 `/` 的桩）。
  - Release（tag 触发）：machine MSI、user EXE、便携 ZIP + `SHA256SUMS`；MSIX 暂只作 CI 工件；
    上传后回读校验；`paths-filter` 纳入 `scripts/**`、`.github/workflows/**`。
- 架构：本轮只 x64；arm64 后续。

## 6. 签名与信任路线（D4）

| 阶段 | MSI/EXE | MSIX | 成本 |
|---|---|---|---|
| 现在 | 未签名（SmartScreen 提示） | 不发布（本地松散注册调试） | 0 |
| 近期 | SignPath Foundation（MIT 符合；需复核依赖许可） | Store 个人开发者（免费、代签） | 0 |
| 备选 | Certum 个人版 ~¥600/年；Azure Artifact Signing ~$10/月 | 同上 | 低 |

签名 ≠ 无 SmartScreen（信誉积累期数周）；Store 渠道天然可信。

## 7. 验证计划（V1–V11）

| # | 验证 | 判定 | 阻塞 |
|---|---|---|---|
| V1 | MSIX AppData 写虚拟化：runFullTrust 包写 `%LOCALAPPDATA%\ClipBridgeProbe\probe.txt`，对比真实路径 vs `Packages\<PFN>\LocalCache` | 真实路径出现 = PASS；仅私有 = FAIL（维持 D2″） | D2″ |
| V2 | MSI 造真实 DB → 装 MSIX 启动：检查 `-wal/-shm` 落点、对端是否见新设备、DB 混合 | 无混合 = PASS | D2″/D9 |
| V3 | 托盘：图标、菜单、Explorer 重启后恢复 | 正常 = PASS | MSIX |
| V4 | StartupTask：uap5 写法下登录启动 + 识别 `ExtendedActivationKind.StartupTask` | 正常 = PASS | 自启 |
| V5 | Toast：运行/未运行/重定向后 payload | 三场景可用 = PASS | 通知 |
| V6 | UDP/mDNS/防火墙：首启、升级一版后、双实例 | 升级后仍发现 = PASS | 网络 |
| V7 | Strings：包内 resw 存在，zh/en 正常 | PASS/FAIL 决定 PRI 方案 | 本地化 |
| V8 | 单实例：MSI + MSIX 各启一次，托盘数/重复采集/互斥提示 | 单进程+提示 = PASS | D9 |
| V9 | Store 身份/版本：草稿提交确认报错、版本规则、runFullTrust 说明 | 掌握规则 = PASS | P2 |
| V10 | 签名安装形态重跑 V3–V7 | 一致 = PASS | Store |
| V11 | **machine MSI + user EXE 互斥**：先装一种再装另一种，检查阻断/提示；卸载后另一种可装；数据保留 | 阻断且提示清晰 = PASS | D3/D16 |

> V1/V2 是判定 D2″ 的实验；V11 是判定 v3 安装器形态的实验。**用户确认后才执行**。

## 8. 阶段计划

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P0** | V1/V2/V11 探针；`Version.props`+`version.ps1`；release-please 修复；`windows-installer` CI；`publish.yml` 重写；修 `CleanupStartupEntryCA` | 探针结论、版本一致、安装器进 CI |
| **P1** | `DataRootResolver`；machine MSI；per-user MSI+EXE；便携 ZIP；winget 三文件两 Scope；Release 流水线（未签名） | 三产物可重复构建；安装/升级/卸载/互斥矩阵通过；winget validate |
| **P2** | Store 账号 + MSIX 提交；SignPath；V3–V10 | Store 通过、签名产物 |
| **P3** | 自研更新器（本轮不做） | 后续 ADR |

## 9. 风险与待验证

| # | 风险 | 处理 |
|---|---|---|
| R1 | 两个 MSI 的 UpgradeCode/检测/互斥设计 | V11 + 评审 |
| R2 | per-user MSI 的 ARP 行为（HKLM 仍写卸载键，winget 可能当 machine） | 评审/实测，必要时用 `ARPSYSTEMCOMPONENT=1` 或 bundle 隐藏 |
| R3 | Burn 内部 per-user MSI 的 scope 与 `MsiProperty` 冲突 | 评审 + 实测 |
| R4 | MSIX AppData 虚拟化 | V1/V2 |
| R5 | StartupTask uap5/激活种类 | V4 |
| R6 | Toast Singleton 依赖/重定向丢参 | V5 |
| R7 | 跨格式单实例 | 互斥量 + 管道 |
| R8 | winget 两条 Scope/hash 时机 | CI pin 版本 + 签名后 hash |
| R9 | Store 版本规则 | version.ps1 + 断言 |
| R10 | 便携 WAL on 可移动/网络卷 | 禁用 WAL 或警告 |
| R11 | 安装器首次进 CI 就发布 | P0 先加 job（continue-on-error） |
| R12 | 2052 单语言 vs winget/Store 本地化 | `InstallerLocale: zh-CN`；Store 补英文资源 |

## 10. 仍待确认的决策

| 决策 | 选项 | 建议 |
|---|---|---|
| **D2** | 接受 D2″（LocalState + 导入）/ 先跑 V1/V2 再定 | 先跑 V1/V2 |
| **Store 首发版本** | `1.0.0.0` / 其他 | `1.0.0.0` |
| **探针** | 现在跑 V1/V2/V11 / 留到实现阶段 | 现在跑（V11 决定安装器形态） |
| D12 设置统一 | 统一到数据根 / 维持分叉 | 统一（含迁移） |

## 11. 成熟实践采纳记录

见 `docs/windows-distribution-practices.md`。已采纳：
- **VS Code / PowerToys 模式**：per-user 与 per-machine 分开构建（D16/D17）。
- **MSIX 官方虚拟化**：LocalState + 导入（D2″）；`unvirtualizedResources` 仅作备选。
- **WinUI3 startupTask**：`uap5:Extension` + `uap5:StartupTask`。
- **Store 签名**：MSIX 免费代签；MSI/EXE 必须自签。
- **便携**：VS Code 的 ZIP + `data` 目录约定。

被否决：
- 单个双用途 MSI（官方支持但主流不用，复杂度高）。
- 一个 EXE 运行时切 scope（WiX 3 Burn 做不到）。
- winget singleton 提交（仅 verified developers）。
- MSIX 包外共享 AppData（需受限能力，Store 理由不足）。
