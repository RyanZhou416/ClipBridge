//! `cb_core` 运行时性能探针（集成测试，默认 `#[ignore]`）。
//!
//! 默认不跑，保证 `cargo test --workspace` 的速度与结果不变。显式运行：
//!
//! ```text
//! cargo test --release -p cb_core --test perf_probe -- --ignored --nocapture
//! ```
//!
//! 设计约定：
//! - 全部在**进程内**完成：没有 FFI、没有 shell、不读剪贴板、不连真实对端；
//! - 每个 section 使用独立的 `tempfile::TempDir`，结束时打印磁盘占用并清理；
//! - 每个操作先 warmup 再采 N 个样本，输出 n / mean / p50 / p95 / p99 / max；
//! - 时间统一用 `Instant`（墙钟），只在“空闲 CPU”一项读取进程级 CPU 时间；
//! - 数字是 release 构建下的本机观测值，不是跨机器基准，别写进 CI 断言。

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use cb_core::api::{AppConfig, Core, CoreConfig, CoreEventSink};
use cb_core::clipboard::{make_ingest_plan, ClipboardSnapshot, LocalIngestDeps};
use cb_core::model::{ItemContent, ItemKind, ItemMeta, ItemPreview};
use cb_core::policy::SizeLimits;
use cb_core::util::{now_ms, sha256_hex};
use serde::Deserialize;

const ACCOUNT_UID: &str = "perf-probe-account";
const DEVICE_ID: &str = "perf-probe-device";
const DEVICE_NAME: &str = "perf-probe";

// 各 section 的样本数 / warmup 数。刻意写死，避免“跑一次就变一个配置”。
const INIT_SAMPLES: usize = 3;
const JSON_120B_SAMPLES: usize = 500;
const JSON_64K_SAMPLES: usize = 200;
const PLAN_PURE_SAMPLES: usize = 1_000;
const PLAN_API_SAMPLES: usize = 500;
const HISTORY_SAMPLES: usize = 100;
const META_SAMPLES: usize = 100;
const TEXT_120B_SAMPLES: usize = 200;
const TEXT_64K_SAMPLES: usize = 50;
const IMAGE_SAMPLES: usize = 20;
const CAS_SAMPLES: usize = 10;
const IDLE_WINDOW: Duration = Duration::from_secs(5);

const TEXT_120B_BYTES: usize = 120;
const TEXT_64K_BYTES: usize = 64 * 1024;
// 192 KiB 原始字节 base64 后正好 256 KiB：模拟“256 KiB base64”的图片快照。
const IMAGE_RAW_BYTES: usize = 192 * 1024;
const IMAGE_WIRE_BASE64_BYTES: usize = 256 * 1024;
const CAS_BLOB_BYTES: usize = 1024 * 1024;
const HISTORY_BASELINE_ROWS: usize = 100;
const HISTORY_TARGET_ROWS: usize = 1_000;

struct NoopSink;

impl CoreEventSink for NoopSink {
	fn emit(&self, _event_json: String) {}
}

fn make_core(root: &Path, disable_network: bool) -> Core {
	let cfg = CoreConfig {
		device_id: DEVICE_ID.to_string(),
		device_name: DEVICE_NAME.to_string(),
		account_uid: ACCOUNT_UID.to_string(),
		account_password: "perf-probe-password".to_string(),
		data_dir: root.join("data").to_string_lossy().into_owned(),
		cache_dir: root.join("cache").to_string_lossy().into_owned(),
		app_config: AppConfig {
			// 探针期间不允许 GC 因条数/容量删数据，否则耗时会被回收动作污染。
			gc_history_max_items: 50_000,
			gc_cas_max_bytes: 1_i64 << 60,
			..Default::default()
		},
		disable_network,
	};

	Core::init(cfg, Arc::new(NoopSink))
}

/// 一次测量的描述性统计。样本单位是微秒（µs）。
struct Stats {
	n: usize,
	mean_us: f64,
	p50_us: f64,
	p95_us: f64,
	p99_us: f64,
	max_us: f64,
}

