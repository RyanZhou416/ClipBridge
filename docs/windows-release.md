# Windows 手动触发自动打包与发布规范

- 状态：按本规范实施并进行本地审查，真实 GitHub 发布、生产签名与正式审批尚未验收。不得据此宣称正式发布已可用。
- 日期：2026-11-17。
- 范围：GitHub Actions 手动构建并发布 x64 机器级 MSI、用户级 Burn EXE、便携 ZIP。
- 明确排除：MSIX、StoreUpload、Microsoft Store、winget 的 GitHub 构建、验证、清单生成、上传及提交。相关本地脚本继续保留。
- 相关：[持续集成规范](<ci-architecture.md>)、[现有 Windows 构建命令](<windows-build.md>)、[渠道实现记录](<adr/005-windows-distribution-implementation.md>)。

## 1. 发布需求与渠道边界

维护者手动决定何时发布、发布哪个提交和哪些格式；GitHub Actions 自动完成检查、构建、封装、最终校验、上传和按指定方式公开 Release。不要求维护者在本地重复打包、下载 artifact 后手工上传，也不把普通 push 当作发布授权。

| 可选择的交付物 | 面向用户的文件名 | 内部构建依赖 |
|---|---|---|
| 机器级 MSI | `ClipBridge-<version>-win-x64-machine.msi` | Rust DLL、Windows publish/stage、WiX |
| 用户级 EXE | `ClipBridge-<version>-win-x64-user.exe` | 同上，以及嵌入 EXE 的 per-user MSI、Burn |
| 便携 ZIP | `ClipBridge-<version>-win-x64-portable.zip` | Rust DLL、Windows publish/stage、便携标记与说明；不需要 WiX |

- 内部 user MSI 不是第四个发布选项，不出现在公开 Release 资产中。
- MSIX/StoreUpload 和 winget 没有发布表单选项或隐藏依赖；不调用其本地脚本、不上传其测试包/清单、不使用 Partner Center 或 winget token。
- GitHub 已发布的 MSI/EXE 可以被维护者在本地用于后续 winget 操作，但那是 GitHub 工作流之外的流程。
- 初期只提供 x64，不根据脚本中已有的参数选项就宣称 arm64 已具备交付能力。

## 2. 用户操作与输入

[Release Windows 工作流](<../.github/workflows/release-windows.yml>) 仅使用 `workflow_dispatch`；已在工作区实现，尚未推送或执行真实发布。

### 2.1 操作入口

```text
GitHub → Actions → Release Windows → Run workflow

工作流定义：main
发布来源：main / 完整提交 SHA / 已有 Windows 版本 tag
预期版本：三段数字版本

[✓] 机器级 MSI
[✓] 用户级 EXE
[✓] 便携 ZIP

发布方式：草稿 / 预发布 / 正式发布
允许未签名公开预发布：默认关闭
```

GitHub 表单用独立布尔输入选择格式，不需要自建前端。点击运行并选择预发布或正式发布，即表达公开发行意图；若仓库设置了环境审批，特权 job 仍须等待审批，但不增加强制的手工下载/上传步骤。

### 2.2 输入契约

| 输入 | 类型 / 默认值 | 校验 |
|---|---|---|
| `source_ref` | 字符串，默认 `main` | 只允许 main、完整 SHA 或 `clipbridge-windows-*` tag；必须能解析为属于本仓库 main 历史的提交 |
| `expected_version` | 必填字符串 | 必须为三段数字，等于所选提交 [Version.props](<../platforms/windows/ClipBridgeShell_CS/Version.props>) 的值，并通过 [版本校验脚本](<../scripts/version.ps1>)；不是构建覆盖值 |
| `machine_msi` | boolean，默认 true | 与其他格式共同组成选择集合 |
| `user_exe` | boolean，默认 true | 自动包含内部 user MSI 的构建，不要求选择 machine MSI |
| `portable_zip` | boolean，默认 true | 可独立发布，无需构建 MSI/EXE |
| `release_kind` | choice，默认 `draft` | 仅 `draft`、`prerelease`、`stable` |
| `allow_unsigned_prerelease` | boolean，默认 false | 仅用于明确接受未签名公开预发布；不能绕过测试、结构校验或正式发布签名要求 |

