//! WAN 延迟 / 带宽基线探针（集成测试，默认 `#[ignore]`，正常 `cargo test` 不会执行）。
//!
//! # 目的
//!
//! loopback 上 RTT≈0，掩盖了长肥管道（long fat network）真正的主导限制：
//! quinn 默认 `stream_receive_window = 1.25 MB`，单流吞吐上限约为
//! `window / RTT`，在 100 ms RTT 上只有约 12.5 MB/s。本探针在**受控 RTT** 下测
//! 纯 quinn 单流吞吐，给出 phase A（窗口 sizing + BDP 分块）改造前的可对比基线。
//!
//! # 运行
//!
//! ```text
//! cargo test --release -p cb_core --test wan_delay_probe -- --ignored --nocapture
//! ```
//!
//! 默认 4 个 RTT × 2 个窗口 × 2 种 chunk = 16 组，每组传 64 MiB，目标总时长 < 90 s。
//! 也可用环境变量覆盖（逗号分隔）：
//!
//! - `CB_WAN_RTTS`：RTT 毫秒值，默认 `0,40,80,150`
//! - `CB_WAN_WINDOWS_MB`：`stream_receive_window`，单位 10^6 字节（1.25 = quinn 默认
//!   1_250_000），默认 `1.25,8`
//! - `CB_WAN_CHUNKS_KIB`：应用层 `write_all` 的分块大小，单位 KiB，默认 `64,256`
//! - `CB_WAN_FILE_MB`：每次传输的文件大小，单位 MiB，默认 `64`
//!
//! # 测量方案（选型说明）
//!
//! 选 **C**：实现一个**进程内虚拟 UDP 链路**（自定义 `quinn::AsyncUdpSocket`），
//! 两端仍是真实的 quinn `Endpoint`，数据报不经过内核 socket，而是在内存队列里按
//! “发送时刻 + 单向延迟（RTT/2）”投递。
//!
//! 先试过方案 A（额外绑一个 loopback UDP 代理 socket 做双向转发），但实测发现
//! Windows 默认 UDP 接收缓冲只有约 64 KiB：1.25 MB/8 MB 窗口下发送端会成批突发，
//! 代理/接收端内核缓冲溢出造成**非注入的丢包**，40 ms 处 1.25 MB 和 8 MB 窗口都被
//! 压到 ~14 MiB/s，窗口是否瓶颈被完全掩盖。方案 C 没有内核缓冲区、没有额外转发跳数，
//! 也不会产生本底丢包，因此能干净地测出“窗口/BDP 是否是唯一约束”。
//!
//! 为什么不做应用端到端：`Core` 通过 mDNS 发现对端并按广播地址拨号，公共 API 没有
//! “把连接指向虚拟链路”的入口；按任务约束不能改应用代码。因此这里直接驱动 quinn，
//! 并让传输配置**镜像** `cb_core::transport::quic_transport_config`（TLS 1.3、
//! ALPN `clipbridge-v1`、10 s idle timeout、2 s keep-alive），只改变
//! `stream_receive_window`。应用层 session/协议帧开销不在本曲线内：应用端到端数字
//! 是从这条传输层曲线**投影**出来的，而不是实测值。
//!
//! # 配置口径
//!
//! - `receive_window` 保持 quinn 默认 `VarInt::MAX`（连接级不限，单流窗口才生效），
//!   与应用当前配置一致；
//! - `send_window` 保持 quinn 默认 10 MB：单流、最大 8 MB 接收窗口时 in-flight
//!   ≤ 8 MB < 10 MB，所以发送侧缓冲不是本轮比较的混淆变量；
//! - TLS 不启用客户端证书（应用会请求但非强制）；握手只发生一次，不影响稳态吞吐；
//! - 只有一个流，所以 `aggregates` 与 `per-stream` 两列相同；
//! - 每组先写 `3 × window` 字节预热（不计入），再计时 64 MiB。预热是为了逃出
//!   Cubic 慢启动：64 MiB 在 8 MB 窗口下若不预热几乎全程都在爬 cwnd，会把
//!   “窗口不是瓶颈”误读成“窗口没生效”。报告的是**稳态**窗口吞吐。
//!
//! # 诚实限制
//!
//! - 虚拟链路是**无损**的：没有丢包/乱序模型，也没有真实网卡的排队与 bufferbloat；
//!   这是刻意的，用来观察窗口是否为限；真实公网丢包只会让数字更低，不会更高；
//! - 因此 `retransmits` 正常情况下应接近 0；非 0 说明发生了 QUIC 超时（探针自身问题），
//!   不能当作公网重传率；
//! - 数据报仍然要经过 tokio 调度与内存拷贝，RTT=0 行是“内存链路 + quinn 栈”的
//!   loopback 基线上限，不是无限大；
//! - `retransmits` 取客户端 `path.lost_packets`（被判丢的包数），不是重传帧精确计数；
//! - RTT 由 tokio timer 注入，精度约 1 ms；实测 RTT 用 `path.rtt` 打印在 notes 里；
//! - BDP 预测 `window / RTT` 只在 RTT>0 时有定义，RTT=0 显示 `n/a`；
//! - 本机 CPU、release/debug 构建都会影响绝对值，跨机器比较请看趋势而不是小数位。

