# Windows 打包渠道（按产物拆分）

> 设计与实现差异见 [ADR-005](adr/005-windows-distribution-implementation.md)；各渠道卸载/数据保留行为以该文为准。

主入口是 `scripts/build-windows-installers.ps1`，用 `-Only` 选择渠道；每个渠道另有一个薄封装脚本，
便于 CI / 本地只构建需要的产物。版本唯一真值仍是
`platforms/windows/ClipBridgeShell_CS/Version.props`，经 `scripts/version.ps1` 派生
（本文实测时是 `0.1.0`，架构 `x64`）。

MSIX 是**独立脚本** `scripts/build-windows-msix.ps1`（不进 `build-windows-all.ps1`，避免把 Store
身份/签名/裁剪强加到 MSI/EXE/便携流程）；它只做 Store 渠道，不影响其它四条渠道。

## 渠道总览

| 渠道 | 命令 | 产物（`<ver>=0.1.0`，`<arch>=x64`） | 前置条件 | 验证状态 |
|---|---|---|---|---|
| 全部（All，默认） | `pwsh scripts/build-windows-all.ps1 -WixInstallPath C:\tools\wix314` | `ClipBridge-<ver>-win-<arch>-machine.msi`、`-user.exe`、`-user.msi`（内部）、`-portable.zip` | WiX 3.14 + VS 2022 MSBuild + .NET 10 SDK + Rust 目标 | 已实测 exit 0（2026-11-14，66.1s），4 类产物齐全、断言通过；未做真机安装 |
| per-machine MSI | `pwsh scripts/build-windows-machine-msi.ps1 -WixInstallPath C:\tools\wix314` | `ClipBridge-<ver>-win-<arch>-machine.msi` | WiX 3.14 + VS 2022 MSBuild + .NET 10 SDK + Rust 目标 | 已实测 exit 0（32.2s），结构断言通过；**2026-11-15 已用管理员权限实测安装/卸载成功**（程序目录/HKLM/快捷方式/ARP/数据目录全部清除） |
| per-user EXE（Burn） | `pwsh scripts/build-windows-user-exe.ps1 -WixInstallPath C:\tools\wix314` | `ClipBridge-<ver>-win-<arch>-user.exe`（+ 内部 `-user.msi`） | WiX 3.14 + VS 2022 MSBuild + .NET 10 SDK + Rust 目标 | 已实测 exit 0（34.3s），结构断言通过；**真机安装验证本次未做** |
| 便携 ZIP | `pwsh scripts/build-windows-portable-zip.ps1` | `ClipBridge-<ver>-win-<arch>-portable.zip` | .NET 10 SDK + Rust 目标（**不经过 WiX**） | 已实测 exit 0（10.3s），ZIP 结构断言通过；**已解压实测便携/无 marker/不可写回退三种模式** |
| winget 清单 | `pwsh scripts/build-windows-winget.ps1` | `dist/winget/` 下三件套 YAML | `dist/` 已有 machine MSI 与 user EXE；winget CLI（可选，缺失时跳过 validate） | 已实测 exit 0（约 1s），`winget validate` 通过；**真实安装需先发布 Release** |
| MSIX（Store） | `pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 [-SelfSign -KeepLayout]` | `dist/msix/ClipBridge-<ver>-win-x64.msix`（+ `unsigned/`、可选 `layout/`） | .NET 10 SDK + Rust target + Windows SDK（makeappx/signtool/makepri）；不需要 WiX | 已实测 exit 0（1.0.0，30.3s）：109.4→89.9 MB，AI/ML/pdb/资源断言通过，自签名成功；**真机 `Add-AppxPackage` 被自签名根信任阻止（0x800B0109）**，已用开发者模式 `-Register` 完成安装/启动/卸载验收 |

## 参数

