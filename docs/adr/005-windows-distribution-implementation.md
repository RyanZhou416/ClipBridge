# ADR-005：Windows 分发渠道实现记录（v4.2）

- 状态：**P0/P1 已实现并验收；MSIX 构建、旧数据导入、StoreUpload 已实现并本地验收；Partner Center 提交待做**
- 日期：2026-11-15
- 关系：本文是 [ADR-004](004-windows-distribution-channels.md)（v4.1 设计稿）的实现记录与差异说明。004 的争议答案、评审历史继续有效；**具体行为以本文为准**，004 中与本文冲突处（HKMU、ProductSearch、默认保留数据、MSIX 状态等）按本文执行。
- 范围：Windows 壳的 MSI / Burn EXE / 便携 ZIP / winget 已落地；MSIX 构建/旧数据导入/StoreUpload/本地验收已落地，Partner Center 提交待做。

## 1. 与 v4.1 的差异（实现修正）

| 项 | v4.1 设计稿 | 实际实现 | 原因 |
|---|---|---|---|
| 注册表根 | MSI 用 `HKMU` 自动切换 | **每变体显式 `RegRoot`**：machine=`HKLM`，user=`HKCU` | 避免 `HKMU` 在双用途/ICE 下的歧义，便于审阅 |
| machine/user 互斥检测 | bundle 用 `util:ProductSearch` 同时探测两个 UpgradeCode | **bundle 用 `util:RegistrySearch` 读 HKLM 的 `Scope=machine` 标记** + `bal:Condition`；MSI 用 AppSearch 读对侧标记 + `Condition` | 标记语义明确；安装/卸载/修复都走同一条件 |
| user 变体上下文 | 仅 `InstallScope="perUser"` | 额外写 `ALLUSERS=2` + `MSIINSTALLPERUSER=1`（双用途授权） | 卸载/修复也稳定落在 per-user 上下文 |
| 卸载数据默认 | machine 保留（对话框可选删）；user 默认保留 | **默认删除**（`REMOVEUSERDATA=1`）；machine 向导默认勾选、可取消；user EXE 用 `RemoveUserData=0` 显式保留 | 干净优先；无 UI 的渠道也能删干净 |
| 组件 GUID | `-ag`，两变体共用 | 保持 `-ag` 一次 heat；**手工组件 GUID 两变体共用** | 升级稳定；早期测试残影会导致 "another client exists"，干净机器无此问题 |
| winget Uninstall 开关 | 清单写 `InstallerSwitches.Uninstall` | **删除该字段**：winget 1.6 schema 不存在；Burn 默认即 `/uninstall /quiet` | 消除 validate warning，validate exit 0 |
| winget burn 条目 | 写 `AppsAndFeaturesEntries.UpgradeCode` | burn 条目只写 `DisplayName`/`Publisher`；machine MSI 才写 UpgradeCode | bundle ARP 不是 MSI upgrade 语义 |
| release tag | `platforms/windows/ClipBridgeShell_CS-x.y.z` | release-please `component: "clipbridge-windows"` → tag **`clipbridge-windows-<ver>`** | 无 `/`，可直接做资产下载 URL |
| 便携数据 | 仅 core 数据 | **core + logs/tls/Cache/Strings/背景图 + `settings.json` + DPAPI `credentials.bin`** | 用户要求"用户信息在文件夹内" |
| 便携自启 | 默认不写 Run | **保持现有 HKCU Run**（用户已确认接受） | 自启必须有一个外部指针；写入仅为 exe 路径 |
| MSIX | 设计完成即可提交 | **构建 + 旧数据导入 + StoreUpload 已实现**（unsigned/自签名，开发者模式安装验收通过）；版本需 ≥1.0.0；Partner Center 身份/提交未做 | 先完成四条非 MSIX 渠道 |

## 2. 渠道与产物（实际）

