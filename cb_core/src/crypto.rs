// cb_core/src/crypto.rs

use opaque_ke::{
    ciphersuite::CipherSuite,
    key_exchange::tripledh::TripleDh,
    ClientRegistration, ServerRegistration,
    ClientRegistrationFinishParameters,
    ServerSetup,
    // 注意：v3.0.0 中 ServerRegistrationStartParameters 可能不需要或路径不同
    // 根据报错"takes 3 arguments"，我们这里就不引入它了
};
use rand::{SeedableRng};
use rand_chacha::ChaCha20Rng;
use sha2::{Sha512, Digest};
use std::convert::TryInto;

// --- 1. 定义加密套件 (v3.0.0 标准) ---
pub struct DefaultCipherSuite;

impl CipherSuite for DefaultCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeGroup = opaque_ke::Ristretto255;
    type KeyExchange = TripleDh;
    type Ksf = opaque_ke::ksf::Identity; // [修正] v3.0.0 使用 Ksf 而不是 Kdf
}

// --- 2. 类型别名 ---
pub type CbClientLogin = opaque_ke::ClientLogin<DefaultCipherSuite>;
pub type CbServerLogin = opaque_ke::ServerLogin<DefaultCipherSuite>;

// v3.0 API 中的 StartResult
pub type CbClientLoginStartResult = opaque_ke::ClientLoginStartResult<DefaultCipherSuite>;
pub type CbServerLoginStartResult = opaque_ke::ServerLoginStartResult<DefaultCipherSuite>;

// [修正] 直接使用具体的泛型类型，而不是查找 Trait
// 在 v3.0.0 中，StartResult.state 的类型就是 ClientLogin<CS>
pub type CbClientLoginState = CbClientLogin;
pub type CbServerLoginState = CbServerLogin;

pub type CbServerRegistration = opaque_ke::ServerRegistration<DefaultCipherSuite>;

// --- 3. P2P 辅助：生成服务器验证记录 ---
// 返回 (ServerSetup, ServerRegistration)
pub fn p2p_get_server_registration(password: &str) -> anyhow::Result<(ServerSetup<DefaultCipherSuite>, CbServerRegistration)> {
    // 1. 使用密码生成确定性种子
    let mut hasher = Sha512::new();
    hasher.update(password.as_bytes());
    let seed: [u8; 32] = hasher.finalize()[0..32].try_into()?;
    let mut rng = ChaCha20Rng::from_seed(seed);

    let password_bytes = password.as_bytes();
    let identifier = b"clipbridge-user";

    // 2. 生成 ServerSetup (OPRF Seed) - 登录时需要用到
    let server_setup = ServerSetup::<DefaultCipherSuite>::new(&mut rng);

    // 3. 模拟注册流程

    // Client: Start
    let client_reg_start = ClientRegistration::<DefaultCipherSuite>::start(
        &mut rng,
        password_bytes,
    )?;

    // Server: Start
    // [修正] v3.0.0 ServerRegistration::start 只需要 3 个参数：(rng, message, identifier)
    // 报错提示：expected 3 arguments, found 5
    let server_reg_start = ServerRegistration::<DefaultCipherSuite>::start(
        &server_setup, // 参数 1: &ServerSetup
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

    // Server: Finish -> 得到最终记录
    let server_registration = ServerRegistration::<DefaultCipherSuite>::finish(
        client_reg_finish.message,
    );

    Ok((server_setup, server_registration))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;
    use opaque_ke::{ClientLoginFinishParameters, ServerLoginStartParameters};

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
            ServerLoginStartParameters::default(),
        ).expect("Server failed to start session");

        let ke2_message = server_start.message;

        // 3. Client Handle KE2
        // [修复] v3.0.0 finish 不需要 rng
        let client_finish = client_start.state.finish(
            shared_key.as_bytes(),
            ke2_message,
            ClientLoginFinishParameters::default(),
        ).expect("Client failed to finish session");

        let ke3_message = client_finish.message;
        let client_session_key = client_finish.session_key;

        // 4. Server Handle KE3
        let server_finish_result = server_start.state.finish(
            ke3_message
        ).expect("Server failed to verify client");

        // [修复] v3.0.0 返回的是 Result 结构体，需要提取 session_key 字段
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
            ServerLoginStartParameters::default(),
        ).unwrap();

        // [修复] 移除 rng
        let client_finish_res = client_start.state.finish(
            wrong_key.as_bytes(),
            server_start.message,
            ClientLoginFinishParameters::default(),
        );

        if let Ok(client_finish) = client_finish_res {
            let server_res = server_start.state.finish(client_finish.message);
            assert!(server_res.is_err(), "Server should reject wrong password");
        } else {
            // Client 可能会直接报错（解密失败），这也是符合预期的
            assert!(client_finish_res.is_err());
        }
    }
}