- 所有构建 wrapper：`-Platform x64|arm64`、`-OutDir dist`、`-SkipRust`、`-KeepStage`。
- `machine` / `user` / `all` 额外支持 `-WixInstallPath <WiX 3.14 目录>`；不传时探测标准安装位置。
- 便携 ZIP 不需要 WiX：为与其他渠道脚本保持同一套参数而保留 `-WixInstallPath`，传入会被忽略。
- 主脚本：`-Only All|MachineMsi|UserExe|PortableZip`（默认 `All`，行为与旧版一致）。
- winget wrapper：`-Version`、`-ReleaseUrlBase`、`-ReleaseTag`、`-OutDir`；清单固定按 `x64` 产物生成。
- MSIX（独立脚本）：`-Version`、`-AllowPreviewVersion`、`-IdentityName`、`-Publisher`、
  `-PublisherDisplayName`、`-DisplayName`、`-SelfSign`、`-OutDir`（默认 `dist/msix`）、
  `-WindowsSdkBin`、`-SkipRust`、`-KeepLayout`。`-SelfSign` **不是默认**，只用于本地验收。
- 每个 wrapper 都有注释帮助，可直接 `Get-Help scripts/<脚本名>.ps1 -Full` 查看。

## 共享的 publish / heat 与断言

- `All`：machine MSI 构建时做一次 `dotnet publish` + `heat`，user MSI 用
  `-p:SkipPublish=true -p:SkipHeat=true` 复用同一份 stage/heat；再构建 bundle 与 ZIP。
- `MachineMsi`：只做 publish + heat + machine MSI，并只断言 machine 的属性。
- `UserExe`：publish + heat + per-user MSI + bundle，并只断言 user MSI 与 bundle。
- `PortableZip`：直接执行与 wixproj `BeforeBuild` 相同的 `dotnet publish` 参数并去掉
  stage 里的 `.pdb`/`.xml`；不打 heat、不构建任何 MSI，因此不需要 WiX/MSBuild。
- 断言按渠道裁剪，未构建的产物不会被断言。ZIP 断言始终包含：
  `ClipBridge.portable` 标记、`core_ffi_windows.dll`、无 `.pdb`、顶层目录名正确。

## 产物命名与本次实测大小（x64 / 0.1.0）

| 文件 | 字节数 | 约 |
|---|---:|---:|
| `ClipBridge-0.1.0-win-x64-machine.msi` | 91,981,292 | 87.7 MB |
| `ClipBridge-0.1.0-win-x64-user.msi` | 91,735,540 | 87.5 MB |
| `ClipBridge-0.1.0-win-x64-user.exe` | 91,009,213 | 86.8 MB |
| `ClipBridge-0.1.0-win-x64-portable.zip` | 95,976,671 | 91.5 MB |

`dist/` 已被 `.gitignore` 忽略，构建产物不进入仓库。

## MSIX 渠道（Store）

### 命令与前置条件

```powershell
# 正式版本（Version.props 必须 >= 1.0.0）：未签名 .msix
pwsh scripts/build-windows-msix.ps1

# 当前 Version.props 仍为 0.1.0：开发/验收显式给 1.0.0（不改 Version.props）
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0

# 本地验收：自签名 + 保留解包布局（Add-AppxPackage -Register 需要）
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 -SelfSign -KeepLayout

# Store 身份替换（Partner Center 分配的值）
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 `
  -IdentityName '12345Ryan416.ClipBridge' `
  -Publisher 'CN=01234567-89ab-cdef-0123-456789abcdef' `
  -PublisherDisplayName 'Ryan416'
