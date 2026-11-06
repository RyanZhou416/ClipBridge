//! Windows UDP 批量收发（GSO / GRO）原始探针 —— 默认 `#[ignore]`。
//!
//! 目的：用可复现的测量回答一个问题 —— “Option B（batched IO / GSO-GRO）在
//! Windows 上值不值得做”。本探针不改协议、不启动应用、不读写剪贴板，只在进程内
//! 用 IPv4 loopback 原始 UDP 做测量。
//!
//! # 为什么直接调用 `quinn::udp`
//!
//! `quinn` 把 `quinn-udp` 以 `pub use udp;` 公开 re-export（quinn 0.11.12
//! `src/lib.rs:75`），所以集成测试可以直接拿到 `quinn::udp::UdpSocketState`
//! 和 `quinn::udp::Transmit`。于是本探针跑的就是 **QUIC 端点真正使用的**
//! Windows 实现：同样的 `WSASendMsg` + cmsg、同样的 `WSARecvMsg` + stride
//! 解析、同样的 `UDP_SEND_MSG_SIZE` / `UDP_RECV_MAX_COALESCED_SIZE` 路径。
//! 差别只在于没有 TLS、ACK、拥塞控制和帧编解码，因此这里量到的是“每包
//! 系统调用 + 拷贝”的下限，而不是 QUIC 端到端吞吐。
//!
//! # 测量口径
//!
//! - 发送端和接收端是同一进程里的两个线程。进程 CPU 时间用 `GetProcessTimes`
//!   （kernel+user）读取，覆盖**两端**。所以 `cpu_ms/GiB` 是双向总和，
//!   与“只统计一个端点的 CPU”不可直接比较。
//! - 吞吐 = 接收端实际收到的字节 / 接收端“从第一个包到最后一个包”的墙钟
//!   区间；发送端墙钟（含阻塞等待）单独列出。丢包时以实际收到为准，并打印
//!   丢包数，绝不把“发出去了”当成“传过去了”。
//! - 所有 socket 都是阻塞模式 + 超时（`UdpSocketState::new` 会把 socket 设成
//!   nonblocking，我们在测量前显式切回阻塞），避免非阻塞自旋把 CPU 数字污染。
//! - 每次运行都用**全新的 socket**，不复用端口，避免 GRO/缓冲设置跨用例泄漏。
//!
//! # 诚实的限制
//!
//! - 只测 IPv4 loopback（127.0.0.1）。回环不经过物理网卡，Windows 回环路径
//!   是否做 UDP RSC/URO、GSO 是否由软件栈模拟，都和真实 10GbE 网卡不同。
//!   本探针**不能**替代双机 + 真实网卡的测量；若将来有第二台主机，应另写
//!   一个带 `CB_UDP_PEER=<ip:port>` 的变体，本文件默认不尝试外网/第二主机。
//! - 防火墙不参与：loopback UDP 不需要入站规则，因此本结果不代表跨机器时
//!   防火墙/过滤驱动的影响。
//! - 不校验每个数据报的内容与顺序，只校验“每次 recv 收到几个数据报、多大”。
//!   UDP 乱序/重复在吞吐测量里不影响结论。
//! - 事件计时包含线程调度噪声；样本只有一次长跑（默认每档 ~200k 包），
//!   温度/频率变化会让数字有百分之几的抖动。要更稳就调大
//!   `CB_UDP_PROBE_DATAGRAMS`。
//! - 本机 `GetProcessTimes` 的更新粒度约 15.625 ms（64 Hz tick），因此短窗口
//!   （<100 ms）的进程 CPU 增量会读出 0 或一个 tick 的整数倍。所以“发送/接收
//!   拆分”和“纯拷贝”两段改用墙钟；它们都是单线程无阻塞循环，墙钟≈CPU。
//! - 纯拷贝一行是“memcpy + 整块读回”的成本，是**一次拷贝的上界**，不是精确的
//!   memcpy 成本；即便如此它也比 UDP 每包成本小两个数量级。
//!
//! # 运行方式
//!
//! ```text
//! cargo test --release -p cb_core --test udp_batch_probe -- --ignored --nocapture
//! ```
//!
//! 必须用 `--release`：debug 下每包都有边界检查，数字没有参考价值。
//!
//! # 环境变量（全部可选，便于复跑/裁剪）
//!
//! - `CB_UDP_PROBE_DATAGRAMS`：每档发送的数据报总数，默认 200000（约 277 MiB）。
//! - `CB_UDP_PROBE_SEGMENTS`：GSO 每批段数的逗号列表，默认 `8,32,64,128`。
//! - `CB_UDP_PROBE_BUFFERS`：基线缓冲档位的逗号列表，`default` 或字节数，
//!   默认 `default,1048576,8388608`（默认 / 1 MiB / 8 MiB）。
//! - `CB_UDP_PROBE_SKIP_GRO=1`：跳过 GRO 相关用例（只想看发送侧时用）。
//! - `CB_UDP_PROBE_QUICK=1`：每档 20000 包，冒烟用。
//!
//! # 输出
//!
//! 先打印 OS build / `quinn::udp::BATCH_SIZE` / `max_gso_segments()`，再打印
//! 原始 `setsockopt` 探针结果（成功或**逐字**错误文本），然后逐档跑，最后
//! 给一张汇总表。任何 `send`/`recv` 错误都会把 OS 错误码和 Display 文本
//! 一并打印出来。

#![cfg(windows)]

use std::{
	io::{self, IoSliceMut},
	net::{Ipv4Addr, UdpSocket},
	sync::{
		atomic::{AtomicBool, Ordering},
		Arc,
	},
	thread,
	time::{Duration, Instant},
};

use quinn::udp::{self, RecvMeta, Transmit, UdpSockRef, UdpSocketState};

/// 单数据报 payload。1452 = 1500(以太网 MTU) - 20(IPv4) - 28(UDP+IP 选项余量)，
/// 也是 QUIC 常见 MTU 探测后的取值级别；它足够大，能让“每包固定成本”显形。
const PAYLOAD_BYTES: usize = 1452;

