use anyhow::{Context, Result};
use rcgen::{generate_simple_self_signed, CertifiedKey};
use ring::aead;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::convert::TryInto;
use std::fs;
use std::path::{Path, PathBuf};

const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.encrypted";
/// 每安装随机生成的密钥加密密钥（KEK），与广播字段 device_id/account_uid 无关。
const KEK_FILE: &str = "kek";

/// 获取证书存储目录路径
fn get_tls_dir(data_dir: impl AsRef<Path>) -> PathBuf {
	data_dir.as_ref().join("tls")
}

/// legacy-once migration：旧版本用固定 salt 从 device_id+account_uid 派生加密密钥。
/// 由于这两个值会通过 mDNS 明文广播，该派生值不再用于新数据；
/// 仅用于首次加载旧 `key.encrypted` 时解密一次，随后立即迁移到随机 KEK。
fn derive_encryption_key_legacy(device_id: &str, account_uid: &str) -> Result<[u8; 32]> {
	use argon2::{Algorithm, Argon2, Params, Version};

	let salt = b"ClipBridge:cert_key:v1"; // 固定 salt（历史遗留，仅为迁移保留）
	let mut output = [0u8; 32];

	// 参数：m=65536 KiB (64 MiB), t=3, p=1
	let params = Params::new(65536, 3, 1, Some(32))
		.map_err(|e| anyhow::anyhow!("failed to create Argon2 params: {e:?}"))?;
	let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

	// 输入：device_id + account_uid
	let password = format!("{device_id}{account_uid}");
	argon2
		.hash_password_into(password.as_bytes(), salt, &mut output)
		.map_err(|e| anyhow::anyhow!("failed to derive encryption key: {e:?}"))?;

	Ok(output)
}

/// 生成 32 字节随机 KEK（来自系统 CSPRNG，与 device_id/account_uid 无关）
fn generate_random_kek() -> Result<[u8; 32]> {
	use ring::rand::{SecureRandom, SystemRandom};

	let mut kek = [0u8; 32];
	SystemRandom::new()
		.fill(&mut kek)
		.map_err(|e| anyhow::anyhow!("failed to generate random KEK: {e:?}"))?;
	Ok(kek)
}

/// 持久化 KEK，并在 Unix 上收紧为 0600。
fn write_kek(kek_path: &Path, kek: &[u8; 32]) -> Result<()> {
	fs::write(kek_path, kek).context("failed to write kek file")?;

	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		fs::set_permissions(kek_path, fs::Permissions::from_mode(0o600))
			.context("failed to set kek file permissions")?;
	}
	// Windows：tls/kek 位于用户数据目录，依赖用户 profile 目录的 ACL 保护；
	// 这里不额外设置 ACL（Windows 没有与 0o600 等价的简单语义）。
	Ok(())
}

/// 读取或创建每安装的随机 KEK。
///
/// 迁移路径（legacy-once migration）：当 `tls/kek` 不存在但 `tls/key.encrypted`
/// 存在时，先用旧的“固定 salt + device_id+account_uid”派生值解密；成功后立刻
/// 生成新的随机 KEK 重新加密私钥并写入 `tls/kek`，之后不再依赖公开广播字段。
fn load_or_create_kek(tls_dir: &Path, device_id: &str, account_uid: &str) -> Result<[u8; 32]> {
	let kek_path = tls_dir.join(KEK_FILE);
	if kek_path.exists() {
		let raw = fs::read(&kek_path).context("failed to read kek file")?;
		let kek: [u8; 32] = raw
			.as_slice()
			.try_into()
			.map_err(|_| anyhow::anyhow!("invalid kek file: expected 32 bytes"))?;
		return Ok(kek);
	}

	fs::create_dir_all(tls_dir).context("failed to create tls directory")?;

	let key_path = tls_dir.join(KEY_FILE);
	if key_path.exists() {
		// legacy-once migration：只在这里使用旧派生值
		let legacy_key = derive_encryption_key_legacy(device_id, account_uid)
			.context("legacy migration: failed to derive legacy key")?;
		let encrypted_key = fs::read(&key_path).context("failed to read encrypted key file")?;
		let key_der = decrypt_private_key(&legacy_key, &encrypted_key)
			.context("legacy migration: failed to decrypt private key with legacy key")?;

		let kek = generate_random_kek()?;
		let reencrypted = encrypt_private_key(&kek, &key_der)
			.context("legacy migration: failed to re-encrypt private key")?;
		// 先写新私钥，再写 KEK 标记迁移完成；即使中途崩溃也只是重新生成证书。
		fs::write(&key_path, &reencrypted)
			.context("legacy migration: failed to rewrite encrypted key file")?;
		write_kek(&kek_path, &kek)?;
		eprintln!("[Transport] Migrated legacy cert key to a per-install random KEK");
		return Ok(kek);
	}

	let kek = generate_random_kek()?;
	write_kek(&kek_path, &kek)?;
	Ok(kek)
}

