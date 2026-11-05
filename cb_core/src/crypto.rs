// cb_core/src/crypto.rs

use opaque_ke::{
	ciphersuite::CipherSuite, key_exchange::tripledh::TripleDh, ClientRegistration,
	ClientRegistrationFinishParameters, ServerRegistration, ServerSetup,
};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use sha2::{Digest, Sha512};
use std::convert::TryInto;
use std::sync::{Mutex, OnceLock};

// --- 0. 密码拉伸（OPAQUE KSF）---
//
// opaque-ke 默认的 Ksf 是 Identity（= 完全不拉伸）。对"用户自选密码 + 陌生设备配对"来说
// 这是实打实的削弱：KSF 的作用就是在 OPRF 之前抬高离线猜测的成本。
//
// 这里用项目已有的 argon2 0.6 自己实现 Ksf（opaque-ke 只公开导出了 trait 与 generic_array，
// 它自带的 argon2 实现绑的是 0.5，不引入以免多一份旧依赖）。
//
// 参数取 OWASP 最低档：Argon2id m=19MiB, t=2, p=1。
// 盐是**固定的域分离串**（不是秘密）：这里的输入本身就是密码，固定盐只用于把这个用途与其他
// Argon2 用途隔开，不需要唯一性（本项目只有一个账户口令，不存在跨用户彩虹表问题）。
const KSF_M_COST_KIB: u32 = 19 * 1024;
const KSF_T_COST: u32 = 2;
const KSF_P_COST: u32 = 1;
const KSF_SALT: &[u8] = b"CB-OPAQUE-KSF-v1";

/// OPAQUE 的密钥拉伸函数：Argon2id(19MiB, t=2, p=1)。
///
/// 说明：`Ksf` trait 要求 `Default`，所以参数固定写在常量里，不做成可配置项 ——
/// 参数一旦变化，已注册的 registration record 就全部失效，属于协议级变更。
#[derive(Default)]
pub struct Argon2idKsf;

impl opaque_ke::ksf::Ksf for Argon2idKsf {
	fn hash<L: opaque_ke::generic_array::ArrayLength<u8>>(
		&self,
		input: opaque_ke::generic_array::GenericArray<u8, L>,
	) -> Result<opaque_ke::generic_array::GenericArray<u8, L>, opaque_ke::errors::InternalError> {
		let params = argon2::Params::new(KSF_M_COST_KIB, KSF_T_COST, KSF_P_COST, Some(input.len()))
			.map_err(|_| opaque_ke::errors::InternalError::KsfError)?;
		let argon2 =
			argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
		let mut output = opaque_ke::generic_array::GenericArray::<u8, L>::default();
		argon2
			.hash_password_into(&input, KSF_SALT, &mut output)
			.map_err(|_| opaque_ke::errors::InternalError::KsfError)?;
		Ok(output)
	}
}

// --- 1. 定义加密套件 (v4.0 标准) ---
//
// ⚠ 安全约束（改动前必读，依据 docs/adr/001-auth-opaque-removal.md §3）：
// 必须保持 Ristretto255 + Sha512。opaque-ke 无条件依赖 elliptic-curve，而
// crypto-bigint 0.5.5 有一条时序可变性告警（RUSTSEC-2026-0387）—— 受影响的正是
// NIST 曲线（p256/p384/p521）的字段算术。我们从不实例化 NIST 曲线，所以该路径不可达，
// deny.toml 据此对告警做了范围收窄。
// 一旦把 OprfCs / KeyExchange 换成 NIST 曲线，上述豁免立即失效，必须重新评估。
pub struct DefaultCipherSuite;

impl CipherSuite for DefaultCipherSuite {
	type OprfCs = opaque_ke::Ristretto255;
	type KeyExchange = TripleDh<opaque_ke::Ristretto255, Sha512>;
	// 密码拉伸：Argon2id(19MiB, t=2, p=1)，见文件顶部 §0。
	// 不要改回 Identity：那等于关掉离线猜测的成本门槛。
	type Ksf = Argon2idKsf;
}