至少选择一种格式。未知输入值、空版本、无效来源或不支持的架构在下载依赖/编译前失败。表单不提供任意脚本、任意路径、仓库 URL、Store 身份或秘密输入。

## 3. 提交与版本的唯一性

1. 工作流定义只从 main 运行。表单中的发布来源是被构建的源码，不是任意来源的特权工作流定义；从其他 workflow ref 启动直接拒绝。
2. 预检将 `source_ref` 解析成完整 SHA，验证其在 main 历史中，后续 job 只使用这个 SHA。运行过程中 main 有新提交不影响本次发布。
3. 从该 SHA 读取版本源，验证 `expected_version` 一致。版本修改由正常开发或版本管理变更完成，发布工作流不编辑源码、不自动 bump、不临时覆盖 MSI 版本。
4. Release tag 固定为 `clipbridge-windows-<version>`。已有 tag 必须解析到同一 SHA；不一致失败，不移动 tag、不 force push。
5. GitHub 的 `prerelease` 标记不改变安装器数字版本，也不使 Windows Installer 支持 SemVer 后缀。同版本的不同二进制不能靠“从预发布改正式”解决升级排序；二进制变化需要新版本。
6. 0.x 的 MSI/EXE/ZIP 可以按既有版本规则构建；本流程不施加 Store 版本限制，也不为了本地 MSIX 抬高产品版本。

`release-please` 若继续使用，只准备版本/变更日志；实施时显式设置 `skip-github-release: true`，停止它的 GitHub Release 及关联 tag 创建，不另用脚本补打同名前缀 tag。`clipbridge-windows-*` 的自动创建权只属于本手动工作流；已有 tag 仅按上述 SHA 校验规则复用。验收须证明合并版本变更本身不会生成 tag/Release。本工作流是 Windows 二进制公开发行的唯一自动化入口；旧全 tag 发布工作流已在本次工作区重构中移除，由 [手动工作流](<../.github/workflows/release-windows.yml>) 替代，尚未执行远程发布验收。

## 4. 工作流结构与权限

采用一个手动工作流内的固定 job，不创建每种包一套发布流水线，也不从普通 PR artifact 晋级。将候选暂存与公开拆开，是为了先展示完整候选，再进行正式发布审批，而不是让审批发生在构建前。

```text
preflight：确定 build-new / reuse-candidate / already-complete
  ├─ build-new → verify → build ─────┐
  ├─ reuse-candidate → verify ───────┤
  │                                 ↓
  │                               stage
  │                    核验/补齐候选，输出不可变摘要
  │                                 ↓
  │                  draft：结束；公开：publish
  │                         stable 先经过环境审批
  └─ already-complete：验证现有状态与 hash 后结束
```

| job | 职责 | 权限与输出 |
|---|---|---|
| `preflight` | 检查输入、来源、版本、选择集合、已有 tag/release；固定 SHA、操作分支及候选来源。只有新构建才检查签名服务是否已配置 | 当前实现为读取草稿配置 `contents: write` + `actions: read`，但 API context 强制 ReadOnly、在网络调用前拒绝 POST/PATCH；能否收紧为 contents: read 待 hosted 实测，不宣称已证明最小权限 |
| `verify` | 新构建或候选复用均对固定 SHA 运行共享核心质量检查和适用的最新依赖政策；不因复用候选忽略过期例外或新增的阻断漏洞 | 只读；检查结果与日志。可运行测试编译，但不能重打待发布的包 |
| `build` | 仅 `build-new` 执行：Windows 原生库、C# Release 测试、依赖检查、所选包构建/签名及最终验证 | 默认只读；需要签名时使用受保护签名环境；输出本次候选 artifact、验证记录 |
| `stage` | 按明确来源取得新/旧候选，校验版本、包 hash、原 Windows 验证证据及签名；复用时按源提交恢复配置、重新检查适用依赖政策但不重建最终包；创建/补齐草稿或核验已有公开预发布 | `contents: write`；读取原候选时增加 `actions: read`；输出 Release ID、候选摘要和审批所需信息。不执行下载的安装器 |
| `publish` | 仅需要公开或晋级时执行：检查 stage 完成与验收证据，审批后重新核对候选未变化，只修改 Release 公开状态/预发布标记/latest | `contents: write`，查询验收记录所需的只读权限；正式发布使用 §7.3 的受保护环境；不上传替换包、不运行安装器 |