```

前置条件：.NET 10 SDK、Rust `x86_64-pc-windows-msvc` target（或用 `-SkipRust` 复用已有 DLL）、
Windows 10/11 SDK 的 x64 工具 `makeappx.exe` / `signtool.exe` / `makepri.exe`
（默认在 `C:\Program Files (x86)\Windows Kits\10\bin\10.*\x64` 找最高版本；找不到会报明确路径，
可用 `-WindowsSdkBin` 指定）。不需要 WiX，也不需要管理员。

### 版本规则

- `Version.props` 是唯一真值；`-Version` 只覆盖**本次构建**，不会修改 `Version.props`。
  MSIX 包版本固定 4 段、Revision=0、major>=1。
- 默认（不传 `-Version`）从 `scripts/version.ps1 -Json` 取；当前 `0.1.0` 的 major=0，
  脚本**直接失败**并提示"MSIX 需 >=1.0.0"。
- `-Version 1.0.0` 或 `-AllowPreviewVersion` 是显式覆盖：前者用于开发/验收，后者把 0.x 映射为
  `max(1,major).minor.patch.0`（0.1.0 → **1.1.0.0**）。预览映射只用于开发，不能提交 Store。
- `AppxPackageVersion` 在 `Microsoft.Windows.SDK.BuildTools.MSIX 1.7.251221100`（WindowsAppSDK
  单项目 MSIX 工具链）里**不是有效属性**，传了会被忽略；实测传
  `-p:AppxPackageVersion=1.2.3.4`、manifest 写 `1.0.9.0`，产物仍是 `1.0.9.0`。正确做法是
  构建前把最终版本写进 manifest：脚本生成
  `platforms/windows/ClipBridgeShell_CS/ClipBridgeShell_CS/obj/msix/Package.appxmanifest`，
  通过 `-p:ClipBridgeAppxManifest=<path>` 传给项目（csproj 里是一个条件 `AppxManifest` item）。
  这样 MakePri 生成 `resources.pri` 时 resource map 名就是最终 `Identity Name`；**不能出包后再改
  Identity**，否则 `ms-resource://` 查找会断。

### Manifest 与身份

- `Identity Name` 默认开发值 `40a26821-a97d-4eb8-ae89-804c2cccf4a0`；Store 提交时用
  `-IdentityName` 换成 Partner Center 的名称。
- `Publisher` 默认 `CN=Ryan416`；Store 提交时必须换成 Partner Center 的 `CN=<GUID>`，
  并且等于重签证书主体。`-SelfSign` 时证书 Subject 直接用这个值。
- `Properties/DisplayName` 固定为 `ClipBridge`（不再用模板名 `ClipBridgeShell_CS`）；
  `PublisherDisplayName` 默认 `Ryan416`。
- `TargetDeviceFamily` 只保留 `Windows.Desktop`（MinVersion 10.0.17763.0，MaxVersionTested 10.0.26100.0）。
- 能力只保留 `runFullTrust`；toast COM 扩展保持原样。

### 启动项（startupTask）

官方 `Windows.ApplicationModel.StartupTask` 文档给 **packaged desktop app** 的示例是
`uap5:Extension Category="windows.startupTask"` + `uap5:StartupTask`（`desktop:` 是 Windows 10
1703 时代的等价旧命名空间）。因此 manifest 已改为：

```xml
<uap5:Extension Category="windows.startupTask"
                 Executable="ClipBridgeShell_CS.exe"
                 EntryPoint="Windows.FullTrustApplication">
  <uap5:StartupTask TaskId="ClipBridgeStartupTask" Enabled="false" DisplayName="ClipBridge" />
</uap5:Extension>
```

`TaskId` 与代码 `StartupService` 的 `StartupTask.GetAsync("ClipBridgeStartupTask")` 一致；
`Enabled=false` 表示首次需用户在设置里启用（`RequestEnableAsync`）；Task Manager 显示
`ClipBridge`。桌面 full-trust 启动任务走正常 `OnLaunched`，不需要 `ExtendedActivationKind` 分支。
参考：<https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask>
（"Packaged desktop app startup task extension"）与
<https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop-startuptask>。

### 裁剪与重打包

1. `dotnet build` 直接产出的 build 布局（109.4 MB，含 WindowsAppSDK AI/ML）。
2. `makeappx unpack` 到 `dist/msix/layout/...`，删除：
   - 未使用 AI/ML：`onnxruntime*.dll`、`DirectML*.dll`、`Microsoft.Windows.AI*`、
     `Microsoft.Windows.Internal.AI*`、`Microsoft.ML.OnnxRuntime*`（实测 23 个文件、42.5 MB 原始大小）；
   - `*.pdb`；
   - 未保留语言的 WinUI MUI 目录（保留 `zh-CN` / `zh-TW` / `en-us`，大小写兼容 `en-US`）；
     实测 83 个目录、3.3 MB。
