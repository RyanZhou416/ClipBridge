//! hub 并发压力探针（集成测试，默认 `#[ignore]`）。
//!
//! 目标：量化“单个 hub Core 在向 K 个对端持续传输大内容的同时，
//! (a) 本地剪贴板摄入延迟、(b) 控制面 `list_peers()` 延迟、以及
//! (c) 聚合吞吐”随 K 的变化，用来观察 P1（多线程 runtime + 非阻塞拨号/广播）
//! 与 P2（日志/DB 移出 runtime worker）修复之后还剩多少天花板。
//!
//! 运行：
//!
//! ```text
//! CB_HUB_K=2 CB_HUB_FILE_MB=16 cargo test -p cb_core --test hub_concurrency_probe -- --ignored --nocapture
//! ```
//!
//! 环境变量（全部可选；非法值回退默认并打印提示）：
//! - `CB_HUB_K`：本轮扫描的**最大** K，只接受 1/2/4/8，默认 2；
//!   探针会从 1 开始按 1/2/4/8 的阶梯一直跑到该值（例如 2 => 依次跑 K=1、K=2）。
//! - `CB_HUB_FILE_MB`：hub 种子大对象的 MiB 数，默认 64。
//! - `CB_HUB_TOTAL_SECS`：整个探针的墙钟预算，默认 60；预算不足时后续 K 会被跳过。
//! - `CB_HUB_RUN_TIMEOUT_SECS`：单个 K 等待全部传输完成的上限，默认 45。
//! - `CB_HUB_CONNECT_SECS`：等待全部 peer 变为 Online 的上限，默认 20。
//! - `CB_HUB_INGEST_MS`：hub 本地摄入的间隔，默认 100 ms。
//! - `CB_HUB_LIST_MS`：`list_peers()` 的采样间隔，默认 250 ms。
//!
//! 设计说明：
//! - 网络：复用现有 ignored 集成测试的 mDNS + QUIC 脚手架。公开 API 没有
//!   “按 device_id 直接拨号”的入口（`discovery` 只产出候选，拨号在
//!   `NetManager` 内部完成），因此只能走 mDNS。同一账号下所有节点会互相可见；
//!   `NetManager` 只让 device_id 字典序较小的一侧主动拨号，所以 hub 的 id
//!   取最小 => 每个 peer 主动连 hub（hub↔peer 是传输路径）。peer 之间也可能
//!   建立空闲会话，这是 mDNS 全互联脚手架的副作用，不影响“单个 hub + K 个
//!   peer”的传输语义。
//! - 大对象：默认 64 MiB 文本。默认 `hard_text_bytes` 只有 16 MiB，因此探针把
//!   所有 Core 的文本硬/软上限提到 8 GiB，并把 `text_auto_prefetch_bytes`
//!   置 0（关闭自动预取，保证走显式 `ensure_content_cached` 拉取路径）。
//!   文本与图片的传输/CAS 路径相同，选文本是为了避免图片 materialize 的额外
//!   文件操作。
//! - 并发：所有 Core API 都是阻塞式 FFI 风格；探针用普通 `std::thread`
//!   （非 tokio 运行时线程）调用 `ensure_content_cached`/`list_peers`，
//!   与现有测试里的 `spawn_blocking` 等价，且不会在 tokio worker 上
//!   `block_on`。
//! - 探针只测量、不判定：除“跑完”之外没有任何断言，避免 CI 抖动。

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cb_core::api::{AppConfig, Core, CoreConfig, CoreEventSink, PeerConnectionState};
use cb_core::clipboard::ClipboardSnapshot;
use cb_core::policy::SizeLimits;
use cb_core::util::now_ms;

// ---------------------------------------------------------------------------
// 常量与可调参数
// ---------------------------------------------------------------------------

/// 允许的 K 阶梯。K 不是任意整数：探针要能随 K 观察缩放，同时限制总开销。
const K_LADDER: [usize; 4] = [1, 2, 4, 8];
const DEFAULT_MAX_K: usize = 2;
const DEFAULT_FILE_MB: usize = 64;
const DEFAULT_TOTAL_SECS: usize = 60;
const DEFAULT_RUN_TIMEOUT_SECS: usize = 45;
const DEFAULT_CONNECT_SECS: usize = 20;
const DEFAULT_INGEST_MS: usize = 100;
const DEFAULT_LIST_MS: usize = 250;

const ACCOUNT_PASSWORD: &str = "hub-concurrency-probe-password";
/// 文本硬/软上限统一抬高，保证 CB_HUB_FILE_MB 在 1..=4096 都能被两端接受。
const TEXT_LIMIT_BYTES: i64 = 8 * 1024 * 1024 * 1024;
/// CAS GC 上限设为 i64::MAX：探针期间绝不希望 GC 把正在传输的 blob 删掉。
const CAS_GC_UNLIMITED: i64 = i64::MAX;
const HISTORY_MAX_ITEMS: i64 = 50_000;

/// 大对象文本的固定前缀，peer 收到后用它做一次廉价完整性抽查。
const BLOB_PREFIX: &str = "cb-hub-probe-blob";

// ---------------------------------------------------------------------------
// 参数解析
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ProbeParams {
	max_k: usize,
	file_mb: usize,
	total_secs: u64,
	run_timeout: Duration,
	connect_timeout: Duration,
	meta_timeout: Duration,
	ingest_period: Duration,
	list_period: Duration,
}

fn env_usize(name: &str, default: usize) -> usize {
	match std::env::var(name) {
		Ok(raw) => match raw.trim().parse::<usize>() {
			Ok(v) => v,
			Err(_) => {
				println!("[probe] 环境变量 {name}={raw:?} 不是合法整数，使用默认值 {default}");
				default
			}
		},
		Err(_) => default,
	}
}

