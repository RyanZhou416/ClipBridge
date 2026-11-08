# 分发与安装方式：行业全景 + ClipBridge 的选择

> 目的：不被"我们现在是 MSI"限制，先从行业里所有可选方式出发，用统一维度对比，再按 CB 的真实约束做选择。
> 关键成本/政策事实（2025-2026）：
> - 微软商店**个人开发者注册已免费**（原 $19 取消）：[Microsoft blog](https://blogs.windows.com/windowsdeveloper/2025/09/10/free-developer-registration-for-individual-developers-on-microsoft-store/)
> - MSIX/代码签名推荐 **Azure Artifact Signing**（原 Trusted Signing），Basic 约 **$10/月**：[官方文档](https://learn.microsoft.com/windows/msix/package/signing-package-overview)
> - Android：Google Play **$25 一次性**，发布必须 **AAB**；
> - macOS：App Store 之外分发需要 **Developer ID 签名 + 公证（notarization）**，Apple Developer **$99/年**。

## 1. CB 的真实约束（先于方案）

| 约束 | 含义 |
|---|---|
| **三端** | Windows（WinUI3）、macOS（SwiftUI）、Android；**发布流水线最好一致** |
| **常驻托盘 + 局域网** | 需要 UDP 组播（mDNS）、后台常驻、开机自启；**商店沙箱/能力声明会带来约束** |
| **大文件缓存** | 内容缓存可达数百 MB～数 GB，更新包体积大（当前 241 MB 目录 / 87.9 MB MSI） |
| **数据是资产** | 历史/配对/缓存要保留；卸载不应默认清空 |
| **单一开发者** | 没人力维护三套安装器 + 三套商店；预算有限（证书/账号是真实成本） |
| **目标用户** | 中文技术用户为主，接受"下载 ZIP/安装包"，也能用 winget |
| **无企业需求（暂）** | 暂时不需要 Intune/SCCM 批量部署，但不应堵死 |
| **安全底线** | 更新通道必须防篡改（签名/哈希校验 + HTTPS），不能裸更新 |

## 2. 行业里的分发/安装方式（全量清单）

### A. 直接下载（自托管）
1. **MSI**（per-user / per-machine / 双用途）
2. **EXE 引导器**：Inno Setup、NSIS、WiX Burn、Squirrel/Velopack、Tauri updater
3. **MSIX 侧载**（需签名）
4. **便携 ZIP / 单文件自解压**
5. **ClickOnce**（.NET 专用，自动更新，但生态老、Mac/Linux 无）

### B. 应用商店
6. **Microsoft Store**（MSIX 为主；也接受 MSI/EXE，但证书必须链到 Microsoft Trusted Root，且无 Store 自动更新）
7. **Mac App Store**（沙箱强制、抽成、审核）
8. **Google Play**（AAB、$25、审核）
9. **F-Droid / 亚马逊 / 三星 Galaxy Store**（开源或区域渠道）
10. **Steam / Epic / itch.io**（游戏向；有自动更新与 SDK）

### C. 包管理器 / 软件目录
11. **winget**（Windows 官方，清单驱动，支持 msi/exe/msix/portable/zip）
12. **Chocolatey**（Windows 社区，商业版有内网源）
13. **Scoop**（Windows 便携优先，装到用户目录，无需管理员）
14. **Homebrew**（macOS，Cask 可分发 dmg/pkg/zip）
15. **Linux**：deb/rpm、AppImage、Flatpak、Snap、AUR（CB 暂无 Linux 端）

### D. 企业 / 批量
16. **Intune / SCCM / GPO** 推送 MSI/MSIX
17. **MSIX App Attach**（AVD/远程桌面）
18. **App-V / ThinApp** 应用虚拟化
19. **内部更新服务器**（企业自建 CDN）

### E. 零安装 / 非常规
20. **Web 应用 / PWA**（无安装，但浏览器沙箱限制剪贴板、无组播、无托盘）
21. **浏览器扩展**（可做剪贴板同步，但依赖浏览器、无本地大文件缓存）
22. **远程托管**（RDP/云桌面里跑）
23. **容器**（Docker Desktop；GUI 托盘应用不适合）
24. **包注册表当渠道**（npm/crates/pip 分发 CLI；GUI 不适用）

### F. 组合/自更新架构（工业界主流）
25. **安装器 + 内置更新器**（VS Code、Discord、Slack 等）
26. **便携 + 自更新**（PortableApps 风格）
27. **GitHub Releases 当"应用商店"**（开源应用最常见）
28. **差分/组件更新**（Courgette、bsdiff、Squirrel delta、MSIX differential）

## 3. 多维对比（主流路线）

| 维度 | MSI | EXE 引导器 | MSIX/Store | winget | Scoop | 便携 ZIP | PWA/Web |
|---|---|---|---|---|---|---|---|
| 用户操作 | 下一步×N | 下一步×N | 商店一键 | 一行命令 | 一行命令 | 解压即用 | 打开网页 |
| 管理员权限 | per-machine 需要 | 取决于范围 | 否 | 随底层 | 否 | 否 | 否 |
| exe 可见性 | ✅ Program Files | ✅ 安装目录 | ❌ WindowsApps 受保护 | 随底层 | ✅ 用户目录 | ✅ | — |
| 数据位置 | 应用自定 | 应用自定 | `Packages\<PFN>` | 随底层 | 应用自定 | 同目录/LocalAppData | 浏览器存储 |
| 卸载语义 | 保留数据（默认） | 自定义 | **删除应用数据** | 随底层 | 删目录 | 手动 | 清浏览器数据 |
| 自动更新 | 需自研/渠道 | 可内置 | ✅ 系统级 | ✅ upgrade | ✅ | 需自研 | 即时 |
| 发现/触达 | 低 | 低 | **最高** | 中高 | 中 | 低 | 高但功能受限 |
| 企业部署 | ✅ 成熟 | 一般 | ✅（Intune） | 一般 | 弱 | 弱 | 弱 |
| 签名成本 | Authenticode（可选） | 同左 | **必须**（~$10/月或 Store 代签） | 随底层 | 无 | 无 | — |
| 离线安装 | ✅ | ✅ | ✅（带证书） | ✅ | ✅ | ✅ | ❌ |
| 跨平台一致 | 仅 Win | 仅 Win | 仅 Win | 仅 Win | 仅 Win | ✅ 三端一致 | 浏览器 |
| 体积/带宽 | 压缩 CAB | 可压缩 | 差分更新强 | 随底层 | 随包 | 原样（大） | 极小 |
| 维护成本（单人） | 中 | 中高 | 高（证书/审核） | 低 | 低 | 低 | 低但功能缺失 |
| 功能约束 | 无 | 无 | 沙箱/能力声明 | 无 | 无 | 无 | **严重**（无组播/托盘/大文件） |

## 4. 从需求反推：CB 应该"长什么样"

CB 不是普通工具：**常驻 + 局域网组播 + 大文件缓存 + 三端**。这决定了：

- **Web/PWA/扩展不可行**：浏览器拿不到 UDP 组播、托盘常驻、本地大文件缓存（虽然 File System Access API 有，但常驻与组播仍缺）。
- **商店不适合做唯一渠道**：能力声明、审核、卸载删数据、企业/内网行为都受限；但**可以做"锦上添花"的第二渠道**。
- **单靠 MSI 不够**：没有自动更新，用户每次手动下载 87.9 MB 的安装包。
- **单靠便携不够**：没有卸载项/开始菜单/自动更新，普通用户不友好。
- **真正合适的是"制品与渠道分离"**：
  - 一个**构建产物**（便携目录）；
  - 由它派生出 **便携 ZIP / MSI / MSIX** 三种"外壳"；
  - 由**内置更新器**负责所有渠道的更新；
  - 商店/winget 只是"发现与分发"的前端，不改变主更新链路。

## 5. 推荐方案（分层，不依赖任何单一方式）

### 5.1 制品层（唯一真相）

- **Canonical artifact**：`ClipBridge-<ver>-win-x64.zip`（便携目录，含 exe + 依赖 + Assets）。
- 由它派生：`ClipBridge-<ver>-win-x64.msi`（双用途）、（未来）`ClipBridge-<ver>-win-x64.msix`。
- macOS：`.dmg`（内含 .app）；Android：`.apk`。
- **同一版本号、同一构建流水线**产出三端制品（GitHub Actions matrix）。

### 5.2 安装方式层（让用户选，而不是我们替他决定）

| 用户类型 | 推荐方式 | 说明 |
|---|---|---|
| 普通用户 | **MSI per-user**（默认，免 UAC） | 开始菜单/卸载项齐全；数据保留 |
| 企业/共享机 | **MSI per-machine** | 管理员安装，写 Program Files |
| 技术用户/绿色版 | **便携 ZIP** | 解压即用；可选"数据随 exe"模式 |
| 包管理用户 | **winget / Scoop** | 一条命令安装与升级 |
| 商店用户 | **Microsoft Store（MSIX）** | 后续可选；免费注册 |

### 5.3 更新层（核心自研，不依赖渠道）

- **通道**：`stable` / `beta`，清单文件（JSON）放在 GitHub Releases + 官网镜像。
- **检查**：启动后延迟 N 分钟或每日一次，GET 清单（HTTPS）。
- **校验**：更新包携带 **SHA-256 + Ed25519 签名**（公钥内置在应用里），不依赖代码签名证书；防止更新通道被劫持。
- **下载**：支持**差分更新**（bsdiff/Courgette 或按块）——241 MB 全量对用户太贵；至少支持"仅下载变化的文件"（文件级差量）。
- **安装**：Windows 静默跑新 MSI（`/qn`）或替换目录；macOS 用 Sparkle 或自研 helper；Android 下载 APK 触发系统安装器。
- **回滚**：保留上一版本目录/安装包；失败自动回滚。
- **不做**：裸 HTTP 更新、无签名更新、静默强更（至少给用户"稍后"）。

### 5.4 签名与信任（分阶段）

| 阶段 | 做法 | 成本 |
|---|---|---|
| 现在 | 不买证书；更新包用 Ed25519 自校验；安装包未签名（SmartScreen 提示一次） | 0 |
| 有条件时 | **Azure Artifact Signing** 签 MSI/EXE/MSIX | ~$10/月 |
| 想上 Store | 个人账号免费注册；MSIX 可用 Store 代签，或 MSI/EXE 路径需 CA 链证书 | 0 或 ~$10/月 |

### 5.5 渠道层（按性价比排序）

1. **GitHub Releases**（主渠道，免费，可放全量+差分+清单）
2. **官网下载页 + 国内镜像**（你的用户群）
3. **winget manifest**（提 PR 到 winget-pkgs；`winget install` 可用）
4. **Scoop manifest**（便携用户；bucket 提 PR）
5. **Microsoft Store**（后续；个人免费；MSIX）
6. 企业渠道（Intune/SCCM）：等有企业用户再接，MSI 已具备基础

## 6. 为什么不选其他路线（明确取舍）

| 路线 | 不选的原因 |
|---|---|
| 纯 Store/MSIX 作为唯一渠道 | 审核/签名/数据语义/内网能力；且用户群不都在 Store |
| 纯便携 | 无卸载/开始菜单/自动更新，普通用户不友好 |
| Web/PWA | 无法组播、托盘、大文件缓存；核心功能做不了 |
| ClickOnce | 生态老、只 .NET/Windows、macOS/Android 无对应 |
| 单文件自解压 | 241 MB 每次启动解压，启动变慢；更新要全量 |
| 容器 | GUI 托盘应用不适合 |
| Chocolatey 为主 | 社区源、安全审核弱于 winget；可作为补充 |

## 7. 落地顺序

| 阶段 | 交付 | 验收 |
|---|---|---|
| **P0** | 更新器（清单+签名校验+文件级差量+回滚）；MSI 双用途；便携 ZIP；发布脚本 | 旧版能自动升到新版；断网/篡改包被拒；回滚可用 |
| **P1** | winget + Scoop 清单；官网/镜像发布页；版本通道 stable/beta | `winget install` 成功；两个通道可切 |
| **P2** | Azure Artifact Signing（~$10/月）；MSIX 可选 + Store 免费个人账号 | 安装无 SmartScreen；Store 审核通过（可选） |
| **P3** | 差分更新优化（块级）、灰度发布、更新失败遥测（可选） | 更新带宽下降 ≥70% |

## 8. 一句话结论

> **CB 的正确形态是"一个可自更新的桌面应用 + 多种安装外壳 + 多个分发渠道"，而不是"选一个安装格式"**。
> 安装格式（MSI/MSIX/便携）是给用户的**入口选项**；更新链路必须由我们自己掌握（签名校验 + 差分 + 回滚），
> 这样无论用户从官网、winget 还是 Store 进来，体验和安全性都一致。

---

> Windows 的落地设计（四渠道、版本、签名、验证计划）见 [ADR-004](adr/004-windows-distribution-channels.md)。