3. 校验布局：`AppxManifest.xml`、`ClipBridgeShell_CS.exe`、`core_ffi_windows.dll`、
   `resources.pri`（>100 KB）、Assets、`Strings/en-us|zh-cn/Resources.resw`、三个语言目录。
4. `makepri dump` 校验：`ResourceMap/@name == Identity Name`，且 `Resources/AppDisplayName`、
   `Strings` 资源树存在。
5. `makeappx pack` 重打包（不带签名），再按需签名。实测 109.4 → **89.9 MB（-17.8%）**。

### 产物与日志

| 路径 | 说明 |
|---|---|
| `dist/msix/ClipBridge-<ver>-win-x64.msix` | 最终包；`-SelfSign` 时已签名 |
| `dist/msix/unsigned/ClipBridge-<ver>-win-x64-unsigned.msix` | 未签名包（始终保留） |
| `dist/msix/layout/ClipBridge-<ver>-win-x64/` | 裁剪后布局（`-KeepLayout`） |
| `dist/msix/certs/*.cer` | `-SelfSign` 的公开证书 |
| `dist/msix/logs/build-msix.log` | 本次构建脚本日志 |
| `dist/msix/logs/dotnet-build.log` | dotnet 构建输出 |
| `dist/msix/logs/makeappx-{unpack,pack}.log` | makeappx 输出 |
| `dist/msix/logs/resources.pri.xml` | PRI dump（身份/资源校验依据） |
| `dist/msix/logs/signtool-{sign,verify}.log` | 签名/验证输出 |
| `dist/msix/logs/msix-summary.json` | 版本/身份/大小/SHA256/签名状态的机器可读摘要 |

### 签名与 Store

- **生产**：不要用 `-SelfSign`。Store 提交由 Store 重签；`Publisher`/`IdentityName` 用
  Partner Center 分配值。Store 重签后 hash 会变，MSIX 不进 `SHA256SUMS`。
- **本地**：`-SelfSign` 生成/复用 `CN=<Publisher CN>` 自签名证书，放在 `Cert:\CurrentUser\My`，
  导出 `.cer`，导入 `Cert:\CurrentUser\TrustedPeople`；如果当前会话已提权，
  再导入 `Cert:\LocalMachine\TrustedPeople`。签名用 `signtool sign /fd SHA256`（不时间戳）。
- 关键限制：**`CurrentUser\TrustedPeople` 不足以让 `Add-AppxPackage` 信任自签名包**。
  实测会报 `0x80073CF0` + `0x800B0109`（"应用包或捆绑包中的签名的根证书必须是受信任的证书"）。
  自签名包要真正侧载，必须由管理员把证书导入 `LocalMachine\TrustedPeople`（或 `LocalMachine\Root`，
  不推荐），或者改为开发者模式注册布局。
- `signtool verify /pa` 在非管理员机器上会因同样的根信任问题 exit 1；这是预期，不代表包结构坏，
  脚本不会因此失败，但会在日志里明确打印。`Get-AuthenticodeSignature` 的
  `Status=UnknownError` + `SignerCertificate` 非空同样表示签名已写入、链不受信。

### 本地安装 / 启动 / 卸载验收（2026-11-15 实测）