`stage` 明确区分 `build` 的计划内跳过与失败：`reuse-candidate` 允许其 skipped，但必须有通过校验的原候选；`build-new` 要求 `verify`、`build` 均成功。新构建与复用分支的门禁不得因条件 job skipped 被整体绕过。

### 4.1 候选来源契约

`preflight` 输出只允许三类操作，不接受任意下载 URL 或任意 run ID 表单输入：

- `build-new`：不存在候选，来源是本次运行成功 `build` 输出的精确 artifact ID。
- `reuse-candidate`：同 tag 已有本流程的完整草稿、部分上传草稿或需要晋级的已签名预发布。来源记录包含候选原 run ID、run attempt、artifact ID、manifest 摘要和各资产 hash。完整候选优先读取 Release 的精确 asset ID；部分草稿仅从原 artifact 补齐。
- `already-complete`：已有公开状态恰好满足本次请求，候选身份、资产和 hash 均符合记录；只输出已完成，不启动构建或写 Release。

跨运行复用仅限本仓库的同一手动发布工作流。必须通过 GitHub 运行记录核对 `workflow_dispatch`、工作流 ref=main、原 `verify/build` 成功，以及原计划中的源码 SHA、版本、格式集合；不能要求原 workflow 整体 success，因为它可能仅在上传或审批阶段失败。来源记录缺失、无法确认原验证成功或实际 hash 不匹配时失败，不回退到“重新构建覆盖”。

若必需原记录/artifact 已过保留期而无法完成上述核对，必须拒绝自动复用。对既有完整公开预发布，晋级只读取和验签现有资产，不接触签名服务，也不因为当前没有签名私钥而重签。

- 日常 CI 与发布共用仓库脚本；初期接受少量工具链 setup 重复，不引入多层 reusable workflow。
- 新候选完整执行共享核心与 Windows 发布所需检查；复用候选要求原构建/测试证据有效，并重新核验现有资产、签名与当前依赖政策，不重建最终包。不为单独 Windows 发布再次构建 Android/macOS，也不宣称已完成三端全量验证。
- 正式发布的签名权限仅向审核过的 main 历史提交开放；普通 push/PR 不获取签名秘密。
- 使用内置 `GITHUB_TOKEN` 完成 GitHub Release 操作，不依赖“先建 tag 再靠事件触发下一 workflow”，也不为此要求 PAT。
- 对提交名、版本、说明等使用参数/环境变量传入；不将表单输入插入 shell 源码。
- 所有 job 有超时；发布使用独立并发组 `windows-release`，不取消正在进行的发布。GitHub 可能替换等待中的运行，摘要应将其标为未执行，不能承诺每个点击都会排队完成。

## 5. 构建计划与复用

已有 [打包主脚本](<../scripts/build-windows-installers.ps1>) 支持单格式或 All；[All 包装脚本](<../scripts/build-windows-all.ps1>) 的实际范围是 MSI/EXE/ZIP，不包括 MSIX/winget。

| 所选集合 | 应执行的工作 |
|---|---|
| 仅 machine MSI | publish/stage → heat → machine MSI |
| 仅 user EXE | publish/stage → heat → user MSI → Burn EXE |
| 仅 portable ZIP | publish/stage → 便携 ZIP；不安装 WiX |
| 多种格式 | 一次 publish/stage；需要 MSI 时 heat 一次；复用 stage/heat 构建所选格式 |

主脚本已增加 `-Formats`（`machine-msi`、`user-exe`、`portable-zip`）集合参数，保留现有单格式 wrapper 的 `-Only` 调用兼容性；两者不能同时显式传入。多个格式共用 publish/stage/heat，仅上传所选的公开产物。纯函数契约已测试，真实构建组合及 hosted runner 验收结果另行记录。

Rust 工作区产物由当前 SHA 构建。发布 build 在 [Windows 验证入口](<../scripts/ci/windows.ps1>) 成功后读取其 native DLL hash，再以 `-SkipRust -ExpectedCoreDllSha256 <已测试的hash>` 调用打包主脚本；禁止为同一发布候选再次 cargo 构建后就假设字节相同。

