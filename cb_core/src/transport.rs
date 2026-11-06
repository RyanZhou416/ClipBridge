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

/// 单条 QUIC 流的接收窗口（8 MiB）。
///
/// 旧实现沿用 quinn 默认的 1.25 MB：在 100 ms RTT 链路上，单流吞吐被 BDP 封顶
/// 在 1.25 MB / 0.1 s ≈ 12.5 MiB/s，与链路实际带宽无关——这是公网（WAN）传输
/// 最主要的限制。8 MB 在同样的 100 ms RTT 下单流可达 8 MB / 0.1 s ≈ 80 MiB/s。
/// 回环（RTT 远小于 1 ms）下窗口从来装不满，因此局域网行为不受影响（此前回环
/// 实测单流约 189 MiB/s，为纯 QUIC 基线的 97%）。
///
/// 单独命名成常量，便于未来的自适应控制器按实测 RTT 选择档位（或后续加环境变量
/// 覆盖）；本轮不引入环境变量。
const QUIC_STREAM_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;

/// 单条 QUIC 连接的接收窗口（64 MiB）。
///
/// 连接级窗口必须显著大于单流窗口，否则多条流会互相挤占同一份连接级额度。
/// 取 [`QUIC_STREAM_RECEIVE_WINDOW`] 的 8 倍，刚好容纳策略允许的 8 个并发
/// receiver 各占满一条流。
const QUIC_RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;

/// 单条 QUIC 连接的发送窗口（32 MiB）。
///
/// 对应 quinn 的 `send_window`（应用写入后仍被 quinn 保留的数据上限，含已确认
/// 但存储尚未释放的部分）。放宽到 32 MiB 后，100 ms RTT 下发送端不会因为等
/// ACK 而频繁阻塞 `write`。
const QUIC_SEND_WINDOW: u32 = 32 * 1024 * 1024;

/// 构造两端共用的 QUIC [`TransportConfig`]。
///
/// 把超时/keep-alive/窗口收在一处，防止客户端、服务端以后只改一边（此前的 bug
/// 就是只给服务端设了 10s，客户端残留 quinn 默认的 ~30s）。
///
/// # 内存代价（中文说明）
///
/// quinn **不会**按窗口大小给每条流预分配缓冲区：流控额度只是"对端最多还能发
/// 多少"的承诺，接收缓冲区随数据真正到达而增长，应用读走并回复 `MAX_DATA` 后
/// 回收。因此大窗口只在满载传输时占内存，空闲/小流量时几乎没有额外成本。
///
/// 最坏情形按 `receive_window × 并发流数 × 连接数` 估算：
/// 64 MiB × 8 × 2 = **1024 MiB = 1 GiB/进程**（接收端策略上限为 8 个并发
/// receiver、2 条连接）。实际上 quinn 的连接级 `receive_window` 已是单连接
/// 聚合上限，真实峰值不超过 64 MiB × 2 = 128 MiB；1 GiB 是"每条流都吃满"
/// 的宽松上界，用于说明最坏情况下也不会失控。
fn quic_transport_config() -> TransportConfig {
	let mut transport_config = TransportConfig::default();
	transport_config.max_idle_timeout(Some(VarInt::from_u32(QUIC_MAX_IDLE_TIMEOUT_MS).into()));
	transport_config.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
	// 显式设置窗口：默认 1.25 MB 的流窗口在 100 ms RTT 下把单流封顶在 ~12.5 MiB/s。
	// 依据见上方常量注释；服务端与客户端共用本函数，保证两端窗口一致。
	transport_config.stream_receive_window(VarInt::from_u32(QUIC_STREAM_RECEIVE_WINDOW));
	transport_config.receive_window(VarInt::from_u32(QUIC_RECEIVE_WINDOW));
	transport_config.send_window(u64::from(QUIC_SEND_WINDOW));
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

#[cfg(test)]
mod tests {
	use super::*;

	/// 读取 `TransportConfig` 中三个窗口字段的实际值。
	///
	/// quinn 0.11.12 的 `TransportConfig` **没有 getter**（只有链式 setter，字段是
	/// `pub(crate)`），因此这里改用公开的 `Debug` 实现：它的输出里带有
	/// `stream_receive_window` / `receive_window` / `send_window` 三个字段，
	/// 且 `VarInt` 的 `Debug` 就是十进制数值。若将来 quinn 提供 getter，应直接
	/// 换成 getter，不再解析字符串。
	fn window_values(config: &TransportConfig) -> (u64, u64, u64) {
		let debug = format!("{config:?}");
		let field = |name: &str| -> u64 {
			// 字段前加一个空格，避免 "receive_window" 误匹配到
			// "... stream_receive_window" 内部。
			let needle = format!(" {name}: ");
			let start = debug
				.find(&needle)
				.unwrap_or_else(|| panic!("TransportConfig 的 Debug 输出缺少 {name}: {debug}"))
				+ needle.len();
			let rest = &debug[start..];
			let end = rest
				.find(|c: char| !c.is_ascii_digit())
				.unwrap_or(rest.len());
			rest[..end]
				.parse()
				.unwrap_or_else(|_| panic!("TransportConfig 的 Debug 中 {name} 不是整数: {debug}"))
		};
		(
			field("stream_receive_window"),
			field("receive_window"),
			field("send_window"),
		)
	}

	/// 服务端与客户端都通过 [`quic_transport_config`] 构造配置（见 `Transport::new`
	/// 中的 `server_config.transport_config(...)` / `client_config.transport_config(...)`），
	/// 这里分别代表两端各取一份，断言它们携带完全一致的窗口值，且与常量一致。
	#[test]
	fn quic_transport_config_carries_same_windows_for_client_and_server() {
		let server = quic_transport_config();
		let client = quic_transport_config();

		let expected = (
			u64::from(QUIC_STREAM_RECEIVE_WINDOW),
			u64::from(QUIC_RECEIVE_WINDOW),
			u64::from(QUIC_SEND_WINDOW),
		);
		assert_eq!(window_values(&server), expected, "server windows");
		assert_eq!(window_values(&client), expected, "client windows");
		assert_eq!(
			window_values(&server),
			window_values(&client),
			"server and client window values must match"
		);
	}
}
