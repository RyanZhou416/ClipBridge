# ADR-001：认证方案从 OPAQUE 迁移（移除 opaque-ke）

- 状态：**待决策**（本文件只做方案与取舍记录，不含实现）
- 日期：2026-11-05
- 背景来源：依赖风险盘点（`cargo deny` + `cargo tree -d`）

## 1. 为什么必须处理

整个 Rust 依赖树里，**`opaque-ke` 是唯一一个"旧一代密码学栈"的来源**，且它一家造成四个问题：

| 问题 | 证据 |
|---|---|
| 唯一的未解漏洞告警 | `crypto-bigint 0.5.5`（RUSTSEC-2026-0387，常数时间时序可变性），来源链 `opaque-ke → elliptic-curve 0.13.8 → crypto-bigint` |
| 两代 RustCrypto 并存 | `digest 0.10 + 0.11`、`rand 0.8 + 0.10`、`rand_core 0.6 + 0.10`、`getrandom 0.2 + 0.4`。新一代本来已由 `quinn-proto`（rand 0.10）与 `argon2 0.6`（digest 0.11）引入 |
| 锁死两个直接依赖 | `sha2` 只能留 0.10、`rand`/`rand_chacha` 只能留 0.8/0.3，否则编译不过（已实测，见 `cb_core/Cargo.toml` 注释） |
| 逼迫跟预发布 | 为"跟到最新"曾精确锁定 `=4.1.0-pre.2`，已回滚为 `^4.0.1`；它自身仍只是 4.x |

## 2. 我们的威胁模型（决定了 PAKE 是否必要）

- 通信双方是**同一个用户的自己两台设备**，共享同一个 `account_password`；
- 密码在**两端都明文存在**（`CoreConfig.account_password`）；
- 已有设备身份：每端自签 TLS 证书 + `trusted_peers` 表按 `fingerprint_sha256` 固定（TOFU）；
- 传输是 TLS 1.3 over QUIC（`rustls 0.23` + `quinn 0.11`）。

OPAQUE 的核心卖点是"**服务端不存储密码等价物**，防脱库后离线爆破"。但我们的"服务端"就是用户自己的另一台设备 —— 它本来就有密码。**这个卖点对本项目价值极低。**

我们真正需要的是：**基于共享密码的相互认证 + 会话密钥 + 防中继**。

## 3. 两个候选方案

### 选项 A：换成 Noise（`snow 0.10.0`）

- 模式建议 `Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s`，PSK 由 Argon2id(密码) 派生。
- 优点：保留"密码不直接上链路"的 PAKE 风格；`snow` 活跃维护。
- 缺点（实测其依赖清单）：
  - 它自己仍是 **0.10 一代**（`chacha20poly1305 ^0.10`、`blake2 ^0.10`、`sha2 ^0.10`、`getrandom ^0.3`）→ **digest 双代无法收敛**；
  - 新增一个安全关键依赖 + 一套新的握手状态机；
  - 仍需自行设计通道绑定（防中继），工作量与 B 相当。

### 选项 B（推荐）：TLS 1.3 双向认证 + 密码确认

把认证拆成两个正交因子，全部用**已有依赖**实现：

1. **设备因子**：把 `client_auth_mandatory` 从 `false` 改为 `true`（`transport.rs` 的
   `RequestClientCertVerifier` 已具备 `offer_client_auth=true`），继续用 `trusted_peers`
   指纹做 TOFU 固定。
2. **密码因子**：`PSK = Argon2id(account_password, salt="ClipBridge-Auth-PSK-v1", m=64MiB, t=3, p=1, out=32)`，
   双方各自推导；在 TLS 建立后交换 `HMAC-SHA256(PSK, transcript)` 做双向"密钥确认"。
   - Argon2id：`argon2 0.6` **已是依赖**（`cert.rs` 已在用它做 KEK）；
   - HMAC：`ring 0.17` **已是依赖**（`ring::hmac`），无需新增任何 crate。
3. **通道绑定**（防中继，必须）：`transcript = "ClipBridge-Auth-v1" || PROTOCOL_VERSION ||
   SHA256(本端证书DER) || SHA256(对端证书DER) || tls_exporter_bytes || c1 || c2`，
   其中 `tls_exporter_bytes` 来自 rustls RFC-5705 exporter（若 quinn 未暴露，则退化为
   双方证书指纹 + 双方随机数 + 协议版本，并在注释里写明这一降级）。

**收益（可量化）**：

- 直接删除依赖：`opaque-ke`、`rand`、`rand_chacha`（`rand` 仅被 OPAQUE 使用，实测无其他调用点）；
- 整树消失：`voprf`、`elliptic-curve`、`crypto-bigint`、`generic-array 0.14`、`curve25519-dalek`、
  `rand_core 0.6`、`getrandom 0.2`（随链消失）；
