# MSIX / Microsoft Store 提交材料与命令清单

- 状态：**准备中**（Partner Center 身份已取得并本地验证；截图/隐私政策/测试账号待准备）
- 适用产物：`dist/msix/ClipBridge-<ver>-win-x64.msixupload`（StoreUpload，未签名，Store 重签）
- 相关：[ADR-005](adr/005-windows-distribution-implementation.md) §7、[windows-build.md](windows-build.md) MSIX 段

## 0. 当前值（开发占位，提交前必须替换）

| 项 | 当前值 | Store 要求 |
|---|---|---|
| `Version.props` | `0.1.0` | **≥1.0.0**，4 段、Revision=0；首发建议 `1.0.0.0` |
| Manifest `Identity Name` | `40a26821-a97d-4eb8-ae89-804c2cccf4a0`（模板 GUID） | Partner Center 分配的 **`<PublisherId>.<AppName>`** |
| Manifest `Publisher` | `CN=Ryan416` | Partner Center 的 **`CN=<GUID>`**，必须与 Store 重签证书主体一致 |
| `PublisherDisplayName` | `Ryan416` | 与 Partner Center 账户/发布者名称一致 |
| `DisplayName` | `ClipBridge` | 与保留的产品名一致 |
| 架构 | x64 | 可只提交 x64；ARM64 后续 |
| 最低系统 | Windows 10 1809（17763） | 与 Store 支持范围一致 |

当前 `Package.appxmanifest` 的 `Version` 是 `0.0.0.0` 占位，构建脚本会注入；**不要手工改**。

## 1. 准备 Partner Center

1. 注册 Microsoft Partner Center（个人开发者免费；需完成身份验证，可能需要 Microsoft Entra 租户）。
2. **Reserve product name**：`ClipBridge`（或可用名；一旦保留，DisplayName 必须完全一致）。
3. 在 **Product identity** 页复制：
   - `Package/Identity/Name`（形如 `12345Ryan416.ClipBridge`）
   - `Package/Identity/Publisher`（`CN=<GUID>`）
   - `Package/Properties/PublisherDisplayName`
   - Store ID（提交后用于链接）
4. 记录到本清单的"身份值"表，后续构建只用这一组值。

## 2. 注入身份并构建 StoreUpload

推荐用**本地未跟踪文件**（不把真实身份提交到仓库）：

```powershell
# 1) 复制模板，填 Partner Center 的真实值；identity.local.json 已被 .gitignore 忽略
Copy-Item store\identity.example.json store\identity.local.json
notepad store\identity.local.json

# 2) 构建（先把 Version.props 抬到 >=1.0.0）
pwsh scripts/build-windows-msix.ps1 `
  -Version 1.0.0.0 `
  -StoreUpload `
  -IdentityFile store/identity.local.json
```

`store/identity.local.json` 格式：

```json
{
  "identityName": "12345Ryan416.ClipBridge",
  "publisher": "CN=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "publisherDisplayName": "Ryan416",
  "displayName": "ClipBridge"
}
```

也可以不用文件、直接传 `-IdentityName/-Publisher/-PublisherDisplayName`（显式参数优先于文件）。

约束：

- `-StoreUpload` 与 `-SelfSign` 互斥；Store 不接受自签名产物。
- 不要用 `-AllowPreviewVersion` 提交 Store（0.1.0 会映射成 1.1.0.0，高于将来真正的 1.0.0）。
- 产物：`dist/msix/ClipBridge-1.0.0-win-x64.msixupload`（约 91 MB，未签名）。
- 后续每次更新版本号必须**严格递增**且 Revision=0（如 `1.0.1.0`）。

### 2.1 身份值不要提交 repo

`Identity Name`、`Publisher CN`、`PublisherDisplayName` **不是秘密**（安装包 manifest、Store 页面、winget 清单里都可见），但也没必要写进仓库文档/源码：

- 仓库内只保留 [store/identity.example.json](../store/identity.example.json) 占位符；
- 真实值写 `store/identity.local.json`（已加入 `.gitignore`）；
- CI/自动化时把这三个值放 GitHub **Variables**（公开值），Partner Center/Entra 凭据才放 **Secrets**；
- **绝不能提交**：`.pfx` 私钥、Partner Center 密码、Entra client secret、测试账号密码。

期望断言（脚本自动执行）：

- upload 是有效 zip，内含单个 `.msix`；
- 内部包可 `makeappx unpack`，版本 `1.0.0.0`，Identity/Publisher/DisplayName 与参数一致；
- 无 `.pdb`、无 AI/ML 大文件；保留 `e_sqlite3.dll`、`Microsoft.Data.Sqlite.dll`、`resources.pri`、Assets/Strings；
- 未签名（内部无 `AppxSignature.p7x`）。

## 3. 预提交验证

> 本地已验证：用真实身份构建的 `.msixupload`，开发者模式安装后 PFN 与 Store 显示的 PFN 一致（`Ryan416.ClipBridge_x7bq0vy4t3gtr`）；版本 `1.0.0.0`。真实身份值只存在本地 `store/identity.local.json`，未提交。

### 3.1 包结构

```powershell
$upload = 'dist\msix\ClipBridge-1.0.0-win-x64.msixupload'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$tmp = "$env:TEMP\cb-msix-verify"; Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
[System.IO.Compression.ZipFile]::ExtractToDirectory((Resolve-Path $upload), $tmp)
Get-ChildItem $tmp -Filter *.msix | Select-Object Name,Length
# 内部包再 unpack 检查 AppxManifest.xml
```

### 3.2 WACK（Windows App Certification Kit）

WACK 已标记为弃用但仍是本地预检工具；**需要管理员权限**：

```powershell
$appcert = "${env:ProgramFiles(x86)}\Windows Kits\10\App Certification Kit\appcert.exe"