fn load_params() -> ProbeParams {
	let requested_k = env_usize("CB_HUB_K", DEFAULT_MAX_K);
	let max_k = if K_LADDER.contains(&requested_k) {
		requested_k
	} else {
		println!(
			"[probe] CB_HUB_K={requested_k} 不在允许集合 {K_LADDER:?} 中，回退到默认 {DEFAULT_MAX_K}"
		);
		DEFAULT_MAX_K
	};

	let file_mb = env_usize("CB_HUB_FILE_MB", DEFAULT_FILE_MB).clamp(1, 4096);
	let total_secs = env_usize("CB_HUB_TOTAL_SECS", DEFAULT_TOTAL_SECS).max(10) as u64;
	let run_timeout_secs =
		env_usize("CB_HUB_RUN_TIMEOUT_SECS", DEFAULT_RUN_TIMEOUT_SECS).max(5) as u64;
	let connect_secs = env_usize("CB_HUB_CONNECT_SECS", DEFAULT_CONNECT_SECS).max(5) as u64;
	let ingest_ms = env_usize("CB_HUB_INGEST_MS", DEFAULT_INGEST_MS).clamp(10, 5_000) as u64;
	let list_ms = env_usize("CB_HUB_LIST_MS", DEFAULT_LIST_MS).clamp(10, 5_000) as u64;

	ProbeParams {
		max_k,
		file_mb,
		total_secs,
		run_timeout: Duration::from_secs(run_timeout_secs),
		connect_timeout: Duration::from_secs(connect_secs),
		// 元数据广播走已有的在线会话，正常在毫秒级；给足 15 s 只是兜底。
		meta_timeout: Duration::from_secs(15),
		ingest_period: Duration::from_millis(ingest_ms),
		list_period: Duration::from_millis(list_ms),
	}
}

fn print_banner(params: &ProbeParams) {
	println!();
	println!("=== cb_core hub concurrency probe ===");
	println!(
		"build: {} | os: {} {} | logical cpus: {}",
		if cfg!(debug_assertions) {
			"debug（吞吐数字不作为跨机构基准；建议 --release）"
		} else {
			"release"
		},
		std::env::consts::OS,
		std::env::consts::ARCH,
		thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get)
	);
	println!(
		"knobs: max_k={} file_mb={} total_secs={} run_timeout={:?} connect_timeout={:?} ingest_period={:?} list_period={:?}",
		params.max_k,
		params.file_mb,
		params.total_secs,
		params.run_timeout,
		params.connect_timeout,
		params.ingest_period,
		params.list_period
	);
	println!("[probe] 说明: 公开 API 没有直接拨号入口，网络使用 mDNS 发现（同账号全互联）；");
	println!(
		"[probe]       hub id 字典序最小，因此由每个 peer 主动连接 hub；peer↔peer 空闲会话是脚手架副作用。"
	);
	println!();
}

// ---------------------------------------------------------------------------
// 事件 sink：把事件送进管道（非阻塞），同时用原子计数统计关键事件
// ---------------------------------------------------------------------------

#[derive(Default)]
struct EventCounters {
	peer_offline: AtomicUsize,
	transfer_failed: AtomicUsize,
	core_error: AtomicUsize,
}

impl EventCounters {
	fn snapshot(&self) -> (usize, usize, usize) {
		(
			self.peer_offline.load(Ordering::Relaxed),
			self.transfer_failed.load(Ordering::Relaxed),
			self.core_error.load(Ordering::Relaxed),
		)
	}
}

struct ProbeSink {
	tx: mpsc::Sender<String>,
	counters: Arc<EventCounters>,
}

impl CoreEventSink for ProbeSink {
	fn emit(&self, event_json: String) {
		// 事件类型由 serde_json 序列化产生，这里用带引号的字面量避免误匹配 payload 文本。
		if event_json.contains("\"PEER_OFFLINE\"") {
			self.counters.peer_offline.fetch_add(1, Ordering::Relaxed);
		}
		if event_json.contains("\"TRANSFER_FAILED\"") {
			self.counters
				.transfer_failed
				.fetch_add(1, Ordering::Relaxed);
		}
		if event_json.contains("\"CORE_ERROR\"") {
			self.counters.core_error.fetch_add(1, Ordering::Relaxed);
		}
		// std mpsc 无界队列：send 不会阻塞 runtime worker；接收端被 drop 后静默忽略。
		let _ = self.tx.send(event_json);
	}
}

fn make_sink(counters: Arc<EventCounters>) -> (Arc<dyn CoreEventSink>, mpsc::Receiver<String>) {
	let (tx, rx) = mpsc::channel();
	(Arc::new(ProbeSink { tx, counters }), rx)
}

// ---------------------------------------------------------------------------
// 描述统计（单位：毫秒）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Stats {
	n: usize,
	mean: f64,
	p50: f64,
	p95: f64,
	p99: f64,
	max: f64,
}

impl Stats {
	fn from_ms(mut samples: Vec<f64>) -> Option<Self> {
		if samples.is_empty() {
			return None;
		}
		samples.sort_by(f64::total_cmp);

		let n = samples.len();
		let mean = samples.iter().sum::<f64>() / n as f64;
		let max = samples[n - 1];
		// 最近秩（nearest-rank），小样本时 p99 会等于 max，这是预期行为。
		let percentile = |q: f64| {
			let idx = ((q * n as f64).ceil() as usize)
				.saturating_sub(1)
				.min(n - 1);
			samples[idx]
		};

		Some(Self {
			n,
			mean,
			p50: percentile(0.50),
			p95: percentile(0.95),
			p99: percentile(0.99),
			max,
		})
	}

