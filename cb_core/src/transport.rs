// cb_core/src/transport.rs

pub mod cert;

use std::fmt::Debug;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket}; // Ipv6Addr 在 v1 暂不强制
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
pub use quinn::{Connection, RecvStream, SendStream};
use quinn::{
	Endpoint, EndpointConfig, MtuDiscoveryConfig, Runtime, ServerConfig, TokioRuntime,
	TransportConfig, VarInt,
};

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

/// UDP payload 的环境变量名：`CB_QUIC_UDP_PAYLOAD`。
const QUIC_UDP_PAYLOAD_ENV: &str = "CB_QUIC_UDP_PAYLOAD";

/// 默认 UDP payload：9000 B（jumbo frame）。
///
/// 依据见 [`quic_transport_config`] 的中文说明：本机 release/loopback 实测
/// 9000 B 相对默认 1452 B 约 3× 吞吐，且 CPU/byte 约 3× 更低。默认值之所以
/// 敢用 9000，是因为 quinn 实现了**主动 DPLPMTUD**（见下），1500 MTU 的路径
/// 会在探测失败后自动回落到路径实际 MTU，不需要用户手工选 1452。
const QUIC_DEFAULT_UDP_PAYLOAD: u16 = 9000;

/// QUIC 规定的最小 UDP payload（RFC 9000 §14：所有路径都必须支持 1200 B）。
///
/// 同时作为 `min_mtu`：黑洞检测命中时，当前 MTU 会被压回这个值，从而保证
/// 任何路径都还能继续发数据，再交给 DPLPMTUD 重新向上探测。
const QUIC_MIN_UDP_PAYLOAD: u16 = 1200;

/// 允许的最大 UDP payload：9000 B（jumbo frame）。
///
/// 超过 9000 不是“更快”，而是**握手死锁**：quinn 0.11.12 默认 Cubic 的初始
/// 拥塞窗口是 14720 B，而 `poll_transmit` 用
/// `in_flight + segment_size >= cwnd` 做门控。payload ≥ 14720 B 时，连第一个
/// 握手包都发不出去，两端都卡住（实测 16000 B 握手超时）。本机 16000 B 即使
/// 把 cwnd 抬到 100 KiB 也没有额外收益，因此硬上限取 9000。
const QUIC_MAX_UDP_PAYLOAD: u16 = 9000;