impl Stats {
	fn from_samples(mut samples: Vec<f64>) -> Self {
		assert!(!samples.is_empty(), "at least one sample is required");
		samples.sort_by(f64::total_cmp);

		let n = samples.len();
		let mean_us = samples.iter().sum::<f64>() / n as f64;
		let max_us = samples[n - 1];
		// 最近秩（nearest-rank）：idx = ceil(q*n)-1，再夹到 [0, n-1]。
		// 小样本（如 init n=3、CAS n=10）时 p99 会等于 max，这是预期行为。
		let percentile = |q: f64| {
			let idx = ((q * n as f64).ceil() as usize)
				.saturating_sub(1)
				.min(n - 1);
			samples[idx]
		};

		Self {
			n,
			mean_us,
			p50_us: percentile(0.50),
			p95_us: percentile(0.95),
			p99_us: percentile(0.99),
			max_us,
		}
	}
}

fn print_table_header() {
	println!();
	println!(
		"{:<42} {:>4} {:>12} {:>12} {:>12} {:>12} {:>12} {:>4}",
		"section", "n", "mean", "p50", "p95", "p99", "max", "unit"
	);
	println!("{}", "-".repeat(117));
}

fn print_row(label: &str, stats: &Stats) {
	// 均值 < 1 ms 用 µs，否则用 ms；同一行内单位统一，避免混读。
	let (factor, unit) = if stats.mean_us >= 1_000.0 {
		(0.001, "ms")
	} else {
		(1.0, "µs")
	};

	let n = stats.n;
	let mean = stats.mean_us * factor;
	let p50 = stats.p50_us * factor;
	let p95 = stats.p95_us * factor;
	let p99 = stats.p99_us * factor;
	let max = stats.max_us * factor;

	println!(
		"{label:<42} {n:>4} {mean:>12.3} {p50:>12.3} {p95:>12.3} {p99:>12.3} {max:>12.3} {unit:>4}"
	);
}

/// 先跑 warmup 次，再对 N 次操作分别计时（单位 µs）。
fn sample_us(n: usize, warmup: usize, mut op: impl FnMut()) -> Stats {
	for _ in 0..warmup {
		op();
	}

	let mut samples = Vec::with_capacity(n);
	for _ in 0..n {
		let started = Instant::now();
		op();
		samples.push(started.elapsed().as_nanos() as f64 / 1_000.0);
	}

	Stats::from_samples(samples)
}

/// 生成可复现、互不相同的文本 payload（长度约 `target_len` 字节）。
/// 唯一性是必要的：`ingest_local_copy` 会对相同内容去重，重复 payload 会让
/// CAS 写入路径变成 no-op，把“摄入”测成“查重”。
fn text_payload(tag: &str, i: usize, target_len: usize) -> String {
	let mut s = format!("cb-perf {tag} sample {i:05} | ");
	while s.len() < target_len {
		s.push('x');
	}
	s
}

/// 与 `cb_core_ffi::marshal::parse_snapshot` 的 text 分支同形的 wire JSON。
/// 注意：`parse_snapshot` 属于 `cb_core_ffi`，`cb_core` 的公开 API 只接收强类型
/// 的 `ClipboardSnapshot`，所以这里由探针自己构造/解析同样的 JSON，用于把
/// “JSON + marshal”成本与 DB 路径分开观测。
fn text_snapshot_json(ts_ms: i64, text: &str) -> String {
	serde_json::json!({
		"type": "ClipboardSnapshot",
		"ts_ms": ts_ms,
		"kind": "text",
		"text": { "mime": "text/plain", "utf8": text }
	})
	.to_string()
}

#[derive(Deserialize)]
struct TextSnapshotJson {
	#[serde(rename = "type")]
	ty: String,
	ts_ms: i64,
	kind: String,
	text: TextBodyJson,
}

#[derive(Deserialize)]
struct TextBodyJson {
	// `mime` 在 cb_core 侧不参与快照构建，但 FFI 会把它反序列化成 String；
	// 保留该字段让探针的 JSON 成本接近真实入口。
	#[allow(dead_code)]
	#[serde(default)]
	mime: Option<String>,
	utf8: String,
}