	/// 表格里的紧凑格式：`n/p50/p95/p99/max`。
	fn compact(&self) -> String {
		let (n, p50, p95, p99, max) = (self.n, self.p50, self.p95, self.p99, self.max);
		format!("{n}/{p50:.2}/{p95:.2}/{p99:.2}/{max:.2}")
	}
}

// ---------------------------------------------------------------------------
// 临时目录 / 清理
// ---------------------------------------------------------------------------

fn unique_probe_root() -> PathBuf {
	std::env::temp_dir().join(format!(
		"cb-hub-concurrency-probe-{}-{}",
		std::process::id(),
		uuid::Uuid::new_v4().simple()
	))
}

/// Windows 上 SQLite/CAS 句柄释放有延迟，删除目录时重试几次。
fn remove_dir_all_retry(path: &Path, max_wait: Duration) -> bool {
	if !path.exists() {
		return true;
	}
	let started = Instant::now();
	loop {
		match fs::remove_dir_all(path) {
			Ok(()) => return true,
			Err(_) if started.elapsed() < max_wait => {
				thread::sleep(Duration::from_millis(50));
			}
			Err(_) => return false,
		}
	}
}

// ---------------------------------------------------------------------------
// Core 构造
// ---------------------------------------------------------------------------

fn make_core(
	device_id: &str,
	device_name: &str,
	account_uid: &str,
	root: &Path,
	sink: Arc<dyn CoreEventSink>,
) -> Core {
	let data_dir = root.join("data");
	let cache_dir = root.join("cache");
	let _ = fs::create_dir_all(&data_dir);
	let _ = fs::create_dir_all(&cache_dir);

	let limits = SizeLimits {
		soft_text_bytes: TEXT_LIMIT_BYTES,
		hard_text_bytes: TEXT_LIMIT_BYTES,
		// 关闭自动预取：本探针要求 peer 显式走 ensure_content_cached。
		text_auto_prefetch_bytes: 0,
		..SizeLimits::default()
	};

	let cfg = CoreConfig {
		device_id: device_id.to_string(),
		device_name: device_name.to_string(),
		account_uid: account_uid.to_string(),
		account_password: ACCOUNT_PASSWORD.to_string(),
		data_dir: data_dir.to_string_lossy().into_owned(),
		cache_dir: cache_dir.to_string_lossy().into_owned(),
		app_config: AppConfig {
			size_limits: limits,
			gc_history_max_items: HISTORY_MAX_ITEMS,
			gc_cas_max_bytes: CAS_GC_UNLIMITED,
			..AppConfig::default()
		},
		disable_network: false,
	};

	Core::init(cfg, sink)
}

/// 一个 K 轮次里创建的全部 Core；`finish` 负责 shutdown + 删目录。
struct CoreSet {
	hub: Core,
	peers: Vec<Core>,
	dirs: Vec<PathBuf>,
}

impl CoreSet {
	/// 关闭所有 Core 并删除本轮目录；返回是否全部清理成功。
	fn finish(self) -> bool {
		let CoreSet { hub, peers, dirs } = self;
		for peer in &peers {
			peer.shutdown();
		}
		hub.shutdown();

		// shutdown 只发停止信号、不 join NetManager 线程；留一点时间让 QUIC/mDNS
		// 线程退出并释放目录句柄，否则 Windows 上删除目录会失败。
		thread::sleep(Duration::from_millis(500));
		drop(peers);
		drop(hub);
		thread::sleep(Duration::from_millis(300));

		let mut ok = true;
		for dir in &dirs {
			if !remove_dir_all_retry(dir, Duration::from_secs(3)) {
				ok = false;
				println!(
					"[probe] 清理失败（可能仍有句柄占用，建议稍后手动删除）: {}",
					dir.display()
				);
			}
		}
		ok
	}
}

// ---------------------------------------------------------------------------
// 启动闸门：让 K 个 peer 尽量同时发起拉取
// ---------------------------------------------------------------------------

struct StartGate {
	state: Mutex<GateState>,
	cv: Condvar,
}

#[derive(Default)]
struct GateState {
	ready: usize,
	open: bool,
}

impl StartGate {
	fn new() -> Self {
		Self {
			state: Mutex::new(GateState::default()),
			cv: Condvar::new(),
		}
	}

	fn mark_ready(&self) {
		let mut state = self.state.lock().unwrap();
		state.ready += 1;
		self.cv.notify_all();
	}

	/// 等待 `expected` 个工作线程到达；超时返回 false，但不会阻塞 `open`。
	fn wait_ready(&self, expected: usize, timeout: Duration) -> bool {
		let deadline = Instant::now() + timeout;
		let mut state = self.state.lock().unwrap();
		while state.ready < expected {
			let now = Instant::now();
			if now >= deadline {
				return false;
			}
			let (guard, _timed_out) = self.cv.wait_timeout(state, deadline - now).unwrap();
			state = guard;
		}
		true
	}

	fn open(&self) {
		let mut state = self.state.lock().unwrap();
		state.open = true;
		self.cv.notify_all();
	}

	fn wait_start(&self) {
		let mut state = self.state.lock().unwrap();
		while !state.open {
			state = self.cv.wait(state).unwrap();
		}
	}
}

/// 在 `stop` 被置位前睡到 `deadline`（10 ms 粒度，避免长时间不响应停止）。
fn sleep_until_or_stop(stop: &AtomicBool, deadline: Instant) {
	while !stop.load(Ordering::Acquire) {
		let now = Instant::now();
		if now >= deadline {
			break;
		}
		thread::sleep((deadline - now).min(Duration::from_millis(10)));
	}
}

// ---------------------------------------------------------------------------
// 监控线程：本地摄入 + 控制面 list_peers
// ---------------------------------------------------------------------------

struct IngestRun {
	samples_ms: Vec<f64>,
	errors: usize,
}

