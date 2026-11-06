//! quinn 调优探针 —— 默认 `#[ignore]`，只在显式运行时执行。
//!
//! 目的：回答“在保留 quinn 的前提下，哪些 quinn 侧可调项真的能在这台机器上
//! 提高吞吐，幅度多大”。所有测量都在**进程内 IPv4 loopback** 完成：不启动应用、
//! 不读写剪贴板、不依赖第二台主机。
//!
//! # 四个维度
//!
//! 1. **UDP payload / PMTU**：`max_udp_payload_size = 1452 / 3000 / 6000 / 9000 /
//!    16000`。QUIC 默认上限 1452 B（quinn 的 `mtu_discovery_config.upper_bound`
//!    默认 1452），10 Gbps 下意味着 ~861k pkt/s；payload 越大，系统调用次数越少，
//!    直接打击“每包固定成本”。两端必须同时放大：
//!    - `EndpointConfig::max_udp_payload_size(payload)`：决定握手时通告的
//!      `max_udp_payload_size` transport parameter，并决定接收缓冲大小；
//!    - `TransportConfig::initial_mtu(payload)` / `min_mtu(payload)` 与
//!      `MtuDiscoveryConfig::upper_bound(payload)`：决定本端实际发送的 UDP
//!      payload。只改一边会被对端的 transport parameter 夹回 1472。
//!
//!    探针不靠“配置回读”证明生效，而是读 `conn.stats().path.current_mtu` 与
//!    `udp_tx.bytes / udp_tx.datagrams`（实际发出的平均数据报大小）来核对。
//!
//!    ⚠ 实测发现比 MTU 更硬的一个上限：quinn 默认 Cubic 的**初始拥塞窗口是
//!    14720 B**，而 `poll_transmit` 用 `in_flight + segment_size >= cwnd` 做门控
//!    （`quinn-proto/src/connection/mod.rs:616`）。payload >= 14720 B 时，
//!    连握手包都发不出去，连接在握手阶段就超时——不是 PMTU/内核限制。
//!    因此 16000 B 必须同时抬高初始 cwnd（`TransportConfig::
//!    congestion_controller_factory` + `CubicConfig::initial_window`）。探针
//!    为此专门跑一行 `payload=16000 + init_cwnd=100KiB` 对照；实际部署目标
//!    9000 B（jumbo frame）不需要这一步。
//!
//! 2. **`MAX_TRANSMIT_SEGMENTS` 10 → 32（上界估计，不改 quinn 源码）**：
//!    quinn 0.11.12 把每次发送的 GSO 段数硬编码为 `MAX_TRANSMIT_SEGMENTS = 10`
//!    （`quinn/src/connection.rs:1341`），而 Windows 报告 512。本探针不能改
//!    quinn，因此用 `quinn::udp`（quinn 实际使用的同一套 Windows GSO/GRO 实现）
//!    直接跑 1 / 10 / 32 段的原始 UDP 发送，量化“把批上限从 10 提到 32”在
//!    syscall 层面的收益上界；另加 payload=16000 的 1 / 10 段诊断，用来证明
//!    “16000 B 的失败在 cwnd，不在 Windows GSO”（原始 UDP 16000 段测正常）。
//!    原始 UDP 没有 TLS/ACK/拥塞控制，所以它测到的是**上限**，不是 QUIC
//!    端到端结果。
//!
//! 3. **Endpoint 数量**：1 / 2 / 4 个**独立** Endpoint 对（每对 1 条连接）
//!    的总吞吐与单连接吞吐；并与“1 个 Endpoint 上 4 条连接”对比。quinn 的
//!    接收循环是 per-Endpoint 的，独立 Endpoint 可以让内核收包与用户态处理
//!    分散到更多核。
//!
//! 4. **单连接上的流数量**：1 / 2 / 4 / 8 条并发单向流（模拟 B2 的分片）。
//!    loopback 上单流窗口 8 MiB 远大于 BDP，预期流数不会提升吞吐；这个维度
//!    用来验证“B2 并行流在局域网不会变快，也不会明显变慢”。
//!
//! # 测量口径
//!
//! - 吞吐 = 服务端**实际收到**的应用字节 / “客户端开始写”到“服务端收完最后
//!   一个流”的墙钟。握手、建流、关闭都不计入。
//! - CPU% = `GetProcessTimes`（本进程 kernel+user，收发两端之和）增量 / 墙钟。
//!   本机 tick 约 15.6 ms，短窗口行有量化误差，表里如实保留一位小数。
//! - `pkt/s` 用客户端 `udp_tx.datagrams / 墙钟`（**实际发出的 UDP 数据报**，
//!   含重传），不是用应用字节反推的估计值。`avg_tx` 是
//!   `udp_tx.bytes / udp_tx.datagrams`，用它核对 payload 是否真的变大。
//! - 每个用例都新建 Endpoint、新证书连接、新 socket，避免状态跨用例泄漏。
//!
//! # 诚实限制
//!
//! - **只有 loopback，没有真实网卡**：没有 NIC 的 UDP RSC/URO，因此 GRO 在
//!   真实网卡上的收益完全未覆盖；loopback 的“MTU”由软件栈决定，所以 9000 /
//!   16000 B 能通过**不代表**局域网/公网能通过。真实环境要开 jumbo 必须端到端
//!   支持，只能是 opt-in。
//! - TLS 不校验服务端证书（等价于应用的 BlindVerifier），且不启用客户端证书；
//!   握手只发生一次，不影响稳态吞吐。
//! - 所有 quinn 用例都把 socket 的 SO_SNDBUF/SO_RCVBUF 设为 8 MiB（可用
//!   `CB_QUINN_PROBE_SOCKBUF_MIB` 改，0 = Windows 默认）。原因：Windows 默认
//!   UDP 接收缓冲只有 64 KiB，9000 B payload 时只够放 ~7 个数据报，不抬高缓冲
//!   的话“payload 变大”会先被内核丢包掩盖，得到错误结论。表里另有一行
//!   `sockbuf=default` 的 1452 对照，用来和历史上的默认缓冲数字对比。
//!   ⚠ 注意：quinn 本身没有暴露设置 socket 缓冲的 API，本探针是用
//!   `quinn::udp::UdpSocketState` 在把 socket 交给 `Endpoint` 之前设置的；
//!   如果要把这一点产品化，需要让 `cb_core` 自己建 socket 再传
//!   `Endpoint::new`，而不是用 `Endpoint::server(addr)`。
//! - 本机是 32 逻辑核的桌面 CPU，测试进程还跑着编辑器/浏览器等负载，数字有
//!   ±10~20% 的抖动；所有比较都应看趋势，不要用小数位当真。
//! - 原始 UDP GSO 段测与 QUIC 段落不是同一量纲：前者没有加密/ACK/拥塞控制。
//!
//! # 运行
//!
//! ```text
//! cargo test --release -p cb_core --test quinn_tuning_probe -- --ignored --nocapture
//! ```
//!
//! 默认 64 MiB/用例，整轮约 20~40 s（视 payload 是否失败）。快速冒烟：
//!
//! ```text
//! $env:CB_QUINN_PROBE_QUICK = "1"
//! cargo test --release -p cb_core --test quinn_tuning_probe -- --ignored --nocapture
//! ```
//!
//! # 环境变量（全部可选）
//!
//! - `CB_QUINN_PROBE_QUICK=1`：8 MiB/用例，payload `1452,9000`，endpoint `1,4`，
//!   stream `1,4`，原始 GSO 20k 包。
//! - `CB_QUINN_PROBE_FILE_MIB`：每个用例传输的 MiB，默认 64。
//! - `CB_QUINN_PROBE_PAYLOADS`：逗号分隔的 payload 字节数，默认
//!   `1452,3000,6000,9000,16000`。
//! - `CB_QUINN_PROBE_ENDPOINTS`：独立 Endpoint 对数量，默认 `1,2,4`。
//! - `CB_QUINN_PROBE_SHARED_CONNS`：共享一个 Endpoint 的连接数，默认 4。
//! - `CB_QUINN_PROBE_STREAMS`：单连接并发流数，默认 `1,2,4,8`。
//! - `CB_QUINN_PROBE_RAW_DATAGRAMS`：原始 GSO 每档数据报数，默认 100000。
//! - `CB_QUINN_PROBE_SOCKBUF_MIB`：socket 缓冲（MiB），默认 8，0 = 系统默认。
//! - `CB_QUINN_PROBE_SKIP_RAW=1`：跳过原始 UDP GSO 段测。

#![cfg(windows)]