fn parse_text_snapshot(json: &str) -> ClipboardSnapshot {
	let dto: TextSnapshotJson = serde_json::from_str(json).expect("parse text snapshot json");
	assert_eq!(dto.ty, "ClipboardSnapshot");
	assert_eq!(dto.kind, "text");

	ClipboardSnapshot::Text {
		text_utf8: dto.text.utf8,
		ts_ms: dto.ts_ms,
	}
}

/// 构造一条历史行（仅元数据，不写 CAS），用于把 list_history 的规模撑起来。
fn seed_meta(i: usize, created_ts_ms: i64) -> ItemMeta {
	let payload = text_payload("seed", i, TEXT_120B_BYTES);
	let bytes = payload.as_bytes();

	ItemMeta {
		ty: "ItemMeta".to_string(),
		item_id: format!("perf-seed-{i:06}"),
		kind: ItemKind::Text,
		created_ts_ms,
		source_device_id: DEVICE_ID.to_string(),
		source_device_name: Some(DEVICE_NAME.to_string()),
		size_bytes: bytes.len() as i64,
		preview: ItemPreview {
			text: Some(payload.chars().take(300).collect()),
			..Default::default()
		},
		content: ItemContent {
			mime: "text/plain".to_string(),
			sha256: sha256_hex(bytes),
			total_bytes: bytes.len() as i64,
		},
		files: Vec::new(),
		expires_ts_ms: Some(created_ts_ms + 7 * 24 * 3600 * 1000),
	}
}

/// 直接走 store 公开 API 批量造历史行：这里只测“查询随行长”的成本，
/// 不把 1000 次 ingest 的 CAS/GC 成本混进 list_history。
fn seed_history(core: &Core, start: usize, count: usize, base_ts: i64) {
	let now = now_ms();
	let mut store = core.inner.store.lock().expect("store lock");

	for i in start..start + count {
		let meta = seed_meta(i, base_ts + i as i64);
		store
			.insert_meta_and_history(ACCOUNT_UID, &meta, now)
			.expect("seed history insert");
	}
}

fn ingest_stats(
	core: &Core,
	mut snaps: impl Iterator<Item = ClipboardSnapshot>,
	warmup: usize,
	n: usize,
) -> Stats {
	for _ in 0..warmup {
		core.ingest_local_copy(snaps.next().expect("warmup snapshot"))
			.expect("warmup ingest");
	}

	let mut samples = Vec::with_capacity(n);
	for _ in 0..n {
		let started = Instant::now();
		let meta = core
			.ingest_local_copy(snaps.next().expect("snapshot"))
			.expect("ingest");
		samples.push(started.elapsed().as_nanos() as f64 / 1_000.0);
		// 防止结果被完全忽略；读一下字段即可，成本可忽略。
		std::hint::black_box(meta.size_bytes);
	}

	Stats::from_samples(samples)
}

/// 1 MiB 的 CAS 样本，每个样本内容不同：`put_blob` 命中相同 sha 会直接返回，
/// 那样“写”就测不到了。
fn cas_payload(i: usize, len: usize) -> Vec<u8> {
	let mut data = vec![0u8; len];
	data[..8].copy_from_slice(&(i as u64).to_le_bytes());
	for (j, b) in data.iter_mut().enumerate().skip(8) {
		*b = ((j.wrapping_mul(31) ^ i) & 0xff) as u8;
	}
	data
}

fn image_payload(i: usize, len: usize) -> Vec<u8> {
	cas_payload(i, len)
}

fn dir_size_bytes(path: &Path) -> u64 {
	let Ok(metadata) = fs::symlink_metadata(path) else {
		return 0;
	};
	if metadata.is_file() {
		return metadata.len();
	}
	let Ok(entries) = fs::read_dir(path) else {
		return 0;
	};

	let mut total = 0;
	for entry in entries.flatten() {
		total += dir_size_bytes(&entry.path());
	}
	total
}