/// 接收缓冲区：256 KiB。GRO 开启后单次 `WSARecvMsg` 理论上最多合并
/// `UDP_RECV_MAX_COALESCED_SIZE`（quinn 设成 65535）字节，256 KiB 留足余量，
/// 避免把“缓冲不够”误判成“不支持 GRO”。
const RECV_BUFFER_BYTES: usize = 256 * 1024;

/// 接收端单次阻塞等待上限。用来在丢包时及时退出，而不是永远等下去。
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// 发送端阻塞上限。UDP 阻塞 `send` 在发送缓冲满时会等待；如果接收端提前退出，
/// 必须有超时兜底，否则线程 join 会挂死。
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// 单个用例的硬上限，防止极端情况下探针卡死。
const MAX_RUN: Duration = Duration::from_secs(120);

/// 发送端结束后，连续多少个 READ_TIMEOUT 没有新包就认为“排空了”。
const IDLE_AFTER_SENDER: u32 = 5;

const DEFAULT_DATAGRAMS: u64 = 200_000;
const DEFAULT_BUFFER_SWEEP: &str = "default,1048576,8388608";
const DEFAULT_SEGMENTS: &str = "8,32,64,128";

// ---------------------------------------------------------------------------
// 环境变量解析
// ---------------------------------------------------------------------------

struct ProbeConfig {
	/// 每档发送的数据报总数（所有档位相同，保证 MiB/s 可直接横比）。
	datagrams: u64,
	/// GSO 每批段数的扫描集合。
	segments: Vec<usize>,
	/// 基线 SO_SNDBUF/SO_RCVBUF 档位；`None` 表示不改（Windows 默认）。
	buffers: Vec<Option<usize>>,
	skip_gro: bool,
}

impl ProbeConfig {
	fn from_env() -> Self {
		let quick = env_flag("CB_UDP_PROBE_QUICK");
		let datagrams = if quick {
			20_000
		} else {
			env_u64("CB_UDP_PROBE_DATAGRAMS", DEFAULT_DATAGRAMS)
		};

		let segments_raw = env_string("CB_UDP_PROBE_SEGMENTS", DEFAULT_SEGMENTS);
		let segments: Vec<usize> = split_list(&segments_raw)
			.iter()
			.map(|s| {
				s.parse::<usize>().unwrap_or_else(|_| {
					panic!("CB_UDP_PROBE_SEGMENTS 里有非整数项: {s:?} (原始值 {segments_raw:?})")
				})
			})
			.collect();
		assert!(!segments.is_empty(), "CB_UDP_PROBE_SEGMENTS 不能为空");
		assert!(
			segments.iter().all(|&n| n >= 2),
			"段数必须 >= 2（1 段没有 GSO 意义）: {segments:?}"
		);

		let buffers_raw = env_string("CB_UDP_PROBE_BUFFERS", DEFAULT_BUFFER_SWEEP);
		let buffers: Vec<Option<usize>> = split_list(&buffers_raw)
			.iter()
			.map(|s| {
				if s.eq_ignore_ascii_case("default") {
					None
				} else {
					Some(s.parse::<usize>().unwrap_or_else(|_| {
						panic!(
							"CB_UDP_PROBE_BUFFERS 项必须是 \"default\" 或字节数: {s:?} (原始值 {buffers_raw:?})"
						)
					}))
				}
			})
			.collect();

		Self {
			datagrams,
			segments,
			buffers,
			skip_gro: env_flag("CB_UDP_PROBE_SKIP_GRO"),
		}
	}

	fn datagrams_label(&self) -> String {
		format!("{}", self.datagrams)
	}
}