use std::{
	io::{self, IoSliceMut},
	net::{Ipv4Addr, UdpSocket},
	sync::Arc,
	thread,
	time::{Duration, Instant},
};

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::udp::{self, RecvMeta, Transmit, UdpSockRef, UdpSocketState};
use quinn::{
	ClientConfig, Endpoint, EndpointConfig, MtuDiscoveryConfig, RecvStream, Runtime, SendStream,
	ServerConfig, TransportConfig, VarInt,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::sync::mpsc;

/// 与 `cb_core::transport` 完全一致的 ALPN。
const ALPN: &[u8] = b"clipbridge-v1";
/// 镜像应用的 QUIC 空闲超时（毫秒）。
const QUIC_MAX_IDLE_TIMEOUT_MS: u32 = 10_000;
/// 镜像应用的 keep-alive 间隔。
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(2);
/// 镜像应用的单流接收窗口（8 MiB）。
const QUIC_STREAM_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;
/// 镜像应用的连接级接收窗口（64 MiB）。
const QUIC_RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;
/// 镜像应用的连接级发送窗口（32 MiB）。
const QUIC_SEND_WINDOW: u32 = 32 * 1024 * 1024;

/// quinn/QUIC 的默认稳态 UDP payload（1500 - 20 IP - 28 UDP/选项余量）。
const DEFAULT_PAYLOAD: u16 = 1452;
/// quinn 0.11.12 的发送批段数硬上限（`quinn/src/connection.rs:1341`）。
const QUINN_MAX_TRANSMIT_SEGMENTS: usize = 10;
/// quinn-proto 0.11.19 默认 Cubic 的初始拥塞窗口（`CubicConfig::default()`：
/// `14720.clamp(2*1200, 10*1200)`）。
///
/// 这个值是一个**隐藏的 payload 上限**：`Connection::poll_transmit` 在
/// `in_flight.bytes + segment_size >= cwnd` 时直接跳过 Data 空间
/// （`quinn-proto/src/connection/mod.rs:616`）。如果 `segment_size`（=当前 MTU）
/// 从第一个包起就不小于 cwnd，连接永远发不出应用数据，cwnd 也不会增长 → 死锁。
/// 因此默认配置下 payload 必须明显小于 14720 B；16000 B 需要同时抬高初始 cwnd。
const QUINN_DEFAULT_INITIAL_CWND: u64 = 14720;
/// 16000 B 诊断行用的初始 cwnd：必须大于 payload（取 100 KiB，留足增长空间）。
const RAISED_INITIAL_CWND: u64 = 100 * 1024;
/// 应用层 `write_all` 的分块大小（与 B2 批量写一致）。
const CHUNK_BYTES: usize = 256 * 1024;
/// 服务端读缓冲。
const SERVER_READ_BUF: usize = 64 * 1024;
/// 原始 UDP GSO 段测使用的固定 payload。
const RAW_PAYLOAD: usize = DEFAULT_PAYLOAD as usize;
/// 原始 UDP 接收缓冲：要装得下一整个 GRO 合并结果 + 余量。
const RAW_RECV_BUF: usize = 256 * 1024;
/// 原始 UDP 接收端阻塞超时。
const RAW_READ_TIMEOUT: Duration = Duration::from_millis(100);
/// 原始 UDP 发送端阻塞超时，避免接收端提前退出时 join 挂死。
const RAW_SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// 原始 UDP 单档硬上限。
const RAW_MAX_RUN: Duration = Duration::from_secs(120);
/// 发送端结束后，连续多少个读超时没有新包就认为排空。
const RAW_IDLE_AFTER_SENDER: u32 = 5;
/// 原始 UDP 段的 socket 缓冲（与历史 `udp_batch_probe` 的 8 MiB 档一致）。
const RAW_SOCKBUF: usize = 8 << 20;

const MIB: f64 = 1024.0 * 1024.0;

// ---------------------------------------------------------------------------
// 进程 CPU 时间（kernel + user）
// ---------------------------------------------------------------------------

/// Windows `GetProcessTimes`。本文件整体 `cfg(windows)`，因此这里总是可用。
mod process_cpu {
	#[repr(C)]
	#[derive(Default)]
	struct FileTime {
		low: u32,
		high: u32,
	}

	impl FileTime {
		fn as_100ns(&self) -> u64 {
			(u64::from(self.high) << 32) | u64::from(self.low)
		}
	}

	#[link(name = "kernel32")]
	extern "system" {
		fn GetCurrentProcess() -> isize;
		fn GetProcessTimes(
			process: isize,
			creation: *mut FileTime,
			exit: *mut FileTime,
			kernel: *mut FileTime,
			user: *mut FileTime,
		) -> i32;
	}

	/// 返回本进程累计 CPU 时间（100ns）。失败返回 `None`。
	pub(super) fn total_100ns() -> Option<u64> {
		let mut creation = FileTime::default();
		let mut exit = FileTime::default();
		let mut kernel = FileTime::default();
		let mut user = FileTime::default();

		// SAFETY: GetCurrentProcess 返回本进程伪句柄（无需关闭）；其余参数都是
		// 指向栈上已初始化 FILETIME 的有效可变指针，调用期间保持有效。
		let ok = unsafe {
			GetProcessTimes(
				GetCurrentProcess(),
				&mut creation,
				&mut exit,
				&mut kernel,
				&mut user,
			)
		};

		(ok != 0).then(|| kernel.as_100ns() + user.as_100ns())
	}
}

// ---------------------------------------------------------------------------
// 环境变量
// ---------------------------------------------------------------------------

struct ProbeConfig {
	file_mib: u64,
	payloads: Vec<u16>,
	endpoint_counts: Vec<usize>,
	shared_conns: usize,
	streams: Vec<usize>,
	raw_datagrams: u64,
	/// 0 = 不改 socket 缓冲（Windows 默认），否则为字节数。
	sockbuf_bytes: usize,
	quick: bool,
}

impl ProbeConfig {
	fn from_env() -> Self {
		let quick = env_flag("CB_QUINN_PROBE_QUICK");
		let file_mib = if quick {
			env_u64("CB_QUINN_PROBE_FILE_MIB", 8)
		} else {
			env_u64("CB_QUINN_PROBE_FILE_MIB", 64)
		};
		let payloads = if quick {
			env_list("CB_QUINN_PROBE_PAYLOADS", &[1452u16, 9000])
		} else {
			env_list(
				"CB_QUINN_PROBE_PAYLOADS",
				&[1452u16, 3000, 6000, 9000, 16000],
			)
		};
		let endpoint_counts = if quick {
			env_list("CB_QUINN_PROBE_ENDPOINTS", &[1usize, 4])
		} else {
			env_list("CB_QUINN_PROBE_ENDPOINTS", &[1usize, 2, 4])
		};
		let streams = if quick {
			env_list("CB_QUINN_PROBE_STREAMS", &[1usize, 4])
		} else {
			env_list("CB_QUINN_PROBE_STREAMS", &[1usize, 2, 4, 8])
		};
		let raw_datagrams = if quick {
			env_u64("CB_QUINN_PROBE_RAW_DATAGRAMS", 20_000)
		} else {
			env_u64("CB_QUINN_PROBE_RAW_DATAGRAMS", 100_000)
		};
		let sockbuf_mib = env_u64("CB_QUINN_PROBE_SOCKBUF_MIB", 8);
		let sockbuf_bytes = usize::try_from(sockbuf_mib)
			.expect("CB_QUINN_PROBE_SOCKBUF_MIB 超出 usize")
			.saturating_mul(1 << 20);

		assert!(file_mib > 0, "CB_QUINN_PROBE_FILE_MIB 必须 > 0");
		assert!(!payloads.is_empty(), "CB_QUINN_PROBE_PAYLOADS 不能为空");
		assert!(
			payloads.iter().all(|p| (1200..=60_000).contains(p)),
			"payload 必须在 1200..=60000 字节（loopback IPv4 上限内）: {payloads:?}"
		);
		assert!(
			!endpoint_counts.is_empty() && endpoint_counts.iter().all(|n| *n > 0),
			"CB_QUINN_PROBE_ENDPOINTS 必须非空且都 >= 1"
		);
		assert!(
			!streams.is_empty() && streams.iter().all(|n| *n > 0),
			"CB_QUINN_PROBE_STREAMS 必须非空且都 >= 1"
		);
		assert!(
			env_u64("CB_QUINN_PROBE_SHARED_CONNS", 4) > 0,
			"CB_QUINN_PROBE_SHARED_CONNS 必须 >= 1"
		);

		Self {
			file_mib,
			payloads,
			endpoint_counts,
			shared_conns: usize::try_from(env_u64("CB_QUINN_PROBE_SHARED_CONNS", 4))
				.expect("shared_conns 超出 usize"),
			streams,
			raw_datagrams,
			sockbuf_bytes,
			quick,
		}
	}

	fn file_bytes(&self) -> u64 {
		self.file_mib.saturating_mul(1 << 20)
	}

	fn sockbuf_label(&self) -> String {
		if self.sockbuf_bytes == 0 {
			"default".to_string()
		} else {
			format!("{}MiB", self.sockbuf_bytes >> 20)
		}
	}
}

fn env_u64(name: &str, default: u64) -> u64 {
	match std::env::var(name) {
		Ok(raw) => raw
			.trim()
			.parse::<u64>()
			.unwrap_or_else(|_| panic!("{name} 必须是非负整数，实际是 {raw:?}")),
		Err(_) => default,
	}
}

fn env_flag(name: &str) -> bool {
	matches!(
		std::env::var(name).ok().as_deref(),
		Some("1") | Some("true") | Some("yes")
	)
}

fn env_list<T>(name: &str, default: &[T]) -> Vec<T>
where
	T: std::str::FromStr + Clone,
	T::Err: std::fmt::Display,
{
	match std::env::var(name) {
		Err(_) => default.to_vec(),
		Ok(raw) => {
			let mut values = Vec::new();
			for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
				values.push(
					item.parse::<T>()
						.unwrap_or_else(|e| panic!("{name} 项 {item:?} 解析失败: {e}")),
				);
			}
			assert!(!values.is_empty(), "{name} 不能为空");
			values
		}
	}
}