打包在 publish 前核对输入 DLL hash，签名前核对 stage DLL hash；`package-summary.json` 的 `nativeEvidence.inputDllSha256`、`stageDllSha256` 必须等于 Windows 测试证据，并且 `sourceReusedVerified=true`。签名后的 hash 变化由最终资产记录负责，不与签名前值混淆。candidate/stage 均重新检查此来源绑定。普通 CI 的独立打包 job 仍可自行构建，但不能据此宣称复用了另一 job 测过的同一字节。

CI checkout、日志和临时目录按运行隔离，不把任意 `-SkipRust` 或历史 target 缓存当作验证依据。

## 6. 签名政策与封装顺序

以下表格适用于新构建。签名服务尚未选定或配置时，必须如实显示能力边界，不在工作流中虚构成功。已有候选按其实际签名状态和当前验签结果判断，不需要为了复用已签名文件重新访问签名服务：

| 发布方式 | 无生产签名配置 | 已配置生产签名 |
|---|---|---|
| 草稿 `draft` | 可生成未签名候选，说明中标记，不能声称可正式分发 | 生成并验证已签名候选 |
| 预发布 `prerelease` | 仅 `allow_unsigned_prerelease=true` 时允许公开，并在说明中明确未签名及系统提示风险 | 默认签名；签名执行失败不能静默降级 |
| 正式 `stable` | 预检失败，不进行重型构建或公开 Release | 验证签名与时间戳后才能公开 |

该表是本项目的发布质量策略，不表示 GitHub 禁止托管未签名文件。明确允许未签名预发布是为了在生产签名就绪前仍能交付测试版本，而不是放宽正式发布门禁。

已签名构建按封装关系执行：

```text
publish/stage
  → 签名本项目拥有的 EXE/DLL/自定义动作载荷
  → 生成并签名 machine MSI / 内部 user MSI
  → 用已签名 user MSI 生成并签名 Burn EXE
  → 从最终 stage 生成便携 ZIP
  → 验证所有所选最终产物
  → 计算 SHA256
```

- 第三方文件保留其原有签名，不盲目重签所有 DLL；项目自有文件清单明确维护。
- ZIP 本身不套 Authenticode，检查内部项目自有可执行文件的签名，并对最终 ZIP 计算 hash。
- 正式签名必须验证信任链、发布者和时间戳；成功调用签名工具不等于验证通过。
- 私钥/服务凭据由受保护配置提供，不提交、不进入 artifact、不写日志。具体 provider 的接入是实施前置，不能用空签名步骤满足 `stable`。

## 7. 最终验证与上传白名单

### 7.1 构建与结构验证

- 共享核心检查、C# Release 构建/MSTest 必须通过；正向原生初始化不能接受 `Degraded`，必需测试不得零执行。
- 检查包版本与预期版本、CPU 架构、必要原生依赖/资源、MSI 安装范围、Burn 内部载荷、便携标记与空数据布局。
- 所选格式必须全部完成验证；未选择的格式不强制构建。任一所选格式失败，整次新发布停在公开之前。
- 检查最终文件而不是只检查签名前 stage；最终 hash 必须在签名与封装全部完成后计算。
- 本地历史验收不能替代本次候选证据。正式发布还应在可丢弃 Windows 环境验证所选格式的安装/启动/卸载、默认删除与显式保留；存在上一公开版时增加升级保数据验证。便携版验证独占临时目录中的首次启动与数据根。
- 系统级验收记录通过 §7.3 的候选摘要与发布环境审批进入流程，不通过任意 JSON 或表单布尔值绕过。缺证据时正式发布等待审批或阻断；若是草稿则保持草稿，若是公开预发布则仍保持预发布，不能标记 stable 成功。首期不新增专用 VM 集群。WACK 是本地 MSIX 流程的一部分，不进入本 GitHub 发布。

### 7.2 公开资产

固定白名单：

```text
ClipBridge-<version>-win-x64-machine.msi    # 仅选择 machine_msi 时
ClipBridge-<version>-win-x64-user.exe       # 仅选择 user_exe 时
ClipBridge-<version>-win-x64-portable.zip   # 仅选择 portable_zip 时
SHA256SUMS.txt
release-manifest.json
```