# 先构建同 payload 的签名 Sideload 包用于本地安装/测试；
# StoreUpload 包本身未签名，不能直接 Add-AppxPackage。
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0.0 -SelfSign -KeepLayout
Import-Certificate -FilePath dist\msix\certs\ClipBridge-CN_Ryan416.cer `
  -CertStoreLocation Cert:\LocalMachine\TrustedPeople
Add-AppxPackage -Path dist\msix\ClipBridge-1.0.0-win-x64.msix

& $appcert reset
& $appcert test -appxpackagepath (Resolve-Path 'dist\msix\ClipBridge-1.0.0-win-x64.msix') `
  -reportoutputpath "$env:TEMP\cb-wack.xml"
# 报告：$env:TEMP\cb-wack.xml；日志：%LOCALAPPDATA%\Microsoft\AppCertKit
```

注意：

- WACK 要测 Release 包，且最好在干净 VM；
- 自签名证书只用于本地测试，**不要**放进 Store 提交；
- WACK 通过不等于 Store 认证通过，但能拦截 API/性能/清单类问题。

### 3.3 干净 VM 冒烟

- 用开发者模式 `Add-AppxPackage -Register` 或受信任自签名包装一次；
- 首次启动：旧数据导入（若存在 MSI/EXE 数据）、登录、剪贴板同步、托盘、开机自启（StartupTask）、卸载后 LocalState 删除；
- 记录截图/日志作为 certification notes。

## 4. Store 列表材料

### 4.1 必备材料

| 材料 | 要求/建议 | ClipBridge 当前 |
|---|---|---|
| 产品名 | 与保留名完全一致 | `ClipBridge`（待保留） |
| 简短描述 | ≤ 100 字符左右，突出"跨设备剪贴板同步" | 见下方草稿 |
| 详细描述 | 功能、隐私、系统要求、使用限制 | 见下方草稿 |
| 截图 | 至少 1 张，≥1366×768（推荐 1920×1080），最多 10 张 | **待制作** |
| Store logo | 300×300 PNG | 包内 `Assets\StoreLogo.png` 可复用；需单独上传 300×300 |
| 分类 | 生产力 / 实用工具 | 建议 Productivity |
| 隐私政策 URL | 收集剪贴板/账号/设备信息，**必须** | **待准备** |
| 支持 URL/联系方式 | 必填 | 待定 |
| 网站（可选） | 产品页 | 待定 |
| 版权/发布者 | 与账户一致 | Ryan416 |
| 年龄分级 | IARC 问卷 | 待填写（无暴力/成人内容；剪贴板可能含用户内容，按实回答） |
| 系统要求 | Windows 10 1809+ x64 | ✅ |
| 认证备注 | 测试账号/服务器、特殊步骤 | **关键，见 4.4** |

### 4.2 简介草稿（中文）

**简短描述**：跨设备剪贴板同步：在 Windows 与手机间安全复制、粘贴历史内容。

**详细描述**（可直接改）：

> ClipBridge 让剪贴板内容在你的设备之间安全流动。复制文本、图片或文件，在另一台设备上直接粘贴；历史记录与常用内容集中管理。
>
> 主要功能：
> - Windows 与移动端剪贴板双向同步
> - 文本、图片与文件历史，支持搜索与固定
> - 端到端加密传输，账号凭据保存在 Windows 凭据管理器
> - 托盘常驻、开机自启可选、全局快捷键
> - 本地数据可导出备份
>
> 隐私：剪贴板内容仅在你登录的设备间同步；本地数据保存在 `%LOCALAPPDATA%\ClipBridge`（MSIX 为应用 LocalState）；卸载可选删除。详见隐私政策。
>
> 系统要求：Windows 10 1809（17763）或更高，x64。

**英文短描述**：Sync your clipboard securely across devices — copy on one, paste on another.

### 4.3 runFullTrust 说明（认证备注模板）

> ClipBridge is a WinUI 3 desktop application packaged as MSIX. It requires `runFullTrust` because it:
> - hosts a WinUI 3 window and a system-tray icon;
> - loads a native Rust FFI library (`core_ffi_windows.dll`) for clipboard capture, crypto and sync;
> - uses global hotkeys and clipboard APIs that are not available in AppContainer;
> - manages a background sync connection while the app is running.
> No `unvirtualizedResources` capability is requested. All app data is stored in the package's LocalState; credentials use Windows Credential Manager. The app does not install drivers or services.

### 4.4 认证测试账号/步骤（关键）

ClipBridge 需要账号与可达的同步服务；Store 认证人员没有你的服务器/账号时无法完成核心流程。提交前必须准备：

- 一个**专用测试账号**（用户名/密码），服务端允许外网访问；
- 测试步骤：登录 → 复制文本 → 另一设备/客户端确认同步 → 托盘/自启 → 卸载；
- 若服务不可用，准备**离线演示模式**或测试服务器；否则认证很可能失败；
- 不要把生产管理员账号写入备注；测试账号应可随时禁用。

### 4.5 隐私政策要点（自查）

- 收集：剪贴板内容（可能含敏感信息）、账号标识、设备 ID、必要的连接日志；
- 用途：仅同步/历史/故障排查，不出售、不用于广告；
- 存储：本地数据目录；服务端存储范围与保留期；
- 加密：传输加密、端到端/服务端可见性说明（按实际实现写，不要夸大）；
- 用户权利：导出、删除、注销；
- 联系方式与生效日期。

## 5. 提交与认证

1. Partner Center → 产品 → **Start submission**；
2. **Packages**：上传 `ClipBridge-1.0.0-win-x64.msixupload`（不要上传自签名包）；确认 Identity/版本与产品一致；
3. **Properties**：分类、系统要求、年龄分级（IARC 问卷）、隐私政策；
4. **Store listing**：名称、描述、截图、Store logo、支持信息；
5. **Pricing & availability**：免费/市场/发布时间；
6. **Notes for certification**：测试账号、服务器、操作步骤、runFullTrust 说明；
7. Submit → 认证（Store 重签）→ 发布；发布后用 Store 安装做一次冒烟。

## 6. 当前阻断项（提交前逐项打勾）

- [ ] Partner Center 账号完成验证
- [ ] 产品名 `ClipBridge` 已保留
- [ ] 取得 Identity Name / Publisher CN / PublisherDisplayName
- [ ] `Version.props` 升至 `1.0.0` 且用 4 段构建
- [ ] 用真实身份重新执行 `-StoreUpload` 构建（非模板 GUID/CN=Ryan416）
- [ ] 隐私政策 URL 可公开访问
- [ ] 至少 1 张合格截图 + 300×300 Store logo
- [ ] 测试账号 + 可达服务端 + 认证步骤
- [ ] WACK 通过（或已知项有解释）
- [ ] 干净 VM 安装/登录/同步/卸载冒烟
- [ ] 签名包本地安装验收（可选，管理员信任自签名证书）
- [ ] `msixupload` 上传成功且版本严格递增

## 7. 命令速查

```powershell
# StoreUpload（未签名，提交用）
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0.0 -StoreUpload `
  -IdentityName '<PublisherId>.<AppName>' -Publisher 'CN=<GUID>' `
  -PublisherDisplayName '<PublisherDisplayName>' -DisplayName 'ClipBridge'

# Sideload + 自签名（仅本地 WACK/冒烟）
pwsh scripts/build-windows-msix.ps1 -Version 1.0.0.0 -SelfSign -KeepLayout

# 信任自签名证书（管理员）
Import-Certificate -FilePath dist\msix\certs\ClipBridge-CN_Ryan416.cer `
  -CertStoreLocation Cert:\LocalMachine\TrustedPeople
Add-AppxPackage -Path dist\msix\ClipBridge-1.0.0-win-x64.msix

# 卸载
Get-AppxPackage -Name '*ClipBridge*' | Remove-AppxPackage

# 版本守卫应失败（0.1.0）
pwsh scripts/build-windows-msix.ps1
```

## 8. 参考

- [Create an app submission for MSIX apps](https://learn.microsoft.com/windows/apps/publish/publish-your-app/msix/create-app-submission)
- [MSIX package identity / version rules](https://learn.microsoft.com/windows/msix/package/identity)
- [runFullTrust capability](https://learn.microsoft.com/windows/msix/package/capabilities)
- [StartupTask API](https://learn.microsoft.com/uwp/api/windows.applicationmodel.startuptask)
- [appcert.exe（WACK）命令行](https://learn.microsoft.com/windows/win32/appxpkg/appcert-exe)
