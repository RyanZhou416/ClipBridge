# Windows 分发 / 安装 / 打包路线

> 本文回答三个问题：Windows 软件有哪些"装法"、每种装法把文件/注册表放到哪、以及 ClipBridge 应该怎么选。
> 参考：Microsoft 官方 [Choose a distribution path](https://learn.microsoft.com/windows/apps/package-and-deploy/choose-distribution-path)、
> [MSIINSTALLPERUSER](https://learn.microsoft.com/windows/win32/msi/msiinstallperuser)、
> [MSIX reset/repair（数据目录）](https://github.com/microsoftdocs/msix-docs/blob/main/msix-src/desktop/managing-your-msix-reset-and-repair.md)、
> [winget install](https://learn.microsoft.com/windows/package-manager/winget/install)。

## 0. 先把三层概念分开

容易混淆的是把"格式"、"渠道"和"安装范围"当成一回事。实际是正交的：

| 层 | 取值 | 说明 |
|---|---|---|
| **安装包格式** | MSI / MSIX(APPX) / EXE 引导器 / 便携 ZIP / 单文件 | 决定"怎么落地到系统" |
| **分发渠道** | 官网下载 / Microsoft Store / winget / 企业分发（Intune/SCCM）/ 便携 | 决定"用户从哪拿到、怎么更新" |
| **安装范围** | per-user（当前用户）/ per-machine（所有用户） | 决定"写到哪、要不要管理员" |

例：同一个 MSI 可以做成"双用途包"，安装时让用户选当前用户/所有用户；同一个应用也可以既有 MSI 又有 MSIX，分别走官网和 Store。

---

## 1. 各路线详解

### 1.1 MSI（Windows Installer）—— ClipBridge 当前路线

- **本质**：微软的安装数据库格式，由 `msiexec` 执行，安装/卸载/修复/升级都是事务化操作。
- **per-machine**：`ALLUSERS=1`。文件进 `C:\Program Files\<App>`，注册表进 `HKLM`，**需要管理员（UAC）**；卸载项在 `HKLM\...\Uninstall\{ProductCode}`。
- **per-user**：`ALLUSERS=2` + `MSIINSTALLPERUSER=1`（[官方文档](https://learn.microsoft.com/windows/win32/msi/msiinstallperuser)）。文件进 `%LOCALAPPDATA%\Programs\<App>`，注册表进 `HKCU`，**不需要管理员**。
- **双用途包（single package authoring）**：同一个 MSI 同时支持两种，安装向导里让用户选。WiX 3 用 `ALLUSERS=2` + `MSIINSTALLPERUSER` 属性；WiX 4/5 有 `<Package Scope="perUserOrMachine">`。
- **优点**：企业批量部署友好、可静默安装（`/qn`）、升级/回滚/修复成熟、卸载项自动注册。
- **缺点**：UI 是"上个时代"的体验；每个用户/机器只能装一个版本（同 UpgradeCode）；需要自建更新器或依赖外部渠道。
- **微软自己的 Win32/桌面软件**大多还是 MSI 或"EXE 引导器 + MSI"。

### 1.2 EXE 引导器（Bootstrapper）

- **本质**：一个 setup.exe，内部可以是自解压 + 自带安装引擎（Inno Setup / NSIS / Squirrel），也可以是"引导器 + 内嵌 MSI"（WiX Burn）。
- **per-user / per-machine**：
  - Inno Setup 用编译期 `PrivilegesRequired=lowest|admin|poweruser` 决定，**默认整包一个范围**；动态切换需要重启自身并提权（不优雅）。
  - NSIS 用 `RequestExecutionLevel`；Squirrel 默认 per-user（`%LOCALAPPDATA%\<App>`）。
- **优点**：完全自定义 UI/流程、可捆绑运行时、可做静默参数、可内置更新逻辑。
- **缺点**：没有 MSI 的标准化卸载/修复/企业部署语义；卸载项要自己写注册表；容易被 SmartScreen 拦（见 §3）。
- **WiX Burn**：微软官方的引导器，可以把"MSI + 前置依赖（如 .NET/VC++）"打包成一个 exe，仍保留 MSI 的安装语义。

### 1.3 MSIX / APPX —— 微软现在的"亲儿子"

- **本质**：带"包身份（Package Identity）"的安装格式，由 Windows 的 AppX 部署服务安装。
- **安装位置**：`C:\Program Files\WindowsApps\<PackageFullName>\`。**普通用户没有访问权限**（ACL 只给 SYSTEM/TrustedInstaller），这就是"跟着快捷方式找不到安装目录和 exe"的原因。
- **快捷方式怎么工作**：MSIX 应用的启动不靠普通 .lnk 指向 exe，而是：
  - 开始菜单项由系统根据包清单（AppxManifest）生成，激活时走 `ApplicationFrameHost`/AppX 激活；
  - 可选"App Execution Alias"（`%LOCALAPPDATA%\Microsoft\WindowsApps\foo.exe`），这是给命令行用的**重解析点**，不是真实 exe；
  - 所以资源管理器里搜不到、也进不去安装目录 —— 这是**设计如此**，不是 bug。
- **用户数据**：`%LOCALAPPDATA%\Packages\<PackageFamilyName>\`，其中
  - `LocalState`：持久本地数据（相当于我们的 `%LOCALAPPDATA%\ClipBridge`）；
  - `LocalCache`：非漫游缓存（VFS 重定向文件也在这）；
  - `RoamingState`：漫游数据；
  - `Settings`：虚拟化注册表（`settings.dat`）。
- **卸载语义**：卸载包会**连同 `Packages\<PFN>` 下的应用数据一起删除**（系统安装界面会提示"应用及其相关信息将被删除"）。这与 MSI"卸载默认保留用户数据"是相反的默认值，需要产品决策。
- **范围**：默认 per-user（装到当前用户）；企业可用 provisioning 让所有用户可用，但"per-machine MSIX"需要额外机制（`Add-AppxProvisionedPackage`/Provisioning），不是普通安装器能直接做到的。
- **签名**：**必须签名**。自签名证书需要用户先导入"受信任的根证书颁发机构"才能装；正式分发要用 CA 证书或由 Store 代签。
- **优点**：干净安装/卸载、无 UAC（per-user）、自动更新（App Installer/Store）、文件与注册表虚拟化、WinUI 3 原生支持（WinUI 3 模板默认就是 packaged）。
- **缺点**：安装目录不可见、部分"绿色版"脚本/插件生态不兼容、必须在包清单里声明能力、签名成本、数据随时被系统清理（`LocalCache` 可能被清理）。

### 1.4 Microsoft Store

- **本质**：分发渠道，**不是格式**。Store 目前主推 MSIX；也支持提交 **MSI/EXE 安装包**（"MSI/EXE installer path"），但有硬性条件：
  - 证书必须链到 **Microsoft Trusted Root Program 里的 CA**，**自签名不接受**；
  - 这条路径**没有 Store 管理的自动更新**（更新要自己发新安装包）。
- **安装效果**：应用仍以 MSIX 形式落在 `WindowsApps`（Store 代签），或按 MSI/EXE 的规则安装。
- **优点**：最大触达、自动更新（MSIX 路径）、信任背书、无需自己维护下载/签名/更新。
- **缺点**：需要开发者账号与认证；审核周期；不能随意改协议/引入需要管理员的功能；抽成/商务条款；对"托盘常驻 + 局域网组播"这类行为可能被认证挑战。
- **适合**：面向普通消费者的最终形态。

### 1.5 winget（Windows 包管理器）

- **本质**：**包目录 + 安装调度器**，不是安装格式。它读 YAML 清单（manifest），按 `InstallerType` 调用底层安装器：
  `msi / msix / exe / inno / nullsoft / portable / zip / msstore` 等。
- **安装位置**：由底层安装器决定（MSI→Program Files 或 LocalAppData；EXE→其安装器决定；`portable`→`%LOCALAPPDATA%\Microsoft\WinGet\Packages\<id>` 并生成 `WindowsApps` 别名）。
- **范围**：`winget install --scope user|machine`（[文档](https://learn.microsoft.com/windows/package-manager/winget/install)）；实际能否生效取决于安装器类型（MSI 支持，MSIX 的 machine scope 需要额外转换）。
- **更新**：`winget upgrade`（按清单里的新版本调用安装器）。
- **上架**：向 [winget-pkgs](https://github.com/microsoft/winget-pkgs) 提 PR，社区/自动验证后合并；企业可自建私有源。
- **优点**：开发者零成本获得"一行命令安装/升级"；用户不需要找你官网；和 Store 并存。
- **缺点**：清单要跟版本维护；安装体验仍是底层安装器的（UAC/UI 取决于它）；不是所有用户都知道 winget。

### 1.6 便携 / 免安装

- **本质**：ZIP 解压即用，或单文件 exe（自解压/自包含）。
- **位置**：用户放哪就在哪；数据可以放 exe 同目录（真便携）或 `%LOCALAPPDATA%`（公共）。
- **优点**：零安装、零 UAC、零注册表、可放 U 盘；对"绿色版"用户友好。
- **缺点**：没有卸载项/开始菜单/自动更新；多用户共享一台机器时数据归属混乱；单文件自解压会拖慢启动（每次解压到临时目录）。

---

## 2. 一图看清"装完后东西在哪"

| 路线 | 程序文件 | 用户数据 | 注册表 | 卸载项 | 需要管理员 |
|---|---|---|---|---|---|
| MSI per-machine | `C:\Program Files\<App>` | 应用自己决定（通常 `%LOCALAPPDATA%`） | `HKLM` + `HKCU` | 系统"应用和功能" | 是 |
| MSI per-user | `%LOCALAPPDATA%\Programs\<App>` | 同上 | `HKCU` | 系统"应用和功能"（当前用户） | 否 |
| EXE（Inno/NSIS per-user） | `%LOCALAPPDATA%\<App>` | 同上 | `HKCU` | 自己写（或安装器写） | 否 |
| EXE（per-machine） | `C:\Program Files\<App>` | 同上 | `HKLM` | 安装器写 | 是 |
| **MSIX / Store** | `C:\Program Files\WindowsApps\<包>`（不可访问） | `%LOCALAPPDATA%\Packages\<PFN>\{LocalState,LocalCache,RoamingState,Settings}` | 虚拟化到 `Settings\settings.dat` | 系统"应用和功能" | 否（per-user） |
| winget | 由底层安装器决定 | 同上 | 同上 | 同上 | 取决于安装器 |
| 便携 | 用户指定目录 | exe 同目录或 `%LOCALAPPDATA%` | 通常无 | 无 | 否 |

> "跟着快捷方式找不到 exe"只发生在 **MSIX/Store**：exe 在 `WindowsApps` 受 ACL 保护，快捷方式是包激活入口而非普通 .lnk。传统 MSI/EXE 安装的软件仍能在 `Program Files` 里找到真实 exe。

---

## 3. 签名、SmartScreen 与信任

| 场景 | 要求 |
|---|---|
| 未签名的 EXE/MSI 从网上下载 | SmartScreen 弹"未知发布者"，需要用户点"仍要运行"；下载量低时更严格 |
| Authenticode 签名（OV/EV 证书） | 减少 SmartScreen 警告；EV 证书能更快建立信誉 |
| MSIX 自签名 | 用户必须先导入证书到"受信任的根"；不适合公开分发 |
| MSIX + CA 证书 | 可直接安装（仍可能弹 UAC 以外的确认） |
| Store 提交 | Store 代签；MSI/EXE 路径要求 CA 链证书，自签名不接受 |
| 企业内分发 | 可用 Intune/SCCM 推 MSI/MSIX，签名策略由企业控制 |

---

## 4. 更新机制

| 路线 | 更新方式 |
|---|---|
| MSI | 自己做"检查更新→下载新 MSI→静默升级"（MajorUpgrade），或走 winget |
| EXE 引导器 | 安装器自带 updater（Squirrel/自研） |
| MSIX | App Installer 自动更新（有网络时）；Store 自动更新 |
| Store | 系统自动更新 |
| winget | `winget upgrade`（手动或计划任务） |
| 便携 | 手动替换 |

---

## 5. ClipBridge 的现状与建议

### 现状

- **格式**：WiX 3 MSI，**per-machine**（`Program Files`，需要管理员）。
- **内容**：unpackaged（`WindowsPackageType=None`），自包含 .NET + WindowsAppSDK，R2R；发布瘦身后 **241 MB**，MSI **87.9 MB**（CAB 压缩）。
- **渠道**：目前只有官网/手动；无 Store、无 winget、无自动更新。
- **卸载**：删程序目录/快捷方式/注册表；用户数据保留（新增了"是否删除用户数据"对话框，默认保留）。
- **签名**：无（SmartScreen 会提示）。

### 推荐路线（按优先级）

1. **短期：MSI 改"双用途"**（per-user 默认、per-machine 可选）
   - 对托盘类应用，per-user 安装进 `%LOCALAPPDATA%\Programs\ClipBridge`，**免 UAC**，更符合"个人工具"定位；
   - 企业用户仍可选 per-machine；
   - 已有 MSI 基础设施，改动集中在 WiX 的 `ALLUSERS`/`MSIINSTALLPERUSER` 与目录变量（`ProgramFiles64Folder` → 条件目录）。
2. **短期：发布便携 ZIP + winget 清单**
   - ZIP 满足"绿色版"用户；
   - winget 清单（`InstallerType: msi` 或 `zip`/`portable`）让 `winget install ClipBridge` 可用，零成本获得发现与升级；
   - 需要为每个版本更新 manifest（可脚本化）。
3. **中期：MSIX + Store（可选）**
   - WinUI 3 本来就适合 packaged；
   - 但需要**签名证书**（CA 链）或走 Store 代签；
   - 数据要从 `%LOCALAPPDATA%\ClipBridge` 迁移到 `Packages\<PFN>\LocalState`，且卸载会删数据 —— 需要产品决策（是否接受"卸载即清空"）；
   - 局域网组播/UDP 监听在 MSIX 下需要声明 `internetClient` 等能力，需验证防火墙/组播行为。
4. **自动更新**
   - 有证书 → MSIX 自带；
   - 没有 → 自研"检查版本 + 下载 MSI + 静默升级"，或依赖 winget upgrade。

### 我们的明确取舍

- **不做**单文件自解压（启动慢、每次解压）。
- **不把 MSIX 当默认**：没有签名证书前，自签名 MSIX 的用户体验比"未签名 MSI"更差（要手动导入证书）。
- **保持数据目录 `%LOCALAPPDATA%\ClipBridge` 不变**，直到决定上 MSIX 时再做一次性迁移。

---

## 6. 关键结论（给非 Windows 架构背景的读者）

1. **"安装"不是一种东西**：MSI/MSIX/EXE 是三种不同的落地机制，winget/Store 是渠道。
2. **找得到 exe 与否**由格式决定：MSIX/Store 的应用 exe 在受保护的 `WindowsApps`，快捷方式是激活器；MSI/EXE 装在 `Program Files` 或 `LocalAppData`，能找到。
3. **per-user vs per-machine** 是安装包属性（MSI 的 `ALLUSERS`/`MSIINSTALLPERUSER`、Inno 的 `PrivilegesRequired`、MSIX 的 scope），决定装到哪、要不要管理员。
4. **MSIX 卸载会删应用数据**，MSI 默认保留 —— 这是两套生态的默认值差异。
5. **签名**是分发成本的主要来源：SmartScreen 与 Store 都围绕它。