fn env_string(name: &str, default: &str) -> String {
	std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_u64(name: &str, default: u64) -> u64 {
	match std::env::var(name) {
		Ok(raw) => raw
			.trim()
			.parse::<u64>()
			.unwrap_or_else(|_| panic!("{name} 必须是正整数，实际是 {raw:?}")),
		Err(_) => default,
	}
}

fn env_flag(name: &str) -> bool {
	matches!(
		std::env::var(name).ok().as_deref(),
		Some("1") | Some("true") | Some("yes")
	)
}

fn split_list(raw: &str) -> Vec<String> {
	raw.split(',')
		.map(|s| s.trim().to_string())
		.filter(|s| !s.is_empty())
		.collect()
}

fn human_bytes(bytes: Option<usize>) -> String {
	match bytes {
		None => "default".to_string(),
		Some(b) if b % (1 << 20) == 0 => format!("{}MiB", b >> 20),
		Some(b) if b % (1 << 10) == 0 => format!("{}KiB", b >> 10),
		Some(b) => format!("{b}B"),
	}
}

// ---------------------------------------------------------------------------
// 进程 CPU 时间（kernel + user）
// ---------------------------------------------------------------------------

/// 进程级 CPU 时间。Windows 用 `GetProcessTimes`；本文件整体 `cfg(windows)`，
/// 因此这里总是可用。返回单位 100ns（FILETIME 的原生单位）。
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
// OS build + 原始 setsockopt 探针
// ---------------------------------------------------------------------------

/// 打印 Windows 真实 build（`winver` 的等价物）。`GetVersionEx` 受清单影响，
/// 所以用 ntdll 的 `RtlGetVersion`，它总是返回真实版本。
mod os_version {
	#[repr(C)]
	struct OsVersionInfoW {
		size: u32,
		major: u32,
		minor: u32,
		build: u32,
		platform: u32,
		csd: [u16; 128],
	}

	#[link(name = "ntdll")]
	extern "system" {
		fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
	}

	pub(super) fn describe() -> String {
		let mut info = OsVersionInfoW {
			size: std::mem::size_of::<OsVersionInfoW>() as u32,
			major: 0,
			minor: 0,
			build: 0,
			platform: 0,
			csd: [0; 128],
		};

		// SAFETY: info 是栈上、大小字段已按要求填好的 OSVERSIONINFOW 兼容结构；
		// RtlGetVersion 写入该结构，调用期间指针有效。
		let rc = unsafe { RtlGetVersion(&mut info) };
		if rc == 0 {
			format!(
				"Windows {}.{}.{} (build {})",
				info.major, info.minor, info.build, info.build
			)
		} else {
			format!("RtlGetVersion 失败 rc={rc}")
		}
	}
}

/// 原始 `setsockopt` 探针。`quinn-udp` 只在内部吞掉这些错误（例如
/// `MAX_GSO_SEGMENTS` 探测失败时静默回退到 1），而本任务要求把**逐字**的
/// 错误报出来，所以这里直接调 ws2_32。socket 由 std 创建，Winsock 已经由
/// std 初始化完成，不需要手动 `WSAStartup`。
mod winsock {
	use std::io;
	use std::net::UdpSocket;
	use std::os::windows::io::AsRawSocket;

	pub(super) const IPPROTO_UDP: i32 = 17;
	pub(super) const UDP_SEND_MSG_SIZE: i32 = 2;
	pub(super) const UDP_RECV_MAX_COALESCED_SIZE: i32 = 3;

	#[link(name = "ws2_32")]
	extern "system" {
		// int setsockopt(SOCKET s, int level, int optname, const char *optval, int optlen)
		fn setsockopt(s: usize, level: i32, optname: i32, optval: *const u8, optlen: i32) -> i32;
	}

	pub(super) fn probe_u32(
		sock: &UdpSocket,
		name: &str,
		level: i32,
		opt: i32,
		value: u32,
	) -> String {
		// SAFETY: sock 是有效 socket；optval 指向栈上 u32，optlen=4 与之匹配；
		// setsockopt 只读取该 4 字节。
		let rc = unsafe {
			setsockopt(
				sock.as_raw_socket() as usize,
				level,
				opt,
				&value as *const u32 as *const u8,
				4,
			)
		};

		if rc == 0 {
			format!("{name}: setsockopt OK (value={value})")
		} else {
			let e = io::Error::last_os_error();
			format!(
				"{name}: setsockopt FAILED rc={rc} raw={:?} msg={e}",
				e.raw_os_error()
			)
		}
	}
}

// ---------------------------------------------------------------------------
// 测量
// ---------------------------------------------------------------------------

fn bind_loopback() -> io::Result<UdpSocket> {
	UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
}

fn is_timeout(e: &io::Error) -> bool {
	matches!(
		e.kind(),
		io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
	)
}

/// 一次 `recv` 调用返回了多少个数据报。
///
/// Windows 上 `UDP_COALESCED_INFO` cmsg 由 `quinn-udp` 解析进 `RecvMeta::stride`：
/// 没有 GRO 时 `stride == len`（就是普通单包）；有 GRO 时 `len` 是合并后的总长，
/// `stride` 是单个数据报大小，最后一个可能更短。因此用 `ceil(len/stride)` 还原。
fn datagrams_in(meta: &RecvMeta) -> u64 {
	if meta.len == 0 {
		0
	} else if meta.stride == 0 {
		1
	} else {
		meta.len.div_ceil(meta.stride) as u64
	}
}

/// 一个用例的发送/接收参数。
struct RunSpec {
	label: String,
	/// 发送系统调用次数（每批一次 `WSASendMsg`）。
	sends: u64,
	/// 每批段数；1 表示“每个系统调用只发一个数据报”的基线。
	segments: usize,
	/// 是否在接收端打开 GRO（`UDP_RECV_MAX_COALESCED_SIZE`）。
	gro: bool,
	/// 是否把 SO_SNDBUF/SO_RCVBUF 设成该值（`None` = 保持系统默认）。
	/// 发送/接收分开设，是为了区分“发送缓冲变大”与“接收缓冲变大”各自的影响。
	send_buffer: Option<usize>,
	recv_buffer: Option<usize>,
	/// 接收端传给 `WSARecvMsg` 的缓冲大小。
	recv_buffer_bytes: usize,
}

impl RunSpec {
	fn expected_datagrams(&self) -> u64 {
		self.sends * self.segments as u64
	}
}

#[derive(Default)]
struct RecvAccum {
	datagrams: u64,
	bytes: u64,
	calls: u64,
	coalesced_calls: u64,
	max_len: usize,
	max_stride: usize,
	anomaly_sizes: u64,
	hard_errors: u64,
	errors: Vec<String>,
	window_start: Option<Duration>,
	window_end: Duration,
}

struct RunResult {
	label: String,
	segments: usize,
	batches_sent: u64,
	expected_datagrams: u64,
	datagrams_recv: u64,
	bytes_recv: u64,
	send_elapsed: Duration,
	recv_active: Duration,
	cpu_seconds: f64,
	recv_calls: u64,
	coalesced_calls: u64,
	max_len: usize,
	max_stride: usize,
	anomaly_sizes: u64,
	hard_errors: u64,
	recv_errors: Vec<String>,
	send_errors: Vec<String>,
	notes: Vec<String>,
}

impl RunResult {
	fn drops(&self) -> u64 {
		self.expected_datagrams.saturating_sub(self.datagrams_recv)
	}

	fn mib_per_s(&self) -> f64 {
		let secs = self.recv_active.as_secs_f64();
		if secs <= 0.0 {
			0.0
		} else {
			self.bytes_recv as f64 / (1024.0 * 1024.0) / secs
		}
	}

	fn datagrams_per_s(&self) -> f64 {
		let secs = self.recv_active.as_secs_f64();
		if secs <= 0.0 {
			0.0
		} else {
			self.datagrams_recv as f64 / secs
		}
	}

	/// 每个**收到的**数据报摊到的进程 CPU（微秒，两端合计）。
	fn cpu_us_per_datagram(&self) -> f64 {
		if self.datagrams_recv == 0 {
			0.0
		} else {
			self.cpu_seconds * 1e6 / self.datagrams_recv as f64
		}
	}

	/// 每个 GiB **收到**的数据摊到的进程 CPU（毫秒，两端合计）。
	fn cpu_ms_per_gib(&self) -> f64 {
		if self.bytes_recv == 0 {
			0.0
		} else {
			self.cpu_seconds * 1000.0 / (self.bytes_recv as f64 / (1u64 << 30) as f64)
		}
	}

	fn datagrams_per_call(&self) -> f64 {
		if self.recv_calls == 0 {
			0.0
		} else {
			self.datagrams_recv as f64 / self.recv_calls as f64
		}
	}

	fn print_row(&self) {
		// 短于 100 ms 的用例：本机 GetProcessTimes 粒度约 15.6 ms，CPU 数字会
		// 变成 0 或一个 tick 的整数倍，直接标 n/a，避免误读。
		let cpu_reliable = self.recv_active >= Duration::from_millis(100);
		println!(
			"{:<48} {:>7} {:>9} {:>11} {:>10} {:>10} {:>11} {:>12} {:>10} {:>9} {:>10} {:>8}",
			self.label,
			self.batches_sent,
			self.datagrams_recv,
			format!("{:.1}", self.mib_per_s()),
			format!("{:.3}", self.datagrams_per_s() / 1e6),
			format!("{:.1}", self.send_elapsed.as_secs_f64() * 1e3),
			format!("{:.1}", self.recv_active.as_secs_f64() * 1e3),
			if cpu_reliable {
				format!("{:.3}", self.cpu_us_per_datagram())
			} else {
				"n/a".to_string()
			},
			if cpu_reliable {
				format!("{:.1}", self.cpu_ms_per_gib())
			} else {
				"n/a".to_string()
			},
			format!("{:.2}", self.datagrams_per_call()),
			self.coalesced_calls,
			self.drops(),
		);
	}

	fn print_details(&self) {
		println!(
			"  [{}] seg/批={} max_recv_len={} max_stride={} anomaly_sizes={} hard_errors={}",
			self.label,
			self.segments,
			self.max_len,
			self.max_stride,
			self.anomaly_sizes,
			self.hard_errors
		);
		for note in &self.notes {
			println!("  [{}] note: {note}", self.label);
		}
		for err in &self.send_errors {
			println!("  [{}] SEND ERROR: {err}", self.label);
		}
		for err in &self.recv_errors {
			println!("  [{}] RECV ERROR: {err}", self.label);
		}
	}
}

/// 跑一个用例。发送线程与接收线程并发；发送端先结束，接收端排空后退出。
fn run(spec: &RunSpec) -> RunResult {
	let recv_sock = bind_loopback().expect("bind receiver");
	let send_sock = bind_loopback().expect("bind sender");
	let dst = recv_sock.local_addr().expect("receiver local_addr");

	let send_state =
		UdpSocketState::new(UdpSockRef::from(&send_sock)).expect("sender UdpSocketState");
	let recv_state =
		UdpSocketState::new(UdpSockRef::from(&recv_sock)).expect("receiver UdpSocketState");

	let mut notes = Vec::new();

	// --- GRO：只能通过 quinn-udp 的显式开关打开（quinn 自身从不调用它） ---
	if spec.gro {
		match recv_state.set_gro(UdpSockRef::from(&recv_sock), true) {
			Ok(()) => notes.push("set_gro(true): OK".to_string()),
			Err(e) => notes.push(format!(
				"set_gro(true): FAILED raw={:?} msg={e}",
				e.raw_os_error()
			)),
		}
	}

	// --- 缓冲：先设，再读回实际值（Windows 可能不按请求值兑现） ---
	if let Some(bytes) = spec.send_buffer {
		match send_state.set_send_buffer_size(UdpSockRef::from(&send_sock), bytes) {
			Ok(()) => {}
			Err(e) => notes.push(format!(
				"set_send_buffer_size({bytes}): FAILED raw={:?} msg={e}",
				e.raw_os_error()
			)),
		}
	}
	if let Some(bytes) = spec.recv_buffer {
		match recv_state.set_recv_buffer_size(UdpSockRef::from(&recv_sock), bytes) {
			Ok(()) => {}
			Err(e) => notes.push(format!(
				"set_recv_buffer_size({bytes}): FAILED raw={:?} msg={e}",
				e.raw_os_error()
			)),
		}
	}
	let send_buf = send_state.send_buffer_size(UdpSockRef::from(&send_sock));
	let recv_buf = recv_state.recv_buffer_size(UdpSockRef::from(&recv_sock));
	notes.push(format!(
		"actual SO_SNDBUF={:?} SO_RCVBUF={:?}",
		send_buf.ok(),
		recv_buf.ok()
	));

	// `UdpSocketState::new` 把 socket 设成了 nonblocking；测量要阻塞 + 超时，
	// 否则空转会把 CPU 数字抬高，或让发送端永远忙等。
	send_sock.set_nonblocking(false).expect("sender blocking");
	recv_sock.set_nonblocking(false).expect("receiver blocking");
	recv_sock
		.set_read_timeout(Some(READ_TIMEOUT))
		.expect("receiver read timeout");
	send_sock
		.set_write_timeout(Some(SEND_TIMEOUT))
		.expect("sender write timeout");

	// 发送缓冲：每段内容相同（0x5A）。这里不写序号，因为写序号本身就是每包
	// 一次额外写，会污染“每包固定成本”；本探针只验证到达的数据报**数量与大小**。
	let payload = vec![0x5Au8; PAYLOAD_BYTES * spec.segments];

	let sender_done = Arc::new(AtomicBool::new(false));
	let sender_flag = Arc::clone(&sender_done);
	let sends = spec.sends;
	let segments = spec.segments;

	let cpu_before = process_cpu::total_100ns().expect("GetProcessTimes before");

	let sender = thread::spawn(move || {
		let mut sent = 0u64;
		let mut errors = Vec::new();
		let started = Instant::now();

		let tx = Transmit {
			destination: dst,
			ecn: None,
			contents: &payload,
			// segments == 1 时传 None：走“一个系统调用一个数据报”的基线路径。
			// segments > 1 时传 Some(PAYLOAD_BYTES)：一次 WSASendMsg + cmsg，
			// 由 Windows 按 cmsg 里的 UDP_SEND_MSG_SIZE 切分。
			segment_size: (segments > 1).then_some(PAYLOAD_BYTES),
			src_ip: None,
		};

		for _ in 0..sends {
			match send_state.try_send(UdpSockRef::from(&send_sock), &tx) {
				Ok(()) => sent += 1,
				Err(e) => {
					if errors.len() < 8 {
						errors.push(format!(
							"{} [raw={:?}, kind={:?}]",
							e,
							e.raw_os_error(),
							e.kind()
						));
					}
					// 超时（接收端 5 s 没排空）或其他硬错误都中止本档，
					// 否则阻塞 send 可能一直卡住。
					break;
				}
			}
		}

		sender_flag.store(true, Ordering::Release);
		(sent, started.elapsed(), errors)
	});

	let expected = spec.expected_datagrams();
	let recv_buffer_bytes = spec.recv_buffer_bytes;
	let recv_flag = Arc::clone(&sender_done);

	let receiver = thread::spawn(move || {
		let mut buf = vec![0u8; recv_buffer_bytes];
		let mut meta = [RecvMeta::default()];
		let mut acc = RecvAccum::default();
		let mut idle = 0u32;
		let mut consecutive_errors = 0u32;
		let started = Instant::now();

		loop {
			let mut iovs = [IoSliceMut::new(buf.as_mut_slice())];
			match recv_state.recv(UdpSockRef::from(&recv_sock), &mut iovs, &mut meta) {
				Ok(n) if n > 0 => {
					idle = 0;
					consecutive_errors = 0;
					acc.calls += 1;

					let m = meta[0];
					let dgrams = datagrams_in(&m);
					acc.datagrams += dgrams;
					acc.bytes += m.len as u64;
					acc.max_len = acc.max_len.max(m.len);
					acc.max_stride = acc.max_stride.max(m.stride);

					// stride < len 是“这一次 recv 真的合并了多个数据报”的判据。
					if m.stride > 0 && m.len > m.stride {
						acc.coalesced_calls += 1;
					}
					// 单包却尺寸不对：要么对端把整批当成了一个巨型数据报（GSO
					// cmsg 被忽略），要么出现了截断。位置和大小会打印出来。
					if dgrams == 1 && m.len != PAYLOAD_BYTES {
						acc.anomaly_sizes += 1;
					}

					let now = started.elapsed();
					if acc.window_start.is_none() {
						acc.window_start = Some(now);
					}
					acc.window_end = now;

					if acc.datagrams >= expected {
						break;
					}
				}
				Ok(_) => {}
				Err(e) if is_timeout(&e) => {
					// 发送端已经结束且连续 5×100 ms 没有新包 → 认为已排空。
					if recv_flag.load(Ordering::Acquire) {
						idle += 1;
						if idle >= IDLE_AFTER_SENDER {
							break;
						}
					}
					if started.elapsed() > MAX_RUN {
						break;
					}
				}
				Err(e) => {
					consecutive_errors += 1;
					acc.hard_errors += 1;
					if acc.errors.len() < 8 {
						acc.errors.push(format!(
							"{} [raw={:?}, kind={:?}]",
							e,
							e.raw_os_error(),
							e.kind()
						));
					}
					// WSAEMSGSIZE 等错误会逐包出现；给一个上限，避免接收端
					// 在发送端已经退出后空转。
					if consecutive_errors >= 256 || started.elapsed() > MAX_RUN {
						break;
					}
				}
			}
		}

		(acc, started.elapsed())
	});

	let (batches_sent, send_elapsed, send_errors) = sender.join().expect("join sender");
	let (acc, recv_total) = receiver.join().expect("join receiver");
	let cpu_after = process_cpu::total_100ns().expect("GetProcessTimes after");
	let cpu_seconds = cpu_after.saturating_sub(cpu_before) as f64 / 10_000_000.0;

	let recv_active = match acc.window_start {
		Some(start) => acc.window_end.saturating_sub(start),
		None => Duration::ZERO,
	};

	if recv_active.is_zero() {
		notes.push(format!(
			"接收端没有收到任何数据报（总等待 {:.1} ms）",
			recv_total.as_secs_f64() * 1e3
		));
	}

	RunResult {
		label: spec.label.clone(),
		segments: spec.segments,
		batches_sent,
		expected_datagrams: batches_sent * spec.segments as u64,
		datagrams_recv: acc.datagrams,
		bytes_recv: acc.bytes,
		send_elapsed,
		recv_active,
		cpu_seconds,
		recv_calls: acc.calls,
		coalesced_calls: acc.coalesced_calls,
		max_len: acc.max_len,
		max_stride: acc.max_stride,
		anomaly_sizes: acc.anomaly_sizes,
		hard_errors: acc.hard_errors,
		recv_errors: acc.errors,
		send_errors,
		notes,
	}
}

/// 纯 userspace 拷贝的参照：每个数据报做一次 `memcpy(1452 B)`。
///
/// 真实 UDP 路径至少有两次等价拷贝（user→kernel、kernel→user），所以这里给出
/// 一个“payload 拷贝”的量级参照。实现上必须保证编译器不能把拷贝优化掉：
/// 每次迭代改一个源字节，并在迭代内把 dst **全部**字节读回来做累加和。这样
/// 上一轮的写一定被下一轮读到，死存储消除无法删掉任何一次 memcpy。
///
/// 代价：每轮实际是“一次 memcpy + 一次完整读回”，所以它是一次拷贝的**上界**，
/// 不是精确的 memcpy 成本。即便按上界算，它也比 UDP 每包成本小两个数量级，
/// 足以支撑“不是拷贝瓶颈”的结论。
///
/// 只返回墙钟：这台机器的 `GetProcessTimes` 精度约 15.625 ms（见输出），而拷贝
/// 循环总耗时只有几毫秒，用进程 CPU 计时会读出 0。单线程无阻塞，墙钟≈CPU，
/// 因此墙钟是这里唯一可用的口径。
fn measure_copy_loop(datagrams: u64) -> Duration {
	let mut src = vec![0xA5u8; PAYLOAD_BYTES];
	let mut dst = vec![0u8; PAYLOAD_BYTES];
	let mut checksum = 0u64;

	let started = Instant::now();
	for i in 0..datagrams {
		src[0] = (i & 0xff) as u8;
		dst.copy_from_slice(&src);
		checksum = checksum.wrapping_add(dst.iter().map(|&b| u64::from(b)).sum::<u64>());
	}
	let elapsed = started.elapsed();
	std::hint::black_box(checksum);

	elapsed
}

/// 把“发送”和“接收”拆开测：先由发送端把一小批数据报灌进 8 MiB 接收缓冲，
/// 然后只给接收端计时，直到排空。
///
/// 为什么需要它：基线用例里发送和接收两个线程并发，进程 CPU 是两者之和，
/// 无法回答“每包 ~19 µs 里多少是发送、多少是接收”。这里发送阶段和接收阶段
/// 串行执行，两个阶段的墙钟就可以分别归因。
///
/// 计时口径：用 `Instant` 墙钟而不是 `GetProcessTimes`，因为每个阶段只有
/// 几十毫秒，而本机进程 CPU 计时精度约 15.625 ms，会带来几十个百分点的量化
/// 误差。两个阶段各自只有一个线程在跑、且不会阻塞（发送时 2.9 MiB < 8 MiB
/// 缓冲；接收时数据已在队列里），所以墙钟≈该线程的 CPU 时间。
///
/// 限制：每批 2000×1452≈2.8 MiB，若系统给的有效接收容量小于它就会丢包；
/// 接收阶段超时提前结束也会如实报告缺口。
struct SplitIoResult {
	send_us_per_dgram: f64,
	recv_us_per_dgram: f64,
	recv_mib_per_s: f64,
	datagrams: u64,
	dropped: u64,
}

fn measure_split_io(total_datagrams: u64) -> SplitIoResult {
	const BURST: u64 = 2000;

	let recv_sock = bind_loopback().expect("split bind receiver");
	let send_sock = bind_loopback().expect("split bind sender");
	let dst = recv_sock.local_addr().expect("split receiver addr");

	let send_state =
		UdpSocketState::new(UdpSockRef::from(&send_sock)).expect("split sender UdpSocketState");
	let recv_state =
		UdpSocketState::new(UdpSockRef::from(&recv_sock)).expect("split receiver UdpSocketState");

	// 8 MiB 才能在一轮里装下 2000 个数据报而不丢包。
	let bytes = 8usize << 20;
	send_state
		.set_send_buffer_size(UdpSockRef::from(&send_sock), bytes)
		.expect("split SO_SNDBUF");
	recv_state
		.set_recv_buffer_size(UdpSockRef::from(&recv_sock), bytes)
		.expect("split SO_RCVBUF");

	send_sock
		.set_nonblocking(false)
		.expect("split sender blocking");
	recv_sock
		.set_nonblocking(false)
		.expect("split receiver blocking");
	recv_sock
		.set_read_timeout(Some(READ_TIMEOUT))
		.expect("split read timeout");
	send_sock
		.set_write_timeout(Some(SEND_TIMEOUT))
		.expect("split write timeout");

	let payload = vec![0x3Cu8; PAYLOAD_BYTES];
	let tx = Transmit {
		destination: dst,
		ecn: None,
		contents: &payload,
		segment_size: None,
		src_ip: None,
	};

	let mut recv_buf = vec![0u8; RECV_BUFFER_BYTES];
	let mut meta = [RecvMeta::default()];

	let mut send_active = Duration::ZERO;
	let mut recv_active = Duration::ZERO;
	let mut datagrams = 0u64;
	let mut dropped = 0u64;

	let rounds = total_datagrams.div_ceil(BURST).max(1);
	for _ in 0..rounds {
		// --- 发送阶段：接收端不读，数据报先进入接收缓冲 ---
		let started = Instant::now();
		for _ in 0..BURST {
			send_state
				.try_send(UdpSockRef::from(&send_sock), &tx)
				.expect("split send");
		}
		send_active += started.elapsed();

		// 让 tcpip.sys 把回环数据放进接收队列（回环上通常是同步的，2 ms 足够）。
		thread::sleep(Duration::from_millis(2));

		// --- 接收阶段：只统计这一段的墙钟 ---
		let started = Instant::now();
		let mut got = 0u64;
		while got < BURST {
			let mut iovs = [IoSliceMut::new(recv_buf.as_mut_slice())];
			match recv_state.recv(UdpSockRef::from(&recv_sock), &mut iovs, &mut meta) {
				Ok(n) if n > 0 => got += datagrams_in(&meta[0]),
				Ok(_) => {}
				Err(e) if is_timeout(&e) => break,
				Err(e) => panic!("split recv unexpected error: {e}"),
			}
		}
		recv_active += started.elapsed();
		datagrams += got;
		dropped += BURST - got;
	}

	SplitIoResult {
		send_us_per_dgram: if datagrams == 0 {
			0.0
		} else {
			send_active.as_secs_f64() * 1e6 / datagrams as f64
		},
		recv_us_per_dgram: if datagrams == 0 {
			0.0
		} else {
			recv_active.as_secs_f64() * 1e6 / datagrams as f64
		},
		recv_mib_per_s: if recv_active.is_zero() {
			0.0
		} else {
			datagrams as f64 * PAYLOAD_BYTES as f64 / (1024.0 * 1024.0) / recv_active.as_secs_f64()
		},
		datagrams,
		dropped,
	}
}

// ---------------------------------------------------------------------------
// 用例编排
// ---------------------------------------------------------------------------

#[test]
#[ignore = "raw loopback UDP GSO/GRO probe; run with: cargo test --release -p cb_core --test udp_batch_probe -- --ignored --nocapture"]
fn udp_batch_probe() {
	let probe_started = Instant::now();
	let cfg = ProbeConfig::from_env();

	println!();
	println!("=== cb_core Windows UDP GSO/GRO raw probe ===");
	println!(
		"[env] os={} arch={} profile={} cpus={}",
		os_version::describe(),
		std::env::consts::ARCH,
		if cfg!(debug_assertions) {
			"debug (数字不可用于结论，请用 --release)"
		} else {
			"release"
		},
		thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get)
	);
	println!(
		"[env] datagrams/case={} segments={:?} buffers={:?} skip_gro={}",
		cfg.datagrams_label(),
		cfg.segments,
		cfg.buffers
			.iter()
			.map(|b| human_bytes(*b))
			.collect::<Vec<_>>(),
		cfg.skip_gro
	);

	// 这些是 quinn-udp 在当前平台上的静态能力探测值；`max_gso_segments` 在
	// Windows 上就是“临时 socket 上 setsockopt(UDP_SEND_MSG_SIZE, 1500) 是否成功”，
	// 失败会静默返回 1，所以后面还要用真实发送来验证。
	let capability_sock = bind_loopback().expect("bind capability socket");
	let capability_state =
		UdpSocketState::new(UdpSockRef::from(&capability_sock)).expect("capability UdpSocketState");
	println!(
		"[cap] quinn_udp BATCH_SIZE={} max_gso_segments={} gro_segments={}",
		udp::BATCH_SIZE,
		capability_state.max_gso_segments(),
		capability_state.gro_segments()
	);
	// UdpSocketState 没有 Drop 实现，离开作用域即可；这里只需关闭 socket。
	drop(capability_sock);

	// --- 原始 sockopt 探针（逐字报告错误） ---
	println!();
	println!("[sockopt] 原始 setsockopt 探针（绑定后的 IPv4 UDP socket）");
	let sockopt_sock = bind_loopback().expect("bind sockopt socket");
	println!(
		"[sockopt] {}",
		winsock::probe_u32(
			&sockopt_sock,
			"UDP_SEND_MSG_SIZE",
			winsock::IPPROTO_UDP,
			winsock::UDP_SEND_MSG_SIZE,
			PAYLOAD_BYTES as u32,
		)
	);
	println!(
		"[sockopt] {}",
		winsock::probe_u32(
			&sockopt_sock,
			"UDP_RECV_MAX_COALESCED_SIZE",
			winsock::IPPROTO_UDP,
			winsock::UDP_RECV_MAX_COALESCED_SIZE,
			u16::MAX as u32,
		)
	);
	drop(sockopt_sock);

	let mut results: Vec<RunResult> = Vec::new();

	// --- 1) 基线 + SO_SNDBUF/SO_RCVBUF 扫描 ---
	// 每档都是“每次 send 一个 1452 B 数据报、每次 recv 一个数据报”，
	// 唯一变化的是 socket 缓冲大小，用来回答“195 MiB/s 里有多少是缓冲/阻塞
	// 造成的”。默认档不调用 set_*_buffer_size，保留系统默认（Windows 常见 64 KiB）。
	for buffers in cfg.buffers.clone() {
		let spec = RunSpec {
			label: format!("baseline 1 pkt/syscall (buf={})", human_bytes(buffers)),
			sends: cfg.datagrams,
			segments: 1,
			gro: false,
			send_buffer: buffers,
			recv_buffer: buffers,
			recv_buffer_bytes: RECV_BUFFER_BYTES,
		};
		println!();
		println!("[case] {}", spec.label);
		results.push(run(&spec));
	}

	// --- 1b) 缓冲归因：只放大发送缓冲 / 只放大接收缓冲 ---
	// 第一轮结果里“缓冲越大反而越慢”是反直觉的，必须把两侧拆开验证，
	// 否则不能把原因写成“接收缓冲变大导致”。
	for (label, send_buffer, recv_buffer) in [
		(
			"baseline 1 pkt/syscall (sndbuf=8MiB only)",
			Some(8 << 20),
			None,
		),
		(
			"baseline 1 pkt/syscall (rcvbuf=8MiB only)",
			None,
			Some(8 << 20),
		),
	] {
		let spec = RunSpec {
			label: label.to_string(),
			sends: cfg.datagrams,
			segments: 1,
			gro: false,
			send_buffer,
			recv_buffer,
			recv_buffer_bytes: RECV_BUFFER_BYTES,
		};
		println!();
		println!("[case] {}", spec.label);
		results.push(run(&spec));
	}

	// --- 1c) 基线重复一次：用同一配置再跑一遍，给读者一个“噪声有多大”的参照 ---
	// 如果两次数值差很多，后面所有百分比都应打折解读。
	{
		let spec = RunSpec {
			label: "baseline 1 pkt/syscall (buf=default) REPEAT".to_string(),
			sends: cfg.datagrams,
			segments: 1,
			gro: false,
			send_buffer: None,
			recv_buffer: None,
			recv_buffer_bytes: RECV_BUFFER_BYTES,
		};
		println!();
		println!("[case] {}", spec.label);
		results.push(run(&spec));
	}

	// --- 2) GSO 发送：一次 WSASendMsg 发 N 段，接收端仍逐包收 ---
	// 这一档与“基线 + 8 MiB 缓冲”只差发送侧系统调用次数，因此两者之差
	// 就是**发送侧每包系统调用 + 栈开销**的直接测量。
	for &segments in &cfg.segments {
		let bytes_per_call = segments * PAYLOAD_BYTES;
		let spec = RunSpec {
			label: format!(
				"GSO send segments={segments} ({}B/call, buf=8MiB)",
				bytes_per_call
			),
			sends: cfg.datagrams / segments as u64,
			segments,
			gro: false,
			send_buffer: Some(8 << 20),
			recv_buffer: Some(8 << 20),
			recv_buffer_bytes: RECV_BUFFER_BYTES,
		};
		println!();
		println!("[case] {}", spec.label);
		results.push(run(&spec));
	}

	// --- 3) 仅 GRO 接收：发送仍逐包，接收端尝试合并 ---
	if !cfg.skip_gro {
		let spec = RunSpec {
			label: "GRO recv only (send 1 pkt/syscall, buf=8MiB)".to_string(),
			sends: cfg.datagrams,
			segments: 1,
			gro: true,
			send_buffer: Some(8 << 20),
			recv_buffer: Some(8 << 20),
			recv_buffer_bytes: RECV_BUFFER_BYTES,
		};
		println!();
		println!("[case] {}", spec.label);
		results.push(run(&spec));

		// --- 4) GSO + GRO：最接近“批处理 IO 全开”的形态 ---
		let segments = 32usize;
		let spec = RunSpec {
			label: format!("GSO send + GRO recv (segments={segments}, buf=8MiB)"),
			sends: cfg.datagrams / segments as u64,
			segments,
			gro: true,
			send_buffer: Some(8 << 20),
			recv_buffer: Some(8 << 20),
			recv_buffer_bytes: RECV_BUFFER_BYTES,
		};
		println!();
		println!("[case] {}", spec.label);
		results.push(run(&spec));

		// --- 5) 接收缓冲过小的诊断：如果 GRO 真的合并了，单次 recv 需要的
		// 空间会超过 2 KiB，`WSARecvMsg` 会返回 WSAEMSGSIZE(10040)。这里只发
		// 少量数据，专门观察错误文本，不参与吞吐对比。
		let spec = RunSpec {
			label: "DIAGNOSTIC: GRO recv 2KiB buffer, 256 dgrams (not a throughput number)"
				.to_string(),
			sends: 8,
			segments: 32,
			gro: true,
			send_buffer: Some(8 << 20),
			recv_buffer: Some(8 << 20),
			recv_buffer_bytes: 2048,
		};
		println!();
		println!("[case] {}", spec.label);
		results.push(run(&spec));
	}

	// --- 6) 纯拷贝参照 ---
	println!();
	println!(
		"[case] userspace memcpy(1452B)+full readback x {} (copy upper bound)",
		cfg.datagrams
	);
	let copy_elapsed = measure_copy_loop(cfg.datagrams);
	let copy_bytes = cfg.datagrams * PAYLOAD_BYTES as u64;
	let copy_mib_s = copy_bytes as f64 / (1024.0 * 1024.0) / copy_elapsed.as_secs_f64();
	let copy_us_per_dgram = copy_elapsed.as_secs_f64() * 1e6 / cfg.datagrams as f64;
	let copy_ms_per_gib =
		copy_elapsed.as_secs_f64() * 1000.0 / (copy_bytes as f64 / (1u64 << 30) as f64);
	println!(
		"[copy] {:.1} MiB/s, {:.3} us/dgram, {:.2} ms/GiB (wall；memcpy+整块读回，一次拷贝的上界)",
		copy_mib_s, copy_us_per_dgram, copy_ms_per_gib
	);

	// --- 7) 发送/接收拆开归因 ---
	// 并发用例的进程 CPU 是两端之和；这里把“灌满接收缓冲”和“排空”串行执行，
	// 分别量发送侧、接收侧的墙钟/包，用来回答“~19 µs/包 里哪一侧占大头”。
	println!();
	println!("[case] split send-only / recv-only attribution (2000-dgram bursts, 8MiB)");
	let split_total = cfg.datagrams.min(40_000);
	let split = measure_split_io(split_total);
	println!(
		"[split] dgrams={} dropped={} send-only={:.3} us/dg recv-only={:.3} us/dg recv-drain={:.1} MiB/s",
		split.datagrams,
		split.dropped,
		split.send_us_per_dgram,
		split.recv_us_per_dgram,
		split.recv_mib_per_s
	);

	// --- 汇总表 ---
	println!();
	println!("--- 明细 ---");
	for result in &results {
		result.print_details();
	}

	println!();
	println!(
		"{:<48} {:>7} {:>9} {:>11} {:>10} {:>10} {:>11} {:>12} {:>10} {:>9} {:>10} {:>8}",
		"variant",
		"batches",
		"dgrams",
		"MiB/s",
		"Mdgm/s",
		"send_ms",
		"active_ms",
		"cpu_us/dg",
		"cpu_ms/GiB",
		"dg/recv",
		"coalesced",
		"drops"
	);
	println!("{}", "-".repeat(168));
	for result in &results {
		result.print_row();
	}

	// --- 便于人工阅读的派生数字 ---
	let baseline = results.first();
	if let Some(base) = baseline {
		println!();
		println!("--- 派生 ---");
		println!(
			"[derive] baseline: {:.1} MiB/s, {:.0} dgram/s, {:.0} us CPU/dgram(两端), tx_batches={} rx_calls={}",
			base.mib_per_s(),
			base.datagrams_per_s(),
			base.cpu_us_per_datagram(),
			base.batches_sent,
			base.recv_calls
		);
		for other in results.iter().skip(1) {
			// 小缓冲诊断只有 256 个包，吞吐数字没有统计意义，不参与对比。
			if other.datagrams_recv < 1_000 || base.datagrams_recv < 1_000 {
				continue;
			}
			let speedup = other.mib_per_s() / base.mib_per_s();
			let cpu_ratio = other.cpu_us_per_datagram() / base.cpu_us_per_datagram();
			println!(
				"[derive] {}: {:+.1}% MiB/s vs baseline, CPU/dgram x{:.3}",
				other.label,
				(speedup - 1.0) * 100.0,
				cpu_ratio
			);
		}

		// 发送/接收拆分与基线总成本的对照。
		let split_sum = split.send_us_per_dgram + split.recv_us_per_dgram;
		println!(
			"[derive] baseline total={:.3} us/dg (CPU); split send-only={:.3} + recv-only={:.3} = {:.3} us/dg (wall, {:.0}% of baseline)",
			base.cpu_us_per_datagram(),
			split.send_us_per_dgram,
			split.recv_us_per_dgram,
			split_sum,
			if base.cpu_us_per_datagram() > 0.0 {
				split_sum / base.cpu_us_per_datagram() * 100.0
			} else {
				0.0
			}
		);
	}

	println!();
	println!(
		"[probe] total wall time: {:.2} s",
		probe_started.elapsed().as_secs_f64()
	);
	println!("[probe] 口径提醒:");
	println!("[probe]   - CPU 为 sender+receiver 同进程的 kernel+user 总和，不是单端点 CPU");
	println!("[probe]   - MiB/s 以接收端实际收到为准，drops 列是差额；loopback 仍可能丢包");
	println!("[probe]   - GSO 发送档的批次数 = datagrams/segments，若发送报错则 batches 提前停止");
	println!("[probe]   - GRO 判据：单次 recv 的 len > stride（stride 即 UDP_COALESCED_INFO）");
	println!("[probe]   - 拆分归因/拷贝用墙钟；本机 GetProcessTimes 粒度约 15.6 ms，短窗口不可用");
	println!("[probe]   - 拷贝行是 memcpy+整块读回的上界，不是精确 memcpy 成本");
	println!("[probe]   - 只测 IPv4 loopback；真实网卡/防火墙/第二主机均未覆盖");
}
