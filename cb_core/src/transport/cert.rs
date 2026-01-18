use anyhow::{Context, Result};
use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::path::{Path, PathBuf};
use std::convert::TryInto;
use std::fs;
use ring::aead;

const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.encrypted";

/// 获取证书存储目录路径
fn get_tls_dir(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("tls")
}

/// 使用 Argon2id 派生加密密钥
fn derive_encryption_key(device_id: &str, account_uid: &str) -> Result<[u8; 32]> {
    use argon2::{Argon2, Algorithm, Version, Params};
    
    let salt = b"ClipBridge:cert_key:v1"; // 固定 salt
    let mut output = [0u8; 32];
    
    // 参数：m=65536 KiB (64 MiB), t=3, p=1
    let params = Params::new(65536, 3, 1, Some(32))
        .map_err(|e| anyhow::anyhow!("failed to create Argon2 params: {:?}", e))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    
    // 输入：device_id + account_uid
    let password = format!("{}{}", device_id, account_uid);
    argon2.hash_password_into(password.as_bytes(), salt, &mut output)
        .map_err(|e| anyhow::anyhow!("failed to derive encryption key: {:?}", e))?;
    
    Ok(output)
}

/// 使用 AES-256-GCM 加密私钥
fn encrypt_private_key(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    use ring::rand::{SecureRandom, SystemRandom};
    
    let unbound_key = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|e| anyhow::anyhow!("failed to create key: {:?}", e))?;
    let sealing_key = aead::LessSafeKey::new(unbound_key);
    
    // 生成随机 nonce（12 字节用于 GCM）
    let mut nonce_bytes = [0u8; 12];
    let rng = SystemRandom::new();
    rng.fill(&mut nonce_bytes)
        .map_err(|e| anyhow::anyhow!("failed to generate nonce: {:?}", e))?;
    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
    
    // 加密
    let mut in_out = plaintext.to_vec();
    let tag = sealing_key.seal_in_place_separate_tag(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|e| anyhow::anyhow!("encryption failed: {:?}", e))?;
    
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
        .map_err(|e| anyhow::anyhow!("failed to create key: {:?}", e))?;
    let opening_key = aead::LessSafeKey::new(unbound_key);
    
    // 分离 nonce、ciphertext 和 tag
    let nonce_bytes: [u8; 12] = encrypted[0..12].try_into()
        .map_err(|_| anyhow::anyhow!("invalid nonce length"))?;
    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
    
    let tag_start = encrypted.len() - 16;
    let ciphertext = &encrypted[12..tag_start];
    let tag_bytes = &encrypted[tag_start..];
    
    // 组合 ciphertext + tag
    let mut in_out = ciphertext.to_vec();
    in_out.extend_from_slice(tag_bytes);
    
    // 解密
    let plaintext = opening_key.open_in_place(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|e| anyhow::anyhow!("decryption failed: {:?}", e))?;
    
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
    let cert_der = fs::read(&cert_path)
        .context("failed to read cert file")?;
    let cert = CertificateDer::from(cert_der);
    let cert_chain = vec![cert];

    // 读取并解密私钥
    let encrypted_key = fs::read(&key_path)
        .context("failed to read encrypted key file")?;
    
    let encryption_key = derive_encryption_key(device_id, account_uid)
        .context("failed to derive encryption key")?;
    
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
    fs::create_dir_all(&tls_dir)
        .context("failed to create tls directory")?;

    // 保存证书（取第一个证书）
    if let Some(cert) = cert_chain.first() {
        let cert_path = tls_dir.join(CERT_FILE);
        fs::write(&cert_path, cert.as_ref())
            .context("failed to write cert file")?;
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

    // 派生加密密钥
    let encryption_key = derive_encryption_key(device_id, account_uid)
        .context("failed to derive encryption key")?;
    
    // 加密私钥
    let encrypted_key = encrypt_private_key(&encryption_key, &key_der)
        .context("failed to encrypt private key")?;

    // 保存加密后的私钥
    let key_path = tls_dir.join(KEY_FILE);
    fs::write(&key_path, &encrypted_key)
        .context("failed to write encrypted key file")?;

    Ok(())
}

/// 生成新的自签名证书
pub fn generate_self_signed_cert(
    common_name: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let subject_alt_names = vec![common_name.to_string()];

    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(subject_alt_names)
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
    let (cert_chain, priv_key) = generate_self_signed_cert(common_name)
        .context("generate_self_signed_cert failed")?;

    // 保存到磁盘
    if let Err(e) = save_cert_and_key(&data_dir, device_id, account_uid, &cert_chain, &priv_key) {
        eprintln!("[Transport] Warning: failed to save cert and key: {}", e);
        // 即使保存失败，也继续使用生成的证书（至少本次运行可用）
    }

    Ok((cert_chain, priv_key))
}

/// 清除本地证书（删除 tls 目录）
pub fn clear_local_cert(data_dir: impl AsRef<Path>) -> Result<()> {
    let tls_dir = get_tls_dir(data_dir);
    if tls_dir.exists() {
        fs::remove_dir_all(&tls_dir)
            .context("failed to remove tls directory")?;
    }
    Ok(())
}