/// 使用 AES-256-GCM 加密私钥
fn encrypt_private_key(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
	use ring::rand::{SecureRandom, SystemRandom};

	let unbound_key = aead::UnboundKey::new(&aead::AES_256_GCM, key)
		.map_err(|e| anyhow::anyhow!("failed to create key: {e:?}"))?;
	let sealing_key = aead::LessSafeKey::new(unbound_key);

	// 生成随机 nonce（12 字节用于 GCM）
	let mut nonce_bytes = [0u8; 12];
	let rng = SystemRandom::new();
	rng.fill(&mut nonce_bytes)
		.map_err(|e| anyhow::anyhow!("failed to generate nonce: {e:?}"))?;
	let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);

	// 加密
	let mut in_out = plaintext.to_vec();
	let tag = sealing_key
		.seal_in_place_separate_tag(nonce, aead::Aad::empty(), &mut in_out)
		.map_err(|e| anyhow::anyhow!("encryption failed: {e:?}"))?;

	// 组合：nonce (12 bytes) + ciphertext + tag (16 bytes)
	let mut result = nonce_bytes.to_vec();
	result.extend_from_slice(&in_out);
	result.extend_from_slice(tag.as_ref());

	Ok(result)
}

/// 使用 AES-256-GCM 解密私钥
fn decrypt_private_key(key: &[u8; 32], encrypted: &[u8]) -> Result<Vec<u8>> {
	if encrypted.len() < 12 + 16 {
		anyhow::bail!("encrypted data too short");
	}

	let unbound_key = aead::UnboundKey::new(&aead::AES_256_GCM, key)
		.map_err(|e| anyhow::anyhow!("failed to create key: {e:?}"))?;
	let opening_key = aead::LessSafeKey::new(unbound_key);

	// 分离 nonce、ciphertext 和 tag
	let nonce_bytes: [u8; 12] = encrypted[0..12]
		.try_into()
		.map_err(|_| anyhow::anyhow!("invalid nonce length"))?;
	let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);

	let tag_start = encrypted.len() - 16;
	let ciphertext = &encrypted[12..tag_start];
	let tag_bytes = &encrypted[tag_start..];

	// 组合 ciphertext + tag
	let mut in_out = ciphertext.to_vec();
	in_out.extend_from_slice(tag_bytes);

	// 解密
	let plaintext = opening_key
		.open_in_place(nonce, aead::Aad::empty(), &mut in_out)
		.map_err(|e| anyhow::anyhow!("decryption failed: {e:?}"))?;

	Ok(plaintext.to_vec())
}

