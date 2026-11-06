// cb_core/src/transport.rs

pub mod cert;

use std::fmt::Debug;
use std::net::{Ipv4Addr, SocketAddr}; // Ipv6Addr 在 v1 暂不强制
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
pub use quinn::{Connection, RecvStream, SendStream};
use quinn::{Endpoint, ServerConfig, TransportConfig, VarInt};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig as TlsClientConfig, ServerConfig as TlsServerConfig};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

/// 自定义 TLS 验证器：接受任何服务器证书 (Blind Trust)
///
/// TODO(security): 客户端必须把服务器证书固定到 TOFU 指纹（首次配对记录、
/// 之后指纹变化即拒绝），否则中间人可以冒充任意设备。
#[derive(Debug)]
struct BlindVerifier(Arc<CryptoProvider>);

impl BlindVerifier {
	fn new(provider: Arc<CryptoProvider>) -> Self {
		Self(provider)
	}
}

impl ServerCertVerifier for BlindVerifier {
	fn verify_server_cert(
		&self,
		_end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp_response: &[u8],
		_now: UnixTime,
	) -> Result<ServerCertVerified, rustls::Error> {
		Ok(ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		verify_tls12_signature(
			message,
			cert,
			dss,
			&self.0.signature_verification_algorithms,
		)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		verify_tls13_signature(
			message,
			cert,
			dss,
			&self.0.signature_verification_algorithms,
		)
	}

	fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
		self.0.signature_verification_algorithms.supported_schemes()
	}
}

/// 服务器端 TLS 验证器：**请求**客户端证书（TOFU 需要指纹），但不在 TLS 层
/// 拒绝未知证书——是否信任由 SessionActor 中的 TOFU 策略决定。
#[derive(Debug)]
struct RequestClientCertVerifier(Arc<CryptoProvider>);

impl RequestClientCertVerifier {
	fn new(provider: Arc<CryptoProvider>) -> Self {
		Self(provider)
	}
}

impl ClientCertVerifier for RequestClientCertVerifier {
	fn offer_client_auth(&self) -> bool {
		true
	}

	fn client_auth_mandatory(&self) -> bool {
		false
	}

	fn root_hint_subjects(&self) -> &[DistinguishedName] {
		&[]
	}

	fn verify_client_cert(
		&self,
		_end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_now: UnixTime,
	) -> Result<ClientCertVerified, rustls::Error> {
		Ok(ClientCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		verify_tls12_signature(
			message,
			cert,
			dss,
			&self.0.signature_verification_algorithms,
		)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		verify_tls13_signature(
			message,
			cert,
			dss,
			&self.0.signature_verification_algorithms,
		)
	}

	fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
		self.0.signature_verification_algorithms.supported_schemes()
	}
}

pub struct Transport {
	endpoint: Endpoint,
	pub local_cert_der: Vec<u8>,
}

/// QUIC 空闲超时（毫秒）：两端必须一致，见 [`Transport::new`] 中的中文说明。
const QUIC_MAX_IDLE_TIMEOUT_MS: u32 = 10_000;

/// QUIC keep-alive 间隔：两端一致，避免空闲超时被误判 / 无谓重连。
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(2);

/// 构造两端共用的 QUIC [`TransportConfig`]。
///
/// 把超时/keep-alive 收在一处，防止客户端、服务端以后只改一边（此前的 bug 就是
/// 只给服务端设了 10s，客户端残留 quinn 默认的 ~30s）。
fn quic_transport_config() -> TransportConfig {
	let mut transport_config = TransportConfig::default();
	transport_config.max_idle_timeout(Some(VarInt::from_u32(QUIC_MAX_IDLE_TIMEOUT_MS).into()));
	transport_config.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
	transport_config
}