- `sha2` 可升 **0.11**（与 `argon2 0.6 → blake2 0.11` 同代）→ `digest` 双代收敛为单代；
- **`cargo deny advisories` 首次全绿**；
- **零新增依赖**，代码量比 A 小得多（无需引入新的握手协议实现）。

**代价/风险**：

- 失去 OPAQUE 的"服务端不存密码等价物"属性 —— 对本项目无实际损失（见 §2）；
- 首次配对语义要定清楚：未知证书 + 正确的密码确认 → 是否自动 pin（见 §4.6）；
- 这是**安全关键改动**，必须配负例测试，不能只测happy path。

## 4. 若选 B，必须先定死的 6 个设计点

1. **PSK 派生**：Argon2id，`m=64MiB, t=3, p=1, out=32`，盐为固定域分离串
   `"ClipBridge-Auth-PSK-v1"`（**不是秘密**，只为把该用途与其他 Argon2 用途隔开）。
   派生**每进程一次**并缓存，不进握手热路径（64MiB 约 100~200ms）。
2. **确认结构**：`client_proof = HMAC(PSK, transcript || "C")`、
   `server_proof = HMAC(PSK, transcript || "S")`，两侧分别校验；两侧 proof 不可互换。
3. **通道绑定**：见 §3 第 3 点。必须绑定到"这两个证书 + 这次 TLS 会话"，否则把 A 的握手
   转接到 B 的连接上（中继）就成立。
4. **消息封装**：`proto.rs` 里 `OpaqueStart/OpaqueResponse/OpaqueFinish` 改名为
   `AuthStart/AuthResponse/AuthFinish`（或一条 `AuthExchange{step,payload}`），
   `PROTOCOL_VERSION` 加 1 —— 老对端在帧层就被拒，而不是走到半截才失败。
5. **失败时机**：证书不受信 → TLS 阶段失败；密码错 → 第一个 proof 校验即失败
   （比现在拖到 `handle_opaque_finish` 更早）。`SessionState` 的
   `HandshakeStep::OpaqueResponse/OpaqueFinish` 一并改名。
6. **首次配对（TOFU 边界）**：`trusted_peers` 无该指纹时，只有**密码 proof 校验通过**
   才允许 pin 并继续；否则拒绝且**不写入**指纹（防被陌生设备占坑）。已 pin 的设备指纹
   变化视为高危事件，必须显式拒绝并要求人工清除（现有 `cb_clear_peer_fingerprint`）。

## 5. 测试计划（负例优先）

- 正例：两端同密码 + 各自自签证书 → 握手成功，双方 session 状态为 `AccountVerified`。
- **密码错** → 失败，且 `trusted_peers` 不新增记录。
- **PSK 相同但 transcript 不同**（通道绑定）→ proof 校验失败（模拟中继）。
- **证书指纹变化**（已 pin 的设备）→ 拒绝。
- **重放**：重放旧的 proof 到新会话 → 因 transcript 含本次证书/随机数而失败。
- 首次配对：未知证书 + 正确密码 → pin 成功；未知证书 + 错误密码 → 拒绝且不 pin。

## 6. 迁移步骤（若批准）

1. `proto.rs`：重命名认证消息 + `PROTOCOL_VERSION += 1`。
2. `transport.rs`：`client_auth_mandatory` 改 `true`；暴露对端证书 DER 与（如可用）exporter。
3. 新增 `cb_core/src/auth.rs`：PSK 派生 + transcript 构造 + proof 生成/校验（用 `ring::hmac`）。
4. `session/actor.rs`：用新状态机替换 `start_opaque_login` / `handle_opaque_response` /
   `handle_opaque_start` / `handle_opaque_finish`；保留 `perform_tofu_check_async` 并接入 §4.6。
5. `crypto.rs`：删除 OPAQUE 相关类型与种子推导；`p2p_get_server_registration` 一并删除。
6. `Cargo.toml`：删 `opaque-ke`、`rand`、`rand_chacha`；`sha2` 升 0.11；注释同步更新。
7. 文档：`ClipBridge项目核心文档.md` 的会话/认证小节做**定点更新**（不整篇重写）。

**回滚**：方案 B 会删除 OPAQUE 代码，无法靠配置切回；因此实现应在独立提交里分步落地，
每一步 build/test 绿；若中途发现设计缺陷，`git revert` 对应提交即可回到 `2ce326c`。

## 7. 需要 owner 拍板的点

- [ ] 选 B（mTLS + 密码确认，零新依赖）还是 A（Noise/snow，保留 PAKE 风格）？
- [ ] 首次配对是否允许"未知证书 + 正确密码 → 自动 pin"？
- [ ] `Argon2id` 参数 `m=64MiB,t=3,p=1` 是否可接受（每进程一次，约 100~200ms）？
- [ ] 是否同意 `PROTOCOL_VERSION` 加 1（老对端直接拒绝，不做兼容）？
