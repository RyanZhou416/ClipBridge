# Windows 打包渠道（按产物拆分）

主入口是 `scripts/build-windows-installers.ps1`，用 `-Only` 选择渠道；每个渠道另有一个薄封装脚本，
便于 CI / 本地只构建需要的产物。版本唯一真值仍是
`platforms/windows/ClipBridgeShell_CS/Version.props`，经 `scripts/version.ps1` 派生
（本文实测时是 `0.1.0`，架构 `x64`）。

## 渠道总览

| 渠道 | 命令 | 产物（`<ver>=0.1.0`，`<arch>=x64`） | 前置条件 | 验证状态 |
|---|---|---|---|---|
| 全部（All，默认） | `pwsh scripts/build-windows-all.ps1 -WixInstallPath C:\tools\wix314` | `ClipBridge-<ver>-win-<arch>-machine.msi`、`-user.exe`、`-user.msi`（内部）、`-portable.zip` | WiX 3.14 + VS 2022 MSBuild + .NET 10 SDK + Rust 目标 | 已实测 exit 0（2026-11-14，66.1s），4 类产物齐全、断言通过；未做真机安装 |
| per-machine MSI | `pwsh scripts/build-windows-machine-msi.ps1 -WixInstallPath C:\tools\wix314` | `ClipBridge-<ver>-win-<arch>-machine.msi` | WiX 3.14 + VS 2022 MSBuild + .NET 10 SDK + Rust 目标 | 已实测 exit 0（32.2s），结构断言通过；**需管理员权限在真实机器上安装验证（本次未做）** |
| per-user EXE（Burn） | `pwsh scripts/build-windows-user-exe.ps1 -WixInstallPath C:\tools\wix314` | `ClipBridge-<ver>-win-<arch>-user.exe`（+ 内部 `-user.msi`） | WiX 3.14 + VS 2022 MSBuild + .NET 10 SDK + Rust 目标 | 已实测 exit 0（34.3s），结构断言通过；**真机安装验证本次未做** |
| 便携 ZIP | `pwsh scripts/build-windows-portable-zip.ps1` | `ClipBridge-<ver>-win-<arch>-portable.zip` | .NET 10 SDK + Rust 目标（**不经过 WiX**） | 已实测 exit 0（10.3s），ZIP 结构断言通过；**未解压实跑** |
| winget 清单 | `pwsh scripts/build-windows-winget.ps1` | `dist/winget/` 下三件套 YAML | `dist/` 已有 machine MSI 与 user EXE；winget CLI（可选，缺失时跳过 validate） | 已实测 exit 0（约 1s），`winget validate` 通过；**真实安装需先发布 Release** |
| MSIX | —（**未实现，P2**） | — | 签名证书 + MSIX 打包 + 数据目录迁移（P2） | 未实现，不在本次交付范围内 |

## 参数

- 所有构建 wrapper：`-Platform x64|arm64`、`-OutDir dist`、`-SkipRust`、`-KeepStage`。
- `machine` / `user` / `all` 额外支持 `-WixInstallPath <WiX 3.14 目录>`；不传时探测标准安装位置。
- 便携 ZIP 不需要 WiX：为与其他渠道脚本保持同一套参数而保留 `-WixInstallPath`，传入会被忽略。
- 主脚本：`-Only All|MachineMsi|UserExe|PortableZip`（默认 `All`，行为与旧版一致）。
- winget wrapper：`-Version`、`-ReleaseUrlBase`、`-ReleaseTag`、`-OutDir`；清单固定按 `x64` 产物生成。
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

## 验证状态与已知限制

- 本次验证只覆盖"构建 + 包结构断言"：所有命令 exit 0、产物存在、MSI/Bundle/ZIP 属性符合预期。
- **machine MSI 需要管理员权限在真实机器上安装验证**（写 `Program Files` / `HKLM`、走 UAC），本次未做。
- per-user EXE 的真机安装/卸载、便携 ZIP 的解压启动，本次也未做。
- **winget 需要先发布 GitHub Release 并上传 machine MSI 与 user EXE**；清单里的 `InstallerUrl`
  指向 Release 资产，未发布前真实 `winget install` 会 404。`winget validate` 只校验清单结构，
  不校验远端 URL。
- winget 清单固化产物 SHA256：每次重新构建（或签名）后 hash 会变，必须重新生成清单。
  因此 `build-windows-all.ps1` 不自动生成 winget，避免签名前把 hash 固化。
- **MSIX 尚未接入（P2）**：没有 MSIX 构建脚本，也不要求签名证书；本次没有实现。
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

# 复用已有 Rust 产物 / 保留 stage
pwsh scripts/build-windows-all.ps1 -WixInstallPath C:\tools\wix314 -SkipRust -KeepStage

# 等价的主脚本调用
pwsh scripts/build-windows-installers.ps1 -Only MachineMsi -WixInstallPath C:\tools\wix314
```