// --- 2. 类型别名 ---
pub type CbClientLogin = opaque_ke::ClientLogin<DefaultCipherSuite>;
pub type CbServerLogin = opaque_ke::ServerLogin<DefaultCipherSuite>;

pub type CbClientLoginStartResult = opaque_ke::ClientLoginStartResult<DefaultCipherSuite>;
pub type CbServerLoginStartResult = opaque_ke::ServerLoginStartResult<DefaultCipherSuite>;

pub type CbClientLoginState = CbClientLogin;
pub type CbServerLoginState = CbServerLogin;

pub type CbServerRegistration = ServerRegistration<DefaultCipherSuite>;

// --- 3. P2P 辅助：生成服务器验证记录 ---
//
// 这个函数内部会跑一遍完整的注册流程（ClientRegistration::start/finish 都会调用 KSF），
// 开密码拉伸后单次约 2×Argon2id(19MiB) ≈ 80~120ms。而它在每次握手（handle_opaque_start）
// 都会被调用，所以必须缓存 —— 否则每次登录都要白付这笔开销。
//
// 缓存键用 SHA-512(password) 的前 16 字节 hex：既避免把明文密码放进进程静态数据，
// 也能在用户改密码后自动失效（键不同）。
type RegistrationCacheEntry = (
	String,
	ServerSetup<DefaultCipherSuite>,
	CbServerRegistration,
);

static REGISTRATION_CACHE: OnceLock<Mutex<Option<RegistrationCacheEntry>>> = OnceLock::new();

fn registration_cache_key(password: &str) -> String {
	let mut hasher = Sha512::new();
	hasher.update(password.as_bytes());
	hex::encode(&hasher.finalize()[..16])
}

/// 取得本机账户密码对应的 `(ServerSetup, ServerRegistration)`。
/// 同一密码在同一进程内只计算一次（结果被缓存），改密码后缓存自然失效。
pub fn p2p_get_server_registration(
	password: &str,
) -> anyhow::Result<(ServerSetup<DefaultCipherSuite>, CbServerRegistration)> {
	let key = registration_cache_key(password);
	let cache = REGISTRATION_CACHE.get_or_init(|| Mutex::new(None));

	{
		let guard = cache
			.lock()
			.map_err(|_| anyhow::anyhow!("registration cache poisoned"))?;
		if let Some((cached_key, setup, registration)) = guard.as_ref() {
			if cached_key == &key {
				return Ok((setup.clone(), registration.clone()));
			}
		}
	}

	let (setup, registration) = compute_server_registration(password)?;
	{
		let mut guard = cache
			.lock()
			.map_err(|_| anyhow::anyhow!("registration cache poisoned"))?;
		*guard = Some((key, setup.clone(), registration.clone()));
	}
	Ok((setup, registration))
}

fn compute_server_registration(
	password: &str,
) -> anyhow::Result<(ServerSetup<DefaultCipherSuite>, CbServerRegistration)> {
	let mut hasher = Sha512::new();
	hasher.update(password.as_bytes());
	let seed: [u8; 32] = hasher.finalize()[0..32].try_into()?;
	let mut rng = ChaCha20Rng::from_seed(seed);

	let password_bytes = password.as_bytes();
	let identifier = b"clipbridge-user";

	let server_setup = ServerSetup::<DefaultCipherSuite>::new(&mut rng);

	// Client: Start
	let client_reg_start =
		ClientRegistration::<DefaultCipherSuite>::start(&mut rng, password_bytes)?;

	// Server: Start
	let server_reg_start = ServerRegistration::<DefaultCipherSuite>::start(
		&server_setup,
		client_reg_start.message,
		identifier,
	)?;

	// Client: Finish
	let client_reg_finish = client_reg_start.state.finish(
		&mut rng,
		password_bytes,
		server_reg_start.message,
		ClientRegistrationFinishParameters::default(),
	)?;

	// Server: Finish
	let server_registration =
		ServerRegistration::<DefaultCipherSuite>::finish(client_reg_finish.message);

	Ok((server_setup, server_registration))
}

