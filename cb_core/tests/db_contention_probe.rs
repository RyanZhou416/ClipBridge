//! `core.db` read/write contention probe（集成测试，默认 `#[ignore]`）。
//!
//! 背景：全仓库只有**一个** `Mutex<Store>`（内部一条 `rusqlite::Connection`），
//! FFI 查询、会话 actor 元数据、GC/维护路径都排队经过它。本探针用真实 `Store`
//! 在临时目录里测量这个设计在两类负载下的读延迟：
//!
//! 1. 常规并发：写线程每 ~5 ms 摄入一条远端元数据，读线程每 ~5 ms 执行
//!    `list_history_metas(100)`，持续 20 s；报告双方 n/p50/p95/p99/max 以及
//!    读线程等待 >1 ms / >10 ms / >50 ms 的次数（包含互斥锁等待）。
//! 2. 长事务：历史保留路径一次性删除 10 000 行 `history`（单条 DELETE 事务）时，
//!    读线程的 p99/max 与写线程耗时。
//!
//! 三个读侧配置：
//! - 1a：读线程单独运行（无写者）——查询本身成本；
//! - 1b：读线程与写者共享同一个 `Mutex<Store>`（Task A 基线，也是修复前线上形态）；
//! - 1c：读线程改走 `StoreReader` 只读连接池（Task B 对照，WAL 快照）。
//!
//! Section 2 同时跑“共享 Mutex”和“只读池”两个读线程，直接对比长事务影响。
//!
//! 这是“Task A 决定是否需要连接池、Task B 验证修复”的测量工具：
//! 决策规则是「并发写期间 reader p99 < 5 ms 且 max < 20 ms => 不需要连接池」。
//!
//! ```text
//! cargo test --release -p cb_core --test db_contention_probe -- --ignored --nocapture
//! ```
//!
//! 约定：
//! - 全部进程内、真实 SQLite（WAL / synchronous=NORMAL / busy_timeout=5000），
//!   没有 FFI/网络/CAS；
//! - 每个 section 独立的 `TempDir`，结束即清理；
//! - 只测量、不判定（除“跑完”外没有断言），数字是本机 release 观测值，
//!   不是跨机器基准。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cb_core::model::{ItemContent, ItemKind, ItemMeta, ItemPreview};
use cb_core::store::{Store, StoreReader};
use cb_core::util::{now_ms, sha256_hex};

const ACCOUNT_UID: &str = "contention-probe-account";
const DEVICE_ID: &str = "contention-probe-device";
/// 读/写两侧的目标间隔。
const PERIOD: Duration = Duration::from_millis(5);
/// 常规并发 section 的墙钟窗口。
const WINDOW: Duration = Duration::from_secs(20);
/// 后台读线程“空转”基线窗口（无写者）。
const BASELINE_WINDOW: Duration = Duration::from_secs(2);
/// 常规并发 section 的初始历史行数（对齐线上 1k 行量级）。
const BASE_HISTORY_ROWS: usize = 1_000;
/// 长事务 section 的初始历史行数：一次 purge 删除其中的 10 000 行。
const LONG_TX_HISTORY_ROWS: usize = 12_000;
const LONG_TX_PURGE_ROWS: i64 = 10_000;

/// 一次测量的描述性统计；样本单位是微秒（µs）。
struct Stats {
	n: usize,
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
		// 最近秩（nearest-rank）：idx = ceil(q*n)-1，再夹到 [0, n-1]。
		let percentile = |q: f64| {
			let idx = ((q * n as f64).ceil() as usize)
				.saturating_sub(1)
				.min(n - 1);
			samples[idx]
		};

		Self {
			n,
			p50_us: percentile(0.50),
			p95_us: percentile(0.95),
			p99_us: percentile(0.99),
			max_us: samples[n - 1],
		}
	}
}

fn dur_us(d: Duration) -> f64 {
	d.as_nanos() as f64 / 1_000.0
}