// ---------------------------------------------------------------------------
// 传输配置 / TLS（镜像应用，只把 payload 参数化）
// ---------------------------------------------------------------------------

/// 探针用 `TransportConfig`：超时/窗口镜像 `cb_core::transport::quic_transport_config`。
///
/// payload > 1452 时再放大 MTU 相关三项。注意三者缺一不可：
/// - `initial_mtu`：本端 DPLPMTUD 的起始/当前 MTU；
/// - `min_mtu`：设置成同一值，避免链路抖动触发黑洞检测后静默退回 1200，
///   让“payload 变大是否真的生效”有一个确定答案（若链路不支持，会以失败/
///   低吞吐暴露，而不是悄悄退化）；
/// - `mtu_discovery_config.upper_bound`：quinn 默认 1452，不抬高的话搜索上界
///   会把当前 MTU 压回 1452。
///
/// `initial_cwnd` 只在诊断 16000 B 时使用：默认 Cubic 初始窗口 14720 B 小于
/// 16000 B 的满包，会导致 `poll_transmit` 永久被 cwnd 门控（见常量注释）。
fn probe_transport_config(payload: u16, initial_cwnd: Option<u64>) -> TransportConfig {
	let mut config = TransportConfig::default();
	config.max_idle_timeout(Some(VarInt::from_u32(QUIC_MAX_IDLE_TIMEOUT_MS).into()));
	config.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
	config.stream_receive_window(VarInt::from_u32(QUIC_STREAM_RECEIVE_WINDOW));
	config.receive_window(VarInt::from_u32(QUIC_RECEIVE_WINDOW));
	config.send_window(u64::from(QUIC_SEND_WINDOW));

	if payload > DEFAULT_PAYLOAD {
		config.initial_mtu(payload);
		config.min_mtu(payload);
		let mut mtud = MtuDiscoveryConfig::default();
		mtud.upper_bound(payload);
		config.mtu_discovery_config(Some(mtud));
	}

	if let Some(cwnd) = initial_cwnd {
		// quinn 通过 re-export 提供 `congestion` 模块；这里用 Cubic（quinn 默认
		// 控制器）只改初始窗口，不改变拥塞控制算法本身。
		let mut cubic = quinn::congestion::CubicConfig::default();
		cubic.initial_window(cwnd);
		config.congestion_controller_factory(Arc::new(cubic));
	}
	config
}

/// `EndpointConfig::max_udp_payload_size` 决定两个东西：握手时通告的
/// `max_udp_payload_size` transport parameter（对端据此决定最多发多大）以及
/// 本端接收缓冲。默认 1472；payload > 1452 时同步抬高。
fn probe_endpoint_config(payload: u16) -> EndpointConfig {
	let mut config = EndpointConfig::default();
	if payload > DEFAULT_PAYLOAD {
		config
			.max_udp_payload_size(payload)
			.expect("payload 已在 1200..=60000 内，EndpointConfig 不会拒绝");
	}
	config
}

/// 探针专用 TLS 校验器：接受任意服务端证书（等价于应用的 BlindVerifier）。
#[derive(Debug)]
struct ProbeVerifier(Arc<CryptoProvider>);

impl ProbeVerifier {
	fn new(provider: Arc<CryptoProvider>) -> Self {
		Self(provider)
	}
}

impl ServerCertVerifier for ProbeVerifier {
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

fn build_server_config(
	payload: u16,
	initial_cwnd: Option<u64>,
	cert_chain: &[CertificateDer<'static>],
	priv_key: &PrivateKeyDer<'static>,
) -> Result<ServerConfig, String> {
	let crypto = Arc::new(rustls::crypto::ring::default_provider());
	let mut tls = rustls::ServerConfig::builder_with_provider(crypto)
		.with_protocol_versions(&[&rustls::version::TLS13])
		.map_err(|e| format!("server TLS version config failed: {e}"))?
		.with_no_client_auth()
		.with_single_cert(cert_chain.to_vec(), priv_key.clone_key())
		.map_err(|e| format!("server cert config failed: {e}"))?;
	tls.alpn_protocols = vec![ALPN.to_vec()];

	let quic = QuicServerConfig::try_from(tls)
		.map_err(|e| format!("QuicServerConfig::try_from failed: {e}"))?;
	let mut config = ServerConfig::with_crypto(Arc::new(quic));
	config.transport_config(Arc::new(probe_transport_config(payload, initial_cwnd)));
	Ok(config)
}

fn build_client_config(payload: u16, initial_cwnd: Option<u64>) -> Result<ClientConfig, String> {
	let crypto = Arc::new(rustls::crypto::ring::default_provider());
	let mut tls = rustls::ClientConfig::builder_with_provider(crypto.clone())
		.with_protocol_versions(&[&rustls::version::TLS13])
		.map_err(|e| format!("client TLS version config failed: {e}"))?
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(ProbeVerifier::new(crypto)))
		.with_no_client_auth();
	tls.alpn_protocols = vec![ALPN.to_vec()];

	let quic = QuicClientConfig::try_from(tls)
		.map_err(|e| format!("QuicClientConfig::try_from failed: {e}"))?;
	let mut config = ClientConfig::new(Arc::new(quic));
	config.transport_config(Arc::new(probe_transport_config(payload, initial_cwnd)));
	Ok(config)
}

// ---------------------------------------------------------------------------
// Endpoint 构造
// ---------------------------------------------------------------------------

/// 建一个 loopback Endpoint；可选地把 socket 缓冲抬到 `sockbuf_bytes`。
///
/// quinn 没有暴露 socket 缓冲设置，但 `quinn::udp::UdpSocketState` 有
/// `set_send_buffer_size` / `set_recv_buffer_size`。所以这里先自己 bind，用
/// `UdpSocketState` 设置缓冲，再把 socket 交给 `Endpoint::new`。`UdpSocketState`
/// 没有 Drop 副作用，离开作用域只是丢弃状态。
fn create_endpoint(
	endpoint_config: EndpointConfig,
	server_config: Option<ServerConfig>,
	client_config: Option<ClientConfig>,
	sockbuf_bytes: usize,
) -> Result<Endpoint, String> {
	let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
		.map_err(|e| format!("bind loopback UDP socket failed: {e}"))?;

	if sockbuf_bytes > 0 {
		let state = UdpSocketState::new(UdpSockRef::from(&socket))
			.map_err(|e| format!("UdpSocketState::new failed: {e}"))?;
		state
			.set_send_buffer_size(UdpSockRef::from(&socket), sockbuf_bytes)
			.map_err(|e| format!("set_send_buffer_size({sockbuf_bytes}) failed: {e}"))?;
		state
			.set_recv_buffer_size(UdpSockRef::from(&socket), sockbuf_bytes)
			.map_err(|e| format!("set_recv_buffer_size({sockbuf_bytes}) failed: {e}"))?;
	}

	let runtime: Arc<dyn Runtime> = Arc::new(quinn::TokioRuntime);
	let mut endpoint = Endpoint::new(endpoint_config, server_config, socket, runtime)
		.map_err(|e| format!("Endpoint::new failed: {e}"))?;
	if let Some(config) = client_config {
		endpoint.set_default_client_config(config);
	}
	Ok(endpoint)
}

// ---------------------------------------------------------------------------
// QUIC 用例
// ---------------------------------------------------------------------------

struct CaseSpec {
	label: String,
	payload: u16,
	conns: usize,
	endpoint_pairs: usize,
	streams: usize,
	file_bytes: u64,
	sockbuf_bytes: usize,
	/// 仅诊断用：覆盖 Cubic 初始拥塞窗口（`None` = quinn 默认 14720 B）。
	initial_cwnd: Option<u64>,
}

struct CaseMetrics {
	bytes: u64,
	elapsed: Duration,
	cpu_seconds: f64,
	tx_datagrams: u64,
	tx_bytes: u64,
	tx_ios: u64,
	lost_packets: u64,
	congestion_events: u64,
	black_holes: u64,
	plpmtud_probes: u64,
	plpmtud_lost: u64,
	mtu: u16,
}

impl CaseMetrics {
	fn mib_per_s(&self) -> f64 {
		self.bytes as f64 / MIB / self.elapsed.as_secs_f64().max(1e-9)
	}