| 渠道 | 产物 | 安装位置 | 数据 | 卸载数据默认 | 签名 |
|---|---|---|---|---|---|
| machine MSI | `ClipBridge-<ver>-win-x64-machine.msi` | `C:\Program Files\ClipBridge` | `%LOCALAPPDATA%\ClipBridge` | **删除**（向导可取消；静默 `REMOVEUSERDATA=0` 保留） | 待 SignPath |
| user EXE | `ClipBridge-<ver>-win-x64-user.exe`（内含 per-user MSI） | `%LOCALAPPDATA%\Programs\ClipBridge` | 同上 | **删除**；`RemoveUserData=0` 保留 | 待 SignPath |
| 便携 ZIP | `ClipBridge-<ver>-win-x64-portable.zip` | 用户选择 | `<exeDir>\data`（不可写回退 `%LOCALAPPDATA%`） | 删目录即全删；保留 `data\` 即保留 | 无 |
| winget | `dist/winget/*.yaml`（三件套） | 由底层安装器决定 | 同底层 | **删除**（winget 无法传 keep） | hash |
| MSIX | `build-windows-msix.ps1`（Sideload/StoreUpload） | Store 管理 | `LocalState` + 首启导入（已实现） | 系统删除 LocalState | Store 代签 |

GUID：

- machine UpgradeCode：`{A1B2C3D4-E5F6-7890-ABCD-EF1234567891}`
- user UpgradeCode：`{0F6B9C1E-2D3A-4B5C-9E7F-8A1B2C3D4E5F}`
- bundle UpgradeCode：`{C3D4E5F6-A7B8-9012-CDEF-123456789012}`

## 3. 构建与调用

```powershell
# 全量（machine MSI + user MSI/EXE + 便携 ZIP + 断言）
pwsh scripts/build-windows-all.ps1 -WixInstallPath C:\tools\wix314

# 单渠道
pwsh scripts/build-windows-machine-msi.ps1 -WixInstallPath C:\tools\wix314
pwsh scripts/build-windows-user-exe.ps1     -WixInstallPath C:\tools\wix314
pwsh scripts/build-windows-portable-zip.ps1
pwsh scripts/build-windows-winget.ps1       # 需 dist 已有 machine MSI + user EXE
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 [-SelfSign]  # MSIX 需 >=1.0.0
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 -StoreUpload  # .msixupload
```

流程：`dotnet publish`（Release、win-x64、self-contained、R2R、裁剪）→ stage（去 pdb/xml）→ heat `-ag` 一次 → machine MSI；user MSI 复用 heat（SkipPublish/SkipHeat）→ Burn bundle；portable ZIP；断言。

版本：`platforms/windows/ClipBridgeShell_CS/Version.props` 是唯一真值；`scripts/version.ps1` 派生 `MsiVersion`/`BundleVersion`，只有 major≥1 才派生 `AppxVersion`。当前 `0.1.0`：MSI/Bundle/便携可发，MSIX 不可发；Store 首发用 `1.0.0.0`。

CI：`.github/workflows/ci.yml` 的 `windows-installer` job（windows-latest、.NET 10、Rust target、固定 WiX 3.14 zip + SHA256）→ 构建 → 生成并 validate winget 清单 → 上传 `dist/**` 与 `dist/winget/**`。

## 4. 关键实现点

### 4.1 安装器

- machine MSI：`Program Files`、HKLM、公共开始菜单/桌面、`ALLUSERS=1`；`RemoveUserDataDlg` 默认勾选删除。
- user MSI：`%LOCALAPPDATA%\Programs`、HKCU、`ALLUSERS=2`+`MSIINSTALLPERUSER=1`、`ARPSYSTEMCOMPONENT=1`；user 变体抑制 `ICE38/ICE64/ICE91`。
- 两变体共用 `Product.wxs`，显式 `InstallScope`/`RegRoot`/`UpgradeCode`；`RemoveFolderEx` 用 Remember Property；AppSearch 同时进 UI/execute 序列；`Condition = Installed OR OTHER_SCOPE_MARKER <> "<other>"`。
- 卸载 CA：`CleanupStartupEntryCA`（清 HKCU Run + 旧计划任务）、`RemoveUserDataCA`（`REMOVEUSERDATA=1` 时删 `%LOCALAPPDATA%\ClipBridge`）、user 变体 `CleanupInstallTraceCA`（清 HKCU 键与安装目录，兜底服务上下文清不到用户 HKCU 的情况）。
- Burn bundle：`util:RegistrySearch` 读 HKLM 机器版标记 → `bal:Condition` 阻断；`MsiPackage Visible=no`；`Variable RemoveUserData=1 bal:Overridable` + `MsiProperty REMOVEUSERDATA`；`ARPSYSTEMCOMPONENT` 显式传入。

### 4.2 数据根与便携

- `CoreConfigBuilder.ResolveDataRoot/ResolveDataRootInfo` 是唯一判定点；按进程缓存。
- 便携标记 `<exeDir>\ClipBridge.portable`：用 `FileOptions.DeleteOnClose` 真写探测；成功 → `<exeDir>\data`；失败 → 删除本次空目录、回退 `%LOCALAPPDATA%\ClipBridge`、写 `portable-fallback.log`、启动时弹「便携模式已回退」。
- 默认（无 marker）：`%LOCALAPPDATA%\ClipBridge`；设置仍写 `%LOCALAPPDATA%\ClipBridgeShell_CS\ApplicationData\LocalSettings.json`。
- 便携：`settings.json`、`credentials.bin` 都在 `data\`；凭据用 DPAPI CurrentUser + 固定 entropy，原子写，损坏 fail-closed 且保留文件；默认模式继续 PasswordVault。
- 自启：MSI/EXE/便携都写 `HKCU\...\Run`（用户已确认接受唯一外部指针）。
- MSIX：构建渠道 + 旧数据导入已实现（manifest 注入、AI/ML/pdb 裁剪、自签名、VACUUM INTO 快照、tls/settings 复制、sentinel/回滚、一次性提示）；`uap5:StartupTask` 已写入 manifest。

## 5. 卸载与数据保留（实测）

| 渠道 | 程序/注册表 | 用户数据 |
|---|---|---|
| machine MSI（提权） | 全清：Program Files、HKLM、公共开始菜单/桌面、ARP | 默认删；向导取消勾选或 `REMOVEUSERDATA=0` 保留 |
| user EXE | 全清：安装目录、开始菜单/桌面、HKCU 键、HKCU Run、HKCU ARP、bundle 缓存 | 默认删；`RemoveUserData=0` 保留 |
| 便携 ZIP | 删目录即全删 | 保留 `data\` 即保留；开过自启先关 Run |
| winget | 由底层安装器清理 | **删**（无法传 keep） |
| MSIX（P2） | Store 清理 | **删** LocalState（系统语义） |

限制：machine MSI 的删数据只覆盖执行卸载的当前用户，不枚举其他用户；user EXE 无 UI 复选框（WixStdBA 限制），只能用命令行保留；便携自启会留 Run 指针。

## 6. 验证记录（2026-11-15）

| 项 | 结果 |
|---|---|
| 壳构建 + MSTest | Release x64 构建 0 warning；**62/62** 通过（含 23 项凭据/便携测试） |
| machine MSI 管理员安装 | exit 0；Program Files/HKLM/公共开始菜单/公共桌面/ARP 正常 |
| machine MSI 管理员卸载 | exit 0；上述位置 + `%LOCALAPPDATA%\ClipBridge` 全部清除（默认删数据） |
| user EXE 安装/卸载 | exit 0；目录/快捷方式/HKCU/Run/ARP 全清，数据默认删 |
| 便携 ZIP | 便携写 `data\`；无 marker 回退 LocalAppData；不可写回退 + 对话框；E2E 20/20 |
| DPAPI 凭据 | 往返、损坏 fail-closed、无明文、写失败抛异常；便携设置落 `data\settings.json` |
| 默认删数据 | 默认卸载数据目录消失；`RemoveUserData=0` 保留 |
| winget | 三件套生成；`winget validate --manifest dist\winget` **exit 0** |
| MSIX 构建（1.0.0） | 109.4 MB → 89.9 MB；AI/ML 23 文件/42.5 MB、pdb 0、语言仅 en-us/zh-CN/zh-TW；manifest/PRI/自签名断言通过 |
| MSIX 开发者模式安装/启动/卸载 | `Add-AppxPackage -Register` exit 0；进程存活、窗口出现；LocalState 生成 core.db/tls/logs/Strings；卸载后包与 `%LOCALAPPDATA%\Packages\<PFN>` 删除 |
| MSIX 数据导入 | 73/73；源 core items=2/history=2、logs=804，目标同；tls 三文件 sha256 一致；设置含 Core_DeviceId；源目录导入前后只读不变；卸载删 LocalState，重装再次导入 |
| MSIX StoreUpload | exit 0；91.2 MB `.msixupload`，内部单 `.msix` 398 文件；裁剪 -19.5 MB；upload/内部包可 unpack；版本/身份/无 AI-ML-pdb/e_sqlite3/resources 断言通过；与 Sideload payload 逐文件一致（除 BlockMap） |
| MSIX 签名包安装 | **未完成**：非管理员无法把自签名根加入 `LocalMachine\TrustedPeople`，`Add-AppxPackage` 报 0x800B0109；管理员导入证书后应可安装 |
| 构建断言 | `REMOVEUSERDATA=1`、ALLUSERS、WordCount bit3、无 pdb、bundle 版本等全部通过 |

开发机曾有两个早期 phantom 产品阻塞手工组件卸载（"another client exists"）；清掉其 Installer 组件引用后上述卸载全清。干净机器/VM 不会出现。

## 7. MSIX 剩余待办（P2）

已实现：`scripts/build-windows-msix.ps1`（版本守卫、manifest 注入、AI/ML/pdb/语言裁剪、Sideload + StoreUpload、自签名、断言）；`Package.appxmanifest` 的 `uap5:StartupTask`、Desktop-only、DisplayName=ClipBridge；开发者模式安装/启动/卸载验收；旧数据导入（VACUUM INTO 快照 + tls/settings + sentinel/回滚 + 一次性提示）。

剩余：

1. **Store 身份**：Partner Center 的 Identity Name / Publisher CN / PublisherDisplayName；`-IdentityName`/`-Publisher`/`-PublisherDisplayName` 参数已就绪。
2. **Partner Center 提交**：用 `-StoreUpload` 产物（未签名，Store 重签），注入 Partner Center 身份，提交年龄分级/截图/隐私政策并过 WACK。
3. **签名包真机安装**：需管理员把自签名证书导入 `LocalMachine\TrustedPeople` 后 `Add-AppxPackage`；Store 由 Store 重签，不走自签名。
4. **V1–V10/V13**：虚拟化落点、导入、托盘、StartupTask、Toast、mDNS、Strings/PRI、单实例、签名、Store 草稿。
5. 本地调试：开发者模式 + `Add-AppxPackage -Register`；不做自签名侧载分发。

## 8. 风险/限制

- 未签名 MSI/EXE：SmartScreen 提示；签名后 winget hash 必须重算。
- winget 真实安装需先发 Release 并上传资产；`validate` 只校验结构。
- machine MSI 多用户数据删除不彻底（仅当前用户）。
- 便携自启留 HKCU Run 指针；删除目录后需手动清理。
- 便携在移动盘/网络盘的 WAL 风险未实现 DriveType 拒绝（待 P2 或后续）。
- x64 only；arm64 后续。
- MSIX 构建/导入/StoreUpload 已实现，但 Partner Center 身份与真实提交未完成；签名包本地安装需管理员把自签名证书导入 `LocalMachine\TrustedPeople`。