/// UDP socket 收发缓冲：8 MiB（`SO_RCVBUF` / `SO_SNDBUF`）。
///
/// 依据（release、本机、loopback、quinn 0.11.12）：Windows 默认 `SO_RCVBUF`
/// 只有 64 KiB，9000 B payload 时只够放 ~7 个数据报；jumbo 下用默认缓冲只有
/// 8 MiB 缓冲的约 65%（526 vs 713 MiB/s），并伴随可观测的丢包与 cwnd 事件。
/// 1452 B 时缓冲是噪声（窗口足够装下在途数据），所以这个设置只在 jumbo 时
/// 真正兑现收益；但始终开启——它没有协议语义、失败也只是回退到系统默认。
const QUIC_SOCKET_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// 构造两端共用的 QUIC [`TransportConfig`]。
///
/// 把超时/keep-alive/窗口收在一处，防止客户端、服务端以后只改一边（此前的 bug
/// 就是只给服务端设了 10s，客户端残留 quinn 默认的 ~30s）。
///
/// # UDP payload / PMTU（中文说明）
///
/// 实测结论（release、本机、loopback，64 MiB 传输）：
/// - 默认 `max_udp_payload_size = 1452`：单连接约 180–240 MiB/s；
/// - 9000 B payload：约 650–780 MiB/s（中位 ~690 MiB/s），约 **3×**，CPU/byte
///   约 3× 更低。数据报变大后每字节的系统调用/加密/ACK 开销都按比例下降。
/// - 9000 B + Windows 默认 64 KiB `SO_RCVBUF` 只有 ~526 MiB/s（约 35% 损失），
///   所以 socket 缓冲在 jumbo 下是必要条件（见 [`QUIC_SOCKET_BUFFER_BYTES`]）。
/// - 16000 B **不能**用：quinn 默认 Cubic 初始 cwnd = 14720 B，payload 不小于
///   它时握手包就发不出去（见 [`QUIC_MAX_UDP_PAYLOAD`]）。
///
/// quinn-proto 0.11.19 **确实**实现了 RFC 8899 的 DPLPMTUD：
/// - 探测是填充到目标大小的 PING（`connection/mod.rs:949-991`，统计见
///   `stats.path.sent_plpmtud_probes`）；
/// - 二分搜索上界 = min(`MtuDiscoveryConfig::upper_bound`, 对端握手时通告的
///   `max_udp_payload_size`)，见 `connection/mtud.rs:300-349`；
/// - 探测丢失 → 降低搜索上界重试（`mtud.rs:257-264`，最多
///   `MAX_PROBE_RETRANSMITS = 3` 次）；
/// - 连续可疑丢包突发 → **黑洞检测**，把当前 MTU 直接压回 `min_mtu`
///   （`mtud.rs:155-167`、`connection/mod.rs:1794`），之后按
///   `black_hole_cooldown`（默认 60 s）再重新探测。
///
/// 因此这里的策略是“**自动**”：把 `endpoint.max_udp_payload_size` 和
/// `MtuDiscoveryConfig::upper_bound` 都抬到目标 payload，但 **`initial_mtu`
/// 保持 1200**、`min_mtu = 1200`。这样：
/// - 在只支持 1500 MTU 的普通局域网/公网，quinn 从 1200 开始探测，失败就
///   自动收敛到路径 MTU（~1452），**绝不会因为一开始就发 9000 B 而握手失败**；
/// - 在整条链路（两端网卡 + 交换机）都支持 jumbo 的局域网，探测会爬到
///   9000，拿到上面的 3×；
/// - 链路中途 MTU 变小（VPN/黑洞）时，黑洞检测会把 MTU 压回 1200 再重新探测。
///
/// ⚠ jumbo 是**端到端**要求：本函数只决定 QUIC 愿意发多大；路径上任何一跳
/// （网卡 jumbo frame、交换机 MTU、虚拟网卡/VPN）不支持 9000，DPLPMTUD 就会
/// 回落到该跳支持的 MTU。也就是说默认 9000 不会让普通网络变慢到不可用，
/// 但只有全链路支持 jumbo 时才会真正兑现 3×。
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
fn quic_transport_config(udp_payload: u16) -> TransportConfig {
	let mut transport_config = TransportConfig::default();
	transport_config.max_idle_timeout(Some(VarInt::from_u32(QUIC_MAX_IDLE_TIMEOUT_MS).into()));
	transport_config.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
	// 显式设置窗口：默认 1.25 MB 的流窗口在 100 ms RTT 下把单流封顶在 ~12.5 MiB/s。
	// 依据见上方常量注释；服务端与客户端共用本函数，保证两端窗口一致。
	transport_config.stream_receive_window(VarInt::from_u32(QUIC_STREAM_RECEIVE_WINDOW));
	transport_config.receive_window(VarInt::from_u32(QUIC_RECEIVE_WINDOW));
	transport_config.send_window(u64::from(QUIC_SEND_WINDOW));

	// --- MTU / PMTU（依据见函数文档） ---
	//
	// 1) initial_mtu：故意保持 1200（quinn 默认值，也是 RFC 9000 的保证下界）。
	//    不能设成 payload：在只支持 1500 MTU 的路径上，一开始就发 9000 B 的
	//    握手/数据包会丢光，只能靠黑洞检测慢慢退回——那会把普通局域网的首连
	//    拖到数秒级。让 DPLPMTUD 从 1200 向上探测才是安全且自适应的做法。
	// 2) min_mtu：1200，黑洞检测命中后压回这里，任何路径都还能继续传输。
	// 3) upper_bound：**必须显式抬高**。quinn 的 `MtuDiscoveryConfig::default()`
	//    上界是 1452（`config/transport.rs:750`），只改 max_udp_payload_size 而
	//    不改它，二分搜索会被静默夹回 1452，jumbo 永远不会生效。
	transport_config.initial_mtu(QUIC_MIN_UDP_PAYLOAD);
	transport_config.min_mtu(QUIC_MIN_UDP_PAYLOAD);
	let mut mtu_discovery = MtuDiscoveryConfig::default();
	mtu_discovery.upper_bound(udp_payload);
	transport_config.mtu_discovery_config(Some(mtu_discovery));

	transport_config
}

