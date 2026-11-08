# Windows 分发的成熟实践（案例研究）

- 目的：用真实项目 + 官方文档校正 CB 的设计，而不是凭记忆拍板。
- 方法：Microsoft Learn 原文 + WiX 官方文档 + winget-pkgs 里的**真实清单** + 开源项目 manifest。
- 日期：2026-11-08
- 关联：[ADR-004](adr/004-windows-distribution-channels.md)

## 1. 案例总表（真实项目怎么做）

| 项目 | 渠道 | 安装器形态 | 关键证据 |
|---|---|---|---|
| **VS Code** | 官网 + winget + 便携 | **user/machine 两个独立安装器**（Inno）；便携 ZIP 用 `data` 目录 | winget 清单里 `VSCodeUserSetup` 与 `VSCodeSetup` 两条 Scope；[portable 文档](https://code.visualstudio.com/docs/setup/portable) |
| **PowerToys** | 官网 + winget | **user/machine 两个独立 Burn 安装器**；WiX 5 构建 | winget 清单两条：`PowerToysUserSetup` / `PowerToysSetup`；[安装器文档](https://microsoft.github.io/PowerToys/core/installer/) 明确"separate builds for machine-wide and user-scope" |
| **Git for Windows** | 官网 + winget | 一个 Inno EXE，**同一 URL 在 winget 里列两次**（user/machine），`ElevationRequirement: elevatesSelf` | [Git.Git 清单](https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/g/Git/Git/2.56.0.4/Git.Git.installer.yaml) |
| **Python** | 官网 + winget | 一个 Burn EXE，两条 Scope，靠 `InstallAllUsers=0/1` 切换；machine 条目标 `elevatesSelf` | [Python 3.14 清单](https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/p/Python/Python/3/14/3.14.0/Python.Python.3.14.installer.yaml) |
| **7-Zip** | 官网 + winget | **MSI 只做 per-machine**；同时提供 EXE | [7zip 清单](https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/7/7zip/7zip/26.06/7zip.7zip.installer.yaml) `Scope: machine` |
| **Files（WinUI3，Store + 侧载）** | Store + 侧载 | MSIX；**声明 `unvirtualizedResources` 并关闭文件系统写虚拟化**；startupTask 用 `uap5:Extension`；Store 版与侧载版 capability 不同 | [Package.appxmanifest](https://raw.githubusercontent.com/files-community/Files/main/src/Files.App/Package.appxmanifest) |
| **ForceAutoHDR（小项目）** | 官网/winget | **per-user MSI**（`Scope="perUser"`，装进 `%LOCALAPPDATA%\Programs`，无提权）；作者记录了"ARR 仍写 HKLM，winget 会当成 machine"的坑 | [README](https://raw.githubusercontent.com/Phoenix-/ForceAutoHDR.Net/main/packaging/msi/README.md) |

**结论先行**：主流项目**没有**用"单个双用途 MSI + 安装时选 scope"；它们要么**分两个安装器**（VS Code、PowerToys），要么**一个 EXE 自己按参数/提权选 scope**（Git、Python）。MSI 通常是 **per-machine**（7-Zip），per-user 是单独选择（ForceAutoHDR）。

## 2. 双 scope 的三种成熟模式

### 模式 A：两个独立安装器（VS Code / PowerToys）
- 构建两次：per-user 与 per-machine 各一个产物。
- winget 清单两条：`Scope: user` / `Scope: machine`，URL 不同。
- 优点：每个产物的 scope 在构建期固定，无运行时分支；安装/升级/卸载语义简单。
- 缺点：产物数量翻倍；两个安装器的 ARP 条目不同，用户混装会冲突（VS Code 也要求先卸载另一种）。

### 模式 B：一个 EXE，运行期选 scope（Git / Python）
- 安装器本身（Inno/NSIS/Burn）在安装时决定 per-user/per-machine；winget 同一 URL 列两条 scope。
- 需要安装器支持（Inno 的 `PrivilegesRequired` + 自定义逻辑；Python 用 `InstallAllUsers` 参数）。
- **WiX Burn + WixStdBA 做不到运行时切 scope**（构建期按 MSI 摘要 bit 8 固定；见 ADR §5.3 评审结论），所以 CB 若选模式 B 需要换安装器技术或迁 WiX 5。

### 模式 C：单个双用途 MSI（微软官方模式，但少见）
- 官方 [Single Package Authoring](https://learn.microsoft.com/en-us/windows/win32/msi/single-package-authoring)：
  `ALLUSERS=2` + `MSIINSTALLPERUSER=1` 默认 per-user；per-user 时 `ProgramFiles64Folder` **自动重定向**到 `%LOCALAPPDATA%\Programs`；registry `Root=-1` 自动 HKCU/HKLM；必须过 **ICE105**；安装后上下文固定（升级/修复/卸载沿用）。
- WiX 支持情况：v3 只有 `perMachine|perUser`；v4+ 有 `perUserOrMachine`；**v7 新增 `perMachineOrUser`**（默认 machine）——[WiX 文档](https://docs.firegiant.com/wix/schema/wxs/packagescopetype/)。
- 现实：本次查到的成熟项目里**没有**使用它；它的复杂度（scope 对话框、条件组件、跨 scope 升级）在评审中也被指出。

## 3. MSIX / Store 的官方事实

- **签名**：MSIX/AppX 提交 Store **不需要** CA 证书，Store 会重签；但 **MSI/EXE 提交必须自己 Authenticode 签名**，Store 不重签（[官方文档](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/package-version-numbering?pivots=store-installer-msix)）。
- **版本**：四段 0–65535；**第一段不能为 0**；**第四段（Revision）留给 Store，构建时必须为 0**；已发布版本单调递增。
- **数据位置（关键）**：MSIX 默认对 AppData **写虚拟化**——新建文件/目录进 per-app 私有位置、对其他进程不可见、卸载时删除；读会先私有后回退真实 AppData（[官方表](https://learn.microsoft.com/en-us/windows/msix/desktop/flexible-virtualization)）。
  要写真实 AppData：`rescap:unvirtualizedResources` + `desktop6:FileSystemWriteVirtualization=disabled`（Win10 1903+），或 Win11 的 `virtualization:ExcludedDirectories`（仅 `%USERPROFILE%\AppData` 内）。都是**受限能力**。
- **现实案例**：Files（WinUI3、MIT、Store 上架）**确实用了** `unvirtualizedResources` + 关闭文件系统写虚拟化，理由是文件管理器需要真实 AppData/回收站状态。说明 Store 可以批准，但需要正当理由；且它维护了 Store/侧载两套 capability。
- **startupTask**：WinUI3 应用应使用 **`uap5:Extension Category="windows.startupTask"` + `uap5:StartupTask`**；带 `Executable/EntryPoint` 的 `desktop:Extension` 写法在 WinUI3 下会出现"登录启动了但窗口不出现"（[Q&A](https://learn.microsoft.com/en-ca/answers/questions/5971935/how-to-set-a-auto-startup-for-an-winui-app)）。Files 用的就是 uap5 写法。
- **单实例**：Windows App SDK 的 `AppInstance` 列表按**应用版本/用户**分开维护；unpackaged 应用做单实例的官方建议是**命名互斥量**（[applifecycle 文档](https://learn.microsoft.com/en-us/windows/apps/windows-app-sdk/applifecycle/applifecycle-instancing)）。

## 4. 便携版：VS Code 的约定

- 便携模式**只支持 ZIP**，不支持从安装版改配置；
- 数据放 exe 旁的 `data` 目录（VS Code 文档明确 "all data ... live near itself"）；
- 便携版与安装版互斥（不要把 portable 标记带进安装目录）。
- CB 的 `ClipBridge.portable` 标记 + `<exeDir>\data` 与这一约定一致；差别是 VS Code 用固定目录名 `data`，我们用标记文件（更显式，但需要在启动时探测可写性）。

## 5. 与 CB 当前设计（ADR-004 v2）的差异与建议

| 主题 | ADR v2 现状 | 成熟实践 | 建议 |
|---|---|---|---|
| **双 scope** | 单个双用途 MSI（per-user 默认 + 自定义 scope 对话框） | 分两个安装器（VS Code/PowerToys）或单 EXE 切 scope（Git/Python）；双用途 MSI 官方但少见 | **改为两个安装器**：per-user EXE（Burn）+ per-machine MSI（或两个 MSI）；如坚持单 MSI 双用途，需接受复杂度与 ICE105/跨 scope 升级风险 |
| **EXE** | 固定 per-user 的 Burn | PowerToys 两个 Burn bundle（user/machine） | 两个 Burn bundle，或"user EXE + machine MSI" |
| **winget** | 先 machine-only，双条目可选 | VS Code/PowerToys 两条不同 URL；Git/Python 同一 URL 两条 | 跟随最终安装器形态：两个产物 → 两条 URL；单 EXE → 同一 URL 两条 |
| **MSIX 数据** | LocalState + 首启导入 | 默认虚拟化；Files 用 `unvirtualizedResources` 共享真实 AppData | 默认 LocalState+导入；若"跨渠道共享"是硬需求，再按 Files 模式申请受限能力（Store 需说明理由） |
| **startupTask** | 现有 `desktop:Extension` + Executable/EntryPoint | Files：`uap5:Extension` + `uap5:StartupTask` | 改为 uap5 写法；并验证 `ExtendedActivationKind.StartupTask` |
| **单实例** | AppInstance + 计划加互斥量 | 官方说 AppInstance 按版本/用户分列表；unpackaged 用命名互斥量 | 保留互斥量方案；激活转发按 Kind 分发 |
| **版本** | Version.props 派生 | MSI 3 段；MSIX 4 段、Major≠0、Revision=0 | 维持，补 Store 规则断言 |
| **签名** | Store 代签 + SignPath | 官方确认 | 维持；MSIX 不要自签分发 |
| **Store 包** | `.msixupload` | 官方推荐 | 维持；身份用 Partner Center 注入 |

## 6. 建议的新决策（待用户确认）

- **D3′（新）**：安装器形态改为"**per-user EXE + per-machine MSI**"（PowerToys/VS Code 模式），而不是单个双用途 MSI。
  - 好处：每个产物 scope 固定，安装/升级/卸载简单；与 winget 两条 Scope 自然对齐；不需要 scope 对话框与 ICE105。
  - 代价：两个安装器；从一种 scope 换另一种仍需先卸载。
- **D10′（新）**：EXE 只做 per-user（Burn）；per-machine 用 MSI。若将来要"一个 EXE 切 scope"，只能换安装器技术（Inno/NSIS）或迁 WiX 5。
- **D2 维持**：LocalState + 导入；`unvirtualizedResources` 只作为"跨渠道共享"变成硬需求时的备选（Files 先例，但需 Store 说明理由）。

> 说明：以上是研究结论，不是实现承诺；确认后才会更新 ADR 与代码。