其中两份元数据是计划新增的构建输出，并非现有源文件：

- `SHA256SUMS.txt`：覆盖所选最终包与发布记录；不对自身生成循环 hash。
- `release-manifest.json`：记录 schema 版本、提交 SHA、产品版本、tag、架构、所选格式、工具链、签名状态、构建验证结果、源 run ID/attempt，以及每个包的文件名/大小/SHA256；不记录密码、私钥、Store 身份、本机用户名或临时绝对路径。
- 候选摘要为该 manifest 文件字节的 SHA256，不写回自身；它与原候选 artifact ID 由 `build` 输出/运行记录保存，用于后续候选绑定。manifest 生成于构建阶段，不包含未来审批或可变的 Release 状态。
- 发布记录由本流程写入并校验，不接受用户上传任意 JSON 当作可信验收证据。后续审批、系统验收链接和状态转换保存在当前运行与部署记录中，不修改已发布 manifest 或校验和。
- `stage` 每次成功处理候选后生成独立的 256-bit 加密随机审批挑战值 `approval_nonce`。它绑定当前 run/attempt 与候选摘要，写入当次 stage 输出及审批提示，不改变候选 manifest；stage 重跑必须刷新，不能复用上次挑战值。

禁止公开：内部 user MSI、PDB、原始日志、stage、obj、证书私钥、真实用户数据、MSIX/StoreUpload、winget YAML 或其 ZIP。需要的诊断信息只进入短期、经检查的 Actions artifact。

### 7.3 正式发布的验收入口

正式发布，包括“已签名预发布 → 正式发布”，使用受保护环境 `windows-release-stable`。该环境配置 required reviewers；维护者可以担任审核者，不强制引入双人团队。环境不存在、未配置要求的审批规则或无法核实该规则时，正式发布入口预检失败，不自动创建同名空环境绕过保护。

1. `stage` 先准备并核对完整候选。摘要包含 Release 链接、源码 SHA、版本、格式集合、各包 hash、候选摘要、原构建记录、本次目标状态，以及当前随机 `approval_nonce`。
2. 随后的 `approve-stable` job 才进入 `windows-release-stable` 环境审批。此时审核者能够取得实际候选；门禁通过后该 job 回传当前挑战值与 run attempt，`publish` 依赖其成功结果。
3. 自动化系统验收已通过时，提供绑定候选摘要/hash 的报告链接；没有自动化覆盖时，由审核者完成隔离环境验收。审批备注必须包含 `nonce=<当次挑战值>`、`candidate=<候选摘要>`、`run_id=<当前运行>`、`run_attempt=<当前尝试>`、`result=pass`、`record=https://<验收记录>`。缺少记录不能仅点击批准。
4. `publish` 核对当前 attempt 的 stage 与审批 job 的真实状态、挑战值回传、受保护环境和 required reviewer；再通过真实审批历史中的 `state/user/environments/comment` 核对上述字段，并重验资产 hash。旧 nonce 的历史记录不作为授权；没有至少一条合法的当前 nonce 审批、当前 nonce 被拒绝或非授权者使用当前 nonce，均阻断。
5. GitHub review history 不提供可靠的审批时间或 attempt 字段。实现不得编造这些字段：使用每次候选准备后才生成的不可预测 nonce、当前 attempt 的环境门禁结果及 GitHub job 顺序建立绑定。job 启动时间只能作为门禁时序证据，不能声称它就是人工审批时间。
6. 审批通过才将目标状态改为 stable。审批否决/超时不删除候选；未公开草稿保持未公开，已公开预发布不被撤销。

挑战值与审批校验已实现离线契约测试，真实环境门禁、签名和发布仍未验收；不能将 Mock 通过描述为正式发布已激活。草稿和预发布不强制经过此正式审批，但仍遵循构建、签名、hash 和权限规则。

## 8. 草稿、公开与失败恢复

### 8.1 新版本执行顺序