impl Transport {
	/// 创建 Transport 实例
	///
	/// # 参数
	/// - `listen_port`: 监听端口（0 表示自动分配）
	/// - `data_dir`: 数据目录，用于持久化证书和私钥
	/// - `device_id`: 设备 ID，用于密钥派生
	/// - `account_uid`: 账号 UID，用于密钥派生
	pub fn new(
		listen_port: u16,
		data_dir: impl AsRef<Path>,
		device_id: &str,
		account_uid: &str,
	) -> Result<Self> {
		// 1. 获取或创建证书（优先加载已保存的，不存在则生成并保存）
		let (cert_chain, priv_key) =
			cert::get_or_create_cert(data_dir, device_id, account_uid, "clipbridge")
				.context("failed to get or create cert")?;

		let local_cert_der = cert_chain[0].as_ref().to_vec();

		// 2. rustls provider
		let crypto = Arc::new(rustls::crypto::ring::default_provider());

		// 3. Server TLS
		let priv_key_server = priv_key.clone_key(); // PrivateKeyDer 不 Clone，要用 clone_key :contentReference[oaicite:3]{index=3}
		let mut server_tls = TlsServerConfig::builder_with_provider(crypto.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.context("server: unsupported TLS versions")?
			.with_client_cert_verifier(Arc::new(RequestClientCertVerifier::new(crypto.clone())))
			.with_single_cert(cert_chain.clone(), priv_key_server)
			.context("invalid server cert")?;

		server_tls.alpn_protocols = vec![b"clipbridge-v1".to_vec()];

		let quic_server = QuicServerConfig::try_from(server_tls)
			.context("failed to build QuicServerConfig from rustls ServerConfig")?;
		let mut server_config = ServerConfig::with_crypto(Arc::new(quic_server));

		// 服务端：10s 空闲超时 + 2s keep-alive（见 quic_transport_config）。
		server_config.transport_config(Arc::new(quic_transport_config()));

		// 3. Client TLS
		let mut client_tls = TlsClientConfig::builder_with_provider(crypto.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.context("client: unsupported TLS versions")?
			.dangerous()
			.with_custom_certificate_verifier(Arc::new(BlindVerifier::new(crypto.clone())))
			.with_client_auth_cert(cert_chain, priv_key)
			.context("invalid client cert")?;

		client_tls.alpn_protocols = vec![b"clipbridge-v1".to_vec()];

		let quic_client = QuicClientConfig::try_from(client_tls)
			.context("failed to build QuicClientConfig from rustls ClientConfig")?;

		// 【并发审计修复】客户端此前没有 `TransportConfig`，走的是 quinn 默认的空闲超时
		// （约 30s）；服务端已经按 10s 判定对端死亡。两端不对称会让客户端在服务端早已
		// 放弃之后还挂着一条"僵尸连接"：心跳/业务帧写进已死连接、错误要 ~30s 才暴露，
		// 期间重连退避也被拖慢。这里复用与服务器完全相同的 10s 超时 + 2s keep-alive：
		// 让两端几乎同时判定对端死亡，更快失败、更快重连，不会长期保留僵尸连接。
		let mut client_config = quinn::ClientConfig::new(Arc::new(quic_client));
		client_config.transport_config(Arc::new(quic_transport_config()));

		// 4. Bind
		let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), listen_port);
		let mut endpoint = Endpoint::server(server_config, addr)?;
		endpoint.set_default_client_config(client_config);

		Ok(Self {
			endpoint,
			local_cert_der,
		})
	}

	pub fn local_port(&self) -> Result<u16> {
		Ok(self.endpoint.local_addr()?.port())
	}

	pub async fn connect(&self, addr_str: &str) -> Result<Connection> {
		let addr: SocketAddr = addr_str.parse().context("invalid socket addr")?;
		// "localhost" 只是占位，BlindVerifier 会忽略
		let connecting = self.endpoint.connect(addr, "localhost")?;
		let conn = connecting.await?;
		Ok(conn)
	}

	pub async fn accept(&self) -> Option<Connection> {
		let incoming = self.endpoint.accept().await?;
		match incoming.await {
			Ok(conn) => Some(conn),
			Err(e) => {
				eprintln!("[Transport] Handshake failed: {e}");
				None
			}
		}
	}

	pub fn is_ipv4(&self) -> bool {
		// 如果获取失败，默认认为是 IPv4（为了兼容性）
		self.endpoint.local_addr().map_or(true, |a| a.is_ipv4())
	}

	pub fn shutdown(&self) {
		self.endpoint.close(0u32.into(), b"shutdown");
	}
}