struct ListRun {
	samples_ms: Vec<f64>,
	errors: usize,
}

fn spawn_ingest_monitor(
	core: Core,
	gate: Arc<StartGate>,
	stop: Arc<AtomicBool>,
	run_tag: String,
	period: Duration,
) -> JoinHandle<IngestRun> {
	thread::Builder::new()
		.name("hub-probe-ingest".to_string())
		.spawn(move || {
			gate.wait_start();
			let mut samples_ms = Vec::new();
			let mut errors = 0usize;
			let mut index = 0usize;

			while !stop.load(Ordering::Acquire) {
				// 唯一文本：确保每次 ingest 真的写 CAS，而不是被去重成 no-op。
				let text = format!(
					"hub-probe ingest run={run_tag} sample={index:05} ts={}",
					now_ms()
				);
				let started = Instant::now();
				if core
					.ingest_local_copy(ClipboardSnapshot::Text {
						text_utf8: text,
						ts_ms: now_ms(),
					})
					.is_err()
				{
					errors += 1;
				}
				samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
				index += 1;

				// 固定“本次结束后再等一个周期”，避免慢调用后出现追赶式突发。
				sleep_until_or_stop(&stop, Instant::now() + period);
			}

			IngestRun { samples_ms, errors }
		})
		.expect("spawn ingest monitor thread")
}

fn spawn_list_monitor(
	core: Core,
	gate: Arc<StartGate>,
	stop: Arc<AtomicBool>,
	period: Duration,
) -> JoinHandle<ListRun> {
	thread::Builder::new()
		.name("hub-probe-list-peers".to_string())
		.spawn(move || {
			gate.wait_start();
			let mut samples_ms = Vec::new();
			let mut errors = 0usize;

			while !stop.load(Ordering::Acquire) {
				let started = Instant::now();
				if core.list_peers().is_err() {
					errors += 1;
				}
				samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
				sleep_until_or_stop(&stop, Instant::now() + period);
			}

			ListRun { samples_ms, errors }
		})
		.expect("spawn list_peers monitor thread")
}

// ---------------------------------------------------------------------------
// peer 拉取任务
// ---------------------------------------------------------------------------

struct PeerJob {
	index: usize,
	core: Core,
	rx: mpsc::Receiver<String>,
	item_id: String,
	expected_bytes: u64,
	meta_timeout: Duration,
	transfer_timeout: Duration,
	gate: Arc<StartGate>,
}

struct PeerOutcome {
	index: usize,
	meta_seen: bool,
	meta_wait_s: f64,
	transfer_id: String,
	completed: bool,
	elapsed_s: f64,
	bytes: u64,
	local_path: String,
	verified: bool,
	error: Option<String>,
}

fn read_prefix(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
	let mut file = fs::File::open(path)?;
	let mut buf = vec![0u8; max_bytes];
	let read = file.read(&mut buf)?;
	buf.truncate(read);
	Ok(buf)
}

fn run_peer_job(job: PeerJob) -> PeerOutcome {
	let PeerJob {
		index,
		core,
		rx,
		item_id,
		expected_bytes,
		meta_timeout,
		transfer_timeout,
		gate,
	} = job;

	// 1) 等 hub 广播的大对象元数据；只有本地 store 里有 owner 才能发起拉取。
	let meta_started = Instant::now();
	let meta_deadline = meta_started + meta_timeout;
	let mut meta_seen = false;
	while Instant::now() < meta_deadline {
		match rx.recv_timeout(Duration::from_millis(100)) {
			Ok(event) => {
				if event.contains("ITEM_META_ADDED") && event.contains(&item_id) {
					meta_seen = true;
					break;
				}
			}
			Err(mpsc::RecvTimeoutError::Timeout) => {}
			Err(mpsc::RecvTimeoutError::Disconnected) => break,
		}
	}
	let meta_wait_s = meta_started.elapsed().as_secs_f64();

	// 2) 所有 peer 在同一个闸门前集合，随后尽量同时调用 ensure_content_cached。
	gate.mark_ready();
	gate.wait_start();

	if !meta_seen {
		return PeerOutcome {
			index,
			meta_seen,
			meta_wait_s,
			transfer_id: String::new(),
			completed: false,
			elapsed_s: 0.0,
			bytes: 0,
			local_path: String::new(),
			verified: false,
			error: Some("未在超时内收到 ITEM_META_ADDED".to_string()),
		};
	}

	// 3) 公开内容 API 拉取（阻塞式，普通线程调用）。
	let started = Instant::now();
	let transfer_id = match core.ensure_content_cached(&item_id, None) {
		Ok(tid) => tid,
		Err(e) => {
			return PeerOutcome {
				index,
				meta_seen,
				meta_wait_s,
				transfer_id: String::new(),
				completed: false,
				elapsed_s: started.elapsed().as_secs_f64(),
				bytes: 0,
				local_path: String::new(),
				verified: false,
				error: Some(format!("ensure_content_cached 失败: {e}")),
			};
		}
	};

	// 4) 等 CONTENT_CACHED / TRANSFER_FAILED。
	let deadline = started + transfer_timeout;
	let mut completed = false;
	let mut local_path = String::new();
	let mut bytes = 0u64;
	let mut error = None;
	while Instant::now() < deadline {
		let wait = deadline
			.saturating_duration_since(Instant::now())
			.min(Duration::from_millis(100));
		match rx.recv_timeout(wait) {
			Ok(event) => {
				if event.contains("CONTENT_CACHED") && event.contains(&transfer_id) {
					if let Ok(value) = serde_json::from_str::<serde_json::Value>(&event) {
						let local_ref = &value["payload"]["local_ref"];
						if let Some(path) = local_ref["local_path"].as_str() {
							local_path = path.to_string();
						}
						bytes = local_ref["total_bytes"].as_u64().unwrap_or(0);
					}
					completed = true;
					break;
				}
				if event.contains("TRANSFER_FAILED") && event.contains(&transfer_id) {
					let code = serde_json::from_str::<serde_json::Value>(&event)
						.ok()
						.and_then(|v| v["payload"]["code"].as_str().map(ToString::to_string))
						.unwrap_or_else(|| "unknown".to_string());
					error = Some(format!("TRANSFER_FAILED: {code}"));
					break;
				}
			}
			Err(mpsc::RecvTimeoutError::Timeout) => {}
			Err(mpsc::RecvTimeoutError::Disconnected) => {
				error = Some("事件通道已关闭".to_string());
				break;
			}
		}
	}

	let elapsed_s = started.elapsed().as_secs_f64();
	if !completed && error.is_none() {
		error = Some(format!("等待 CONTENT_CACHED 超时（{transfer_timeout:?}）"));
	}

	// 5) 传输完成后做廉价校验（不计入 elapsed）：文件长度 + 固定前缀。
	let mut verified = false;
	if completed && !local_path.is_empty() {
		if let Ok(metadata) = fs::metadata(&local_path) {
			let len_ok = metadata.len() == expected_bytes;
			let prefix_ok = read_prefix(Path::new(&local_path), 32)
				.map(|prefix| prefix.starts_with(BLOB_PREFIX.as_bytes()))
				.unwrap_or(false);
			verified = len_ok && prefix_ok && (bytes == 0 || bytes == expected_bytes);
		}
	}

	PeerOutcome {
		index,
		meta_seen,
		meta_wait_s,
		transfer_id,
		completed,
		elapsed_s,
		bytes,
		local_path,
		verified,
		error,
	}
}