/// Windows 删除目录可能撞上还没彻底关闭的 SQLite/CAS 句柄，重试几次。
fn remove_dir_all_retry(path: &Path, max_wait: Duration) -> bool {
	let started = Instant::now();
	loop {
		match fs::remove_dir_all(path) {
			Ok(()) => return true,
			Err(_) if started.elapsed() < max_wait => {
				std::thread::sleep(Duration::from_millis(50));
			}
			Err(_) => return false,
		}
	}
}

/// 进程级 CPU 时间（kernel + user）。Windows 用 `GetProcessTimes`；其他平台
/// 没有引入依赖（本任务明确禁止新增依赖），直接跳过该子测量。
#[cfg(windows)]
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

	/// 返回本进程累计 CPU 时间（单位 100ns）。失败时返回 `None`。
	pub(super) fn total_100ns() -> Option<u64> {
		let mut creation = FileTime::default();
		let mut exit = FileTime::default();
		let mut kernel = FileTime::default();
		let mut user = FileTime::default();

		// SAFETY: GetCurrentProcess 返回本进程伪句柄；其余参数都是指向栈上
		// 已初始化 FILETIME 的有效可变指针，调用期间保持有效。
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

#[cfg(not(windows))]
mod process_cpu {
	pub(super) fn total_100ns() -> Option<u64> {
		None
	}
}

/// 在 `window` 内 sleep，并返回窗口内消耗的 CPU：
/// `(cpu_ms, percent_of_one_core, actual_elapsed)`。
fn measure_idle_cpu(window: Duration) -> Option<(f64, f64, Duration)> {
	let before = process_cpu::total_100ns()?;
	let started = Instant::now();
	std::thread::sleep(window);
	let elapsed = started.elapsed();
	let after = process_cpu::total_100ns()?;

	let cpu_ms = after.saturating_sub(before) as f64 / 10_000.0; // 1 ms = 10_000 * 100ns
	let percent = cpu_ms / elapsed.as_secs_f64() / 1_000.0 * 100.0;

	Some((cpu_ms, percent, elapsed))
}

fn print_idle_cpu(label: &str, measured: Option<(f64, f64, Duration)>) {
	match measured {
		Some((cpu_ms, percent, elapsed)) => {
			let seconds = elapsed.as_secs_f64();
			let ms_per_s = cpu_ms / seconds;
			println!(
				"[perf]   {label}: {cpu_ms:.3} ms CPU over {seconds:.3} s = {ms_per_s:.3} ms/s = {percent:.4} % of one core"
			);
		}
		None => {
			println!("[perf]   {label}: skipped (process CPU time unavailable on this platform)")
		}
	}
}

#[test]
#[ignore = "runs the real runtime perf probe: cargo test --release -p cb_core --test perf_probe -- --ignored --nocapture"]
fn perf_probe() {
	let probe_started = Instant::now();
	let mut rows: Vec<(String, Stats)> = Vec::new();

	println!();
	println!("=== cb_core runtime perf probe ===");
	println!(
		"build: {} | os: {} {} | logical cpus: {}",
		if cfg!(debug_assertions) {
			"debug (numbers are NOT representative; use --release)"
		} else {
			"release"
		},
		std::env::consts::OS,
		std::env::consts::ARCH,
		std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get)
	);
	println!();

	// ------------------------------------------------------------------
	// 1) init：3 个全新目录，disable_network=true。
	// 包含 logs.db/core.db 打开、schema 检查、data/cache 目录创建、CAS 目录
	// 创建、启动对账与启动 GC；不包含网络证书生成（网络关闭）。
	// init 没有“warmup”概念：每次都换新目录，本身就是冷启动观测。
	// ------------------------------------------------------------------
	println!("[perf] 1/9 init: {INIT_SAMPLES} fresh data dirs (disable_network=true)");
	let mut init_samples = Vec::with_capacity(INIT_SAMPLES);
	for i in 0..INIT_SAMPLES {
		let dir = tempfile::tempdir().expect("init tempdir");
		let started = Instant::now();
		let core = make_core(dir.path(), true);
		let elapsed_us = started.elapsed().as_nanos() as f64 / 1_000.0;
		init_samples.push(elapsed_us);

		core.shutdown();
		drop(core);
		drop(dir);
		println!(
			"[perf]   init sample {}: {:.3} ms",
			i + 1,
			elapsed_us / 1_000.0
		);
	}
	rows.push((
		"core_init (fresh dir, net off)".to_string(),
		Stats::from_samples(init_samples),
	));

	// 后续所有 DB/CAS 测量共用这个核心（网络关闭）。
	let main_dir = tempfile::tempdir().expect("main tempdir");
	let main_core = make_core(main_dir.path(), true);
	let ts_base = now_ms();
	println!("[perf] main core ready at {}", main_dir.path().display());

	// ------------------------------------------------------------------
	// 2) JSON + plan：纯内存路径，与 DB 路径分开。
	// - JSON：解析与 FFI 同形的 text 快照（含 String 分配）；
	// - make_ingest_plan：纯函数（UUID + SHA-256 + 策略判断）；
	// - plan_local_ingest：Core API（多了 logs.db 写入，但不碰 store/CAS）。
	// ------------------------------------------------------------------
	println!("[perf] 2/9 pure JSON / plan cost");
	let small_text = text_payload("json120", 0, TEXT_120B_BYTES);
	let small_json = text_snapshot_json(ts_base, &small_text);
	let large_text = text_payload("json64k", 0, TEXT_64K_BYTES);
	let large_json = text_snapshot_json(ts_base, &large_text);

	rows.push((
		"snapshot_json_parse (text ~120 B)".to_string(),
		sample_us(JSON_120B_SAMPLES, 20, || {
			std::hint::black_box(parse_text_snapshot(&small_json));
		}),
	));
	rows.push((
		"snapshot_json_parse (text 64 KiB)".to_string(),
		sample_us(JSON_64K_SAMPLES, 10, || {
			std::hint::black_box(parse_text_snapshot(&large_json));
		}),
	));

	let plan_snapshot = ClipboardSnapshot::Text {
		text_utf8: small_text.clone(),
		ts_ms: ts_base,
	};
	let deps = LocalIngestDeps {
		device_id: DEVICE_ID,
		device_name: DEVICE_NAME,
		account_uid: ACCOUNT_UID,
	};
	let limits = SizeLimits::default();
	rows.push((
		"make_ingest_plan (text ~120 B, pure)".to_string(),
		sample_us(PLAN_PURE_SAMPLES, 50, || {
			let plan = make_ingest_plan(&deps, &plan_snapshot, &limits, false).expect("make plan");
			std::hint::black_box(plan.meta.item_id.len());
		}),
	));
	rows.push((
		"plan_local_ingest (text ~120 B, logs.db)".to_string(),
		sample_us(PLAN_API_SAMPLES, 20, || {
			let plan = main_core
				.plan_local_ingest(&plan_snapshot, false)
				.expect("plan_local_ingest");
			std::hint::black_box(plan.meta.item_id.len());
		}),
	));

	// ------------------------------------------------------------------
	// 3) list_history，先测 ~100 行。
	// 历史行直接走 store API 造（不含 CAS），只观察查询/JSON 反序列化成本。
	// ------------------------------------------------------------------
	println!("[perf] 3/9 list_history with ~{HISTORY_BASELINE_ROWS} rows");
	let seed_base_ts = now_ms() - 1_000_000;
	seed_history(&main_core, 0, HISTORY_BASELINE_ROWS, seed_base_ts);

	let mut history_checksum = 0usize;
	rows.push((
		"list_history limit=100 @ 100 rows".to_string(),
		sample_us(HISTORY_SAMPLES, 20, || {
			let list = main_core
				.list_history(100, None)
				.expect("list_history 100 rows");
			history_checksum = history_checksum.wrapping_add(list.len());
		}),
	));
	assert!(history_checksum > 0, "list_history returned no rows");

	// ------------------------------------------------------------------
	// 4) get_item_meta：主键直查（O(1)），N=100。
	// ------------------------------------------------------------------
	println!("[perf] 4/9 get_item_meta (known id)");
	let known_item_id = "perf-seed-000000";
	assert!(
		main_core
			.get_item_meta(known_item_id)
			.expect("get_item_meta")
			.is_some(),
		"known item must exist"
	);

	let mut meta_checksum = 0usize;
	rows.push((
		"get_item_meta (known id)".to_string(),
		sample_us(META_SAMPLES, 20, || {
			let meta = main_core
				.get_item_meta(known_item_id)
				.expect("get_item_meta");
			meta_checksum = meta_checksum.wrapping_add(usize::from(meta.is_some()));
		}),
	));
	assert!(meta_checksum > 0, "get_item_meta never returned Some");

	// ------------------------------------------------------------------
	// 5) ingest 三种尺寸：120 B 文本 / 64 KiB 文本 / 256 KiB base64 图片。
	// 输入的 JSON 解析在计时外完成（真实 FFI 入口也是先 parse 再调 ingest）；
	// ingest 计时包含：plan、SQLite 事务、CAS 写入/去重、事件 emit，
	// 以及 ingest_local_copy 内部 `run_gc("AfterIngest")` 的账实核算
	// （历史计数 + CAS 目录遍历）。这就是当前公开 API 的真实成本。
	// ------------------------------------------------------------------
	println!("[perf] 5/9 ingest: text 120 B / text 64 KiB / image 256 KiB(b64)");

	let text_120_snaps: Vec<ClipboardSnapshot> = (0..TEXT_120B_SAMPLES + 10)
		.map(|i| {
			let json = text_snapshot_json(
				ts_base + i as i64,
				&text_payload("ingest120", i, TEXT_120B_BYTES),
			);
			parse_text_snapshot(&json)
		})
		.collect();
	rows.push((
		"ingest text 120 B".to_string(),
		ingest_stats(
			&main_core,
			text_120_snaps.into_iter(),
			10,
			TEXT_120B_SAMPLES,
		),
	));

	let text_64k_snaps: Vec<ClipboardSnapshot> = (0..TEXT_64K_SAMPLES + 5)
		.map(|i| {
			let json = text_snapshot_json(
				ts_base + i as i64,
				&text_payload("ingest64k", i, TEXT_64K_BYTES),
			);
			parse_text_snapshot(&json)
		})
		.collect();
	rows.push((
		"ingest text 64 KiB".to_string(),
		ingest_stats(&main_core, text_64k_snaps.into_iter(), 5, TEXT_64K_SAMPLES),
	));

	let image_snaps: Vec<ClipboardSnapshot> = (0..IMAGE_SAMPLES + 3)
		.map(|i| {
			let raw = image_payload(i, IMAGE_RAW_BYTES);
			// 校验 wire 尺寸：256 KiB base64 对应 192 KiB 原始字节。
			assert_eq!(BASE64.encode(&raw).len(), IMAGE_WIRE_BASE64_BYTES);
			ClipboardSnapshot::Image {
				bytes: raw,
				mime: "image/png".to_string(),
				ts_ms: ts_base + i as i64,
			}
		})
		.collect();
	rows.push((
		"ingest image 192 KiB (256 KiB b64)".to_string(),
		ingest_stats(&main_core, image_snaps.into_iter(), 3, IMAGE_SAMPLES),
	));

	// ------------------------------------------------------------------
	// 6) 把历史行补到 1000，再测一次同样的查询。
	// 这里用 store API 补齐（任务允许），避免把 600+ 次 ingest 的 CAS/GC
	// 成本掺进“查询随行数”的对比。
	// ------------------------------------------------------------------
	let rows_before_pad = usize::try_from(
		main_core
			.inner
			.store
			.lock()
			.expect("store lock")
			.history_count_for_account(ACCOUNT_UID)
			.expect("history count"),
	)
	.expect("history count fits usize");
	let pad = HISTORY_TARGET_ROWS.saturating_sub(rows_before_pad);
	seed_history(&main_core, HISTORY_BASELINE_ROWS, pad, seed_base_ts);

	let rows_after_pad = main_core
		.inner
		.store
		.lock()
		.expect("store lock")
		.history_count_for_account(ACCOUNT_UID)
		.expect("history count");
	assert_eq!(
		usize::try_from(rows_after_pad).expect("row count"),
		HISTORY_TARGET_ROWS,
		"history should be padded to the target row count"
	);
	println!(
		"[perf] 6/9 list_history after padding {rows_before_pad} -> {HISTORY_TARGET_ROWS} rows"
	);

	let mut history_1000_checksum = 0usize;
	rows.push((
		"list_history limit=100 @ 1000 rows".to_string(),
		sample_us(HISTORY_SAMPLES, 20, || {
			let list = main_core
				.list_history(100, None)
				.expect("list_history 1000 rows");
			history_1000_checksum = history_1000_checksum.wrapping_add(list.len());
		}),
	));
	assert!(
		history_1000_checksum > 0,
		"list_history 1000 rows returned no rows"
	);

	// ------------------------------------------------------------------
	// 7) CAS：1 MiB 写入 / 读回，各 N=10；外加 run_gc 维护成本。
	// 每个样本内容不同，确保 put_blob 真的落盘（否则 dedup 会让写变成 0）。
	// 写包含 SHA-256 + tmp 文件 + rename；读是 blob_path + fs::read。
	// 读回的逐字节比较在计时外做。
	//
	// run_gc 单独测量：ingest_local_copy 每次结束都会调用它，而它的
	// `total_size_bytes()` 会遍历整个 CAS 目录。这一行用来解释 ingest 的
	// 固定开销，避免把“目录遍历”误读成“120 B 文本很慢”。
	// ------------------------------------------------------------------
	println!("[perf] 7/9 CAS 1 MiB write/read + run_gc maintenance");
	rows.push((
		"run_gc (1000 rows, ~288 blobs)".to_string(),
		sample_us(50, 5, || {
			main_core.run_gc("PerfProbe").expect("run_gc");
		}),
	));

	let cas_warmup = 2;
	let mut write_samples = Vec::with_capacity(CAS_SAMPLES);
	let mut read_samples = Vec::with_capacity(CAS_SAMPLES);

	for i in 0..CAS_SAMPLES + cas_warmup {
		let data = cas_payload(i, CAS_BLOB_BYTES);

		let started = Instant::now();
		let sha = main_core.inner.cas.put_blob(&data).expect("cas put_blob");
		let write_us = started.elapsed().as_nanos() as f64 / 1_000.0;

		let started = Instant::now();
		let path = main_core.inner.cas.blob_path(&sha).expect("cas blob_path");
		let got = fs::read(path.as_path()).expect("cas read blob");
		let read_us = started.elapsed().as_nanos() as f64 / 1_000.0;

		assert_eq!(got, data, "CAS readback content mismatch");

		if i >= cas_warmup {
			write_samples.push(write_us);
			read_samples.push(read_us);
		}
	}
	rows.push((
		"cas put_blob 1 MiB (incl. sha256)".to_string(),
		Stats::from_samples(write_samples),
	));
	rows.push((
		"cas read 1 MiB (blob_path + fs::read)".to_string(),
		Stats::from_samples(read_samples),
	));

	// ------------------------------------------------------------------
	// 8) 空闲 CPU：网络关闭 vs 开启，各一个 5 s 窗口。
	// 进程级 CPU（kernel+user）。先测“关闭”：此时进程里还没有任何长驻网络
	// 线程，基线最干净；再测“开启”（QUIC 监听 + mDNS 发布/浏览）。之后不再
	// 采样，所以 mDNS 的 ServiceDaemon 线程即便退出慢也不会污染基线。
	// 网络开启时探一次 list_peers，确认 NetManager 真的在运行（而不是 bind
	// 失败后线程提前退出）。
	// ------------------------------------------------------------------
	println!("[perf] 8/9 idle CPU (5 s windows)");

	let net_off_dir = tempfile::tempdir().expect("net-off tempdir");
	let net_off_core = make_core(net_off_dir.path(), true);
	std::thread::sleep(Duration::from_secs(1));
	print_idle_cpu("idle CPU network=DISABLED", measure_idle_cpu(IDLE_WINDOW));
	net_off_core.shutdown();
	drop(net_off_core);
	std::thread::sleep(Duration::from_millis(300));

	let net_on_dir = tempfile::tempdir().expect("net-on tempdir");
	let net_on_core = make_core(net_on_dir.path(), false);
	let status = net_on_core.get_status().expect("get_status");
	println!(
		"[perf]   net-enabled core: net_enabled={} status={}",
		status["net_enabled"], status["status"]
	);

	// 让 Transport/mDNS 初始化完成，避免把启动 CPU 算进“空闲”。
	std::thread::sleep(Duration::from_millis(500));
	match net_on_core.list_peers() {
		Ok(peers) => println!(
			"[perf]   NetManager alive, list_peers -> {} peer(s)",
			peers.len()
		),
		Err(e) => println!("[perf]   NetManager unavailable: {e}"),
	}
	std::thread::sleep(Duration::from_millis(500));

	print_idle_cpu("idle CPU network=ENABLED", measure_idle_cpu(IDLE_WINDOW));
	net_on_core.shutdown();
	drop(net_on_core);
	// 等 NetManager/mDNS 线程退出，再删除目录。
	std::thread::sleep(Duration::from_secs(1));

	// ------------------------------------------------------------------
	// 9) 磁盘占用 + 清理。
	// 只统计三个探针目录（init 的临时目录已经各自删除）。
	// ------------------------------------------------------------------
	println!("[perf] 9/9 disk footprint + cleanup");
	let roots = [
		("main", main_dir.path().to_path_buf()),
		("idle-net-on", net_on_dir.path().to_path_buf()),
		("idle-net-off", net_off_dir.path().to_path_buf()),
	];
	let mut total_bytes = 0u64;
	for (name, root) in &roots {
		let data_bytes = dir_size_bytes(&root.join("data"));
		let cache_bytes = dir_size_bytes(&root.join("cache"));
		total_bytes += data_bytes + cache_bytes;
		println!("[perf]   {name}: data={data_bytes} B cache={cache_bytes} B");
	}
	println!(
		"[perf]   total on-disk footprint: {total_bytes} B ({:.2} MiB)",
		total_bytes as f64 / (1024.0 * 1024.0)
	);

	main_core.shutdown();
	drop(main_core);
	drop(main_dir);
	drop(net_on_dir);
	drop(net_off_dir);
	std::thread::sleep(Duration::from_millis(300));

	for (name, root) in roots {
		let removed = !root.exists() || remove_dir_all_retry(&root, Duration::from_millis(3_000));
		println!(
			"[perf]   cleanup {name}: {}",
			if removed {
				"ok"
			} else {
				"FAILED (dir still exists)"
			}
		);
	}

	// ------------------------------------------------------------------
	// 汇总表
	// ------------------------------------------------------------------
	print_table_header();
	for (label, stats) in &rows {
		print_row(label, stats);
	}

	println!();
	println!(
		"[perf] total wall time: {:.2} s",
		probe_started.elapsed().as_secs_f64()
	);
	println!("[perf] caveats:");
	println!("[perf]   - ingest includes plan + SQLite tx + CAS write/dedup + emit + run_gc(AfterIngest)");
	println!("[perf]   - run_gc row quantifies that maintenance walk; subtract it to read the payload-only cost");
	println!("[perf]   - init includes logs.db/core.db open, schema check, dirs, startup reconcile/GC; no cert/net");
	println!(
		"[perf]   - list_history rows are seeded via store API (no CAS), so it isolates query cost"
	);
	println!("[perf]   - CAS writes use distinct payloads per sample (dedup would otherwise turn writes into no-ops)");
	println!("[perf]   - idle CPU is process-wide CPU time over a 5 s window after a 1 s settle");
}
