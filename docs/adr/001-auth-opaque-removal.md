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

## 4. 遗留的真实安全缺口（比告警更值得处理）

`crypto.rs` 当前是：

```rust
type Ksf = opaque_ke::ksf::Identity;   // ← 密码拉伸是关掉的
```

OPAQUE 的 KSF 是对密码做 OPRF 前的拉伸，用于**提升离线猜测成本**。
`Identity` 等于完全没有拉伸 —— 对"用户自选密码 + 陌生设备配对"来说是实打实的削弱。

可选修法（零新增依赖：`opaque_ke` 公开导出了 `ksf`、`errors`，并 `pub use generic_array`）：

```rust
#[derive(Default)]
pub struct Argon2idKsf;

impl opaque_ke::ksf::Ksf for Argon2idKsf {
    fn hash<L: opaque_ke::generic_array::ArrayLength<u8>>(
        &self,
        input: opaque_ke::generic_array::GenericArray<u8, L>,
    ) -> Result<opaque_ke::generic_array::GenericArray<u8, L>, opaque_ke::errors::InternalError> {
        // 用我们已有的 argon2 0.6（cert.rs 已在用），不引入 opaque-ke 的 argon2 0.5。
        // Argon2id(m=64MiB, t=3, p=1)，固定域分离盐 "CB-OPAQUE-KSF-v1"。
        // ...
    }
}
```

需要 owner 决定的取舍：

- **登录额外延迟**：Argon2id 64MiB/t=3 约 100~200ms；而 `p2p_get_server_registration`
  在每次握手里都会被调用（服务端侧），实际可能增加 200~400ms/次登录。
  折中可用 `m=19MiB, t=2`（OWASP 最低档，约 40~60ms）。
- **会使已有 registration record 失效**：KSF 变了派生结果就变。项目尚未发布，没有历史包袱 ——
  正是现在改的时机。

## 5. 决策与后续

- [x] **保留 `opaque-ke`**，不换 Noise、不换成 mTLS+HMAC（安全论证见 §2）。
- [x] 保持 `^4.0.1` 稳定版，不锁预发布。
- [x] `RUSTSEC-2026-0387` 按 §3 收窄后在 `deny.toml` 显式 ignore，并写明"换成 NIST 曲线须重新评估"。
- [ ] **待定**：是否启用 `Argon2id` KSF（§4），以及参数档位（64MiB/t=3 vs 19MiB/t=2）。
- [ ] 待上游：`opaque-ke` 升到 RustCrypto 0.11 一代后，`sha2`/`rand` 可跟进、`crypto-bigint` 告警自然消失。