use std::collections::VecDeque;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::udp;
use quinn::{
	AsyncUdpSocket, Endpoint, EndpointConfig, Runtime, ServerConfig, TransportConfig, UdpPoller,
	VarInt,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// 默认 RTT 阶梯（毫秒）。覆盖 loopback、同城、跨省、跨洋的量级。
const DEFAULT_RTTS_MS: &[u64] = &[0, 40, 80, 150];
/// 默认 `stream_receive_window`（10^6 字节）。1.25 MB 精确等于 quinn 0.11 默认值。
const DEFAULT_WINDOWS_MB: &[f64] = &[1.25, 8.0];
/// 默认应用层分块（KiB）。
const DEFAULT_CHUNKS_KIB: &[usize] = &[64, 256];
/// 默认每次传输文件大小（MiB）。
const DEFAULT_FILE_MB: u64 = 64;

/// 镜像 `cb_core::transport` 的 QUIC 空闲超时（毫秒）。
const QUIC_MAX_IDLE_TIMEOUT_MS: u32 = 10_000;
/// 镜像 `cb_core::transport` 的 keep-alive 间隔。
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(2);
/// 镜像 `cb_core::transport` 的 ALPN。
const ALPN: &[u8] = b"clipbridge-v1";

/// 计时前的预热倍数：先生成 `3 × stream_receive_window` 字节不计入吞吐，
/// 让 Cubic 的慢启动把 cwnd 抬到窗口量级，计时阶段才是稳态。
///
/// 不预热的话 64 MiB 在 8 MB 窗口下几乎全程处于慢启动（抬到 8 MB 窗口
/// 约需 9~10 个 RTT），会把“窗口不是瓶颈”误读成“窗口没生效”。
const WARMUP_WINDOW_MULTIPLIER: u64 = 3;

const MIB: f64 = 1024.0 * 1024.0;

// ---------------------------------------------------------------------------
// 参数解析
// ---------------------------------------------------------------------------

/// 读取逗号分隔的环境变量列表；未设置或为空时返回默认值。
fn env_list<T: FromStr + Clone>(key: &str, default: &[T]) -> Vec<T> {
	match std::env::var(key) {
		Ok(raw) if !raw.trim().is_empty() => {
			let values: Vec<T> = raw
				.split(',')
				.filter_map(|part| part.trim().parse::<T>().ok())
				.collect();
			assert!(
				!values.is_empty(),
				"{key} 至少需要一个可解析的值，实际为 {raw:?}"
			);
			values
		}
		_ => default.to_vec(),
	}
}

fn env_u64(key: &str, default: u64) -> u64 {
	std::env::var(key)
		.ok()
		.and_then(|raw| raw.trim().parse::<u64>().ok())
		.unwrap_or(default)
}

/// 打印窗口标签：8.0 -> "8MB"，1.25 -> "1.25MB"。
fn window_label(mb: f64) -> String {
	format!("{mb}MB")
}

fn window_bytes(mb: f64) -> u64 {
	assert!(mb > 0.0, "CB_WAN_WINDOWS_MB 必须为正数，实际为 {mb}");
	assert!(mb < 1_000_000.0, "CB_WAN_WINDOWS_MB 过大: {mb}");
	(mb * 1_000_000.0).round() as u64
}

/// BDP 预测：`stream_receive_window / RTT`。RTT=0 时无定义。
fn bdp_mib_s(window_bytes: u64, rtt_ms: u64) -> Option<f64> {
	if rtt_ms == 0 {
		None
	} else {
		Some(window_bytes as f64 / (rtt_ms as f64 / 1000.0) / MIB)
	}
}

// ---------------------------------------------------------------------------
// 传输配置 / TLS（镜像应用，只把 stream_receive_window 参数化）
// ---------------------------------------------------------------------------

/// 构造探针用 `TransportConfig`。
///
/// 除 `stream_receive_window` 外全部保持 quinn 默认，并补齐应用两端一致的
/// 10 s idle timeout / 2 s keep-alive，避免空闲超时或 keep-alive 差异污染吞吐。
fn probe_transport_config(stream_receive_window: u64) -> TransportConfig {
	let mut config = TransportConfig::default();
	config.max_idle_timeout(Some(VarInt::from_u32(QUIC_MAX_IDLE_TIMEOUT_MS).into()));
	config.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
	config.stream_receive_window(
		VarInt::from_u64(stream_receive_window).expect("stream_receive_window 必须能装进 VarInt"),
	);
	// receive_window / send_window 刻意保持 quinn 默认：
	// - receive_window = VarInt::MAX，与应用一致，单流只受 stream_receive_window 约束；
	// - send_window = 10 MB > 最大 in-flight（单流 8 MB），不会成为本轮比较的混淆变量。
	config
}

/// 探针专用 TLS 校验器：接受任意服务端证书（等价于应用里的 BlindVerifier）。
///
/// 应用里的 `BlindVerifier` 是私有类型；这里复制同样的逻辑，避免为了做一个探针
/// 去扩大应用公开 API。仅测试代码使用。
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
	stream_receive_window: u64,
	cert_chain: &[CertificateDer<'static>],
	priv_key: &PrivateKeyDer<'static>,
) -> Result<ServerConfig, String> {
	let crypto = Arc::new(rustls::crypto::ring::default_provider());
	let mut tls = rustls::ServerConfig::builder_with_provider(crypto)
		.with_protocol_versions(&[&rustls::version::TLS13])
		.map_err(|e| format!("server TLS version config failed: {e}"))?
		// 应用会请求客户端证书；探针不校验客户端证书（对稳态吞吐无影响），
		// 这里省掉一套自定义 ClientCertVerifier。
		.with_no_client_auth()
		.with_single_cert(cert_chain.to_vec(), priv_key.clone_key())
		.map_err(|e| format!("server cert config failed: {e}"))?;
	tls.alpn_protocols = vec![ALPN.to_vec()];

	let quic = QuicServerConfig::try_from(tls)
		.map_err(|e| format!("QuicServerConfig::try_from failed: {e}"))?;
	let mut config = ServerConfig::with_crypto(Arc::new(quic));
	config.transport_config(Arc::new(probe_transport_config(stream_receive_window)));
	Ok(config)
}

fn build_client_config(stream_receive_window: u64) -> Result<quinn::ClientConfig, String> {
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
	let mut config = quinn::ClientConfig::new(Arc::new(quic));
	config.transport_config(Arc::new(probe_transport_config(stream_receive_window)));
	Ok(config)
}

// ---------------------------------------------------------------------------
// 进程内虚拟 UDP 链路（自定义 quinn::AsyncUdpSocket）
// ---------------------------------------------------------------------------

/// 虚拟链路：两个 `VirtualSocket` 共享一组内存队列，按固定单向延迟投递。
///
/// 相比额外绑一个 loopback UDP 代理，虚拟链路不经过内核 socket，没有约 64 KiB
/// 接收缓冲溢出造成的本底丢包，也不会因为代理自身的转发任务成为吞吐瓶颈。
/// 代价是没有真实网卡的排队/bufferbloat/丢包模型（见模块文档的“诚实限制”）。
#[derive(Debug)]
struct VirtualLink {
	/// 单向延迟。
	delay: Duration,
	state: std::sync::Mutex<LinkState>,
	/// 有新包进入延迟队列时唤醒投递任务。
	notify: tokio::sync::Notify,
}

/// 两个端点共享的队列与唤醒器。
#[derive(Debug)]
struct LinkState {
	/// `inbound[i]`：正在延迟、最终投递给端点 i 的数据报。
	inbound: [VecDeque<VirtualPacket>; 2],
	/// `ready[i]`：已到期、等待端点 i 的 `poll_recv` 取走的数据报。
	ready: [VecDeque<VirtualPacket>; 2],
	/// 端点 i 注册的接收唤醒器。
	waker: [Option<Waker>; 2],
}

#[derive(Debug)]
struct VirtualPacket {
	/// 允许被端点取走的时刻。
	due: Instant,
	/// 发送端地址，作为 `RecvMeta::addr` 返回给 quinn。
	src: SocketAddr,
	payload: Vec<u8>,
}

impl VirtualLink {
	fn new(delay: Duration) -> Arc<Self> {
		Arc::new(Self {
			delay,
			state: std::sync::Mutex::new(LinkState {
				inbound: std::array::from_fn(|_| VecDeque::new()),
				ready: std::array::from_fn(|_| VecDeque::new()),
				waker: std::array::from_fn(|_| None),
			}),
			notify: tokio::sync::Notify::new(),
		})
	}
}

/// 虚拟链路的投递任务：把到期的 `inbound` 搬到 `ready` 并唤醒接收端。
///
/// 用单独任务而不是在 `poll_recv` 里 sleep：`poll_recv` 可能被反复轮询，
/// 在 poll 内等待会与 quinn 的驱动循环耦合，容易产生忙等或漏唤醒。
async fn deliver_loop(link: Arc<VirtualLink>) {
	loop {
		let next_due = {
			let mut state = link.state.lock().expect("virtual link mutex poisoned");
			let now = Instant::now();
			let mut earliest: Option<Instant> = None;
			let mut wakers: Vec<Waker> = Vec::new();
			for index in 0..2 {
				while let Some(packet) = state.inbound[index].front() {
					if packet.due > now {
						earliest = Some(
							earliest.map_or(packet.due, |value: Instant| value.min(packet.due)),
						);
						break;
					}
					let packet = state.inbound[index]
						.pop_front()
						.expect("front was checked non-empty");
					state.ready[index].push_back(packet);
					if let Some(waker) = state.waker[index].take() {
						wakers.push(waker);
					}
				}
			}
			drop(state);
			for waker in wakers {
				waker.wake();
			}
			earliest
		};

		match next_due {
			Some(due) => tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await,
			None => link.notify.notified().await,
		}
	}
}

/// 虚拟链路的一端，实现 quinn 的 `AsyncUdpSocket`。
#[derive(Debug)]
struct VirtualSocket {
	/// 0 = client，1 = server；对端下标是 `1 - index`。
	index: usize,
	local: SocketAddr,
	link: Arc<VirtualLink>,
}

impl VirtualSocket {
	fn new(index: usize, local: SocketAddr, link: Arc<VirtualLink>) -> Self {
		Self { index, local, link }
	}
}

impl AsyncUdpSocket for VirtualSocket {
	fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
		// try_send 永不返回 WouldBlock，因此 poller 永远 ready。
		Box::pin(ImmediatePoller)
	}

	fn try_send(&self, transmit: &udp::Transmit<'_>) -> io::Result<()> {
		let peer = 1 - self.index;
		let delayed = !self.link.delay.is_zero();
		let packet = VirtualPacket {
			due: Instant::now() + self.link.delay,
			src: self.local,
			payload: transmit.contents.to_vec(),
		};

		let waker = {
			let mut state = self.link.state.lock().expect("virtual link mutex poisoned");
			if delayed {
				state.inbound[peer].push_back(packet);
				None
			} else {
				// RTT=0 没必要经过定时器：直接放到对端的 ready 队列。
				state.ready[peer].push_back(packet);
				state.waker[peer].take()
			}
		};
		if let Some(waker) = waker {
			waker.wake();
		}
		if delayed {
			self.link.notify.notify_one();
		}
		Ok(())
	}

	fn poll_recv(
		&self,
		cx: &mut Context<'_>,
		bufs: &mut [IoSliceMut<'_>],
		meta: &mut [udp::RecvMeta],
	) -> Poll<io::Result<usize>> {
		let mut state = self.link.state.lock().expect("virtual link mutex poisoned");
		if state.ready[self.index].is_empty() {
			state.waker[self.index] = Some(cx.waker().clone());
			return Poll::Pending;
		}

		let count = bufs
			.len()
			.min(meta.len())
			.min(state.ready[self.index].len());
		for (slot, buffer) in bufs.iter_mut().take(count).enumerate() {
			let packet = state.ready[self.index]
				.pop_front()
				.expect("ready queue length was checked");
			if packet.payload.len() > buffer.len() {
				return Poll::Ready(Err(io::Error::new(
					io::ErrorKind::InvalidData,
					"virtual UDP datagram larger than receive buffer",
				)));
			}
			buffer[..packet.payload.len()].copy_from_slice(&packet.payload);
			meta[slot] = udp::RecvMeta {
				addr: packet.src,
				len: packet.payload.len(),
				// 单个数据报：stride 等于长度（quinn 用 stride 切分 GRO 批次）。
				stride: packet.payload.len(),
				ecn: None,
				dst_ip: None,
			};
		}
		Poll::Ready(Ok(count))
	}

	fn local_addr(&self) -> io::Result<SocketAddr> {
		Ok(self.local)
	}

	fn may_fragment(&self) -> bool {
		// 虚拟链路不涉及 IP 分片；返回 false 让 quinn 开启 MTU 发现（最大 1452）。
		false
	}
}

/// `try_send` 永不阻塞，`poll_writable` 直接 Ready。
#[derive(Debug)]
struct ImmediatePoller;

impl UdpPoller for ImmediatePoller {
	fn poll_writable(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}
}

// ---------------------------------------------------------------------------
// 单组测量
// ---------------------------------------------------------------------------

/// 一组测量参数：RTT × 窗口 × chunk。
struct CaseSpec {
	rtt_ms: u64,
	window_bytes: u64,
	chunk_bytes: usize,
	/// 计时前不计入吞吐的预热字节数。
	warmup_bytes: u64,
	file_bytes: u64,
}

/// 一组测量的观测结果。
struct CaseOutcome {
	elapsed: Duration,
	bytes: u64,
	/// 客户端 `path.rtt`：验证注入延迟是否生效。
	rtt_est: Duration,
	/// 客户端 `path.lost_packets`：被判丢的包数（近似重传次数）。
	lost_packets: u64,
	sent_packets: u64,
	cwnd: u64,
	mtu: u16,
}

impl CaseOutcome {
	fn throughput_mib_s(&self) -> f64 {
		self.bytes as f64 / MIB / self.elapsed.as_secs_f64().max(1e-9)
	}
}

/// 单组测量的超时上限：按 (预热 + 文件) 的 BDP 估计耗时 ×2.5 + 20 s，夹在 [20 s, 90 s]。
///
/// 超时只用于防止探针挂死；正常路径远小于该值。
fn case_timeout(spec: &CaseSpec) -> Duration {
	let bdp = bdp_mib_s(spec.window_bytes, spec.rtt_ms).unwrap_or(4096.0);
	let total_mib = (spec.warmup_bytes + spec.file_bytes) as f64 / MIB;
	let estimated_s = total_mib / bdp.max(1.0);
	Duration::from_secs_f64((estimated_s * 2.5 + 20.0).clamp(20.0, 90.0))
}

/// 虚拟链路两端的伪造地址。不绑定真实网卡；quinn 只把它当作连接的对端标识。
fn probe_socket_addr(host: u8) -> SocketAddr {
	SocketAddr::from(([10, 0, 0, host], 4433))
}

/// 同步构造两端 endpoint；失败时由调用方负责取消投递任务。
fn build_endpoints(
	spec: &CaseSpec,
	cert_chain: &[CertificateDer<'static>],
	priv_key: &PrivateKeyDer<'static>,
	link: Arc<VirtualLink>,
) -> Result<(Endpoint, Endpoint, SocketAddr), String> {
	let server_config = build_server_config(spec.window_bytes, cert_chain, priv_key)?;
	let client_config = build_client_config(spec.window_bytes)?;

	let client_addr = probe_socket_addr(1);
	let server_addr = probe_socket_addr(2);
	let runtime: Arc<dyn Runtime> = Arc::new(quinn::TokioRuntime);

	let server_socket = Arc::new(VirtualSocket::new(1, server_addr, link.clone()));
	let server_endpoint = Endpoint::new_with_abstract_socket(
		EndpointConfig::default(),
		Some(server_config),
		server_socket,
		runtime.clone(),
	)
	.map_err(|e| format!("create virtual server endpoint failed: {e}"))?;

	let client_socket = Arc::new(VirtualSocket::new(0, client_addr, link));
	let mut client_endpoint =
		Endpoint::new_with_abstract_socket(EndpointConfig::default(), None, client_socket, runtime)
			.map_err(|e| format!("create virtual client endpoint failed: {e}"))?;
	client_endpoint.set_default_client_config(client_config);

	Ok((client_endpoint, server_endpoint, server_addr))
}

/// 跑一组测量：新建 quinn 两端 -> 经虚拟延迟链路建连 -> 单流传输 file_bytes -> 统计。
async fn run_case(
	spec: &CaseSpec,
	cert_chain: &[CertificateDer<'static>],
	priv_key: &PrivateKeyDer<'static>,
) -> Result<CaseOutcome, String> {
	// 单向延迟 = RTT/2，两个方向各加一次。
	let one_way = Duration::from_micros(spec.rtt_ms.saturating_mul(500));
	let link = VirtualLink::new(one_way);
	let deliver_task = tokio::spawn(deliver_loop(link.clone()));

	let (client_endpoint, server_endpoint, server_addr) =
		match build_endpoints(spec, cert_chain, priv_key, link) {
			Ok(value) => value,
			Err(error) => {
				deliver_task.abort();
				return Err(error);
			}
		};

	// 先挂上 accept，再拨号；无论成功失败，下面的 shutdown 都会关闭 endpoint 让 accept 退出。
	let accept_endpoint = server_endpoint.clone();
	let warmup_bytes = spec.warmup_bytes;
	let expected_bytes = spec.warmup_bytes + spec.file_bytes;
	let accept_task = tokio::spawn(async move {
		accept_and_read(accept_endpoint, warmup_bytes, expected_bytes).await
	});

	let outcome = transfer(spec, &client_endpoint, server_addr, accept_task).await;

	// 尽力干净关闭：让双方发出 CONNECTION_CLOSE，避免上一组的连接/定时器拖到下一组。
	client_endpoint.close(VarInt::from_u32(0), b"probe case done");
	server_endpoint.close(VarInt::from_u32(0), b"probe case done");
	let _ = tokio::time::timeout(Duration::from_secs(3), async {
		client_endpoint.wait_idle().await;
		server_endpoint.wait_idle().await;
	})
	.await;
	deliver_task.abort();

	outcome
}

/// 建立连接并完成“预热 + 计时文件”的单向字节流传输。
///
/// 计时口径：客户端先写 `warmup_bytes` 预热（不计入），服务端在收到预热字节的
/// 那一刻记 t0，在读到全部数据后记 t1；吞吐 = `file_bytes / (t1 - t0)`。
/// 这样既排除了握手/建流，也排除了慢启动，得到的是**稳态窗口吞吐**。
async fn transfer(
	spec: &CaseSpec,
	client_endpoint: &Endpoint,
	server_addr: SocketAddr,
	accept_task: tokio::task::JoinHandle<Result<(Instant, Instant, u64), String>>,
) -> Result<CaseOutcome, String> {
	let connecting = client_endpoint
		.connect(server_addr, "localhost")
		.map_err(|e| format!("client connect() failed: {e}"))?;
	let conn = connecting
		.await
		.map_err(|e| format!("client handshake failed: {e}"))?;

	let mut send = conn
		.open_uni()
		.await
		.map_err(|e| format!("client open_uni failed: {e}"))?;
	let chunk_buf = vec![0xABu8; spec.chunk_bytes];

	// 预热：不计入吞吐，只为把 cwnd 抬到窗口量级（见 WARMUP_WINDOW_MULTIPLIER）。
	let mut remaining = spec.warmup_bytes;
	while remaining > 0 {
		let n = remaining.min(spec.chunk_bytes as u64) as usize;
		send.write_all(&chunk_buf[..n])
			.await
			.map_err(|e| format!("client warmup write_all failed: {e}"))?;
		remaining -= n as u64;
	}

	// 计时段：服务端收到预热字节时记 t0，读完计时文件时记 t1。
	let mut remaining = spec.file_bytes;
	while remaining > 0 {
		let n = remaining.min(spec.chunk_bytes as u64) as usize;
		send.write_all(&chunk_buf[..n])
			.await
			.map_err(|e| format!("client write_all failed: {e}"))?;
		remaining -= n as u64;
	}
	send.finish()
		.map_err(|e| format!("client finish failed: {e}"))?;

	let (warm_at, server_done, received) = accept_task
		.await
		.map_err(|e| format!("accept task join failed: {e}"))??;
	let elapsed = server_done.saturating_duration_since(warm_at);

	let expected = spec.warmup_bytes + spec.file_bytes;
	if received != expected {
		return Err(format!(
			"字节数不匹配: 服务端收到 {received}，期望 {expected}"
		));
	}

	let stats = conn.stats();
	Ok(CaseOutcome {
		elapsed,
		bytes: spec.file_bytes,
		rtt_est: stats.path.rtt,
		lost_packets: stats.path.lost_packets,
		sent_packets: stats.path.sent_packets,
		cwnd: stats.path.cwnd,
		mtu: stats.path.current_mtu,
	})
}

/// 服务端：接受一条连接、接受一个单向流、读到 EOF。
/// 返回（预热字节读完时刻，全部读完时刻，总字节数）。
async fn accept_and_read(
	endpoint: Endpoint,
	warmup_bytes: u64,
	expected_bytes: u64,
) -> Result<(Instant, Instant, u64), String> {
	let incoming = endpoint
		.accept()
		.await
		.ok_or_else(|| "server endpoint closed before accepting a connection".to_string())?;
	let conn = incoming
		.await
		.map_err(|e| format!("server handshake failed: {e}"))?;
	let mut recv = conn
		.accept_uni()
		.await
		.map_err(|e| format!("server accept_uni failed: {e}"))?;

	let mut buf = vec![0u8; 64 * 1024];
	let mut received = 0u64;
	let mut warm_at: Option<Instant> = None;
	loop {
		match recv.read(&mut buf).await {
			Ok(Some(n)) => {
				received += n as u64;
				if warm_at.is_none() && received >= warmup_bytes {
					warm_at = Some(Instant::now());
				}
			}
			Ok(None) => break,
			Err(e) => return Err(format!("server read failed: {e}")),
		}
	}
	let done = Instant::now();

	let warm_at = warm_at
		.ok_or_else(|| format!("服务端只收到 {received} 字节，未达到预热 {warmup_bytes} 字节"))?;
	if received != expected_bytes {
		return Err(format!(
			"字节数不匹配: 服务端收到 {received}，期望 {expected_bytes}"
		));
	}
	Ok((warm_at, done, received))
}

// ---------------------------------------------------------------------------
// 输出
// ---------------------------------------------------------------------------

fn print_banner(rtts: &[u64], windows: &[(u64, String)], chunks: &[(usize, String)], file_mb: u64) {
	println!();
	println!("=== ClipBridge WAN baseline probe: pure quinn throughput vs injected RTT ===");
	println!(
		"[setup] build={} os={} {} logical_cpus={}",
		if cfg!(debug_assertions) {
			"debug (绝对值偏慢，建议 --release)"
		} else {
			"release"
		},
		std::env::consts::OS,
		std::env::consts::ARCH,
		std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get)
	);
	println!(
		"[setup] file={file_mb}MiB rtts={rtts:?}ms windows={:?} chunks={:?} streams=1",
		windows
			.iter()
			.map(|(_, label)| label.as_str())
			.collect::<Vec<_>>(),
		chunks
			.iter()
			.map(|(_, label)| label.as_str())
			.collect::<Vec<_>>()
	);
	println!(
		"[setup] transport mirrors cb_core::transport: TLS1.3, ALPN clipbridge-v1, idle 10s, keepalive 2s"
	);
	println!(
		"[setup] only stream_receive_window varies; receive_window=quinn default (VarInt::MAX); send_window=quinn default 10MB"
	);
	println!(
		"[setup] delay: in-memory virtual UDP link (quinn::AsyncUdpSocket), one-way delay = RTT/2 per direction, lossless"
	);
	println!(
		"[setup] retransmits = client path.lost_packets; lossless link should keep this near 0"
	);
	println!(
		"[setup] each case sends {WARMUP_WINDOW_MULTIPLIER}x stream_receive_window warmup bytes untimed, then times {file_mb}MiB (steady-state, escapes slow start)"
	);
	println!(
		"[limit] 不是应用端到端：直接驱动 quinn Endpoint，不经过 Core/session（Core 用 mDNS 发现并按广播地址拨号，公共 API 无法注入虚拟链路，按约束不改应用代码）。"
	);
	println!(
		"[limit] 应用端到端吞吐需从本传输层曲线投影，不能当作实测；协议帧/加密模式/会话层开销均未计入。"
	);
	println!(
		"[limit] 虚拟链路在内存中投递数据报，无真实网卡排队/bufferbloat/丢包模型；这是刻意的，用来观察窗口/BDP 是否为唯一约束。真实公网丢包只会让数字更低。"
	);
	println!(
		"[limit] TLS 未启用客户端证书（应用请求但非强制），对稳态吞吐无影响；单流，故 aggregates 与 per-stream 相同。"
	);
	println!();
}

fn print_table_header() {
	println!(
		"{:<6} | {:<6} | {:<5} | {:>12} | {:>15} | {:>14} | {:>11} | notes",
		"rtt_ms",
		"window",
		"chunk",
		"bdp MiB/s",
		"aggregates MiB/s",
		"per-stream MiB/s",
		"retransmits"
	);
	println!("{}", "-".repeat(130));
}

fn print_row(
	rtt_ms: u64,
	window: &str,
	chunk: &str,
	bdp: Option<f64>,
	outcome: &Result<CaseOutcome, String>,
) {
	match outcome {
		Ok(outcome) => {
			let measured = outcome.throughput_mib_s();
			let bdp_text = bdp.map_or_else(|| "n/a".to_string(), |value| format!("{value:.2}"));
			let ratio_text = bdp.map_or_else(
				|| "bdp=n/a(0ms)".to_string(),
				|value| format!("measured/bdp={:.2}", measured / value.max(1e-9)),
			);
			let notes = format!(
				"{ratio_text}; rtt_est={:.2}ms; cwnd={:.1}KiB; mtu={}B; sent={}",
				outcome.rtt_est.as_secs_f64() * 1000.0,
				outcome.cwnd as f64 / 1024.0,
				outcome.mtu,
				outcome.sent_packets
			);
			println!(
				"{:<6} | {:<6} | {:<5} | {:>12} | {:>15.2} | {:>14.2} | {:>11} | {notes}",
				rtt_ms, window, chunk, bdp_text, measured, measured, outcome.lost_packets
			);
		}
		Err(error) => {
			let bdp_text = bdp.map_or_else(|| "n/a".to_string(), |value| format!("{value:.2}"));
			println!(
				"{:<6} | {:<6} | {:<5} | {:>12} | {:>15} | {:>14} | {:>11} | ERROR: {}",
				rtt_ms, window, chunk, bdp_text, "FAILED", "FAILED", "-", error
			);
		}
	}
}

// ---------------------------------------------------------------------------
// 测试入口
// ---------------------------------------------------------------------------

#[test]
#[ignore = "runs the WAN RTT/window throughput probe: cargo test --release -p cb_core --test wan_delay_probe -- --ignored --nocapture"]
fn wan_delay_probe() {
	let rtts = env_list::<u64>("CB_WAN_RTTS", DEFAULT_RTTS_MS);
	let windows_mb = env_list::<f64>("CB_WAN_WINDOWS_MB", DEFAULT_WINDOWS_MB);
	let chunks_kib = env_list::<usize>("CB_WAN_CHUNKS_KIB", DEFAULT_CHUNKS_KIB);
	let file_mb = env_u64("CB_WAN_FILE_MB", DEFAULT_FILE_MB);
	assert!(file_mb > 0, "CB_WAN_FILE_MB 必须 > 0");
	assert!(
		chunks_kib.iter().all(|kib| *kib > 0),
		"CB_WAN_CHUNKS_KIB 必须全部 > 0"
	);

	let windows: Vec<(u64, String)> = windows_mb
		.iter()
		.map(|mb| (window_bytes(*mb), window_label(*mb)))
		.collect();
	let chunks: Vec<(usize, String)> = chunks_kib
		.iter()
		.map(|kib| (kib * 1024, format!("{kib}K")))
		.collect();
	let file_bytes = file_mb * 1024 * 1024;

	print_banner(&rtts, &windows, &chunks, file_mb);
	print_table_header();

	// 证书/私钥整个探针只生成一次；TLS 握手不在计时范围内。
	let (cert_chain, priv_key) = cb_core::transport::cert::generate_self_signed_cert("localhost")
		.expect("generate probe self-signed certificate");

	let runtime = tokio::runtime::Builder::new_multi_thread()
		.worker_threads(4)
		.enable_all()
		.build()
		.expect("build tokio runtime for WAN probe");

	let probe_started = Instant::now();
	for &rtt_ms in &rtts {
		for (window_bytes, window) in &windows {
			for (chunk_bytes, chunk) in &chunks {
				let spec = CaseSpec {
					rtt_ms,
					window_bytes: *window_bytes,
					chunk_bytes: *chunk_bytes,
					warmup_bytes: window_bytes.saturating_mul(WARMUP_WINDOW_MULTIPLIER),
					file_bytes,
				};
				let timeout = case_timeout(&spec);
				let outcome = runtime.block_on(async {
					match tokio::time::timeout(timeout, run_case(&spec, &cert_chain, &priv_key))
						.await
					{
						Ok(result) => result,
						Err(_) => Err(format!("单组超时（>{timeout:?}）")),
					}
				});
				print_row(
					rtt_ms,
					window,
					chunk,
					bdp_mib_s(*window_bytes, rtt_ms),
					&outcome,
				);
			}
		}
	}

	println!();
	println!(
		"[done] 总耗时 {:.1}s | bdp = window/(rtt/1000)；预热后 measured/bdp ≈ 1 表示窗口是唯一约束；明显 < 1 说明还有调度/CPU 等其它瓶颈（本链路无丢包）。",
		probe_started.elapsed().as_secs_f64()
	);
}