/// 把一组 `(延迟, 是否超过阈值...)` 汇总成一行表格。
fn print_row(label: &str, latencies: &[Duration]) {
	if latencies.is_empty() {
		println!("{label:<46} {:>5} (no samples)", 0);
		return;
	}
	let stats = Stats::from_samples(latencies.iter().copied().map(dur_us).collect());
	let over = |limit: Duration| latencies.iter().filter(|d| **d > limit).count();
	println!(
		"{label:<46} {:>5} {:>11.3} {:>11.3} {:>11.3} {:>11.3} | >1ms {:>5} >10ms {:>4} >50ms {:>3}",
		stats.n,
		stats.p50_us,
		stats.p95_us,
		stats.p99_us,
		stats.max_us,
		over(Duration::from_millis(1)),
		over(Duration::from_millis(10)),
		over(Duration::from_millis(50)),
	);
}

fn print_header() {
	println!();
	println!(
		"{:<46} {:>5} {:>11} {:>11} {:>11} {:>11} | {:<32}",
		"measurement", "n", "p50 µs", "p95 µs", "p99 µs", "max µs", "tail counts"
	);
	println!("{}", "-".repeat(120));
}

/// 一条可复现、内容互不相同的写样本（CAS 未参与，本探针只测 DB）。
fn write_meta(i: usize) -> ItemMeta {
	let payload = format!("cb contention probe write {i:08} {}", "x".repeat(48));
	let bytes = payload.as_bytes();
	ItemMeta {
		ty: "ItemMeta".to_string(),
		item_id: format!("probe-write-{i:08}"),
		kind: ItemKind::Text,
		created_ts_ms: i as i64,
		source_device_id: DEVICE_ID.to_string(),
		source_device_name: None,
		size_bytes: bytes.len() as i64,
		preview: ItemPreview {
			text: Some(payload.chars().take(64).collect()),
			..Default::default()
		},
		content: ItemContent {
			mime: "text/plain".to_string(),
			sha256: sha256_hex(bytes),
			total_bytes: bytes.len() as i64,
		},
		files: Vec::new(),
		expires_ts_ms: None,
	}
}

fn seed_history(store: &mut Store, count: usize, base_ts: i64) {
	let now = now_ms();
	for i in 0..count {
		let mut meta = write_meta(i);
		meta.item_id = format!("probe-seed-{i:08}");
		meta.created_ts_ms = base_ts + i as i64;
		store
			.insert_meta_and_history(ACCOUNT_UID, &meta, now)
			.expect("seed history row");
	}
}

/// 在 `stop` 变 true 前，按 `PERIOD` 节奏执行 `op` 并记录**含锁等待**的耗时。
fn run_timed_loop(stop: Arc<AtomicBool>, mut op: impl FnMut()) -> Vec<Duration> {
	let mut samples = Vec::new();
	let mut next = Instant::now();
	while !stop.load(Ordering::Acquire) {
		next += PERIOD;
		let started = Instant::now();
		op();
		samples.push(started.elapsed());

		let now = Instant::now();
		if next > now {
			thread::sleep(next - now);
		} else {
			// 落后于计划（例如等待长事务）：不补跑，直接重置节奏。
			next = now;
		}
	}
	samples
}

fn open_store(dir: &std::path::Path) -> Arc<Mutex<Store>> {
	Arc::new(Mutex::new(Store::open(dir).expect("open store")))
}