	fn packets_per_s(&self) -> f64 {
		self.tx_datagrams as f64 / self.elapsed.as_secs_f64().max(1e-9)
	}

	fn cpu_percent(&self) -> f64 {
		self.cpu_seconds / self.elapsed.as_secs_f64().max(1e-9) * 100.0
	}

	fn avg_tx_datagram_bytes(&self) -> f64 {
		if self.tx_datagrams == 0 {
			0.0
		} else {
			self.tx_bytes as f64 / self.tx_datagrams as f64
		}
	}

	fn datagrams_per_io(&self) -> f64 {
		if self.tx_ios == 0 {
			0.0
		} else {
			self.tx_datagrams as f64 / self.tx_ios as f64
		}
	}
}

struct CaseRow {
	spec: CaseSpec,
	outcome: Result<CaseMetrics, String>,
}

/// 跑一个 QUIC 用例。所有连接先建好，再同时开始写，服务端收到全部字节后结束计时。
async fn run_case(
	spec: &CaseSpec,
	cert_chain: &[CertificateDer<'static>],
	priv_key: &PrivateKeyDer<'static>,
) -> Result<CaseMetrics, String> {
	assert!(spec.endpoint_pairs >= 1, "endpoint_pairs 必须 >= 1");
	assert!(
		spec.conns >= spec.endpoint_pairs && spec.conns.is_multiple_of(spec.endpoint_pairs),
		"conns({}) 必须能被 endpoint_pairs({}) 整除且不少于它",
		spec.conns,
		spec.endpoint_pairs
	);
	let conns_per_pair = spec.conns / spec.endpoint_pairs;
	let total_streams = spec.conns * spec.streams;

	// quinn 默认 Cubic 初始窗口 14720 B。payload 不小于它时，
	// `poll_transmit` 的 `in_flight + segment_size >= cwnd` 门控连**握手包**都
	// 发不出去（门控用 segment_size=当前 MTU 作估算，而不是实际包长），两端
	// 都卡住，表现为握手超时而不是数据阶段变慢。这里用更短的握手超时把
	// 失败尽快暴露出来；诊断行会显式抬高 cwnd 作为对照。
	let payload_exceeds_cwnd =
		u64::from(spec.payload) >= QUINN_DEFAULT_INITIAL_CWND && spec.initial_cwnd.is_none();
	let handshake_timeout = if payload_exceeds_cwnd {
		Duration::from_secs(3)
	} else {
		Duration::from_secs(10)
	};

	// --- 服务端 Endpoint + 每对端一个 accept 任务 ---
	let mut server_endpoints = Vec::with_capacity(spec.endpoint_pairs);
	let mut server_addrs = Vec::with_capacity(spec.endpoint_pairs);
	let (done_tx, mut done_rx) = mpsc::channel::<Result<u64, String>>(total_streams);

	for _ in 0..spec.endpoint_pairs {
		let server_config =
			build_server_config(spec.payload, spec.initial_cwnd, cert_chain, priv_key)?;
		let endpoint = create_endpoint(
			probe_endpoint_config(spec.payload),
			Some(server_config),
			None,
			spec.sockbuf_bytes,
		)?;
		let addr = endpoint
			.local_addr()
			.map_err(|e| format!("server local_addr failed: {e}"))?;
		let accept_endpoint = endpoint.clone();
		let tx = done_tx.clone();
		let streams = spec.streams;
		tokio::spawn(async move {
			accept_connections(accept_endpoint, conns_per_pair, streams, tx).await;
		});
		server_addrs.push(addr);
		server_endpoints.push(endpoint);
	}

	// --- 客户端 Endpoint ---
	let mut client_endpoints = Vec::with_capacity(spec.endpoint_pairs);
	for _ in 0..spec.endpoint_pairs {
		let client_config = build_client_config(spec.payload, spec.initial_cwnd)?;
		let endpoint = create_endpoint(
			probe_endpoint_config(spec.payload),
			None,
			Some(client_config),
			spec.sockbuf_bytes,
		)?;
		client_endpoints.push(endpoint);
	}

	// --- 建连（握手不计入吞吐） ---
	let mut conns = Vec::with_capacity(spec.conns);
	for i in 0..spec.conns {
		let pair = i % spec.endpoint_pairs;
		let connecting = client_endpoints[pair]
			.connect(server_addrs[pair], "localhost")
			.map_err(|e| format!("client connect() failed: {e}"))?;
		let conn = match tokio::time::timeout(handshake_timeout, connecting).await {
			Ok(Ok(conn)) => conn,
			Ok(Err(e)) => return Err(format!("client handshake failed: {e}")),
			Err(_) => {
				return Err(format!(
					"握手 {}s 超时：payload={}B >= quinn 默认初始拥塞窗口 {}B，`poll_transmit` 因 `in_flight + segment_size >= cwnd` 连握手包都发不出去（两端都卡）。对照行 `+init_cwnd` 可验证。",
					handshake_timeout.as_secs(),
					spec.payload,
					QUINN_DEFAULT_INITIAL_CWND
				));
			}
		};
		conns.push(conn);
	}

	// --- 计时开始，并发写所有流 ---
	let per_stream = spec.file_bytes / total_streams as u64;
	let remainder = spec.file_bytes % total_streams as u64;
	let chunk = Arc::new(vec![0xABu8; CHUNK_BYTES]);

	// 数据阶段内部超时：比外层 timeout 更早触发，这样还能拿到连接统计来解释
	// “为什么卡住”（外层 timeout 直接 drop future，统计就丢了）。
	let data_timeout = if payload_exceeds_cwnd {
		Duration::from_secs(4)
	} else {
		Duration::from_secs(20)
	};

	let cpu_before = process_cpu::total_100ns().ok_or("GetProcessTimes before failed")?;
	let transfer = async {
		let started = Instant::now();
		let mut writers = Vec::with_capacity(total_streams);
		for (conn_idx, conn) in conns.iter().enumerate() {
			for stream_idx in 0..spec.streams {
				let global_idx = conn_idx * spec.streams + stream_idx;
				let bytes = per_stream + u64::from((global_idx as u64) < remainder);
				let conn = conn.clone();
				let chunk = chunk.clone();
				writers.push(tokio::spawn(async move {
					let send = conn
						.open_uni()
						.await
						.map_err(|e| format!("client open_uni failed: {e}"))?;
					write_stream(send, bytes, chunk).await
				}));
			}
		}
		for writer in writers {
			writer
				.await
				.map_err(|e| format!("client writer task panicked: {e}"))??;
		}

		let mut received = 0u64;
		for _ in 0..total_streams {
			match done_rx.recv().await {
				Some(Ok(n)) => received += n,
				Some(Err(e)) => return Err(e),
				None => {
					return Err("server reader channel closed before all streams finished".into())
				}
			}
		}
		Ok::<(u64, Duration), String>((received, started.elapsed()))
	};

	let transfer_result = tokio::time::timeout(data_timeout, transfer).await;
	let (received, elapsed) = match transfer_result {
		Ok(Ok(value)) => value,
		Ok(Err(error)) => return Err(error),
		Err(_) => {
			let mut detail = Vec::new();
			for (idx, conn) in conns.iter().enumerate() {
				let stats = conn.stats();
				detail.push(format!(
					"conn{idx}: sent_pkts={} cwnd={} mtu={} lost={}",
					stats.path.sent_packets,
					stats.path.cwnd,
					stats.path.current_mtu,
					stats.path.lost_packets
				));
			}
			let hint = if payload_exceeds_cwnd {
				format!(
					"；payload {}B >= quinn 默认初始拥塞窗口 {QUINN_DEFAULT_INITIAL_CWND}B：poll_transmit 被 cwnd 门控，第一个满包就发不出去（payload=16000+init_cwnd 是下面的对照行）",
					spec.payload
				)
			} else {
				String::new()
			};
			return Err(format!(
				"数据阶段 {}s 超时（writer 被发送窗口/拥塞控制卡住）。{}{hint}",
				data_timeout.as_secs(),
				detail.join("; ")
			));
		}
	};
	let cpu_after = process_cpu::total_100ns().ok_or("GetProcessTimes after failed")?;
	let cpu_seconds = cpu_after.saturating_sub(cpu_before) as f64 / 10_000_000.0;

	if received != spec.file_bytes {
		return Err(format!(
			"服务端收到 {received} 字节，期望 {} 字节",
			spec.file_bytes
		));
	}

	// --- 聚合客户端连接统计：这是“payload 是否真的变大”的硬证据 ---
	let mut metrics = CaseMetrics {
		bytes: received,
		elapsed,
		cpu_seconds,
		tx_datagrams: 0,
		tx_bytes: 0,
		tx_ios: 0,
		lost_packets: 0,
		congestion_events: 0,
		black_holes: 0,
		plpmtud_probes: 0,
		plpmtud_lost: 0,
		mtu: 0,
	};
	for conn in &conns {
		let stats = conn.stats();
		metrics.tx_datagrams += stats.udp_tx.datagrams;
		metrics.tx_bytes += stats.udp_tx.bytes;
		metrics.tx_ios += stats.udp_tx.ios;
		metrics.lost_packets += stats.path.lost_packets;
		metrics.congestion_events += stats.path.congestion_events;
		metrics.black_holes += stats.path.black_holes_detected;
		metrics.plpmtud_probes += stats.path.sent_plpmtud_probes;
		metrics.plpmtud_lost += stats.path.lost_plpmtud_probes;
		metrics.mtu = metrics.mtu.max(stats.path.current_mtu);
	}

	// --- 尽力干净关闭，避免上一用例的连接/定时器拖到下一用例 ---
	shutdown_endpoints(&client_endpoints, &server_endpoints).await;

	Ok(metrics)
}

/// 服务端：先接受 `expected_conns` 条连接，再为每条连接接受
/// `streams_per_conn` 条单向流并并发读到 EOF。
///
/// 必须**先把所有连接 accept 完**再去 accept 流：服务端的握手要等应用调用
/// `Incoming::await` 才会推进，如果 accept 完第一条连接就阻塞在
/// `conn.accept_uni()` 上（此时客户端还没开始开流），后面的连接会握手超时
/// ——本探针第一版就踩过这个坑（4 conns / 1 endpoint 报 handshake timed out）。
///
/// 任何一步失败都把错误填满剩余的名额，避免主循环永远等不到 `total` 条完成消息。
async fn accept_connections(
	endpoint: Endpoint,
	expected_conns: usize,
	streams_per_conn: usize,
	done_tx: mpsc::Sender<Result<u64, String>>,
) {
	let total = expected_conns * streams_per_conn;
	let mut spawned = 0usize;
	let mut failure: Option<String> = None;
	let mut conns = Vec::with_capacity(expected_conns);

	for _ in 0..expected_conns {
		let incoming = match endpoint.accept().await {
			Some(incoming) => incoming,
			None => {
				failure = Some("server endpoint closed before accept".to_string());
				break;
			}
		};
		match incoming.await {
			Ok(conn) => conns.push(conn),
			Err(e) => {
				failure = Some(format!("server handshake failed: {e}"));
				break;
			}
		}
	}

	if failure.is_none() {
		'outer: for conn in conns {
			for _ in 0..streams_per_conn {
				match conn.accept_uni().await {
					Ok(mut recv) => {
						let tx = done_tx.clone();
						tokio::spawn(async move {
							let result = read_stream_to_end(&mut recv).await;
							let _ = tx.send(result).await;
						});
						spawned += 1;
					}
					Err(e) => {
						failure = Some(format!("server accept_uni failed: {e}"));
						break 'outer;
					}
				}
			}
		}
	}

	if let Some(error) = failure {
		for _ in spawned..total {
			let _ = done_tx.send(Err(error.clone())).await;
		}
	}
}

async fn read_stream_to_end(recv: &mut RecvStream) -> Result<u64, String> {
	let mut buf = vec![0u8; SERVER_READ_BUF];
	let mut total = 0u64;
	loop {
		match recv.read(&mut buf).await {
			Ok(Some(n)) => total += n as u64,
			Ok(None) => return Ok(total),
			Err(e) => return Err(format!("server read failed: {e}")),
		}
	}
}

async fn write_stream(mut send: SendStream, bytes: u64, chunk: Arc<Vec<u8>>) -> Result<(), String> {
	let chunk_len = chunk.len() as u64;
	let mut remaining = bytes;
	while remaining > 0 {
		let n = remaining.min(chunk_len) as usize;
		send.write_all(&chunk[..n])
			.await
			.map_err(|e| format!("client write_all failed: {e}"))?;
		remaining -= n as u64;
	}
	send.finish()
		.map_err(|e| format!("client finish failed: {e}"))?;
	Ok(())
}

/// 关闭并等待 idle（有界），主要目的是让用例之间不互相污染 CPU 统计。
async fn shutdown_endpoints(clients: &[Endpoint], servers: &[Endpoint]) {
	for endpoint in clients.iter().chain(servers.iter()) {
		endpoint.close(VarInt::from_u32(0), b"probe case done");
	}
	let wait = async {
		for endpoint in clients.iter().chain(servers.iter()) {
			endpoint.wait_idle().await;
		}
	};
	let _ = tokio::time::timeout(Duration::from_millis(500), wait).await;
}

// ---------------------------------------------------------------------------
// 原始 UDP GSO 段测（MAX_TRANSMIT_SEGMENTS 上界估计）
// ---------------------------------------------------------------------------

struct RawRow {
	payload: usize,
	segments: usize,
	datagrams: u64,
	bytes: u64,
	recv_active: Duration,
	cpu_seconds: f64,
	drops: u64,
	coalesced_calls: u64,
	max_stride: usize,
	errors: Vec<String>,
}

impl RawRow {
	fn mib_per_s(&self) -> f64 {
		self.bytes as f64 / MIB / self.recv_active.as_secs_f64().max(1e-9)
	}

