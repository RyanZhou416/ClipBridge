# ADR-004：Windows 分发渠道与安装形态（v4.1，定稿）

- 状态：**定稿，待实施**（v1 初稿 → v2 三路评审 → v3 成熟实践采纳 → v4 四路评审 + 源码对照）
- 日期：2026-11-08
- 参考实现：`docs/windows-distribution-practices.md`、PowerToys 安装器源码、VS Code/Git/Python 的 winget 清单、Files 的 MSIX manifest
- 范围：**仅 Windows 壳**；本文不含实现。

## 0. 争议点的成熟答案（不再自行推演）

这些问题是行业里反复解决过的，直接采用成熟项目的做法：

| 问题 | 成熟答案 | 依据 |
|---|---|---|
| 两个 MSI 怎么构建 | **一个 wixproj，`PerUser` 定义构建两遍**；`OutputName`/`OutputPath`/`IntermediateOutputPath` 按变体分开（MachineSetup / UserSetup），共享 `Product.wxs` | PowerToys `PowerToysInstallerVNext.wixproj`（`PerUser` 定义 + 两套 OutputPath） |
| machine / user 的 UpgradeCode | **两套独立 UpgradeCode**；machine 沿用现有 GUID（可升级现场），user 用新 GUID；bundle 用 `util:ProductSearch` **同时探测两者** | PowerToys `PowerToys.wxs:41-42`（ProductSearch 两个 UpgradeCode） |
| per-user MSI 的 ARP 会不会落 HKLM | 会；**Burn 不会自动传 ARPSYSTEMCOMPONENT**（WiX Burn 源码 0 命中）→ bundle 里必须显式 MsiProperty ARPSYSTEMCOMPONENT=1；winget 跳过 SystemComponent 条目 → 用户看到 **bundle 的 HKCU ARP** | WiX Burn 源码 + winget-cli#6063 |
| 安装目录 | machine：`[ProgramFiles64Folder]\ClipBridge`；user：**显式** `[LocalAppDataFolder]\Programs\ClipBridge`（不依赖隐式重定向） | MS Installation Context（per-user 重定向存在，但显式更稳；A 的 spike 建议） |
| 注册表根/开始菜单要不要参数化 | **不需要**：注册表根用 `HKMU`（-1 → machine=HKLM / user=HKCU）；`ProgramMenuFolder` 随 ALLUSERS 自动切换 | MS Installation Context |
| DesktopFolder 含义 | per-machine 下 `DesktopFolder` 本身就是 **All Users 桌面**，不是"安装者桌面"；MSI **没有** `CommonDesktopFolder` | MS Installation Context（A/B 一致） |
| RemoveFolderEx | **目录 id 不可用**：属性必须在 `CostInitialize` 之前解析，而 Directory 属性要到 `CostFinalize` 才有值 → 用 **Remember Property**（安装时写注册表，卸载时 RegistrySearch 读回） | WiX 官方 RemoveFolderEx 文档 |
| MSI 静默安装的互斥检测 | 标准 RegistrySearch 不够，**AppSearch 必须同时排进 InstallUISequence 与 InstallExecuteSequence**；util:RegistrySearch 是 Burn 侧的（父元素 Bundle/Fragment），不能用于 MSI | MS AppSearch 文档 + WiX util 文档 |
| MSIX 数据 | LocalState + 首启导入；**不要直接拷活库**。export_backup 需要已初始化 core（FFI 传 handle），首启登录前不可用 → 需新增无 handle 的快照函数（§5.1） | 官方虚拟化文档 + exports.rs:709 |
| WinUI3 开机启动 | **`uap5:Extension` + `uap5:StartupTask`**，不要 `desktop:Extension` + Executable/EntryPoint | Files 的 manifest、MS Q&A |
| 单实例 | `AppInstance` 按身份/版本分列表、跨 MSIX/MSI 不互通 → **`Global\ClipBridge-<SID>-<数据根哈希>` 互斥量 + 命名管道**；`Local\` 不够（RDP 多会话） | AppLifecycle 文档 + C/D 评审合并 |
| release-please 触发发布 | `GITHUB_TOKEN` 创建的 tag **不触发**其它 workflow → 配 **PAT/App token**，或发布并入同一 workflow | GitHub 递归保护（多个真实仓库的修复 commit） |
| 版本映射 | MSI 只吃 **3 段**（第 4 段被忽略）；MSIX **4 段、Major≠0、Revision=0**；`AppxVersion = max(1,major).minor.patch.0` | MS ProductVersion / Store 包版本文档 |
| PDB 入包 | heat 直接吃 publish 目录会把 `.pdb` 打进去 → **stage 目录**（去 pdb/xml）后 heat + ZIP 共用 | 现有 `HeatGenerated.wxs` 实测含 2 个 pdb |
| 组件 GUID 随机 | heat `-gg` 每次构建生成随机 GUID → 改 **`-ag`（Guid="*"）并只 heat 一次**，两变体共用 | 现有 heat 产物 385 个随机 GUID |
| 卸载"删数据"在 bundle 下 | WixStdBA 不显示 MSI UI、winget `/qn` → `REMOVEUSERDATA` 永远拿不到 → **user 版默认保留数据**；machine MSI 保留对话框 | PowerToys `bal:DisplayInternalUICondition=false` 同类做法 |

## 1. 背景与目标

Windows 壳已有 WinUI 3 应用、WiX 3.14 MSI、MSTest；目前只有 per-machine MSI，安装器不进 CI。
目标：**一个构建源、四条渠道**：

| 渠道 | 产物 | 面向 |
|---|---|---|
| 官网 | per-user EXE（双击装，免 UAC）+ per-machine MSI（管理员/企业）+ 便携 ZIP | 普通用户 / 企业 / 绿色版 |
| Microsoft Store | MSIX | 商店用户 |
| winget | 多文件清单：machine MSI + user EXE 两条 Scope | 命令行用户 |

**本轮不做**（D7）：自研更新器。
**签名方向**（D4，本轮不申请）：MSIX 走 Store（免费代签）；MSI/EXE 走 SignPath（免费 OSS）或 Certum（约 ¥600/年）。

## 2. 决策记录（v4）

| 编号 | 决策 | 选择 |
|---|---|---|
| D1 | 产物关系 | 一次构建派生：machine MSI、user MSI→EXE、便携 ZIP；MSIX 走 build 布局 |
| D2′ | MSI/便携数据根 | `%LOCALAPPDATA%\ClipBridge` |
| D2″ | MSIX 数据根 | `LocalState` + 首启从真实 `%LOCALAPPDATA%` 只读导入 + 显式导出 |
| D3 | 安装器形态 | per-machine MSI + per-user EXE（两套 UpgradeCode，互斥） |
| D4 | 签名 | 本轮不买证书；Store 代签 MSIX + SignPath 签 MSI/EXE（后续） |
| D5 | MSIX 定位 | 仅 Store 渠道；不做自签名侧载分发 |
| D6 | winget | 多文件三件套；两条 Scope（machine MSI / user Burn EXE） |
| D7 | 更新责任 | 本轮不做自研更新器 |
| D8 | 版本与发布 | `Version.props` 单一来源 + release-please（PAT）+ 重写 Release 流水线 |
| D9 | 共存 | 安装期互斥（专用键 + ProductSearch/RegistrySearch）+ 运行期互斥量/管道 |
| D10 | EXE | Burn，fixed per-user；内部包 per-user MSI（bundle **显式**传 ARPSYSTEMCOMPONENT=1） |
| D11 | MSI 工具链 | WiX 3.14（`InstallScope="perMachine|perUser"`；不需要双用途/ICE105） |
| D12 | 设置存储 | 统一到数据根（MSIX 为 LocalState）`settings.json`，含迁移 |
| D13 | 便携默认 | 数据随 exe（标记）；不可写回退并提示 |
| D14 | MSIX 包外数据 | 不采用（受限能力仅作备选） |
| D15 | EXE scope | 仅 per-user；per-machine 只走 MSI |
| D16 | 安装器形态 | A：per-user EXE + per-machine MSI（已确认） |
| D17 | EXE 形态 | 单 per-user bundle + machine MSI（已确认） |

## 3. 渠道与产物矩阵

| 产物 | 生成方式 | 安装位置 | 数据 | 卸载 | 签名 | 更新 |
|---|---|---|---|---|---|---|
| per-machine MSI | wixproj `PerUser=false` | `C:\Program Files\ClipBridge` | `%LOCALAPPDATA%\ClipBridge`（每用户） | 保留数据（对话框可选删） | SignPath（后续） | 手动/winget |
| per-user EXE | wixproj `PerUser=true` + Burn | `%LOCALAPPDATA%\Programs\ClipBridge` | 同上 | 保留数据（无 UI） | SignPath（后续） | 手动 |
| 便携 ZIP | publish + 标记 + 顶层目录 | 用户选择 | exe 旁 `data\`（可回退） | 手动 | 无 | 手动 |
| MSIX | build 布局 + `StoreUpload` | Store 管理 | `LocalState` + 导入 | 包删 LocalState | Store 代签 | Store 自动 |
| winget 清单 | 三文件 | 由安装器决定 | 同安装器 | 同安装器 | SHA256 | `winget upgrade` |

## 4. 构建与产物派生

```
dotnet publish（Release、win-x64、self-contained、R2R、瘦身）→ stage（删 *.pdb/*.xml）
  ├─ 便携 ZIP（P1）：stage + ClipBridge.portable + 顶层目录
  ├─ machine MSI：heat(stage, -ag) 一次 → candle/light（PerUser=false）
  ├─ user MSI：共用 heat 输出 → candle/light（PerUser=true）
  │    └─ Burn bundle（WixStdBA，PerUser=true）→ ClipBridge-Setup-<ver>-win-x64-user.exe
  └─ MSIX：dotnet build 布局 + StoreUpload（裁剪对 build 布局生效）
```

- **one wixproj, two builds**：`-p:PerUser=false|true`；`OutputName`/`OutputPath`/`IntermediateOutputPath`
  按变体分开（PowerToys 同款）；共享 `Product.wxs`。
- **heat 只跑一次**（`-ag`），两个 candle 共用 `HeatGenerated.wxs`；stage 是唯一载荷来源。
- **MSIX** 不走 publish：MSBuild 处理 `$targetnametoken$`/`x-generate`/PRI；裁剪后重生成/校验 PRI。
- **瘦身**：移除未使用 WindowsAppSDK AI/ML；保留 zh-CN/zh-TW/en-us。量级：publish 287→241MB；MSI 87.9MB；MSIX 109→90MB；ZIP 91.7MB。
- 资产命名：`ClipBridge-<ver>-win-x64.msi`、`ClipBridge-Setup-<ver>-win-x64-user.exe`、
  `ClipBridge-<ver>-win-x64.zip`（arch 后缀必带；不能含 `/`）。

## 5. 详细设计

### 5.1 数据模型（D2′/D2″）

**统一解析器** `DataRootResolver`（唯一判定点），收敛现有 8 处硬编码：
`CoreConfigBuilder.cs:30-38`、`CoreHostService.cs:141-157,372-379`、`App.xaml.cs:705-716`、
`StashLogManager.cs:22-29`、`LocalSettingsService.cs:56-92`、`SettingsPage.xaml.cs:169-179,651-664`、
`StartupTrace.cs:36-40`、`FrameTrace.cs:2010-2015`。

- **MSI / 便携**：`%LOCALAPPDATA%\ClipBridge`；便携标记（exe 旁 `ClipBridge.portable`）→
  `<exeDir>\data`；不可写回退并明确提示。
- **MSIX**：`LocalFolder`（LocalState）+ `LocalCacheFolder`；设置统一到 `<LocalState>\settings.json`（D12）。
  - **导入规格**（C 的 blocker）：
    1. 检测：LocalState 无 `core.db` 且真实 `%LOCALAPPDATA%\ClipBridge\core.db` 存在；
    2. **一致性快照**：export_backup 需要已初始化的 core（exports.rs:709 的 handle），首启登录前不可用 → 新增**无 handle 的 cb_snapshot_db(src, dst)**（只读打开旧库 + VACUUM INTO，复用 core 的 rusqlite）或 shell 侧 Microsoft.Data.Sqlite；不要直接拷活库；
    3. 复制 `tls\`、设置、`device_id`（设置键 `Core_DeviceId`）；CAS 缓存可选导入（否则历史图片/文件悬空，需提示）；
    4. 写 sentinel（记录来源版本/时间）；失败则删除半成品、保留原数据、下次重试；
    5. 凭证：PasswordVault 对**非 AppContainer 的桌面应用**是用户级共享（官方文档），full-trust MSIX 可读旧凭证；仅读不到时回退到重新登录（fail-closed Degraded），不是强制重登；
    6. 卸载重装后 sentinel 随 LocalState 消失 → 会再次导入旧真实数据：UI 明确提示"已从旧版本导入"，并提供"不再导入"开关。
  - **禁止**两个渠道同时指向同一 DB（真实 `core.db` + 私有 `-wal/-shm` = 混合损坏态）。
- **device_id 语义**：一台机器 N 个用户 = N 个 device_id（各自数据根）；换渠道导入保留 device_id，
  但**不要**让两个渠道同时运行（mDNS 实例名相同 → TOFU 冲突）。
- **WAL**：便携数据在 U 盘/网络盘时 WAL 可能损坏 → 检测 `DriveType`（Removable/Network）时拒绝或只读回退并提示；
  **不**静默改 core 的 WAL（core 硬编码 `journal_mode=WAL`，测试有断言）。
- **卸载**：machine MSI 有"删数据"对话框（默认保留）；user EXE 无 UI → **默认保留**；MSIX 卸载删 LocalState；
  便携手动。machine 的"删数据"仅覆盖执行卸载的用户。
- **诊断**：packaged 下 EventLog/真实 LocalAppData 日志不可见 → 写 LocalState + 设置页导出；
  `Diagnostics` 显示实际传入路径。

### 5.2 per-machine MSI

- `Package InstallScope="perMachine"`（WiX 3 原生）；`UpgradeCode` 沿用现有 `…ef1234567891`。
- 目录：`[ProgramFiles64Folder]\ClipBridge`；删除 `SetDirectory`（`Product.wxs:29`），让 Directory 表解析。
- 注册表：`Root="HKMU"`（machine→HKLM）；快捷方式 keypath 改 `HKMU`；ARP 显示名统一 `ClipBridge`。
- 桌面：`DesktopFolder` 在 per-machine 下即 All Users 桌面（无需 `CommonDesktopFolder`）。
- 自启：应用写 `HKCU\...\Run`（每用户各自启用）；卸载只清当前用户。
- 修现有 bug：
  - `RemoveFolderEx` 改 **Remember Property**（安装写 `HKLM\...\InstallPath`，卸载 RegistrySearch 读回）；
  - `REMOVEUSERDATA` 设 `Secure="yes"`；
  - `CleanupStartupEntryCA` 加 `NOT UPGRADINGPRODUCTCODE`（否则 major upgrade 删自启）；
  - `Product/@Version` 3 段 `$(var.ProductVersion)`，ARP `[ProductVersion]`；
  - heat 去 `.pdb`；恢复被抑制的 ICE03/30（先跑一遍看）。
- 升级：machine 专用 UpgradeCode + `MajorUpgrade`。
- 互斥：安装前检测 user 版标记（**专用键**，见 §5.7）；`LaunchCondition` 必须带 `NOT Installed`
  （卸载/修复走同一序列，否则死锁）；MSI 侧用标准 RegistrySearch，并把 AppSearch **同时排进 InstallUISequence 与 InstallExecuteSequence**（官方 AppSearch 文档），否则 /qn 下条件不生效。
- 测试矩阵：install / major-upgrade / repair / uninstall；与 user 版互斥；数据保留；`/qn`。

### 5.3 per-user EXE（Burn）

- **per-user MSI**（`PerUser=true`）：
  - `InstallScope="perUser"`；目录**显式** `[LocalAppDataFolder]\Programs\ClipBridge`（不依赖隐式重定向）；
  - 注册表 `HKMU`（user→HKCU）；无 WixUI/License；`ARPSYSTEMCOMPONENT=1`（**Burn 不会自动传**，必须显式）；
  - 新 `UpgradeCode`（与 machine 不同）。
- **bundle**（`PerUser=true`）：
  - Burn + WixStdBA；`MsiPackage Visible="no"`（默认）+ `bal:DisplayInternalUICondition="false"`；
  - bundle 自身 `UpgradeCode`（第三个 GUID）、`Version` 来自 Version.props；
  - 链中**不能有需要提权的包**（否则 bundle 变成 per-machine）；文档禁止"以管理员身份运行"；
  - 安装前 `util:ProductSearch` 查 machine UpgradeCode + `bal:Condition` 阻断（apply 前，无回滚）；
  - 卸载：bundle 显示 ARP；MSI 隐藏；**默认保留用户数据**（无 UI）。
- **静默**：Burn `-quiet/-passive`；winget 用内置 burn 开关（`Silent=/quiet` 等）。
- **签名顺序**：签 per-user MSI → 打 bundle → 签 EXE。
- 不做：一个 EXE 选 scope（Burn 做不到；D15）。

### 5.4 MSIX 与 Store

- **身份**：构建期注入 Partner Center 的 `Name`/`Publisher(CN=GUID)`/`PublisherDisplayName`/`DisplayName`/`Version`；
  开发/上架 manifest 分离；Publisher 必须等于重签证书主体。
- **能力**：`runFullTrust` 必需；`internetClient`/`privateNetworkClientServer` 对 mediumIL 惰性；
  `unvirtualizedResources` 不申请（备选）。
- **TargetDeviceFamily**：只留 `Windows.Desktop`；MinVersion 按实测；MaxVersionTested 提到当前 OS。
- **startupTask**：`uap5:Extension` + `uap5:StartupTask`；用 `ExtendedActivationKind.StartupTask` 识别自启；
  MSIX 下 **Registry 自启方式必须 NotSupported**（否则写 WindowsApps 路径/丢身份）。
- **通知**：保留 toast COM；self-contained 的 AppNotification 依赖 Singleton 包 → V5 验证，必要时补
  `PackageDependency`；重定向激活按 `Kind` 分发；移除终结器里的 `Unregister`。
- **Strings**：验证包内 `Strings\...\resw` 存在；否则改 PRI/ResourceLoader。
- **数据**：LocalState + 导入（§5.1）。
- **版本**：4 段、Major≠0、Revision=0；首发 `1.0.0.0`。
- **出包**：`-p:GenerateAppxPackageOnBuild=true -p:UapAppxPackageBuildMode=StoreUpload` → `.msixupload`；
  裁剪对构建布局生效并重生成 PRI。
- **本地调试**：开发者模式 + `Add-AppxPackage -Register <layout>\AppxManifest.xml`；不自签名分发。
- **Store 准入**：隐私政策、年龄分级、截图、`runFullTrust` 说明；WACK。

### 5.5 winget（两条 Scope）

```yaml
PackageIdentifier: Ryan416.ClipBridge
PackageVersion: <ver>
InstallerLocale: zh-CN
Platform: [Windows.Desktop]
InstallModes: [interactive, silent, silentWithProgress]
Installers:
  - Architecture: x64
    InstallerType: msi
    Scope: machine
    ElevationRequirement: elevatesSelf
    InstallerUrl: <release>/ClipBridge-<ver>-win-x64.msi
    InstallerSha256: <签名后>
    AppsAndFeaturesEntries:
      - DisplayName: ClipBridge
        Publisher: Ryan416
        UpgradeCode: "{……7891}"
  - Architecture: x64
    InstallerType: burn
    Scope: user
    ElevationRequirement: elevationProhibited
    InstallerUrl: <release>/ClipBridge-Setup-<ver>-win-x64-user.exe
    InstallerSha256: <签名后>
    InstallerSwitches: {Silent: /quiet, SilentWithProgress: /passive, Uninstall: /uninstall /quiet}
    AppsAndFeaturesEntries:
      - DisplayName: ClipBridge
        Publisher: Ryan416
```

- 多文件三件套；提交到 winget-pkgs（singleton 仅 verified developers，不能提交）。
- 两条 Scope 各自独立升级；hash 在签名后计算；资产上传后冻结。
- 显示名与 ARP 统一；不写死 `ProductCode`（每版变化），用 `AppsAndFeaturesEntries.UpgradeCode`。
- 便携 ZIP 不进 winget。

### 5.6 便携版（P1）

- ZIP 顶层 `ClipBridge-<ver>-win-x64/`，含 exe/依赖 + 空标记 `ClipBridge.portable` + `version.txt`/README；
  不含 `data\`；过滤 `.pdb/.xml`。
- 数据默认 `<exeDir>\data`；不可写回退 LocalAppData 并提示；可移动/网络盘拒绝或只读回退。
- 默认**不写** HKCU Run；用户手动开自启时检测冲突；与安装版共存用互斥量提示。
- 无卸载项；删除目录前先关自启；无签名 → SmartScreen 提示。

### 5.7 共存与单实例

- **安装期互斥**：
  - 专用标记：`HKLM\SOFTWARE\ClipBridge\Installer`（Scope=machine, ProductCode, Version）/
    `HKCU\Software\ClipBridge\Installer`（Scope=user, …）；**不复用**现有 `ClipBridgeShell\Installed`（machine 也写）。
  - bundle：`util:ProductSearch` 查 machine UpgradeCode + `bal:Condition`；
  - MSI：`util:RegistrySearch`（execute 序列）+ `LaunchCondition`（带 `NOT Installed`）；
  - 已知局限：machine 安装者看不到其他用户的 HKCU（best effort + 运行期兜底）。
- **运行期单实例**：`Global\ClipBridge-<UserSID>-<数据根哈希>` 互斥量 + 命名管道
  `\\.\pipe\ClipBridge-<SID>-<dataHash>`；冲突提示"另一渠道/版本正在运行"；
  `AppInstance` 仅用于同身份内的激活转发；`Activated` 按 Kind 分发，窗口未建好时排队。
- **自启**：MSIX StartupTask（uap5）；MSI/EXE HKCU Run；便携默认不注册。
- 卸载守卫：`LaunchCondition` 必须 `NOT Installed`，否则另一 scope 存在时无法卸载/修复。

### 5.8 版本与 CI

- `platforms/windows/ClipBridgeShell_CS/Version.props`（唯一真值）；`Directory.Build.props` **上移**到该目录并
  Import Version.props；app 级那份删除；wixproj 显式 Import 或 `-p:ProductVersion`。
- `scripts/version.ps1`：`MsiVersion = major.minor.patch`（x/y≤255、z≤65535，越界 fail）；
  `AppxVersion = max(1,major).minor.patch.0`；`BundleVersion = MsiVersion`；构建后回读断言。
- **release-please**：`manifest-file: release-please-manifest.json`；extra-file
  `{type: xml, path: platforms/windows/ClipBridgeShell_CS/Version.props, xpath: //Project/PropertyGroup/Version}`；
  workflow 配 **PAT/App token**（GITHUB_TOKEN 的 tag 不触发发布）；tag 方案
  `platforms/windows/ClipBridgeShell_CS-x.y.z`。
- **CI**：
  - `windows-installer` job（可复用 `workflow_call`）：setup .NET 10 + Rust + 固定版 WiX zip（校验 SHA256）
    → publish → stage（去 pdb/xml）→ heat 一次（`-ag`）→ 两个 MSI（`PerUser=false|true`，独立 obj/OutputName）
    → bundle → ZIP（P1）→ 断言 → 签名占位 → hash → upload（`compression-level: 0`）。
  - 断言：MSI `Product/@Version == MsiVersion`、MSI 无 `.pdb`、`core_ffi_windows.dll` 存在、
    bundle ProductVersion、user 变体 WordCount bit3=1 / machine=0、ZIP 标记。
  - `publish.yml` 整体重写（现为 .NET 9/restore 目录/tag 含 `/` 的桩）；Release 先 draft → 回读 hash → publish。
  - `ci.yml` 增加 `installer` filter 与 job（初期 `continue-on-error`）；release 不设 cancel-in-progress。
- 架构：本轮只 x64；arm64 后续（wixproj 对非 x64 显式报错，避免 harvest 错目录）。

## 6. 签名与信任

| 阶段 | MSI/EXE | MSIX | 成本 |
|---|---|---|---|
| 现在 | 未签名（SmartScreen） | 不发布（本地 Register 调试） | 0 |
| 近期 | SignPath Foundation（MIT 符合；复核依赖许可） | Store 个人开发者（免费、代签） | 0 |
| 备选 | Certum ~¥600/年；Azure Artifact Signing ~$10/月 | 同上 | 低 |

签名 ≠ 无 SmartScreen（信誉积累期数周）；MSIX 由 Store 重签后 hash 会变，**不进** SHA256SUMS。

## 7. 验证计划

| # | 验证 | 判定 | 阶段 |
|---|---|---|---|
| V1 | MSIX 写虚拟化探针（真实路径 vs LocalCache） | 真实 = PASS；仅私有 = 维持 D2″ | P0/P2 |
| V2 | MSI 真实 DB → MSIX 导入：WAL 落点/设备重复/混合 | 无混合 = PASS | P2 |
| V3 | 托盘（图标/菜单/Explorer 重启） | 正常 | P2 |
| V4 | `uap5` StartupTask 登录启动 + Kind 识别 | 正常 | P2 |
| V5 | Toast（运行/未运行/重定向 payload） | 三场景 | P2 |
| V6 | UDP/mDNS/防火墙（首启/升级后/双实例） | 升级后仍发现 | P2 |
| V7 | Strings/PRI | 包内 resw 存在 | P2 |
| V8 | 单实例（MSI+MSIX，双会话） | 单进程+提示 | P2 |
| V9 | Store 身份/版本草稿提交 | 掌握规则 | P2 |
| V10 | 签名安装形态重跑 V3–V7 | 一致 | P2 |
| V11 | **per-user MSI + bundle 机制 spike**：标准用户 `/qn` 安装 → 查 3 处 ARP（HKLM/HKCU/bundle）、`SystemComponent`、`winget list --scope user`、卸载残留、管理员运行 | 用户版 ARP 只在 HKCU/bundle；winget 识别 user | **P0** |
| V12 | 两 scope 互斥矩阵：machine↔user、同版本/跨版本、`/qn`、另一 scope 存在时卸载 | 阻断且可卸载；日志无跨删 | P1 |
| V13 | 导入中断/回滚/重装再导入 | 半成品清理、可重试 | P2 |

## 8. 阶段计划

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P0（机制 spike）** | 最小 per-user MSI + Burn bundle（V11）；`Version.props`+`version.ps1`；release-please 修复（PAT）；stage/heat 改造；`windows-installer` 骨架；修 `CleanupStartupEntryCA` | V11 结论；版本可复算；安装器能出两 MSI + bundle |
| **P1** | `DataRootResolver`；machine MSI 定稿（HKMU/Remember Property/Secure/CA）；user 变体；便携 ZIP；winget 清单+validate；Release 流水线；V12 | 三产物可重复构建；互斥矩阵通过 |
| **P2** | Store 账号 + MSIX 提交；SignPath；V1–V10/V13 | Store 通过、签名产物 |
| **P3** | 自研更新器（本轮不做） | 后续 ADR |

## 9. 风险

| # | 风险 | 处理 |
|---|---|---|
| R1 | per-user MSI ARP 落 HKLM 导致 winget 误判 | V11 + bundle 显式 MsiProperty ARPSYSTEMCOMPONENT=1 + bundle ARP |
| R2 | 两 UpgradeCode 的升级/降级跨 scope | V12 + 专用标记 + `NOT Installed` |
| R3 | /qn 下互斥条件失效（AppSearch 只在 UI） | MSI：AppSearch 排入 execute；bundle：util:ProductSearch；V11 |
| R4 | MSIX 导入/凭证/缓存 | 导入规格（§5.1）+ V2/V13 |
| R5 | StartupTask/Toast | V4/V5 |
| R6 | 双渠道 mDNS/TOFU 冲突 | 互斥量 + 文档"先退另一个" |
| R7 | winget hash/冻结 | 签名后 hash；draft→回读→publish |
| R8 | Store 版本规则 | version.ps1 + 首发 1.0.0.0 |
| R9 | 便携 WAL 可移动盘 | DriveType 检测 + 提示 |
| R10 | 安装器首次进 CI 就发布 | P0 骨架 + continue-on-error |
| R11 | 2052 单语言 | winget `InstallerLocale: zh-CN`；Store 补英文 |

## 10. 仍待确认

| 决策 | 建议 |
|---|---|
| D2 是否先跑 V1/V2 | 先跑（P0 顺手做 V1；V2 在 P2 前） |
| Store 首发版本 | `1.0.0.0` |
| P0 范围 | 仅 V11 机制 spike + 版本/CI 地基，不做完整安装器 |

## 11. 成熟实现参考

- PowerToys：`installer/PowerToysSetupVNext/{PowerToysInstallerVNext.wixproj, Product.wxs, PowerToys.wxs}` —
  一个 wixproj `PerUser` 两变体、两 UpgradeCode、bundle `ProductSearch` 两者、`DisplayInternalUICondition=false`。
- VS Code：`VSCodeUserSetup`/`VSCodeSetup` 两条 winget Scope；便携 ZIP + `data` 目录。
- Git / Python：单个 EXE、winget 同一 URL 两条 Scope（不用双用途 MSI）。
- Files：WinUI3 Store + 侧载；`unvirtualizedResources`（文件管理器正当理由）、`uap5:StartupTask`。
- 官方文档：Single Package Authoring（仅作对照，已否决）、MSIX flexible virtualization、Store 包版本/签名、
  WiX RemoveFolderEx、AppLifecycle instancing。

## 12. v4.1 验证记录（官方文档 / 源码）

| 假设 | 验证结果 | 依据 |
|---|---|---|
| Burn 自动传 ARPSYSTEMCOMPONENT | 不成立（引擎源码 0 命中）→ 显式 MsiProperty | WiX Burn msiengine.cpp / engine.cpp / apply.cpp |
| util:RegistrySearch 可用于 MSI | 不成立：父元素仅 Bundle/Fragment（Burn 侧） | WiX util RegistrySearch 文档 |
| AppSearch 只在 UI 序列 | 不成立：官方要求同时排进 UI 与 execute | MS AppSearch Action 文档 |
| PasswordVault 按包身份隔离 | 不成立：非 AppContainer 桌面应用可访问用户全部 locker | MS PasswordVault 文档 |
| export_backup 可在未登录时用 | 不成立：FFI 需要 core handle | crates/cb_core_ffi/src/exports.rs:709 |
| winget 支持 burn / ElevationRequirement | 成立（elevationRequired / elevationProhibited / elevatesSelf） | winget 1.10 schema |
| PowerToys 一套 wixproj 两变体 | 成立（PerUser + 两套 OutputPath） | PowerToys PowerToysInstallerVNext.wixproj |
| per-user 安装目录 | PowerToys 用 %LOCALAPPDATA%\PowerToys；CB 显式 %LOCALAPPDATA%\Programs\ClipBridge 合法 | PowerToys Common.wxi |