/// 解析 `CB_QUIC_UDP_PAYLOAD` 的原始值（纯函数，便于单测）。
///
/// 语义（见 [`Transport::new`] 的日志与函数文档）：
/// - 未设置 / 空串 / 非整数 → 告警并使用默认值 [`QUIC_DEFAULT_UDP_PAYLOAD`]；
/// - 小于 1200 或大于 u32 范围 → 同上（无效值，回退默认）；
/// - 大于 9000 → **夹到 9000** 并告警：不是不想更大，而是 quinn 默认初始
///   cwnd 14720 B 会让 ≥14720 的 payload 在握手阶段死锁（见
///   [`QUIC_MAX_UDP_PAYLOAD`]），16000 B 也没有额外收益；
/// - 1200..=9000 → 原样采用。
fn parse_udp_payload(raw: Option<&str>) -> u16 {
	let Some(raw) = raw else {
		return QUIC_DEFAULT_UDP_PAYLOAD;
	};
	let trimmed = raw.trim();
	if trimmed.is_empty() {
		eprintln!("[Transport] {QUIC_UDP_PAYLOAD_ENV} 为空；使用默认 {QUIC_DEFAULT_UDP_PAYLOAD} B");
		return QUIC_DEFAULT_UDP_PAYLOAD;
	}
	match trimmed.parse::<u32>() {
		Ok(value) if value > u32::from(QUIC_MAX_UDP_PAYLOAD) => {
			eprintln!(
				"[Transport] {QUIC_UDP_PAYLOAD_ENV}={trimmed} 超过上限 {QUIC_MAX_UDP_PAYLOAD} B；\
				 夹到 {QUIC_MAX_UDP_PAYLOAD} B（更大 payload 会触发 quinn 默认 Cubic 初始 \
				 cwnd=14720 B 的握手死锁，且实测无额外收益）"
			);
			QUIC_MAX_UDP_PAYLOAD
		}
		Ok(value) if value >= u32::from(QUIC_MIN_UDP_PAYLOAD) => {
			u16::try_from(value).expect("value 已限制在 1200..=9000，必然能装进 u16")
		}
		Ok(value) => {
			eprintln!(
				"[Transport] {QUIC_UDP_PAYLOAD_ENV}={value} 小于 QUIC 保证下界 \
				 {QUIC_MIN_UDP_PAYLOAD} B；无效，回退默认 {QUIC_DEFAULT_UDP_PAYLOAD} B"
			);
			QUIC_DEFAULT_UDP_PAYLOAD
		}
		Err(error) => {
			eprintln!(
				"[Transport] {QUIC_UDP_PAYLOAD_ENV}={trimmed:?} 不是合法整数（{error}）；\
				 回退默认 {QUIC_DEFAULT_UDP_PAYLOAD} B"
			);
			QUIC_DEFAULT_UDP_PAYLOAD
		}
	}
}

/// 从环境变量读取并解析 UDP payload（每次构造 [`Transport`] 读一次）。
fn udp_payload_from_env() -> u16 {
	parse_udp_payload(std::env::var(QUIC_UDP_PAYLOAD_ENV).ok().as_deref())
}

/// 构造两端共用的 QUIC [`EndpointConfig`]。
///
/// `max_udp_payload_size` 决定两件事：
/// 1. 握手时通告给对端的 `max_udp_payload_size` transport parameter（对端据此
///    决定最多能给自己发多大，DPLPMTUD 的上界也会被它夹住）；
/// 2. 本端接收缓冲/校验尺寸（超过它的数据报会被丢弃）。
///
/// 因此服务端与客户端**必须**用同一个值；本项目的单个 endpoint 同时承担
/// accept 与 connect，天然一致。
fn quic_endpoint_config(udp_payload: u16) -> EndpointConfig {
	let mut endpoint_config = EndpointConfig::default();
	endpoint_config
		.max_udp_payload_size(udp_payload)
		.expect("udp_payload 已在 1200..=9000 内，EndpointConfig 不会拒绝");
	endpoint_config
}

/// 绑定 UDP socket 并把收发缓冲抬到 [`QUIC_SOCKET_BUFFER_BYTES`]。
///
/// 为什么必须自己建 socket：quinn 的 `Endpoint::server(addr)` 内部直接
/// `UdpSocket::bind`，没有任何设置 `SO_RCVBUF`/`SO_SNDBUF` 的入口，Windows
/// 默认 64 KiB 的接收缓冲在 jumbo 下会先丢包（见常量注释）。`Endpoint::new`
/// （quinn 0.11.12 签名：`new(config, server_config, socket, runtime)`）允许
/// 传入预先绑定好的 `std::net::UdpSocket`，所以这里先 bind、再用 `socket2`
/// 设置缓冲，最后交给 quinn。
///
/// 失败策略：设置缓冲失败只告警、不阻断启动——它影响的是 jumbo 吞吐，不是
/// 正确性；绑定失败仍然返回错误。
fn bind_udp_socket(addr: SocketAddr) -> Result<UdpSocket> {
	let socket = UdpSocket::bind(addr)
		.with_context(|| format!("failed to bind QUIC UDP socket at {addr}"))?;
	configure_udp_socket_buffers(&socket);
	Ok(socket)
}