	fn packets_per_s(&self) -> f64 {
		self.datagrams as f64 / self.recv_active.as_secs_f64().max(1e-9)
	}

	fn cpu_percent(&self) -> f64 {
		self.cpu_seconds / self.recv_active.as_secs_f64().max(1e-9) * 100.0
	}
}

/// 一次 `recv` 调用合并了多少个数据报（`stride < len` 即 GRO 生效）。
fn datagrams_in(meta: &RecvMeta) -> u64 {
	if meta.len == 0 {
		0
	} else if meta.stride == 0 {
		1
	} else {
		meta.len.div_ceil(meta.stride) as u64
	}
}

fn is_timeout(error: &io::Error) -> bool {
	matches!(
		error.kind(),
		io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
	)
}

/// 原始 UDP 段测：一次 `WSASendMsg` 发 `segments` 个 `payload` 字节的数据报（GSO），
/// 接收端逐包收。`segments = 1` 是“一包一次 syscall”基线。
///
/// 这就是 quinn 实际使用的 `quinn-udp` Windows 路径，只是去掉了 TLS/ACK/
/// 拥塞控制/帧编解码，因此得到的是“发送 syscall 批次变大”的**吞吐上界**。
fn raw_gso_case(payload: usize, segments: usize, total_datagrams: u64) -> RawRow {
	let recv_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("raw bind receiver");
	let send_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("raw bind sender");
	let dst = recv_sock.local_addr().expect("raw receiver local_addr");

	let send_state = UdpSocketState::new(UdpSockRef::from(&send_sock)).expect("raw sender state");
	let recv_state = UdpSocketState::new(UdpSockRef::from(&recv_sock)).expect("raw receiver state");
	send_state
		.set_send_buffer_size(UdpSockRef::from(&send_sock), RAW_SOCKBUF)
		.expect("raw SO_SNDBUF");
	recv_state
		.set_recv_buffer_size(UdpSockRef::from(&recv_sock), RAW_SOCKBUF)
		.expect("raw SO_RCVBUF");

	send_sock
		.set_nonblocking(false)
		.expect("raw sender blocking");
	recv_sock
		.set_nonblocking(false)
		.expect("raw receiver blocking");
	recv_sock
		.set_read_timeout(Some(RAW_READ_TIMEOUT))
		.expect("raw read timeout");
	send_sock
		.set_write_timeout(Some(RAW_SEND_TIMEOUT))
		.expect("raw write timeout");

	let sends = total_datagrams.div_ceil(segments as u64);
	let buffer = vec![0x5Au8; payload * segments];

	let sender_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
	let sender_flag = Arc::clone(&sender_done);
	let cpu_before = process_cpu::total_100ns().expect("GetProcessTimes before raw");

	let sender = thread::spawn(move || {
		let transmit = Transmit {
			destination: dst,
			ecn: None,
			contents: &buffer,
			segment_size: (segments > 1).then_some(payload),
			src_ip: None,
		};
		let mut sent = 0u64;
		let mut errors = Vec::new();
		for _ in 0..sends {
			match send_state.try_send(UdpSockRef::from(&send_sock), &transmit) {
				Ok(()) => sent += 1,
				Err(e) => {
					if errors.len() < 4 {
						errors.push(format!("{e} [raw={:?}]", e.raw_os_error()));
					}
					break;
				}
			}
		}
		sender_flag.store(true, std::sync::atomic::Ordering::Release);
		(sent, errors)
	});

	let receiver = {
		let sender_done = Arc::clone(&sender_done);
		thread::spawn(move || {
			let mut buf = vec![0u8; RAW_RECV_BUF];
			let mut meta = [RecvMeta::default()];
			let mut datagrams = 0u64;
			let mut bytes = 0u64;
			let mut calls = 0u64;
			let mut coalesced_calls = 0u64;
			let mut max_stride = 0usize;
			let mut errors: Vec<String> = Vec::new();
			let mut start: Option<Duration> = None;
			let mut end = Duration::ZERO;
			let mut idle = 0u32;
			let started = Instant::now();

			loop {
				let mut iovs = [IoSliceMut::new(buf.as_mut_slice())];
				match recv_state.recv(UdpSockRef::from(&recv_sock), &mut iovs, &mut meta) {
					Ok(n) if n > 0 => {
						idle = 0;
						calls += 1;
						let m = meta[0];
						let got = datagrams_in(&m);
						datagrams += got;
						bytes += m.len as u64;
						max_stride = max_stride.max(m.stride);
						if m.stride > 0 && m.len > m.stride {
							coalesced_calls += 1;
						}
						let now = started.elapsed();
						if start.is_none() {
							start = Some(now);
						}
						end = now;
						if datagrams >= total_datagrams {
							break;
						}
					}
					Ok(_) => {}
					Err(e) if is_timeout(&e) => {
						if sender_done.load(std::sync::atomic::Ordering::Acquire) {
							idle += 1;
							if idle >= RAW_IDLE_AFTER_SENDER {
								break;
							}
						}
						if started.elapsed() > RAW_MAX_RUN {
							break;
						}
					}
					Err(e) => {
						if errors.len() < 4 {
							errors.push(format!("{e} [raw={:?}]", e.raw_os_error()));
						}
						break;
					}
				}
			}

			let recv_active = start.map_or(Duration::ZERO, |s| end.saturating_sub(s));
			(
				datagrams,
				bytes,
				recv_active,
				calls,
				coalesced_calls,
				max_stride,
				errors,
			)
		})
	};

	let (sent, mut errors) = sender.join().expect("raw sender join");
	let (datagrams, bytes, recv_active, _calls, coalesced_calls, max_stride, mut recv_errors) =
		receiver.join().expect("raw receiver join");
	let cpu_after = process_cpu::total_100ns().expect("GetProcessTimes after raw");
	let cpu_seconds = cpu_after.saturating_sub(cpu_before) as f64 / 10_000_000.0;
	errors.append(&mut recv_errors);

	RawRow {
		payload,
		segments,
		datagrams,
		bytes,
		recv_active,
		cpu_seconds,
		drops: sent
			.saturating_mul(segments as u64)
			.saturating_sub(datagrams),
		coalesced_calls,
		max_stride,
		errors,
	}
}

// ---------------------------------------------------------------------------
// 输出
// ---------------------------------------------------------------------------

fn format_rate(rate: f64) -> String {
	if rate >= 1_000_000.0 {
		format!("{:.2}M", rate / 1e6)
	} else if rate >= 1_000.0 {
		format!("{:.1}k", rate / 1e3)
	} else {
		format!("{rate:.0}")
	}
}

fn print_table_header(title: &str) {
	println!();
	println!("--- {title} ---");
	println!(
		"{:<42} | {:>9} | {:>5} | {:>9} | {:>7} | {:>9} | {:>10} | {:>6} | notes",
		"variant", "payload_B", "conns", "endpoints", "streams", "MiB/s", "pkt/s(est)", "CPU%"
	);
	println!("{}", "-".repeat(180));
}

fn print_case_row(row: &CaseRow) {
	let spec = &row.spec;
	match &row.outcome {
		Ok(metrics) => {
			let mut notes = Vec::new();
			notes.push(format!("mtu={}", metrics.mtu));
			notes.push(format!("avg_tx={:.0}B", metrics.avg_tx_datagram_bytes()));
			notes.push(format!("dg/io={:.1}", metrics.datagrams_per_io()));
			notes.push(format!("lost_pkt={}", metrics.lost_packets));
			if metrics.black_holes > 0
				|| metrics.plpmtud_probes > 0
				|| metrics.congestion_events > 0
			{
				notes.push(format!(
					"cwnd_ev={}/bh={}/plpmtu={}s/{}l",
					metrics.congestion_events,
					metrics.black_holes,
					metrics.plpmtud_probes,
					metrics.plpmtud_lost
				));
			}
			if spec.conns > 1 {
				notes.push(format!(
					"per-conn={:.1}",
					metrics.mib_per_s() / spec.conns as f64
				));
			}
			if spec.streams > 1 {
				notes.push(format!(
					"per-stream={:.1}",
					metrics.mib_per_s() / spec.streams as f64
				));
			}
			notes.push(format!(
				"sbuf={}",
				if spec.sockbuf_bytes == 0 {
					"default".to_string()
				} else {
					format!("{}MiB", spec.sockbuf_bytes >> 20)
				}
			));
			println!(
				"{:<42} | {:>9} | {:>5} | {:>9} | {:>7} | {:>9.1} | {:>10} | {:>6.1} | {}",
				spec.label,
				spec.payload,
				spec.conns,
				spec.endpoint_pairs,
				spec.streams,
				metrics.mib_per_s(),
				format_rate(metrics.packets_per_s()),
				metrics.cpu_percent(),
				notes.join(" ")
			);
		}
		Err(error) => {
			println!(
				"{:<42} | {:>9} | {:>5} | {:>9} | {:>7} | {:>9} | {:>10} | {:>6} | ERROR: {error}",
				spec.label,
				spec.payload,
				spec.conns,
				spec.endpoint_pairs,
				spec.streams,
				"-",
				"-",
				"-"
			);
		}
	}
}

fn print_raw_row(row: &RawRow) {
	let mut notes = Vec::new();
	notes.push(format!("raw quinn::udp gso segs={}", row.segments));
	notes.push(format!("drops={}", row.drops));
	notes.push(format!("coalesced={}", row.coalesced_calls));
	notes.push(format!("max_stride={}", row.max_stride));
	for error in &row.errors {
		notes.push(format!("ERR: {error}"));
	}
	println!(
		"{:<42} | {:>9} | {:>5} | {:>9} | {:>7} | {:>9.1} | {:>10} | {:>6.1} | {}",
		format!("raw-gso payload={} segs={}", row.payload, row.segments),
		row.payload,
		"-",
		"-",
		"-",
		row.mib_per_s(),
		format_rate(row.packets_per_s()),
		row.cpu_percent(),
		notes.join(" ")
	);
}

fn run_and_push(
	runtime: &tokio::runtime::Runtime,
	spec: CaseSpec,
	cert_chain: &[CertificateDer<'static>],
	priv_key: &PrivateKeyDer<'static>,
	timeout: Duration,
	rows: &mut Vec<CaseRow>,
) {
	println!("[case] {}", spec.label);
	let outcome = runtime.block_on(async {
		tokio::time::timeout(timeout, run_case(&spec, cert_chain, priv_key)).await
	});
	let outcome = match outcome {
		Ok(result) => result,
		Err(_) => Err(format!("用例超时（>{}s）", timeout.as_secs())),
	};
	// 用例之间留一点静默期：让上一轮的 close/drain 任务、socket 驱动线程退出，
	// 避免它们的 CPU 被计入下一用例（本机 tick 15.6 ms，几十毫秒就够）。
	runtime.block_on(async {
		tokio::time::sleep(Duration::from_millis(50)).await;
	});
	rows.push(CaseRow { spec, outcome });
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loopback quinn tuning probe: cargo test --release -p cb_core --test quinn_tuning_probe -- --ignored --nocapture"]
fn quinn_tuning_probe() {
	let probe_started = Instant::now();
	let cfg = ProbeConfig::from_env();

	println!();
	println!("=== ClipBridge quinn tuning probe (loopback, in-process) ===");
	println!(
		"[setup] build={} os={} arch={} logical_cpus={}",
		if cfg!(debug_assertions) {
			"debug (数字不可用于结论，请用 --release)"
		} else {
			"release"
		},
		std::env::consts::OS,
		std::env::consts::ARCH,
		thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get)
	);
	println!(
		"[setup] file={}MiB payloads={:?} endpoint_pairs={:?} shared_conns={} streams={:?} sockbuf={} raw_dgrams={}",
		cfg.file_mib,
		cfg.payloads,
		cfg.endpoint_counts,
		cfg.shared_conns,
		cfg.streams,
		cfg.sockbuf_label(),
		cfg.raw_datagrams
	);
	println!(
		"[setup] transport mirrors cb_core: TLS1.3 ALPN clipbridge-v1 idle=10s keepalive=2s stream_window=8MiB conn_window=64MiB send_window=32MiB"
	);
	println!(
		"[setup] quinn 0.11.12 caps GSO batches at MAX_TRANSMIT_SEGMENTS={QUINN_MAX_TRANSMIT_SEGMENTS}; payload>1452 needs EndpointConfig::max_udp_payload_size + TransportConfig::initial_mtu/min_mtu + MtuDiscoveryConfig::upper_bound on BOTH sides"
	);
	println!(
		"[setup] quinn default Cubic initial cwnd={QUINN_DEFAULT_INITIAL_CWND}B; poll_transmit gates on `in_flight + segment_size >= cwnd`, so payload>={QUINN_DEFAULT_INITIAL_CWND}B stalls even the handshake unless initial cwnd is raised (see +init_cwnd control row)"
	);

	// quinn-udp 的静态能力值（Windows 上 max_gso_segments 由临时 socket 的
	// UDP_SEND_MSG_SIZE setsockopt 探测得出）。
	{
		let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("capability socket");
		let state = UdpSocketState::new(UdpSockRef::from(&socket)).expect("capability state");
		println!(
			"[cap] quinn_udp BATCH_SIZE={} max_gso_segments={} gro_segments={}",
			udp::BATCH_SIZE,
			state.max_gso_segments(),
			state.gro_segments()
		);
		// socket 缓冲回读：Windows 可能不按请求值兑现。
		if cfg.sockbuf_bytes > 0 {
			let sent = state.set_send_buffer_size(UdpSockRef::from(&socket), cfg.sockbuf_bytes);
			let recv = state.set_recv_buffer_size(UdpSockRef::from(&socket), cfg.sockbuf_bytes);
			println!(
				"[cap] set sockbuf={}MiB: send={:?} recv={:?}; actual SO_SNDBUF={:?} SO_RCVBUF={:?}",
				cfg.sockbuf_bytes >> 20,
				sent.map(|()| "OK"),
				recv.map(|()| "OK"),
				state.send_buffer_size(UdpSockRef::from(&socket)).ok(),
				state.recv_buffer_size(UdpSockRef::from(&socket)).ok()
			);
		}
	}

	// --- 维度 2：原始 UDP GSO 段测（先跑，不占 tokio 线程） ---
	let mut raw_rows = Vec::new();
	if env_flag("CB_QUINN_PROBE_SKIP_RAW") {
		println!();
		println!("[raw] 跳过（CB_QUINN_PROBE_SKIP_RAW=1）");
	} else {
		println!();
		println!(
			"[raw] 原始 UDP GSO 段测：payload=1452 的 1 / 10 / 32 段；另加 payload=16000 的 1 / 10 段诊断（QUIC 16000 用例超时，用来区分是“大 payload 本身”还是“GSO 批次太大”）"
		);
		let diag_datagrams = cfg.raw_datagrams.min(20_000);
		for (payload, segments, datagrams) in [
			(RAW_PAYLOAD, 1usize, cfg.raw_datagrams),
			(RAW_PAYLOAD, QUINN_MAX_TRANSMIT_SEGMENTS, cfg.raw_datagrams),
			(RAW_PAYLOAD, 32, cfg.raw_datagrams),
			(16_000usize, 1, diag_datagrams),
			(16_000usize, QUINN_MAX_TRANSMIT_SEGMENTS, diag_datagrams),
		] {
			raw_rows.push(raw_gso_case(payload, segments, datagrams));
		}
	}

	// --- QUIC 用例 ---
	let (cert_chain, priv_key) =
		cb_core::transport::cert::generate_self_signed_cert("localhost").expect("生成自签证书失败");
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("创建 tokio runtime 失败");
	let case_timeout = Duration::from_secs(if cfg.quick { 20 } else { 30 });

	// 维度 1：payload / PMTU。
	let mut payload_rows = Vec::new();
	if cfg.sockbuf_bytes > 0 {
		run_and_push(
			&runtime,
			CaseSpec {
				label: "control: payload=1452 sockbuf=default".to_string(),
				payload: DEFAULT_PAYLOAD,
				conns: 1,
				endpoint_pairs: 1,
				streams: 1,
				file_bytes: cfg.file_bytes(),
				sockbuf_bytes: 0,
				initial_cwnd: None,
			},
			&cert_chain,
			&priv_key,
			case_timeout,
			&mut payload_rows,
		);
	}
	for &payload in &cfg.payloads {
		run_and_push(
			&runtime,
			CaseSpec {
				label: format!("payload={payload}"),
				payload,
				conns: 1,
				endpoint_pairs: 1,
				streams: 1,
				file_bytes: cfg.file_bytes(),
				sockbuf_bytes: cfg.sockbuf_bytes,
				initial_cwnd: None,
			},
			&cert_chain,
			&priv_key,
			case_timeout,
			&mut payload_rows,
		);
	}
	// 16000 B 的对照行：默认 Cubic 初始 cwnd 14720 B 会把第一个满包卡死
	// （见常量注释）。把 cwnd 抬到 100 KiB 后如果仍然跑不动，才说明问题在
	// 内核/loopback；如果跑得动，则证明限制来自 quinn 的 cwnd，而不是 MTU。
	if cfg.payloads.contains(&16_000) {
		run_and_push(
			&runtime,
			CaseSpec {
				label: "payload=16000 + init_cwnd=100KiB".to_string(),
				payload: 16_000,
				conns: 1,
				endpoint_pairs: 1,
				streams: 1,
				file_bytes: cfg.file_bytes(),
				sockbuf_bytes: cfg.sockbuf_bytes,
				initial_cwnd: Some(RAISED_INITIAL_CWND),
			},
			&cert_chain,
			&priv_key,
			case_timeout,
			&mut payload_rows,
		);
	}

	// 维度 3：Endpoint 数量（全部 1452 B，与历史 402-478 MiB/s 数字可比）。
	let mut endpoint_rows = Vec::new();
	for &pairs in &cfg.endpoint_counts {
		run_and_push(
			&runtime,
			CaseSpec {
				label: format!("endpoints={pairs} conns={pairs}"),
				payload: DEFAULT_PAYLOAD,
				conns: pairs,
				endpoint_pairs: pairs,
				streams: 1,
				file_bytes: cfg.file_bytes(),
				sockbuf_bytes: cfg.sockbuf_bytes,
				initial_cwnd: None,
			},
			&cert_chain,
			&priv_key,
			case_timeout,
			&mut endpoint_rows,
		);
	}
	run_and_push(
		&runtime,
		CaseSpec {
			label: format!("endpoints=1 conns={} (shared)", cfg.shared_conns),
			payload: DEFAULT_PAYLOAD,
			conns: cfg.shared_conns,
			endpoint_pairs: 1,
			streams: 1,
			file_bytes: cfg.file_bytes(),
			sockbuf_bytes: cfg.sockbuf_bytes,
			initial_cwnd: None,
		},
		&cert_chain,
		&priv_key,
		case_timeout,
		&mut endpoint_rows,
	);
	// 追加两行：payload=9000（jumbo）下，1 个或 4 个 Endpoint 的聚合吞吐。
	// 1452 的表只回答“接收循环是否按 Endpoint 分布”；这两行回答“payload
	// 放大后聚合还能不能继续涨”，是给方案排序用的。
	for (label, conns, pairs) in [
		(
			format!("endpoints=1 conns={} payload=9000", cfg.shared_conns),
			cfg.shared_conns,
			1usize,
		),
		(
			"endpoints=4 conns=4 payload=9000".to_string(),
			4usize,
			4usize,
		),
	] {
		run_and_push(
			&runtime,
			CaseSpec {
				label,
				payload: 9000,
				conns,
				endpoint_pairs: pairs,
				streams: 1,
				file_bytes: cfg.file_bytes(),
				sockbuf_bytes: cfg.sockbuf_bytes,
				initial_cwnd: None,
			},
			&cert_chain,
			&priv_key,
			case_timeout,
			&mut endpoint_rows,
		);
	}

	// 维度 4：单连接流数量（1452 B）。
	let mut stream_rows = Vec::new();
	for &streams in &cfg.streams {
		run_and_push(
			&runtime,
			CaseSpec {
				label: format!("conns=1 streams={streams}"),
				payload: DEFAULT_PAYLOAD,
				conns: 1,
				endpoint_pairs: 1,
				streams,
				file_bytes: cfg.file_bytes(),
				sockbuf_bytes: cfg.sockbuf_bytes,
				initial_cwnd: None,
			},
			&cert_chain,
			&priv_key,
			case_timeout,
			&mut stream_rows,
		);
	}

	// --- 表 ---
	print_table_header("1) payload / PMTU（1 conn, 1 endpoint, 1 stream）");
	for row in &payload_rows {
		print_case_row(row);
	}

	print_table_header("2) Endpoint 数量（payload=1452）");
	for row in &endpoint_rows {
		print_case_row(row);
	}

	print_table_header("3) 单连接并发流数量（payload=1452, 1 endpoint）");
	for row in &stream_rows {
		print_case_row(row);
	}

	print_table_header("4) 原始 UDP GSO 段测 + 16000 B 诊断（quinn::udp，非 QUIC）");
	for row in &raw_rows {
		print_raw_row(row);
	}

	// --- 派生数字 ---
	println!();
	println!("--- 派生 / 结论辅助 ---");

	let mut payload_refs: Vec<(u16, &str, &CaseMetrics)> = Vec::new();
	for row in &payload_rows {
		if let Ok(metrics) = &row.outcome {
			if row.spec.sockbuf_bytes > 0 {
				payload_refs.push((row.spec.payload, row.spec.label.as_str(), metrics));
			}
		}
	}
	if let Some((base_payload, _, base)) = payload_refs
		.iter()
		.find(|(payload, _, _)| *payload == DEFAULT_PAYLOAD)
	{
		println!(
			"[derive] payload baseline {}: {:.1} MiB/s, {:.1}k pkt/s, avg_tx={:.0}B",
			base_payload,
			base.mib_per_s(),
			base.packets_per_s() / 1e3,
			base.avg_tx_datagram_bytes()
		);
		for (payload, label, metrics) in &payload_refs {
			if *payload == *base_payload {
				continue;
			}
			println!(
				"[derive] {}: {:+.1}% MiB/s, {:+.1}% pkt/s, avg_tx={:.0}B, mtu={}",
				label,
				(metrics.mib_per_s() / base.mib_per_s() - 1.0) * 100.0,
				(metrics.packets_per_s() / base.packets_per_s() - 1.0) * 100.0,
				metrics.avg_tx_datagram_bytes(),
				metrics.mtu
			);
		}
	}
	// 默认缓冲对照（有配置缓冲时）。
	if cfg.sockbuf_bytes > 0 {
		if let Some(control) = payload_rows
			.iter()
			.find(|row| row.spec.sockbuf_bytes == 0 && row.outcome.is_ok())
		{
			if let Ok(metrics) = &control.outcome {
				if let Some((_, _, base)) = payload_refs
					.iter()
					.find(|(payload, _, _)| *payload == DEFAULT_PAYLOAD)
				{
					println!(
						"[derive] sockbuf: default={:.1} MiB/s vs {}={:.1} MiB/s ({:+.1}%)",
						metrics.mib_per_s(),
						cfg.sockbuf_label(),
						base.mib_per_s(),
						(base.mib_per_s() / metrics.mib_per_s() - 1.0) * 100.0
					);
				}
			}
		}
	}

	// Endpoint 扩展。
	let endpoint_mib = |conns: usize, pairs: usize| -> Option<f64> {
		endpoint_rows
			.iter()
			.find(|row| {
				row.spec.conns == conns && row.spec.endpoint_pairs == pairs && row.outcome.is_ok()
			})
			.and_then(|row| row.outcome.as_ref().ok().map(CaseMetrics::mib_per_s))
	};
	if let Some(single) = endpoint_mib(1, 1) {
		println!("[derive] endpoint baseline 1 endpoint / 1 conn: {single:.1} MiB/s");
		for &pairs in &cfg.endpoint_counts {
			if let Some(total) = endpoint_mib(pairs, pairs) {
				println!(
					"[derive] {pairs} independent endpoints x 1 conn: {total:.1} MiB/s total, {:.1} MiB/s per conn ({:+.1}% vs 1x1)",
					total / pairs as f64,
					(total / single - 1.0) * 100.0
				);
			}
		}
		if let Some(shared) = endpoint_mib(cfg.shared_conns, 1) {
			println!(
				"[derive] 1 endpoint x {} conns: {shared:.1} MiB/s total, {:.1} MiB/s per conn",
				cfg.shared_conns,
				shared / cfg.shared_conns as f64
			);
			if let Some(independent) = endpoint_mib(cfg.shared_conns, cfg.shared_conns) {
				println!(
					"[derive] {} conns: independent endpoints {:.1} vs shared endpoint {:.1} MiB/s ({:+.1}%)",
					cfg.shared_conns,
					independent,
					shared,
					(independent / shared - 1.0) * 100.0
				);
			}
		}
		// jumbo（9000 B）下的聚合：回答“payload 放大之后，多 Endpoint 还能不能
		// 继续线性涨”。两条行由主流程显式追加。
		for (label, title) in [
			(
				format!("endpoints=1 conns={} payload=9000", cfg.shared_conns),
				"1 endpoint (shared) @9000",
			),
			(
				"endpoints=4 conns=4 payload=9000".to_string(),
				"4 independent endpoints @9000",
			),
		] {
			if let Some(row) = endpoint_rows
				.iter()
				.find(|row| row.spec.label == label && row.outcome.is_ok())
			{
				if let Ok(metrics) = &row.outcome {
					println!(
						"[derive] {title}: {:.1} MiB/s total, {:.1} MiB/s per conn, pkt/s={:.1}k, mtu={}",
						metrics.mib_per_s(),
						metrics.mib_per_s() / row.spec.conns as f64,
						metrics.packets_per_s() / 1e3,
						metrics.mtu
					);
				}
			}
		}
	}

	// 流数量。
	let stream_mib = |streams: usize| -> Option<f64> {
		stream_rows
			.iter()
			.find(|row| row.spec.streams == streams && row.outcome.is_ok())
			.and_then(|row| row.outcome.as_ref().ok().map(CaseMetrics::mib_per_s))
	};
	if let Some(single) = stream_mib(1) {
		println!("[derive] stream baseline 1 stream: {single:.1} MiB/s");
		for &streams in &cfg.streams {
			if streams == 1 {
				continue;
			}
			if let Some(total) = stream_mib(streams) {
				println!(
					"[derive] {streams} streams: {:+.1}% total, {:.1} MiB/s per stream",
					(total / single - 1.0) * 100.0,
					total / streams as f64
				);
			}
		}
	}

	// 原始 GSO 段测：quinn 已用 10 段，所以只看 10 -> 32 的增量。
	if let (Some(ten), Some(thirty_two)) = (
		raw_rows
			.iter()
			.find(|row| row.payload == RAW_PAYLOAD && row.segments == QUINN_MAX_TRANSMIT_SEGMENTS),
		raw_rows
			.iter()
			.find(|row| row.payload == RAW_PAYLOAD && row.segments == 32),
	) {
		let delta = (thirty_two.mib_per_s() / ten.mib_per_s() - 1.0) * 100.0;
		println!(
			"[derive] raw GSO segs {}->32: {:.1} -> {:.1} MiB/s ({delta:+.1}%), CPU% {:.0} -> {:.0}",
			QUINN_MAX_TRANSMIT_SEGMENTS,
			ten.mib_per_s(),
			thirty_two.mib_per_s(),
			ten.cpu_percent(),
			thirty_two.cpu_percent()
		);
		println!(
			"[derive] 投影：quinn 已经在用 GSO(<=10 段)，所以 10->32 的收益只能来自发送 syscall 减少；收包侧每包成本不变。把上面原始 UDP 的 MiB/s 增量当作 QUIC 端到端的**乐观上界**，实际还要打折（加密/ACK/帧处理不随批大小缩小）。"
		);
	}

	// 最优单连接 / 最优聚合。
	let best_single = payload_rows
		.iter()
		.filter_map(|row| {
			row.outcome
				.as_ref()
				.ok()
				.map(|metrics| (row.spec.label.clone(), metrics.mib_per_s()))
		})
		.max_by(|a, b| a.1.total_cmp(&b.1));
	if let Some((label, mib)) = best_single {
		println!("[derive] best single-connection payload row: {label} = {mib:.1} MiB/s");
	}
	let best_aggregate = endpoint_rows
		.iter()
		.filter_map(|row| {
			row.outcome
				.as_ref()
				.ok()
				.map(|metrics| (row.spec.label.clone(), metrics.mib_per_s()))
		})
		.max_by(|a, b| a.1.total_cmp(&b.1));
	if let Some((label, mib)) = best_aggregate {
		println!("[derive] best aggregate endpoint row: {label} = {mib:.1} MiB/s");
	}

	println!();
	println!(
		"[probe] total wall time: {:.2} s",
		probe_started.elapsed().as_secs_f64()
	);
	println!("[probe] 口径提醒:");
	println!("[probe]   - 只测 IPv4 loopback；无真实网卡/URO/防火墙，jumbo 未被端到端验证");
	println!(
		"[probe]   - CPU% 为收发两端同进程 kernel+user；本机 tick 约 15.6 ms，短用例有量化误差"
	);
	println!(
		"[probe]   - pkt/s 用客户端 udp_tx.datagrams/墙钟（含重传）；avg_tx 是实际 UDP 数据报均值"
	);
	println!("[probe]   - payload>1452 时 initial_mtu=min_mtu=upper_bound=payload；失败/黑洞会以低吞吐或 ERROR 暴露");
	println!("[probe]   - payload>=14720B 会被 quinn 默认初始 cwnd 卡死在握手；16000 行必须配合 +init_cwnd 对照解读");
	println!("[probe]   - 原始 GSO 段测没有 TLS/ACK/拥塞控制，只能当 QUIC 收益的上界");
	println!("[probe]   - 历史默认缓冲基线可对比 control 行；其余行统一把 SO_SNDBUF/SO_RCVBUF 设为配置值");
}