1. 预检完成并固定 SHA、版本、格式集合、签名策略及目标 Release 状态；无候选时选择 `build-new`。
2. 完成所有选定包的构建和检查，将最终候选与机器可读记录上传到本次运行的精确 artifact，保留 30 天供故障恢复。
3. `stage` 重新核对是否已有同 tag 的 Release；无候选时创建草稿，显式将 tag 指向固定 SHA。若与预检观察不同则停止并重新确认状态，不在同次运行擅自接管他人新建的候选。不能先创建公开的空 Release 等待构建。
4. `stage` 按白名单上传并逐项核对文件名、大小和下载 hash，输出 Release ID、候选摘要及报告。草稿上传失败不进入公开步骤。
5. `draft` 到此成功结束；`prerelease` 由 `publish` 按政策自动公开；`stable` 的 `publish` 必须完成 §7.3 的当前候选审批与验收校验，再公开。正式发布等待审批时，草稿已存在且可供验收。
6. 预发布不标记 latest；正式版本仅在不低于当前正式最新版时才更新 latest，避免发布旧版本覆盖入口。
7. 公开后再次检查下载资产。若公开后核验失败，运行报告“已公开但核验失败”，阻断后续操作并通知维护者；不伪装成未发布，也不自动删除/替换公开资产。

### 8.2 重试与重复发布

- 不可变候选身份为 `(version, commit SHA, architecture, formats, signing policy, candidate digest)`；Release 当前状态与本次 `release_kind` 另行比较，不能只因包 hash 相同就断言状态转换已完成。
- 同一运行失败后优先重跑失败的 `stage/publish`，读取原候选 artifact，不重新构建另一套字节；每次写操作前重新核对 Release ID、tag、候选摘要和当前状态。
- 新的手动运行复用候选时遵循 §4.1：`preflight` 固定原来源，`verify` 做当前必要检查，`build` 明确跳过，`stage` 读取原完整资产或从原 artifact 补齐，`publish` 按 §8.3 转换状态。不存在“新运行必须有新构建 artifact”的假设。
- 草稿未上传完整时，只允许从原始候选 artifact 恢复缺失文件。同名同 hash 可跳过；同名异 hash、缺少可信记录、产物过期或候选身份不同直接失败。
- 初期不支持向已公开版本补增另一种格式，避免改变资产集合及校验和；首次选择时应选齐计划交付格式。
- 不执行 `--clobber` 覆盖公开资产、不移动已有 tag。确需重建失败草稿，由维护者明确清理未公开草稿并重新发起；工作流不自动进行破坏性清理。
- 不承诺不同打包时间产生相同字节，因此不能用“同一源码”作为覆盖已有候选的理由。未签名公开预发布若要重新签名，必须用新版本，不能在原版本替换包。

### 8.3 Release 状态转换

所有允许的转换都要求候选身份、签名要求、原验证证据和资产 hash 匹配；状态转换只修改 Release 元数据，不改变安装包、manifest 或校验和。

| 现有状态 | 手动请求 | 处理 |
|---|---|---|
| 无 Release | draft / prerelease / stable | `build-new`，按 §8.1 先完成草稿再处理目标状态 |
| 完整草稿 | draft | 核验候选后保留草稿，不重复构建/上传 |
| 完整草稿 | prerelease | `reuse-candidate`，满足签名/未签名预发布政策后公开 |
| 完整已签名草稿 | stable | `reuse-candidate`，通过本次 §7.3 正式验收和环境审批后公开 |
| 未签名草稿 | stable | 拒绝原候选晋级；不能隐式重签替换。若尚未公开，维护者可明确清理后重建已签名候选 |
| 已公开预发布 | prerelease | 核验身份、hash 与请求政策均一致后返回 `already-complete` |
| 已签名公开预发布 | stable | `reuse-candidate`；重新执行本次正式验收/环境审批，校验资产未变后 PATCH `prerelease=false`，按版本规则处理 latest |
| 未签名公开预发布 | stable | 拒绝；使用新版本生成已签名候选 |
| 已公开正式版 | stable | 核验身份和 hash 后返回 `already-complete`，不重复上传 |
| 任一公开版 | draft；或正式版请求 prerelease | 拒绝状态倒退，不自动撤回、重新隐藏或降级公开版本 |

部分上传草稿必须先恢复成完整候选才能按表转换。任何不在表内的转换失败而非猜测。一次 stable 晋级失败后，重试仍需核对当前状态：若已变为 stable 且资产未变，则仅完成公开后核验；若仍为 prerelease，则继续受正式审批约束。