| 步骤 | 命令 | 结果 |
|---|---|---|
| 签名包安装 | `Add-AppxPackage -Path dist/msix/ClipBridge-1.0.0-win-x64.msix` | **失败**：`0x80073CF0`，具体 `0x800B0109` 根证书不受信；ActivityId `cd2b3a2a-73bf-000a-756c-32d1bf73dd01`；AppXDeploymentServer/Operational 事件 404。原因见上（需管理员导 `LocalMachine\TrustedPeople`） |
| 结构校验（回退） | `makeappx unpack` + manifest 检查 + `signtool verify` | unpack exit 0；`Identity Name/Publisher/Version/Arch`、`DisplayName=ClipBridge`、`TargetDeviceFamily=Windows.Desktop only`、`uap5:StartupTask` 字段全部正确；`signtool verify` exit 1（自签名根，预期） |
| 开发者模式安装 | `Add-AppxPackage -Register dist/msix/layout/ClipBridge-1.0.0-win-x64/AppxManifest.xml` | **成功**；`Get-AppxPackage` 可见，`IsDevelopmentMode=True`，Status=Ok |
| 启动 | `Start-Process shell:AppsFolder\<PFN>!App` | **成功**；进程存活、窗口响应（标题"剪切桥"），无 .NET/Application Error |
| 首次运行数据 | `%LOCALAPPDATA%\Packages\<PFN>\LocalState` | `core.db`、`logs.db`、`tls\`、`logs\`、`Strings\` 均生成；`LocalCache\blobs` 生成。本机已有 PasswordVault 账号，因此 core 正常初始化；**无账号的首次运行**只会创建 `logs/`、`Strings/`，`core.db/tls` 要等登录后（`CoreHostService` 在 `accountState.Configured=false` 时保持 NotLoaded） |
| 卸载 | `Remove-AppxPackage` | **成功**；`Get-AppxPackage` 计数 0，`%LOCALAPPDATA%\Packages\<PFN>`（含 LocalState）被删除，符合 Store 语义 |

> 结论：MSIX 包构建/裁剪/签名/结构断言全部通过；**签名包的 `Add-AppxPackage` 因当前会话非管理员、
> 只能把证书放到 CurrentUser 而未完成**，改用开发者模式 `-Register` 完成了安装/启动/卸载验收。
> 在管理员机器上把证书导入 `LocalMachine\TrustedPeople` 后应按标准侧载流程安装。

## 验证状态与已知限制

- 本轮 MSI/Bundle/ZIP 验证只覆盖"构建 + 包结构断言"：所有命令 exit 0、产物存在、MSI/Bundle/ZIP 属性符合预期。
- **machine MSI 需要管理员权限在真实机器上安装验证**（写 `Program Files` / `HKLM`、走 UAC），本次未做。
- per-user EXE 的真机安装/卸载、便携 ZIP 的解压启动，本次也未做。
- **winget 需要先发布 GitHub Release 并上传 machine MSI 与 user EXE**；清单里的 `InstallerUrl`
  指向 Release 资产，未发布前真实 `winget install` 会 404。`winget validate` 只校验清单结构，
  不校验远端 URL。
- winget 清单固化产物 SHA256：每次重新构建（或签名）后 hash 会变，必须重新生成清单。
  因此 `build-windows-all.ps1` 不自动生成 winget，避免签名前把 hash 固化。
- **MSIX 已接入（独立脚本）**：`-Version 1.0.0` 验收通过（构建/裁剪/自签名/结构断言）；
  签名包 `Add-AppxPackage` 受自签名根信任限制（本机非管理员，`0x800B0109`），已用开发者模式
  `-Register` 完成安装/启动/卸载验收。Store 正式提交仍待 Partner Center 身份与 Store 重签。
- **MSIX 未做 MSI→MSIX 数据导入**（ADR-005 P2 待办）：打包后应用数据根是 `LocalState` /
  `LocalCache`，不会自动导入 `%LOCALAPPDATA%\ClipBridge`；本机因 PasswordVault 账号可复用，
  core 能正常初始化，但旧 core.db/tls 不会迁移。
- 当前产物未签名，首次运行仍会被 SmartScreen 拦截。

## 常用命令

```powershell
# 全部渠道
pwsh scripts/build-windows-all.ps1 -WixInstallPath C:\tools\wix314

# 单渠道
pwsh scripts/build-windows-machine-msi.ps1 -WixInstallPath C:\tools\wix314
pwsh scripts/build-windows-user-exe.ps1 -WixInstallPath C:\tools\wix314
pwsh scripts/build-windows-portable-zip.ps1
pwsh scripts/build-windows-winget.ps1

# MSIX（0.1.0 不可发；开发/验收显式 1.0.0）
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0 -SelfSign -KeepLayout

