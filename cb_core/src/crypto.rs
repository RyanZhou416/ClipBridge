// cb_core/src/crypto.rs

use opaque_ke::{
	ciphersuite::CipherSuite, key_exchange::tripledh::TripleDh, ClientRegistration,
	ClientRegistrationFinishParameters, ServerRegistration, ServerSetup,
};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use sha2::{Digest, Sha512};
use std::convert::TryInto;

// --- 1. 定义加密套件 (v4.0 标准) ---
pub struct DefaultCipherSuite;

impl CipherSuite for DefaultCipherSuite {
	type OprfCs = opaque_ke::Ristretto255;
	type KeyExchange = TripleDh<opaque_ke::Ristretto255, Sha512>;
	type Ksf = opaque_ke::ksf::Identity;
}

// --- 2. 类型别名 ---
pub type CbClientLogin = opaque_ke::ClientLogin<DefaultCipherSuite>;
pub type CbServerLogin = opaque_ke::ServerLogin<DefaultCipherSuite>;

pub type CbClientLoginStartResult = opaque_ke::ClientLoginStartResult<DefaultCipherSuite>;
pub type CbServerLoginStartResult = opaque_ke::ServerLoginStartResult<DefaultCipherSuite>;

pub type CbClientLoginState = CbClientLogin;
pub type CbServerLoginState = CbServerLogin;

pub type CbServerRegistration = opaque_ke::ServerRegistration<DefaultCipherSuite>;

// --- 3. P2P 辅助：生成服务器验证记录 ---
pub fn p2p_get_server_registration(
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
}