#[test]
#[ignore = "measures cb_core store read/write contention: cargo test --release -p cb_core --test db_contention_probe -- --ignored --nocapture"]
fn db_contention_probe() {
	println!();
	println!("=== core.db read/write contention probe ===");
	println!(
		"build: {} | os: {} {} | window: {:?} | cadence: {:?}",
		if cfg!(debug_assertions) {
			"debug (numbers are NOT representative; use --release)"
		} else {
			"release"
		},
		std::env::consts::OS,
		std::env::consts::ARCH,
		WINDOW,
		PERIOD
	);
	print_header();

	// ------------------------------------------------------------------
	// Section 1a：读线程单独运行（无写者）——基线，用于区分“查询本身”
	// 与“被写者/锁拖慢的部分”。
	// ------------------------------------------------------------------
	let dir = tempfile::tempdir().expect("tempdir");
	let store = open_store(dir.path());
	{
		let mut guard = store.lock().expect("store lock");
		seed_history(&mut guard, BASE_HISTORY_ROWS, now_ms() - 10_000_000);
	}
	println!(
		"[probe] 1a/4 reader alone for {BASELINE_WINDOW:?} ({BASE_HISTORY_ROWS} history rows)"
	);
	let stop = Arc::new(AtomicBool::new(false));
	let reader_store = Arc::clone(&store);
	let reader_stop = Arc::clone(&stop);
	let reader = thread::spawn(move || {
		run_timed_loop(reader_stop, || {
			let guard = reader_store.lock().expect("store lock");
			let metas = guard
				.list_history_metas(ACCOUNT_UID, 100)
				.expect("list_history_metas");
			std::hint::black_box(metas.len());
		})
	});
	thread::sleep(BASELINE_WINDOW);
	stop.store(true, Ordering::Release);
	let baseline = reader.join().expect("reader join");
	print_row("reader list_history(100), no writer", &baseline);

	// ------------------------------------------------------------------
	// Section 1b：写线程每 ~5 ms 摄入一条远端元数据，读线程每 ~5 ms 查询，
	// 持续 20 s。两侧都持有同一个 Mutex<Store>（当前线上结构）。
	// ------------------------------------------------------------------
	println!();
	println!(
		"[probe] 1b/4 writer insert_remote_item + reader list_history(100), {:?}",
		WINDOW
	);
	let stop = Arc::new(AtomicBool::new(false));

	let writer_store = Arc::clone(&store);
	let writer_stop = Arc::clone(&stop);
	let writer = thread::spawn(move || {
		let mut i = 0usize;
		run_timed_loop(writer_stop, || {
			let mut guard = writer_store.lock().expect("store lock");
			let meta = write_meta(10_000 + i);
			guard
				.insert_remote_item(ACCOUNT_UID, &meta, now_ms())
				.expect("insert_remote_item");
			i += 1;
		})
	});
	let reader_store = Arc::clone(&store);
	let reader_stop = Arc::clone(&stop);
	let reader = thread::spawn(move || {
		run_timed_loop(reader_stop, || {
			let guard = reader_store.lock().expect("store lock");
			let metas = guard
				.list_history_metas(ACCOUNT_UID, 100)
				.expect("list_history_metas");
			std::hint::black_box(metas.len());
		})
	});

	thread::sleep(WINDOW);
	stop.store(true, Ordering::Release);
	let writer_samples = writer.join().expect("writer join");
	let reader_samples = reader.join().expect("reader join");
	print_row("writer insert_remote_item (concurrent)", &writer_samples);
	print_row("reader list_history(100) (concurrent)", &reader_samples);

	// 最后再对一次账：写者确实写入了。
	let total = store
		.lock()
		.expect("store lock")
		.history_count_for_account(ACCOUNT_UID)
		.expect("history count");
	println!("[probe] rows for account after section 1b: {total}");

	// ------------------------------------------------------------------
	// Section 1c：同样的写入负载，但读线程改用 Task B 的只读连接池
	// （`StoreReader`，`query_only=ON`，WAL 快照）。这是修复后的对照。
	// ------------------------------------------------------------------
	println!();
	println!(
		"[probe] 1c/4 writer insert_remote_item + reader via StoreReader::open, {:?}",
		WINDOW
	);
	let pool = Arc::new(StoreReader::open(dir.path()));
	let stop = Arc::new(AtomicBool::new(false));

	let writer_store = Arc::clone(&store);
	let writer_stop = Arc::clone(&stop);
	let writer = thread::spawn(move || {
		let mut i = 0usize;
		run_timed_loop(writer_stop, || {
			let mut guard = writer_store.lock().expect("store lock");
			let meta = write_meta(20_000 + i);
			guard
				.insert_remote_item(ACCOUNT_UID, &meta, now_ms())
				.expect("insert_remote_item");
			i += 1;
		})
	});
	let pool_reader = Arc::clone(&pool);
	let reader_stop = Arc::clone(&stop);
	let reader = thread::spawn(move || {
		run_timed_loop(reader_stop, || {
			let metas = pool_reader
				.list_history_metas(ACCOUNT_UID, 100)
				.expect("pooled list_history_metas");
			std::hint::black_box(metas.len());
		})
	});

	thread::sleep(WINDOW);
	stop.store(true, Ordering::Release);
	let writer_samples = writer.join().expect("writer join");
	let reader_samples = reader.join().expect("reader join");
	print_row("writer insert_remote_item (pool run)", &writer_samples);
	print_row("reader via read pool (concurrent)", &reader_samples);

	let total = store
		.lock()
		.expect("store lock")
		.history_count_for_account(ACCOUNT_UID)
		.expect("history count");
	println!("[probe] rows for account after section 1c: {total}");
	drop(pool);
	drop(store);
	drop(dir);

	// ------------------------------------------------------------------
	// Section 2：长事务影响。历史保留路径用一条 DELETE 删除 10 000 行
	// `history`（单个隐式事务）；两个读线程同时每 ~5 ms 查询一次：
	// 一个走 `Mutex<Store>`（旧基线），一个走只读连接池（Task B）。
	// 该事务持有写锁/Mutex，基线读线程的等待时间就是它感知到的停顿。
	// ------------------------------------------------------------------
	let dir = tempfile::tempdir().expect("tempdir");
	let store = open_store(dir.path());
	{
		let mut guard = store.lock().expect("store lock");
		let seed_started = Instant::now();
		seed_history(&mut guard, LONG_TX_HISTORY_ROWS, now_ms() - 20_000_000);
		println!();
		println!(
			"[probe] 2/4 seeded {LONG_TX_HISTORY_ROWS} history rows in {:?}; purging {LONG_TX_PURGE_ROWS} in one DELETE while two readers query",
			seed_started.elapsed()
		);
	}

	let pool = Arc::new(StoreReader::open(dir.path()));
	let stop = Arc::new(AtomicBool::new(false));

	let reader_store = Arc::clone(&store);
	let reader_stop = Arc::clone(&stop);
	let baseline_reader = thread::spawn(move || {
		run_timed_loop(reader_stop, || {
			let guard = reader_store.lock().expect("store lock");
			let metas = guard
				.list_history_metas(ACCOUNT_UID, 100)
				.expect("list_history_metas");
			std::hint::black_box(metas.len());
		})
	});
	let pool_reader = Arc::clone(&pool);
	let reader_stop = Arc::clone(&stop);
	let pooled_reader = thread::spawn(move || {
		run_timed_loop(reader_stop, || {
			let metas = pool_reader
				.list_history_metas(ACCOUNT_UID, 100)
				.expect("pooled list_history_metas");
			std::hint::black_box(metas.len());
		})
	});

	// 让两个读线程先稳定运行，再开始长事务。
	thread::sleep(Duration::from_millis(100));
	let purge_started = Instant::now();
	let purged = {
		let mut guard = store.lock().expect("store lock");
		guard
			.purge_history_oldest_excess(ACCOUNT_UID, LONG_TX_PURGE_ROWS)
			.expect("purge history")
	};
	let purge_elapsed = purge_started.elapsed();
	thread::sleep(Duration::from_millis(100));
	stop.store(true, Ordering::Release);
	let baseline_samples = baseline_reader.join().expect("reader join");
	let pooled_samples = pooled_reader.join().expect("reader join");
	println!(
		"[probe] writer retention purge: {purged} row(s) deleted, single DELETE took {purge_elapsed:?}"
	);
	print_row("reader during 10k purge (Mutex<Store>)", &baseline_samples);
	print_row("reader during 10k purge (read pool)", &pooled_samples);

	println!();
	println!("[probe] decision rule (task A): if baseline reader p99 < 5 ms AND max < 20 ms during concurrent writes => single connection is enough; otherwise add a read pool");
	println!(
		"[probe] section 1b is the baseline (single Mutex); 1c/2 show the read-pool comparison"
	);
	println!("[probe] done");
}