这是对本发布候选的有限恢复与状态管理，不是通用的跨 CI 运行产物晋级系统。

## 9. 与日常 CI 和本地渠道的关系

- 普通 push/PR：只运行受影响代码检查；打包输入变化时检查相关 MSI/EXE/ZIP，结果是测试 artifact，不创建 Release。
- 手动全量 CI：验证当前 GitHub 支持范围，不发布，不包含 MSIX/winget。
- 手动 Windows 发布：新候选不依赖最近一次 CI 绿色，必须执行指定提交的共享核心/Windows 检查及所选包构建；复用原候选则严格核对原检查证据并执行当前政策校验，不能重建替换最终资产。只有明确的目标状态请求和相应审批可以使结果公开或晋级。
- MSIX、StoreUpload、winget：完全保留在本地脚本/人工渠道流程中。无需为它们配置 GitHub 环境身份、签名、API 或自动提交凭据。
- 既有文档中的历史命令或“上传四个产物”提示不改变本规范的公开白名单；实施时应同步修正文案，但不能为此删除仍被 EXE 使用的内部 MSI。

## 10. 实施与验收

以下是最终验收清单；本地实现和 Mock/夹具测试不能代替真实托管运行、签名、审批或公开发布。未勾选项仍需相应证据。本轮未配置生产凭据、未修改仓库权限，也未创建或公开任何 Release/tag。

- [ ] 只有手动入口；普通 push、tag、release-please 不自动启动二进制公开发行。release-please 显式关闭 release/tag 创建，版本合并后不生成同名 tag。
- [ ] 表单只提供 MSI/EXE/ZIP；至少选一项，任意单项或组合可用；仅 ZIP 不要求 WiX。
- [ ] `source_ref` 固定为 main 历史中的 SHA；版本不一致、tag 指向不一致、未知格式在编译前失败。
- [ ] 共享构建入口支持格式集合，多个格式复用 publish/stage/heat；不将未选格式上传。
- [ ] 代码、原生集成、数据隔离和所选包验证失败时均不公开；不能以最近一次部分 CI 绿色替代本次验证。
- [ ] 签名缺失/失败遵循 §6；未经明确允许不公开未签名预发布，正式版不静默降级。
- [ ] 生产签名按载荷→MSI→Burn 顺序；ZIP 内项目自有程序验签；最终 hash 在全部处理之后生成。
- [ ] `stage` 先生成可验收候选，`publish` 才进入 `windows-release-stable` 审批；未配置 required reviewers、缺审批记录、旧审批或候选 hash 变化均不能正式发布。
- [ ] 正式发布的系统级验收绑定当前运行与候选 SHA/hash；没有证据则保持草稿/预发布状态并明确阻断原因。
- [ ] 草稿资产齐全并核对下载 hash 后才公开；内部 user MSI、日志、私钥、MSIX/winget 不在白名单。
- [ ] `build-new`、`reuse-candidate`、`already-complete` 三条路径都有用例；复用时 `build` 跳过、`stage` 仍能按精确来源核验完整候选，不读取“最新 artifact”。
- [ ] 重试复用原候选；同名不同 hash、来源记录缺失、版本冲突或候选过期明确失败。
- [ ] §8.3 状态表逐项验证：已签名预发布转正式必须重新审批，未签名预发布不能原地变正式，公开状态不能倒退。
- [ ] 同身份、同目标状态的已公开发布可安全重复查询；不会静默改包、追加未选格式或移动 tag。
- [ ] 日常 CI 与发布并发互不取消；构建与发布权限分离；失败摘要能够区分草稿未公开和公开后核验失败。
- [ ] 使用测试草稿验证流程，不为验证架构而自动公开测试版本；真实发布另行由维护者手动启动。

## 11. 参考

- [GitHub：手动运行工作流](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)：`workflow_dispatch` 是人工选择入口。
- [GitHub：管理部署环境](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)：对签名/发布权限设置可选审批与受保护配置。
- [GitHub：Release API](https://docs.github.com/en/rest/releases/releases)：草稿、公开状态、target commit 和 Release 元数据。
- [GitHub：Release assets API](https://docs.github.com/en/rest/releases/assets)：资产上传/下载核验；本项目额外禁止覆盖公开版本。