// ---------------------------------------------------------------------------
// 单轮测量
// ---------------------------------------------------------------------------

struct RunResult {
	k: usize,
	skipped: Option<String>,
	peers_ok: usize,
	wall_s: f64,
	aggregate_mib_s: f64,
	per_peer_mib_s: Vec<f64>,
	ingest: Option<Stats>,
	list_peers: Option<Stats>,
	disconnects: usize,
}

impl RunResult {
	fn skipped(k: usize, reason: String) -> Self {
		Self {
			k,
			skipped: Some(reason),
			peers_ok: 0,
			wall_s: 0.0,
			aggregate_mib_s: 0.0,
			per_peer_mib_s: Vec::new(),
			ingest: None,
			list_peers: None,
			disconnects: 0,
		}
	}
}

/// 构造固定长度的文本：头部用于 peer 侧完整性抽查，其余用 `x` 填充。
fn build_probe_text(run_tag: &str, size_bytes: usize, kind: &str) -> String {
	let header = format!("{BLOB_PREFIX} run={run_tag} kind={kind} bytes={size_bytes}\n");
	let mut text = String::with_capacity(size_bytes);
	if header.len() <= size_bytes {
		text.push_str(&header);
	}
	while text.len() < size_bytes {
		text.push('x');
	}
	text
}

/// 等待 hub 看到全部 peer 处于 Online；返回 (在线数, 缺失的 device_id)。
fn wait_all_online(hub: &Core, peer_ids: &[String], timeout: Duration) -> (usize, Vec<String>) {
	let deadline = Instant::now() + timeout;
	loop {
		let peers = hub.list_peers().unwrap_or_default();
		let missing: Vec<String> = peer_ids
			.iter()
			.filter(|id| {
				!peers
					.iter()
					.any(|p| p.device_id == **id && p.state == PeerConnectionState::Online)
			})
			.cloned()
			.collect();
		let online = peer_ids.len() - missing.len();
		if missing.is_empty() || Instant::now() >= deadline {
			return (online, missing);
		}
		thread::sleep(Duration::from_millis(200));
	}
}