# 复用已有 Rust 产物 / 保留 stage
pwsh scripts/build-windows-all.ps1 -WixInstallPath C:\tools\wix314 -SkipRust -KeepStage

# 等价的主脚本调用
pwsh scripts/build-windows-installers.ps1 -Only MachineMsi -WixInstallPath C:\tools\wix314
```
## 安装 / 卸载与数据保留

用户数据在 `%LOCALAPPDATA%\ClipBridge`（MSI/EXE）或便携目录的 `data\`（便携 ZIP）。

| 渠道 | 卸载是否清理程序 | 用户数据默认 | 用户能否选择删除 |
|---|---|---|---|
| machine MSI | 程序目录、HKLM 注册表、开始菜单/桌面快捷方式、ARP | **删除**（默认勾选） | ✅ 卸载向导可取消勾选保留；静默默认删，`REMOVEUSERDATA=0` 保留 |
| user EXE | 安装目录、HKCU 键、开机自启、bundle ARP（已实测） | **删除** | ✅ 默认删；`-uninstall -quiet RemoveUserData=0` 保留（WixStdBA 无复选框 UI） |
| 便携 ZIP | 无卸载器：删除目录即删除 | 保留（在 `data\` 内） | ✅ 保留 `data\` 即保留数据；删除整个目录即全删；开启过自启的先关闭（Run 键） |
| winget | 调底层安装器静默卸载 | **删除**（默认 1） | ❌ 无法保留（winget 不能传 `REMOVEUSERDATA=0`）；要保留请改用 EXE/MSI 卸载并显式传 0 |
| MSIX（已实现） | `Remove-AppxPackage` 移除包 | **删除**（LocalState 随包删除，已实测） | ❌ 系统语义；需要的话要额外做导出/备份 |

注意事项：
- 默认策略是"干净优先"：无 UI 选择的渠道（winget/静默卸载）一律删数据；需要保留时必须显式传 0。
- machine MSI 的"删除用户数据"只覆盖**执行卸载的当前用户**（`%LOCALAPPDATA%` 是每用户目录）；其它用户的数据不会被枚举删除。
- user EXE 的 `RemoveUserData` 是 Burn 可覆盖变量：默认 1（删除）；实测传 0 后数据目录保留。
- 本机开发环境有早期测试残留的 2 个 hidden ARP 项（`SystemComponent=1`）和对应快捷方式组件引用，会让"新装再卸"看起来有残留；干净 VM/干净用户下按标准 MSI 行为清理。
- machine MSI 已于 2026-11-15 用管理员权限实测：安装 exit 0，卸载后程序目录、HKLM 键、公共开始菜单/桌面快捷方式、ARP、`%LOCALAPPDATA%\ClipBridge` 全部清除（默认删除数据）。
## 卸载实测记录（2026-11-15）

| 渠道 | 结果 |
|---|---|
| machine MSI（提权） | 安装 → 卸载 exit 0；`Program Files\ClipBridge`、HKLM 标记、公共开始菜单/桌面、ARP、`%LOCALAPPDATA%\ClipBridge` 全部清除 |
| user EXE | 安装 → 卸载 exit 0；安装目录、开始菜单/桌面快捷方式、HKCU 键、HKCU Run、HKCU ARP、数据目录全部清除 |
| 便携 ZIP | 便携模式写 `data\`；无 marker 回退 LocalAppData；不可写回退并提示（此前已测） |
| winget | `winget validate` 通过；真实安装待 Release 发布 |
| MSIX（开发者模式 `-Register`） | 卸载 exit 0；`Get-AppxPackage` 计数 0，`%LOCALAPPDATA%\Packages\<PFN>`（含 LocalState）全部删除；**签名包的 `Add-AppxPackage` 未完成**（非管理员，`0x800B0109`），详见"MSIX 渠道"节 |

> 本机在测试早期曾留下两个 phantom 产品，阻塞了手工组件的卸载；清掉其 Installer 组件引用后，
> 上述 machine/user 卸载均为全清。干净机器不会出现该问题。