/// 加载已保存的证书和私钥
pub fn load_cert_and_key(
	data_dir: impl AsRef<Path>,
	device_id: &str,
	account_uid: &str,
) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>> {
	let tls_dir = get_tls_dir(data_dir);
	let cert_path = tls_dir.join(CERT_FILE);
	let key_path = tls_dir.join(KEY_FILE);

	// 如果证书或私钥文件不存在，返回 None
	if !cert_path.exists() || !key_path.exists() {
		return Ok(None);
	}

	// 读取证书
	let cert_der = fs::read(&cert_path).context("failed to read cert file")?;
	let cert = CertificateDer::from(cert_der);
	let cert_chain = vec![cert];

	// 先确保 KEK 已就绪（首次遇到旧格式时会在此时把 key.encrypted 迁移重写），
	// 再读取密文，避免用新 KEK 去解密迁移前的旧密文。
	let encryption_key = load_or_create_kek(&tls_dir, device_id, account_uid)
		.context("failed to load or create encryption key")?;

	let encrypted_key = fs::read(&key_path).context("failed to read encrypted key file")?;

	let key_der = decrypt_private_key(&encryption_key, &encrypted_key)
		.context("failed to decrypt private key")?;

	let key_pkcs8 = PrivatePkcs8KeyDer::from(key_der);
	let priv_key = PrivateKeyDer::from(key_pkcs8);

	Ok(Some((cert_chain, priv_key)))
}

/// 保存证书和私钥到磁盘（私钥加密）
pub fn save_cert_and_key(
	data_dir: impl AsRef<Path>,
	device_id: &str,
	account_uid: &str,
	cert_chain: &[CertificateDer],
	priv_key: &PrivateKeyDer,
) -> Result<()> {
	let tls_dir = get_tls_dir(data_dir);

	// 确保目录存在
	fs::create_dir_all(&tls_dir).context("failed to create tls directory")?;

	// 保存证书（取第一个证书）
	if let Some(cert) = cert_chain.first() {
		let cert_path = tls_dir.join(CERT_FILE);
		fs::write(&cert_path, cert.as_ref()).context("failed to write cert file")?;
	}

	// 提取私钥的 PKCS#8 DER 格式
	let key_der = match priv_key {
		PrivateKeyDer::Pkcs8(key_pkcs8) => key_pkcs8.secret_pkcs8_der().to_vec(),
		PrivateKeyDer::Sec1(_) => {
			anyhow::bail!("SEC1 key format not supported for persistence");
		}
		PrivateKeyDer::Pkcs1(_) => {
			anyhow::bail!("PKCS1 key format not supported for persistence");
		}
		_ => {
			anyhow::bail!("Unknown private key format");
		}
	};

	// 读取（或首次创建/迁移）每安装随机 KEK
	let encryption_key = load_or_create_kek(&tls_dir, device_id, account_uid)
		.context("failed to load or create encryption key")?;

	// 加密私钥
	let encrypted_key =
		encrypt_private_key(&encryption_key, &key_der).context("failed to encrypt private key")?;

	// 保存加密后的私钥
	let key_path = tls_dir.join(KEY_FILE);
	fs::write(&key_path, &encrypted_key).context("failed to write encrypted key file")?;

	Ok(())
}

/// 生成新的自签名证书
pub fn generate_self_signed_cert(
	common_name: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	let subject_alt_names = vec![common_name.to_string()];

	let CertifiedKey { cert, signing_key } = generate_simple_self_signed(subject_alt_names)
		.context("rcgen::generate_simple_self_signed failed")?;

	let cert_chain = vec![CertificateDer::from(cert.der().to_vec())];
	let key_der = PrivatePkcs8KeyDer::from(signing_key.serialize_der());
	let priv_key = PrivateKeyDer::from(key_der);

	Ok((cert_chain, priv_key))
}