fn run_one(k: usize, params: &ProbeParams, probe_root: &Path, run_seq: usize) -> RunResult {
	let run_tag = uuid::Uuid::new_v4().simple().to_string();
	let account_uid = format!("cbhubprobe-{run_tag}");
	let run_root = probe_root.join(format!("run{run_seq}-k{k}"));
	let _ = fs::create_dir_all(&run_root);

	let counters = Arc::new(EventCounters::default());

	// hub 的 device_id 字典序最小（cbhub- < cbnode-），由 peer 主动拨号。
	let hub_id = format!("cbhub-{run_tag}");
	let (hub_sink, _hub_rx) = make_sink(counters.clone());
	let hub = make_core(
		&hub_id,
		"hub",
		&account_uid,
		&run_root.join("hub"),
		hub_sink,
	);
	println!("[probe] K={k}: hub {hub_id} 已启动");

	let mut peers = Vec::with_capacity(k);
	let mut peer_rx = Vec::with_capacity(k);
	let mut peer_ids = Vec::with_capacity(k);
	for index in 0..k {
		let device_id = format!("cbnode{index}-{run_tag}");
		let (sink, rx) = make_sink(counters.clone());
		let peer = make_core(
			&device_id,
			&format!("peer{index}"),
			&account_uid,
			&run_root.join(format!("peer{index}")),
			sink,
		);
		peer_ids.push(device_id);
		peers.push(peer);
		peer_rx.push(rx);
	}

	let set = CoreSet {
		hub,
		peers,
		dirs: vec![run_root.clone()],
	};

	// 1) 等待全部 peer 与 hub 建立 Online 会话。
	let (online, missing) = wait_all_online(&set.hub, &peer_ids, params.connect_timeout);
	if online < k {
		let cleanup_ok = set.finish();
		let reason = format!("等待 {k} 个 peer Online 超时: 在线 {online}/{k}，缺失 {missing:?}");
		println!(
			"[probe] K={k}: {reason}；清理 {}",
			if cleanup_ok { "ok" } else { "失败" }
		);
		return RunResult::skipped(k, reason);
	}
	println!(
		"[probe] K={k}: {k}/{k} peers Online（等待上限 {:?}）",
		params.connect_timeout
	);

	// 2) hub 种入大对象并等待广播（peer 工作线程里等 ITEM_META_ADDED）。
	let size_bytes = params.file_mb * 1024 * 1024;
	let payload = build_probe_text(&run_tag, size_bytes, "blob");
	let seed_started = Instant::now();
	let seed_meta = match set.hub.ingest_local_copy(ClipboardSnapshot::Text {
		text_utf8: payload,
		ts_ms: now_ms(),
	}) {
		Ok(meta) => meta,
		Err(e) => {
			let cleanup_ok = set.finish();
			let reason = format!("hub 摄入 {size_bytes} B 大对象失败: {e}");
			println!(
				"[probe] K={k}: {reason}；清理 {}",
				if cleanup_ok { "ok" } else { "失败" }
			);
			return RunResult::skipped(k, reason);
		}
	};
	let seed_s = seed_started.elapsed().as_secs_f64();
	let item_id = seed_meta.item_id.clone();
	println!(
		"[probe] K={k}: 种入 {} MiB 对象 item_id={} sha={} 用时 {:.3}s",
		params.file_mb, item_id, seed_meta.content.sha256, seed_s
	);

	// 3) 启动 K 个 peer 工作线程，等待它们都拿到 meta 并到达闸门。
	let gate = Arc::new(StartGate::new());
	let mut handles: Vec<JoinHandle<PeerOutcome>> = Vec::with_capacity(k);
	let expected_bytes = size_bytes as u64;
	for (index, (peer, rx)) in set.peers.iter().cloned().zip(peer_rx).enumerate() {
		let job = PeerJob {
			index,
			core: peer,
			rx,
			item_id: item_id.clone(),
			expected_bytes,
			meta_timeout: params.meta_timeout,
			transfer_timeout: params.run_timeout,
			gate: gate.clone(),
		};
		match thread::Builder::new()
			.name(format!("hub-probe-peer-{index}"))
			.spawn(move || run_peer_job(job))
		{
			Ok(handle) => handles.push(handle),
			Err(e) => println!("[probe] K={k}: 无法启动 peer{index} 工作线程: {e}"),
		}
	}

	let worker_count = handles.len();
	let ready = gate.wait_ready(worker_count, params.meta_timeout + Duration::from_secs(10));
	if !ready {
		println!(
			"[probe] K={k}: 警告: 只有部分 peer 工作线程在超时内就绪（worker={worker_count}）"
		);
	}

	// 4) 监控线程也挂在同一闸门上，保证采样窗口与传输窗口对齐。
	let stop = Arc::new(AtomicBool::new(false));
	let ingest_handle = spawn_ingest_monitor(
		set.hub.clone(),
		gate.clone(),
		stop.clone(),
		run_tag.clone(),
		params.ingest_period,
	);
	let list_handle = spawn_list_monitor(
		set.hub.clone(),
		gate.clone(),
		stop.clone(),
		params.list_period,
	);

	// 5) 放行：K 个 peer 同时发起拉取，hub 同时持续本地摄入 + 控制面查询。
	let wall_started = Instant::now();
	gate.open();

	let mut outcomes = Vec::with_capacity(worker_count);
	for handle in handles {
		match handle.join() {
			Ok(outcome) => outcomes.push(outcome),
			Err(_) => println!("[probe] K={k}: 某个 peer 工作线程 panic"),
		}
	}
	let wall_s = wall_started.elapsed().as_secs_f64();

	stop.store(true, Ordering::Release);
	let ingest_run = ingest_handle.join().unwrap_or(IngestRun {
		samples_ms: Vec::new(),
		errors: 0,
	});
	let list_run = list_handle.join().unwrap_or(ListRun {
		samples_ms: Vec::new(),
		errors: 0,
	});

	let (disconnects, transfer_failed, core_errors) = counters.snapshot();

	// 6) 统计并打印本轮明细。
	let per_peer_mib_s: Vec<f64> = outcomes
		.iter()
		.map(|outcome| {
			if outcome.completed && outcome.elapsed_s > 0.0 {
				(expected_bytes as f64 / (1024.0 * 1024.0)) / outcome.elapsed_s
			} else {
				0.0
			}
		})
		.collect();
	let peers_ok = outcomes.iter().filter(|outcome| outcome.completed).count();
	let total_bytes: u64 = if peers_ok == 0 {
		0
	} else {
		expected_bytes * peers_ok as u64
	};
	let aggregate_mib_s = if wall_s > 0.0 {
		(total_bytes as f64 / (1024.0 * 1024.0)) / wall_s
	} else {
		0.0
	};
	let ingest_stats = Stats::from_ms(ingest_run.samples_ms.clone());
	let list_stats = Stats::from_ms(list_run.samples_ms.clone());

	println!(
		"[probe] K={k}: 完成 {peers_ok}/{k}；wall={wall_s:.3}s；聚合 {aggregate_mib_s:.2} MiB/s"
	);
	for outcome in &outcomes {
		let status = if outcome.completed { "ok" } else { "FAIL" };
		let rate = per_peer_mib_s.get(outcome.index).copied().unwrap_or(0.0);
		println!(
			"[probe]   peer{}: {status} elapsed={:.3}s rate={rate:.2} MiB/s bytes={} verified={} meta_seen={} meta_wait={:.3}s tid={} path={} err={}",
			outcome.index,
			outcome.elapsed_s,
			outcome.bytes,
			outcome.verified,
			outcome.meta_seen,
			outcome.meta_wait_s,
			if outcome.transfer_id.is_empty() {
				"-"
			} else {
				&outcome.transfer_id
			},
			if outcome.local_path.is_empty() {
				"-"
			} else {
				&outcome.local_path
			},
			outcome.error.as_deref().unwrap_or("-")
		);
	}
	if let Some(stats) = &ingest_stats {
		println!(
			"[probe]   ingest_local_copy: n={} mean={:.3} p50={:.3} p95={:.3} p99={:.3} max={:.3} ms (errors={})",
			stats.n,
			stats.mean,
			stats.p50,
			stats.p95,
			stats.p99,
			stats.max,
			ingest_run.errors
		);
	} else {
		println!(
			"[probe]   ingest_local_copy: 无样本 (errors={})",
			ingest_run.errors
		);
	}
	if let Some(stats) = &list_stats {
		println!(
			"[probe]   list_peers: n={} mean={:.3} p50={:.3} p95={:.3} p99={:.3} max={:.3} ms (errors={})",
			stats.n,
			stats.mean,
			stats.p50,
			stats.p95,
			stats.p99,
			stats.max,
			list_run.errors
		);
	} else {
		println!("[probe]   list_peers: 无样本 (errors={})", list_run.errors);
	}
	println!(
		"[probe]   events: PEER_OFFLINE={disconnects} TRANSFER_FAILED={transfer_failed} CORE_ERROR={core_errors}"
	);

	// 7) 关闭本轮所有 Core 并清理目录。
	let cleanup_ok = set.finish();
	println!(
		"[probe] K={k}: 关闭/清理 {}",
		if cleanup_ok {
			"ok"
		} else {
			"失败（见上方路径）"
		}
	);

	RunResult {
		k,
		skipped: None,
		peers_ok,
		wall_s,
		aggregate_mib_s,
		per_peer_mib_s,
		ingest: ingest_stats,
		list_peers: list_stats,
		disconnects,
	}
}