#[cfg(test)]
mod tests {
	use super::*;
	use opaque_ke::{ClientLoginFinishParameters, ServerLoginParameters};
	use rand::rngs::OsRng;

	#[test]
	fn test_p2p_crypto_flow_correctness() {
		let shared_key = "user_secret_key_123456";
		let (server_setup, server_rec) = p2p_get_server_registration(shared_key).unwrap();

		// 1. Client Start
		let mut client_rng = OsRng;
		let client_start = CbClientLogin::start(&mut client_rng, shared_key.as_bytes()).unwrap();
		let ke1_message = client_start.message;

		// 2. Server Handle KE1
		let mut server_rng = OsRng;
		let server_start = CbServerLogin::start(
			&mut server_rng,
			&server_setup,
			Some(server_rec),
			ke1_message,
			b"clipbridge-user",
			ServerLoginParameters::default(),
		)
		.expect("Server failed to start session");

		let ke2_message = server_start.message;

		// 3. Client Handle KE2
		let client_finish = client_start
			.state
			.finish(
				&mut client_rng,
				shared_key.as_bytes(),
				ke2_message,
				ClientLoginFinishParameters::default(),
			)
			.expect("Client failed to finish session");

		let ke3_message = client_finish.message;
		let client_session_key = client_finish.session_key;

		// 4. Server Handle KE3
		let server_finish_result = server_start
			.state
			.finish(ke3_message, ServerLoginParameters::default())
			.expect("Server failed to verify client");

		let server_session_key = server_finish_result.session_key;

		assert_eq!(client_session_key, server_session_key);
		println!("OPAQUE P2P Handshake math checks out!");
	}

	#[test]
	fn test_p2p_crypto_wrong_password_fails() {
		let correct_key = "correct_key";
		let wrong_key = "wrong_key";
		let (server_setup, server_rec) = p2p_get_server_registration(correct_key).unwrap();

		let mut client_rng = OsRng;
		let client_start = CbClientLogin::start(&mut client_rng, wrong_key.as_bytes()).unwrap();

		let mut server_rng = OsRng;
		let server_start = CbServerLogin::start(
			&mut server_rng,
			&server_setup,
			Some(server_rec),
			client_start.message,
			b"clipbridge-user",
			ServerLoginParameters::default(),
		)
		.unwrap();

		let client_finish_res = client_start.state.finish(
			&mut client_rng,
			wrong_key.as_bytes(),
			server_start.message,
			ClientLoginFinishParameters::default(),
		);

		if let Ok(client_finish) = client_finish_res {
			let server_res = server_start
				.state
				.finish(client_finish.message, ServerLoginParameters::default());
			assert!(server_res.is_err(), "Server should reject wrong password");
		} else {
			assert!(client_finish_res.is_err());
		}
	}

	/// 注册记录必须对同一密码确定、且被缓存。
	///
	/// 开密码拉伸后，`p2p_get_server_registration` 内部会跑两次 Argon2id(19MiB)
	/// ≈ 80~120ms；而它在每次握手里都会被调用，所以必须命中缓存。
	/// 这里同时打印两次调用的耗时，便于回归时观察（不做硬性时间断言，避免 CI 抖动）。
	#[test]
	fn p2p_registration_is_deterministic_and_cached() {
		let password = "cache-probe-password";

		let t0 = std::time::Instant::now();
		let (setup1, reg1) = p2p_get_server_registration(password).unwrap();
		let first = t0.elapsed();

		let t1 = std::time::Instant::now();
		let (setup2, reg2) = p2p_get_server_registration(password).unwrap();
		let second = t1.elapsed();

		assert_eq!(setup1, setup2, "同一密码必须得到相同的 ServerSetup");
		assert_eq!(reg1, reg2, "同一密码必须得到相同的 ServerRegistration");
		println!("registration: 首次 {first:?}（含 Argon2id KSF），命中缓存 {second:?}");

		// 换密码后必须得到不同记录（缓存键随之失效）。
		let (setup3, _) = p2p_get_server_registration("another-password").unwrap();
		assert_ne!(setup1, setup3, "不同密码必须得到不同的 ServerSetup");
	}
}