/// 获取或创建证书（优先加载已保存的，不存在则生成并保存）
pub fn get_or_create_cert(
	data_dir: impl AsRef<Path>,
	device_id: &str,
	account_uid: &str,
	common_name: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	// 尝试加载已保存的证书
	match load_cert_and_key(&data_dir, device_id, account_uid) {
		Ok(Some((cert_chain, priv_key))) => {
			// 成功加载，直接返回
			return Ok((cert_chain, priv_key));
		}
		Ok(None) => {
			// 不存在，生成新证书
		}
		Err(_e) => {
			// 加载失败（可能是 account_uid 改变导致解密失败），删除旧证书并重新生成
			// 清除旧的证书文件
			let _ = clear_local_cert(&data_dir);
		}
	}

	// 生成新证书
	let (cert_chain, priv_key) =
		generate_self_signed_cert(common_name).context("generate_self_signed_cert failed")?;

	// 保存到磁盘
	if let Err(e) = save_cert_and_key(&data_dir, device_id, account_uid, &cert_chain, &priv_key) {
		eprintln!("[Transport] Warning: failed to save cert and key: {e}");
		// 即使保存失败，也继续使用生成的证书（至少本次运行可用）
	}

	Ok((cert_chain, priv_key))
}

/// 清除本地证书（删除 tls 目录）
///
/// tls/ 目录同时包含 cert.der、key.encrypted 与 kek，整体删除即完成重置。
pub fn clear_local_cert(data_dir: impl AsRef<Path>) -> Result<()> {
	let tls_dir = get_tls_dir(data_dir);
	if tls_dir.exists() {
		fs::remove_dir_all(&tls_dir).context("failed to remove tls directory")?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fresh_save_creates_random_kek() {
		let dir = tempfile::tempdir().unwrap();
		let (cert_chain, priv_key) = generate_self_signed_cert("clipbridge").unwrap();
		save_cert_and_key(dir.path(), "dev", "acct", &cert_chain, &priv_key).unwrap();

		let kek = fs::read(dir.path().join("tls").join(KEK_FILE)).unwrap();
		assert_eq!(kek.len(), 32, "KEK 必须是 32 字节随机值");

		// 再次加载不再依赖 device_id/account_uid
		let loaded = load_cert_and_key(dir.path(), "other-device", "other-acct")
			.unwrap()
			.expect("cert + key should load");
		assert_eq!(loaded.0[0].as_ref(), cert_chain[0].as_ref());
	}

	#[test]
	fn legacy_key_is_migrated_once() {
		// 构造旧版本格式：cert.der + 固定 salt 派生的 key.encrypted，且没有 kek
		let dir = tempfile::tempdir().unwrap();
		let tls_dir = dir.path().join("tls");
		fs::create_dir_all(&tls_dir).unwrap();

		let CertifiedKey { cert, signing_key } =
			generate_simple_self_signed(vec!["clipbridge".to_string()]).unwrap();
		let cert_der = cert.der().to_vec();
		let key_der = signing_key.serialize_der();
		let legacy_key = derive_encryption_key_legacy("dev", "acct").unwrap();

		fs::write(tls_dir.join(CERT_FILE), &cert_der).unwrap();
		fs::write(
			tls_dir.join(KEY_FILE),
			encrypt_private_key(&legacy_key, &key_der).unwrap(),
		)
		.unwrap();
		assert!(!tls_dir.join(KEK_FILE).exists());

		// 首次加载触发迁移
		let loaded = load_cert_and_key(dir.path(), "dev", "acct")
			.unwrap()
			.expect("legacy cert + key should load");
		assert_eq!(loaded.0[0].as_ref(), cert_der.as_slice());

		let kek = fs::read(tls_dir.join(KEK_FILE)).unwrap();
		assert_eq!(kek.len(), 32, "迁移后必须落盘 32 字节随机 KEK");

		// 迁移完成后，即使 device_id/account_uid 变化也能解密（不再依赖公开广播值）
		let again = load_cert_and_key(dir.path(), "changed-device", "changed-acct")
			.unwrap()
			.expect("cert + key should load with new KEK");
		assert_eq!(again.0[0].as_ref(), cert_der.as_slice());
	}
}