// ---------------------------------------------------------------------------
// 表格与解读
// ---------------------------------------------------------------------------

fn print_run_row(result: &RunResult) {
	if let Some(reason) = &result.skipped {
		println!(
			"{:<3} | {:<9} | {:>7} | {:>12} | {:>14} | {:>28} | {:>21} | {:>11}",
			result.k, "skipped", "-", "-", "-", "-", "-", "-"
		);
		println!("    reason: {reason}");
		return;
	}

	let peers = format!("{}/{}", result.peers_ok, result.k);
	let per_peer_mean = if result.per_peer_mib_s.is_empty() {
		0.0
	} else {
		result.per_peer_mib_s.iter().sum::<f64>() / result.per_peer_mib_s.len() as f64
	};
	let ingest = result
		.ingest
		.as_ref()
		.map_or_else(|| "-".to_string(), Stats::compact);
	let list = result.list_peers.as_ref().map_or_else(
		|| "-".to_string(),
		|stats| {
			let (p99, max) = (stats.p99, stats.max);
			format!("{p99:.2}/{max:.2}")
		},
	);

	println!(
		"{:<3} | {:<9} | {:>7.3} | {:>12.2} | {:>14.2} | {:>28} | {:>21} | {:>11}",
		result.k,
		peers,
		result.wall_s,
		result.aggregate_mib_s,
		per_peer_mean,
		ingest,
		list,
		result.disconnects
	);
}

fn print_table(results: &[RunResult]) {
	println!();
	println!("=== hub concurrency probe: K sweep ===");
	println!(
		"{:<3} | {:<9} | {:>7} | {:>12} | {:>14} | {:>28} | {:>21} | {:>11}",
		"K",
		"peers",
		"wall_s",
		"total_MiB/s",
		"per_peer_MiB/s",
		"ingest n/p50/p95/p99/max ms",
		"list_peers p99/max ms",
		"disconnects"
	);
	println!("{}", "-".repeat(137));
	for result in results {
		print_run_row(result);
	}
	println!("{}", "-".repeat(137));
	println!(
		"[probe] 注: peers=成功/请求；per_peer_MiB/s 是各成功 peer 的均值（逐 peer 数值见上方明细）；disconnects=本轮所有 Core 的 PEER_OFFLINE 事件总数。"
	);
}

