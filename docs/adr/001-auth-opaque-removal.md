# ADR-001：认证方案 —— 保留 OPAQUE（否决替换方案）

- 状态：**已决策：保留 `opaque-ke`**（原稿曾推荐"移除 OPAQUE"，因安全论证有误已推翻）
- 日期：2026-11-05
- 背景来源：依赖风险盘点（`cargo deny` + `cargo tree -d`）

## 1. 原始问题（仍然成立）

`opaque-ke` 是整棵依赖树里**唯一**的"旧一代密码学栈"来源，一家造成四项负担：

| 问题 | 证据 |
|---|---|
| 唯一的未解告警 | `crypto-bigint 0.5.5`（RUSTSEC-2026-0387） |
| 两代 RustCrypto 并存 | `digest 0.10+0.11`、`rand 0.8+0.10`、`rand_core 0.6+0.10`、`getrandom 0.2+0.4` |
| 锁死两个直接依赖 | `sha2` 只能 0.10、`rand`/`rand_chacha` 只能 0.8/0.3 |
| 曾被逼跟预发布 | `=4.1.0-pre.2`（已回滚为 `^4.0.1`） |

## 2. 为什么不能换（推翻原稿的关键）

原稿提出两个替换方案，**两者都会实质削弱"陌生设备配对"的安全性** —— 而这正是 OPAQUE 存在的理由。

### 方案 B（原推荐）：TLS 1.3 双向认证 + `HMAC(PSK, transcript)` 确认

- `PSK = Argon2id(password)`，而 `transcript` 必然包含**握手双方可见的公开材料**（证书、随机数、版本）。
- 一旦攻击者拿到 `transcript` 与对应 `proof`（首次配对是 TOFU 场景，中间人可以终止 TLS 并冒充服务端），
  他就能**离线穷举密码**：对每个候选密码算 `Argon2id → PSK → HMAC → 比对`。
- Argon2id 只让每次尝试变慢，**不能阻止离线攻击** —— 这是"密码验证器泄漏"的经典错误。

### 方案 A：Noise `XXpsk3` + 密码派生 PSK

- 被动监听者能看到双方临时公钥 `e1`、`e2`，因此可自行算出 `ee`；
- 用 `ee` 解密 msg2 可拿到响应方静态公钥 → 算出 `es`；
- msg3 同理暴露发起方静态公钥 → 算出 `se`；
- 于是**除 PSK 之外的所有 DH 秘密都可算**，攻击者可对 PSK 做**离线穷举**（试解 msg3）。
- Noise 的 PSK 模式假设 PSK 是**高熵**的；把低熵密码塞进 PSK 是误用。

### 结论

**抗离线字典攻击只有 aPAKE 能给**（OPAQUE / SRP / SPAKE2）。本项目未来会出现陌生设备，
密码就是配对凭据，因此 `opaque-ke` 必须保留。SPAKE2 唯一的 Rust 实现
（`spake2 0.5.0-pre.0`）仍是预发布，没有更好的替代。

## 3. 告警如何收窄（不是忽略，是限定范围）

引入链是 `opaque-ke → elliptic-curve 0.13.8 → crypto-bigint 0.5.5`。
`elliptic-curve` 是 opaque-ke 的**非可选**依赖，但它只服务于 NIST 曲线选项
（`p256` / `p384` / `p521`）。我们的套件是：

```rust
type OprfCs      = opaque_ke::Ristretto255;                    // curve25519-dalek
type KeyExchange = TripleDh<opaque_ke::Ristretto255, Sha512>;  // Sha512
```

**从不实例化 NIST 曲线** → `crypto-bigint` 的常数时间字段算术（即 RUSTSEC-2026-0387
描述的可变时代码路径）在本配置下**不可达**。

据此在 `deny.toml` 对该 advisory 做**带理由的 ignore**，并在 `crypto.rs` 写入约束注释：
**若将来把 cipher suite 换成 NIST 曲线，必须重新评估该 advisory（不得静默切换）**。

## 4. 已修复的真实安全缺口：密码拉伸（KSF）

**决策：已启用**，参数取 OWASP 最低档 `Argon2id(19MiB, t=2, p=1)`，实现见
`crypto.rs` 的 `Argon2idKsf` + `type Ksf = Argon2idKsf`。

- **原缺口**：`type Ksf = opaque_ke::ksf::Identity` 等于把密码拉伸整个关掉。KSF 的作用是
  在 OPRF 之前抬高离线猜测成本，对"用户自选密码 + 陌生设备配对"是实打实的削弱。
- **实现方式**：用项目已有的 `argon2 0.6` 自己实现 `Ksf`（`opaque-ke` 公开导出了 `ksf`、
  `errors`，并 `pub use generic_array`），**零新增依赖**；不启用 `opaque-ke` 自带的 argon2
  实现，以免再引入一份 `argon2 0.5`。
- **盐**：固定域分离串 `"CB-OPAQUE-KSF-v1"`（不是秘密，只用于与其他 Argon2 用途隔开）。
- **参数固定**在常量里：`Ksf` trait 要求 `Default`，且参数一旦变化会让已注册的
  registration record 全部失效（协议级变更）。

### 配套：注册记录缓存

`p2p_get_server_registration()` 内部会跑完整注册流程（`ClientRegistration::start/finish`
各调用一次 KSF），且它在**每次握手**（`handle_opaque_start`）都会被调用。因此加了进程内缓存：

- 键 = `SHA-512(password)` 前 16 字节 hex（不放明文密码进静态数据；改密码后自动失效）；
- 命中时 clone `ServerSetup` / `ServerRegistration` 返回。

### 实测开销（含两次 KSF）

| 构建 | 首次 | 命中缓存 |
|---|---|---|
| debug | 237 ms | 22.8 µs |
| **release** | **11.1 ms** | **2 µs** |

即开启后每次登录的真实代价约 **11ms**（服务端侧），客户端侧约 5ms —— 不构成性能问题。
（这比设计时估的 100~200ms 低一个量级，因为 19MiB/t=2 单次在 release 下仅约 5ms。）

## 5. 决策与后续

- [x] **保留 `opaque-ke`**，不换 Noise、不换成 mTLS+HMAC（安全论证见 §2）。
- [x] 保持 `^4.0.1` 稳定版，不锁预发布。
- [x] `RUSTSEC-2026-0387` 按 §3 收窄后在 `deny.toml` 显式 ignore，并写明"换成 NIST 曲线须重新评估"。
- [x] 启用 `Argon2id KSF`（19MiB / t=2 / p=1）并加注册记录缓存（§4）。
- [ ] 待上游：`opaque-ke` 升到 RustCrypto 0.11 一代后，`sha2`/`rand` 可跟进、`crypto-bigint` 告警自然消失。
- [ ] 后续可做（与认证正交）：`PROTOCOL_VERSION` 加 1，使 KSF 变更后的老对端在帧层被明确拒绝，
      而不是握到一半才失败。