/// 设置 `SO_SNDBUF` / `SO_RCVBUF` 并记录**内核实际生效**的值。
///
/// Windows/macOS 可能不按请求值兑现（内核会取整/翻倍/受上限约束），所以
/// `getsockopt` 回读的值比请求值更有诊断意义；这里用 eprintln 打在启动日志里
/// （本模块没有 LogSink 依赖，Transport 构造只发生一次，不会刷屏）。
fn configure_udp_socket_buffers(socket: &UdpSocket) {
	// socket2 的 `SockRef` 只借用底层句柄，不会关闭 socket，也不改变所有权。
	let sock = socket2::SockRef::from(socket);

	if let Err(error) = sock.set_send_buffer_size(QUIC_SOCKET_BUFFER_BYTES) {
		eprintln!(
			"[Transport] 设置 SO_SNDBUF={QUIC_SOCKET_BUFFER_BYTES} 失败（继续用系统默认）: {error}"
		);
	}
	if let Err(error) = sock.set_recv_buffer_size(QUIC_SOCKET_BUFFER_BYTES) {
		eprintln!(
			"[Transport] 设置 SO_RCVBUF={QUIC_SOCKET_BUFFER_BYTES} 失败（继续用系统默认）: {error}"
		);
	}

	let effective_send = sock.send_buffer_size();
	let effective_recv = sock.recv_buffer_size();
	eprintln!(
		"[Transport] UDP socket buffers: 请求 {QUIC_SOCKET_BUFFER_BYTES} B \
		 (SO_SNDBUF 生效={effective_send:?}, SO_RCVBUF 生效={effective_recv:?})"
	);
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
		// 0. 本端 QUIC 允许的最大 UDP payload（默认 9000 / jumbo，见
		//    quic_transport_config 的中文说明）。每次构造 Transport 读一次环境变量。
		let udp_payload = udp_payload_from_env();
		eprintln!(
			"[Transport] QUIC UDP payload = {udp_payload} B（{QUIC_UDP_PAYLOAD_ENV}，\
			 默认 {QUIC_DEFAULT_UDP_PAYLOAD}）"
		);

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
		// MTU/PMTU 也由同一个函数设置；客户端侧调用完全相同的函数，保证两端
		// 的 max_udp_payload_size / min_mtu / discovery upper_bound 一致。
		server_config.transport_config(Arc::new(quic_transport_config(udp_payload)));

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
		client_config.transport_config(Arc::new(quic_transport_config(udp_payload)));

		// 4. Bind
		//
		// 不使用 `Endpoint::server(addr)`：它内部直接 bind，无法在交给 quinn 之前
		// 设置 SO_RCVBUF/SO_SNDBUF。这里自己 bind（同一 socket 同时用于 accept 与
		// connect，所以不存在单独的“客户端临时端口”第二条路径），设置缓冲后用
		// `Endpoint::new` 交给 quinn（0.11.12 签名见 `bind_udp_socket`）。
		let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), listen_port);
		let socket = bind_udp_socket(addr)?;
		let runtime: Arc<dyn Runtime> = Arc::new(TokioRuntime);
		let mut endpoint = Endpoint::new(
			quic_endpoint_config(udp_payload),
			Some(server_config),
			socket,
			runtime,
		)
		.context("failed to create QUIC endpoint with pre-configured UDP socket")?;
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
		let server = quic_transport_config(QUIC_DEFAULT_UDP_PAYLOAD);
		let client = quic_transport_config(QUIC_DEFAULT_UDP_PAYLOAD);

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

	/// 读取 `TransportConfig` 中 MTU 相关字段的实际值：
	/// `(initial_mtu, min_mtu, mtu_discovery_config.upper_bound)`。
	///
	/// 与 [`window_values`] 一样，quinn 0.11.12 没有 getter，只能解析 `Debug`。
	/// `upper_bound` 在嵌套的 `MtuDiscoveryConfig` 里；`initial_mtu`/`min_mtu`
	/// 是顶层字段。字段名前的空格用于避免 `min_mtu` 误匹配 `initial_mtu`。
	fn mtu_values(config: &TransportConfig) -> (u64, u64, u64) {
		let debug = format!("{config:?}");
		let field = |name: &str| -> u64 {
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
		(field("initial_mtu"), field("min_mtu"), field("upper_bound"))
	}

	/// 两端必须携带同一组 MTU 参数：`initial_mtu`/`min_mtu` 固定为 1200（安全
	/// 探测起点 + 黑洞回落点），`upper_bound` 等于配置的 payload（决定 DPLPMTUD
	/// 能否爬到 jumbo；quinn 默认 1452，不显式抬高会被静默夹回 1452）。
	#[test]
	fn quic_transport_config_carries_same_mtu_for_client_and_server() {
		for payload in [QUIC_MIN_UDP_PAYLOAD, 1452, QUIC_MAX_UDP_PAYLOAD] {
			let server = quic_transport_config(payload);
			let client = quic_transport_config(payload);

			let expected = (
				u64::from(QUIC_MIN_UDP_PAYLOAD),
				u64::from(QUIC_MIN_UDP_PAYLOAD),
				u64::from(payload),
			);
			assert_eq!(
				mtu_values(&server),
				expected,
				"server mtu, payload={payload}"
			);
			assert_eq!(
				mtu_values(&client),
				expected,
				"client mtu, payload={payload}"
			);
			assert_eq!(
				mtu_values(&server),
				mtu_values(&client),
				"server and client MTU values must match (payload={payload})"
			);
		}
	}

	/// `CB_QUIC_UDP_PAYLOAD` 的解析/夹取语义：1200..=9000 原样采用；>9000
	/// 夹到 9000（cwnd 陷阱 + 无额外收益）；非法值告警并回退默认。
	///
	/// 用纯函数 [`parse_udp_payload`] 测试，避免在并行测试里改进程环境变量。
	#[test]
	fn udp_payload_parsing_clamps_and_defaults_invalid_values() {
		// 未设置 / 空值 → 默认。
		assert_eq!(parse_udp_payload(None), QUIC_DEFAULT_UDP_PAYLOAD);
		assert_eq!(parse_udp_payload(Some("")), QUIC_DEFAULT_UDP_PAYLOAD);
		assert_eq!(parse_udp_payload(Some("   ")), QUIC_DEFAULT_UDP_PAYLOAD);

		// 合法区间原样采用（允许前后空白）。
		assert_eq!(parse_udp_payload(Some("1200")), 1200);
		assert_eq!(parse_udp_payload(Some("1452")), 1452);
		assert_eq!(parse_udp_payload(Some(" 9000 ")), 9000);

		// 大于 9000：夹到 9000（含 16000 这个已知会握手死锁的值）。
		assert_eq!(parse_udp_payload(Some("9001")), QUIC_MAX_UDP_PAYLOAD);
		assert_eq!(parse_udp_payload(Some("16000")), QUIC_MAX_UDP_PAYLOAD);
		assert_eq!(parse_udp_payload(Some("65535")), QUIC_MAX_UDP_PAYLOAD);

		// 非法：小于 1200、非整数、超出 u32 → 告警 + 默认。
		assert_eq!(parse_udp_payload(Some("1199")), QUIC_DEFAULT_UDP_PAYLOAD);
		assert_eq!(parse_udp_payload(Some("0")), QUIC_DEFAULT_UDP_PAYLOAD);
		assert_eq!(parse_udp_payload(Some("-1")), QUIC_DEFAULT_UDP_PAYLOAD);
		assert_eq!(parse_udp_payload(Some("jumbo")), QUIC_DEFAULT_UDP_PAYLOAD);
		assert_eq!(
			parse_udp_payload(Some("4294967296")),
			QUIC_DEFAULT_UDP_PAYLOAD
		);
	}

	/// `EndpointConfig::max_udp_payload_size` 必须就是配置的 payload：它同时是
	/// 通告给对端的 transport parameter（对端 DPLPMTUD 的上界）和本端接收缓冲
	/// 尺寸。两端一致由 `Transport::new` 使用同一个 `quic_endpoint_config` 保证。
	#[test]
	fn endpoint_config_advertises_configured_payload() {
		for payload in [QUIC_MIN_UDP_PAYLOAD, 1452, QUIC_MAX_UDP_PAYLOAD] {
			let config = quic_endpoint_config(payload);
			assert_eq!(
				config.get_max_udp_payload_size(),
				u64::from(payload),
				"endpoint max_udp_payload_size, payload={payload}"
			);
		}
	}
}