/// 比较第一个与最后一个有有效传输的 K，给出“哪个指标随 K 退化、退化多少”的说明。
fn print_interpretation(results: &[RunResult]) {
	println!();
	println!("[probe] interpretation（测量值，不是断言；debug 构建请谨慎解读）");

	let measured: Vec<&RunResult> = results
		.iter()
		.filter(|result| result.skipped.is_none() && result.peers_ok > 0)
		.collect();
	if measured.len() < 2 {
		println!(
			"[probe]   只有 {} 个有效 K 轮次，无法给出缩放趋势；请检查 mDNS/防火墙或提高 CB_HUB_TOTAL_SECS。",
			measured.len()
		);
		return;
	}

	let base = measured[0];
	let last = measured[measured.len() - 1];
	let k_base = base.k as f64;
	let k_last = last.k as f64;

	if base.aggregate_mib_s > 0.0 {
		let scale = last.aggregate_mib_s / base.aggregate_mib_s;
		let ideal = k_last / k_base;
		println!(
			"[probe]   聚合吞吐: K={} {:.2} MiB/s -> K={} {:.2} MiB/s（×{scale:.2}；线性理想为 ×{ideal:.2}，效率 {:.0}%）",
			base.k,
			base.aggregate_mib_s,
			last.k,
			last.aggregate_mib_s,
			scale / ideal * 100.0
		);
	} else {
		println!(
			"[probe]   聚合吞吐: K={} 轮次没有成功传输，无法比较",
			base.k
		);
	}

	let metric_ratio = |a: f64, b: f64| -> Option<f64> {
		if a > 0.0 {
			Some(b / a)
		} else {
			None
		}
	};

	let ingest_ratio = match (base.ingest, last.ingest) {
		(Some(a), Some(b)) => {
			let ratio = metric_ratio(a.p99, b.p99);
			println!(
				"[probe]   本地摄入 p99: K={} {:.2} ms -> K={} {:.2} ms{}",
				base.k,
				a.p99,
				last.k,
				b.p99,
				ratio.map_or_else(String::new, |r| format!("（×{r:.2}）"))
			);
			ratio
		}
		_ => {
			println!("[probe]   本地摄入: 至少一轮没有样本，无法比较");
			None
		}
	};

	let control_ratio = match (base.list_peers, last.list_peers) {
		(Some(a), Some(b)) => {
			let ratio = metric_ratio(a.p99, b.p99);
			println!(
				"[probe]   list_peers p99: K={} {:.2} ms -> K={} {:.2} ms{}",
				base.k,
				a.p99,
				last.k,
				b.p99,
				ratio.map_or_else(String::new, |r| format!("（×{r:.2}）"))
			);
			ratio
		}
		_ => {
			println!("[probe]   list_peers: 至少一轮没有样本，无法比较");
			None
		}
	};

	println!(
		"[probe]   断连事件: K={} PEER_OFFLINE={} -> K={} PEER_OFFLINE={}",
		base.k, base.disconnects, last.k, last.disconnects
	);

	match (ingest_ratio, control_ratio) {
		(Some(ingest), Some(control)) if ingest > control * 1.5 && ingest > 1.5 => {
			println!(
				"[probe]   观察: 本地摄入 p99 的增长（×{ingest:.2}）明显快于控制面（×{control:.2}），"
			);
			println!(
				"[probe]         说明剩余瓶颈更偏向 hub 的本地摄入/DB 路径，而不是 list_peers 的命令循环。"
			);
		}
		(Some(ingest), Some(control)) if control > ingest * 1.5 && control > 1.5 => {
			println!(
				"[probe]   观察: list_peers p99 的增长（×{control:.2}）明显快于本地摄入（×{ingest:.2}），"
			);
			println!("[probe]         说明 NetManager 命令循环/控制面仍可能被传输路径干扰。");
		}
		_ => {
			println!(
				"[probe]   观察: 两项延迟都没有出现 >1.5× 的明显增长；结合吞吐缩放判断是否为网络/CPU 上限。"
			);
		}
	}
}

// ---------------------------------------------------------------------------
// 测试入口
// ---------------------------------------------------------------------------

#[test]
#[ignore = "runs a real multi-peer QUIC/mDNS hub scaling probe: CB_HUB_K=2 CB_HUB_FILE_MB=16 cargo test -p cb_core --test hub_concurrency_probe -- --ignored --nocapture"]
fn hub_concurrency_probe() {
	let overall_started = Instant::now();
	let params = load_params();
	print_banner(&params);

	let probe_root = unique_probe_root();
	let _ = fs::create_dir_all(&probe_root);
	println!("[probe] 临时根目录: {}", probe_root.display());

	let mut results: Vec<RunResult> = Vec::new();
	let mut run_seq = 0usize;
	let mut previous_wall_s: Option<f64> = None;

	for k in K_LADDER.iter().copied().filter(|k| *k <= params.max_k) {
		let elapsed_s = overall_started.elapsed().as_secs_f64();
		let budget_s = params.total_secs as f64;
		let remaining_s = budget_s - elapsed_s;

		if remaining_s < 5.0 {
			let reason = format!("总预算 {budget_s:.0}s 已用 {elapsed_s:.1}s，剩余不足以跑一轮");
			println!("[probe] 跳过 K={k}: {reason}");
			results.push(RunResult::skipped(k, reason));
			continue;
		}

		// 上一轮耗时过长时保守跳过后续 K：单轮预计 ≈ 上一轮 ×1.5 + 5s 固定开销。
		if let Some(previous) = previous_wall_s {
			let estimate = previous * 1.5 + 5.0;
			if elapsed_s + estimate > budget_s {
				let reason = format!(
					"上一轮 wall={previous:.1}s，预计本轮需 {estimate:.1}s，会超出总预算 {budget_s:.0}s"
				);
				println!("[probe] 跳过 K={k}: {reason}");
				results.push(RunResult::skipped(k, reason));
				continue;
			}
		}

		// 单轮等待上限夹在剩余预算内，避免一轮把总时长拖爆。
		let mut run_params = params.clone();
		run_params.run_timeout = Duration::from_secs_f64(
			params
				.run_timeout
				.as_secs_f64()
				.min((remaining_s - 5.0).max(5.0)),
		);

		run_seq += 1;
		println!(
			"[probe] ---- run {run_seq}: K={k}, {} MiB/blob, run_timeout={:?} ----",
			run_params.file_mb, run_params.run_timeout
		);
		let result = run_one(k, &run_params, &probe_root, run_seq);
		if result.skipped.is_none() {
			previous_wall_s = Some(result.wall_s);
		}
		results.push(result);
	}

	print_table(&results);
	print_interpretation(&results);

	// 各轮目录已在 CoreSet::finish 里删除；这里兜底删根目录。
	let root_cleanup_ok = remove_dir_all_retry(&probe_root, Duration::from_secs(3));
	println!(
		"[probe] 临时根目录清理: {} ({})",
		if root_cleanup_ok { "ok" } else { "FAILED" },
		probe_root.display()
	);
	println!(
		"[probe] 总墙钟 {:.2}s / 预算 {}s；probe completed",
		overall_started.elapsed().as_secs_f64(),
		params.total_secs
	);
}
