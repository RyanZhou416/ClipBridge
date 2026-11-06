// cb_core/src/api.rs

use std::sync::{
	atomic::{AtomicBool, Ordering},
	Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};

use crate::clipboard::{make_ingest_plan, ClipboardSnapshot, IngestPlan, LocalIngestDeps};
pub(crate) use crate::model::ItemMeta;
use crate::net::{NetCmd, NetManager};
pub use crate::policy::{AppConfig, GlobalPolicy};
use crate::{
	cas::{Cas, FsyncPolicy, PendingSync},
	logs::LogSink,
	logs::LogStore,
	store::Store,
	util::now_ms,
};

/// CAS 缓存 GC 的“廉价预检”阈值：`content_cache` 的 SQL 账面占用低于
/// 上限的 `1 - 1/GC_CAS_ACCOUNT_TRIGGER_DIVISOR`（即 90%）时，非启动路径
/// 不再遍历缓存目录做账实核算（见 [`Core::run_gc`]）。
const GC_CAS_ACCOUNT_TRIGGER_DIVISOR: i64 = 10;

/// 后台 durable sweep 周期（ADR-002 §3.7）：队列满/进程重启遗留的 `durable=0`
/// 行最迟 30 s 后被重新同步。
const DURABILITY_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// 单次 sweep 最多处理的行数：每 30 s 分批推进，避免一次性长时间占住后台线程。
const DURABILITY_SWEEP_BATCH: i64 = 64;

// 单元测试探针：本线程执行过多少次 `run_gc` 里的 CAS 整树遍历。
// 用 thread-local 而不是全局原子：libtest 默认每个测试一个线程，这样并行
// 测试之间不会互相污染计数。（macro 调用上不能写 `///`，否则是 unused doc comment。）
#[cfg(test)]
thread_local! {
	pub(crate) static GC_CAS_FULL_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/**
 * Core 的配置项。
 *
 * 定义了 Core 启动所需的设备 ID、名称、账户信息、存储路径以及各项限制策略。
 */
#[derive(Clone, Default)]
pub struct CoreConfig {
	pub device_id: String,        // 本机 device_id（先由壳传入）
	pub device_name: String,      // 设备显示名
	pub account_uid: String,      // 本机当前账号域（history 分区键，账号明文）
	pub account_password: String, // 账号密码（用于OPAQUE握手验证）
	pub data_dir: String,         // 持久：core.db
	pub cache_dir: String,        // 可清空：CAS blobs/tmp
	pub app_config: AppConfig,
	/// 为 true 时完全跳过 NetManager（不 bind socket、不启动 mDNS）。
	/// 默认 false（= 网络开启），保证正式应用行为不变；仅供测试/CI 关闭网络使用。
	pub disable_network: bool,
}

/// 手写 Debug：绝不把账号密码写进日志/panic 输出。
impl std::fmt::Debug for CoreConfig {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("CoreConfig")
			.field("device_id", &self.device_id)
			.field("device_name", &self.device_name)
			.field("account_uid", &self.account_uid)
			.field("account_password", &"<redacted>")
			.field("data_dir", &self.data_dir)
			.field("cache_dir", &self.cache_dir)
			.field("app_config", &self.app_config)
			.field("disable_network", &self.disable_network)
			.finish()
	}
}

/**
 * 摄入计划的评估结果。
 *
 * 用于告知调用方该次摄入的元数据信息、是否需要用户确认以及采取的摄入策略。
 */
#[derive(Clone, Debug)]
pub struct PlanResult {
	pub meta: ItemMeta,
	pub needs_user_confirm: bool,
	pub strategy: String, // 用字符串，FFI/壳侧更省事
}

/**
 * 事件回调接口（Core → 壳）。
 *
 * 壳层通过实现此接口来接收 Core 内部产生的事件（如新项目添加等）。
 */
pub trait CoreEventSink: Send + Sync + 'static {
	fn emit(&self, event_json: String);
}

/// 对外暴露的设备状态 DTO
#[derive(Debug, Clone, serde::Serialize)]
pub struct PeerStatus {
	pub device_id: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub device_name: Option<String>, // 设备名称（从 HELLO 或 discovery 获取）
	pub state: PeerConnectionState, // 连接状态
	pub last_seen_ts_ms: i64,       // 最后见到时间
	pub share_to_peer: bool,        // Outbound allow（策略状态）
	pub accept_from_peer: bool,     // Inbound allow（策略状态）
}

/// 详细的连接状态枚举
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub enum PeerConnectionState {
	Discovered, // 知道地址，但在退避或未连接
	Connecting, // 正在 TCP/QUIC 握手
	TransportReady,
	AccountVerifying,
	AccountVerified, // 账号已验证，正在查 Policy/TOFU
	Online,          // 完全可用
	Backoff,
	Offline, // 彻底断开
}

/**
 * ClipBridge Core 的权威句柄。
 *
 * 这是整个核心库的唯一入口点，通过它来调用所有的业务逻辑。
 */
#[derive(Clone)]
pub struct Core {
	pub inner: Arc<Inner>,
}

impl Core {
	/**
	 * 初始化核心实例。
	 *
	 * @param cfg 核心配置信息
	 * @param sink 事件回调接口实现
	 * @return 返回初始化完成的 Core 实例
	 */
	pub fn init(cfg: CoreConfig, sink: Arc<dyn CoreEventSink>) -> Self {
		// 初始化日志存储（需要先创建才能记录日志）
		let log_store = LogStore::open(&cfg.data_dir).expect("open log store");
		let logs_rebuilt = log_store.rebuilt;
		let log_store_arc = Arc::new(Mutex::new(log_store));
		// 专用日志 writer 线程：运行时（NetManager / SessionActor）只调用
		// `log_sink.log_*`（try_send，永不阻塞），查询路径继续共享
		// `log_store_arc`（FFI 线程阻塞等待 SQLite 是可接受的）。
		// 顺序：writer 线程在 Core 初始化期间启动，`Core::shutdown` 负责
		// 关闭队列、排空并 join（见 `Inner::shutdown`）。
		let log_sink = LogSink::new(Arc::clone(&log_store_arc));

		// ADR-002 §3.7：fsync 策略只在本函数解析一次，随后由 `Cas` 持有并供所有
		// 会话共享。非法值绝不能 panic 或悄悄切换语义：记录 warn 后回退默认
		// `large-only`（小文件同步 fsync，大文件后台补同步）。
		let fsync_policy =
			match FsyncPolicy::parse(std::env::var(crate::cas::FSYNC_POLICY_ENV).ok().as_deref()) {
				Ok(policy) => policy,
				Err(invalid) => {
					let mut log_store = log_store_arc.lock().unwrap();
					let _ = log_store.log_warn(
						"Init",
						&format!(
							"Invalid {}={invalid:?}; falling back to default '{}'",
							crate::cas::FSYNC_POLICY_ENV,
							FsyncPolicy::default().as_str()
						),
						Some(&format!(
							"{} 取值非法: {invalid:?}，已回退默认策略 '{}'",
							crate::cas::FSYNC_POLICY_ENV,
							FsyncPolicy::default().as_str()
						)),
					);
					FsyncPolicy::default()
				}
			};

		// 记录初始化开始日志
		{
			let mut log_store = log_store_arc.lock().unwrap();
			let device_id = &cfg.device_id;
			let _ = log_store.log_info(
				"Init",
				&format!("Core initializing with device_id: {device_id}"),
				Some(&format!("核心正在初始化，设备 ID: {device_id}")),
			);
			// 日志库重建会清空旧日志，这条 warn 会作为新库里的第一条记录。
			if logs_rebuilt {
				let _ = log_store.log_warn(
					"Init",
					&format!(
						"logs.db schema changed (-> v{}): diagnostic logs were dropped and the table rebuilt",
						crate::logs::LOG_SCHEMA_VERSION
					),
					Some(&format!(
						"日志库 schema 已切换到 v{}：诊断日志已清空并重建",
						crate::logs::LOG_SCHEMA_VERSION
					)),
				);
			}
		}

		// `stats.db` 是已移除的死子系统：它从未被写入过，三个统计接口改为按需从
		// `core.db` / CAS / `history` 计算。这里尽力删除旧版本留下的文件（含 WAL/SHM
		// 边车文件），失败只记录、不阻断启动。
		let stats_db = std::path::Path::new(&cfg.data_dir).join("stats.db");
		for path in [
			stats_db.clone(),
			stats_db.with_file_name("stats.db-wal"),
			stats_db.with_file_name("stats.db-shm"),
		] {
			match std::fs::remove_file(&path) {
				Ok(()) => {
					let mut log_store = log_store_arc.lock().unwrap();
					let _ = log_store.log_info(
						"Init",
						&format!("Removed obsolete stats.db file: {}", path.display()),
						Some(&format!("已删除废弃的 stats.db 文件: {}", path.display())),
					);
				}
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
				Err(e) => {
					let mut log_store = log_store_arc.lock().unwrap();
					let _ = log_store.log_warn(
						"Init",
						&format!(
							"Failed to remove obsolete stats.db file {}: {e}",
							path.display()
						),
						Some(&format!(
							"删除废弃的 stats.db 文件失败 {}: {e}",
							path.display()
						)),
					);
				}
			}
		}

		// 初始化各个组件
		let store = match Store::open(&cfg.data_dir) {
			Ok(s) => {
				let rebuilt = s.rebuilt;
				let mut log_store = log_store_arc.lock().unwrap();
				if rebuilt {
					// 这是设计决定，不是意外：v2 用整库重建代替迁移，身份表也会被销毁。
					let _ = log_store.log_warn(
						"Init",
						&format!(
							"core.db schema changed (-> v{}): ALL content/identity tables were dropped and recreated, \
                             including trusted_peers and peer_rules. Pairing data is lost by design; \
                             devices must re-pair with the password.",
							crate::store::SCHEMA_VERSION
						),
						Some(&format!(
							"核心库 schema 已切换到 v{}：所有内容/身份表（含 trusted_peers 与 peer_rules）\
                             已按设计销毁重建，配对数据丢失，设备需重新输入密码配对。",
							crate::store::SCHEMA_VERSION
						)),
					);
				} else {
					let _ = log_store.log_info(
						"Init",
						"Store initialized successfully",
						Some("存储初始化成功"),
					);
				}
				s
			}
			Err(e) => {
				let mut log_store = log_store_arc.lock().unwrap();
				let _ = log_store.log_error(
					"Init",
					&format!("Failed to initialize Store: {e}"),
					Some(&format!("存储初始化失败: {e}")),
					Some(&e.to_string()),
				);
				panic!("Failed to open store: {e}");
			}
		};
		let store_rebuilt = store.rebuilt;
		let store_arc = Arc::new(Mutex::new(store));

		let cas = match Cas::new_with_policy(&cfg.cache_dir, fsync_policy) {
			Ok(c) => {
				let mut log_store = log_store_arc.lock().unwrap();
				let _ = log_store.log_info(
					"Init",
					&format!(
						"CAS initialized successfully (fsync_policy={})",
						fsync_policy.as_str()
					),
					Some(&format!(
						"CAS 初始化成功（fsync 策略={}：always=每次传输同步；large-only=小于 8 MiB 同步、大文件后台补同步；never=不同步）",
						fsync_policy.as_str()
					)),
				);
				c
			}
			Err(e) => {
				let mut log_store = log_store_arc.lock().unwrap();
				let _ = log_store.log_error(
					"Init",
					&format!("Failed to initialize CAS: {e}"),
					Some(&format!("CAS 初始化失败: {e}")),
					Some(&e.to_string()),
				);
				panic!("Failed to init cas: {e}");
			}
		};

		// 核心库整库重建后，`content_cache` 里的每一行都没了；磁盘上的 CAS 对象
		// 已经没有元数据引用它们，不清理就会变成永远无法被 LRU/GC 回收的孤儿，
		// 持续占用 gc_cas_max_bytes 预算。这里按设计把缓存目录（blobs/files/
		// downloads/tmp）一并清空，然后才做存在性对账。
		if store_rebuilt {
			let before_bytes = cas.total_size_bytes().unwrap_or(0);
			match cas.clear_all() {
				Ok(()) => {
					let mut log_store = log_store_arc.lock().unwrap();
					let _ = log_store.log_warn(
						"Init",
						&format!(
							"core.db was rebuilt: cleared CAS cache (blobs/files/downloads/tmp), \
                             freed ~{before_bytes} bytes; content_cache is empty so all blobs were orphaned"
						),
						Some(&format!(
							"核心库已重建：已清空 CAS 缓存（blobs/files/downloads/tmp），约释放 {before_bytes} 字节；\
                             因为 content_cache 已清空，这些 blob 都已成为孤儿"
						)),
					);
				}
				Err(e) => {
					// 清理失败不应阻断启动，但必须留下明确错误（磁盘可能仍被占用）。
					let mut log_store = log_store_arc.lock().unwrap();
					let _ = log_store.log_error(
						"Init",
						&format!("core.db was rebuilt but CAS cleanup failed: {e}; orphan blobs may remain"),
						Some(&format!(
							"核心库已重建，但 CAS 缓存清理失败: {e}；可能仍残留孤儿 blob"
						)),
						Some(&e.to_string()),
					);
				}
			}
		}

		// 启动对账：`content_cache` 里声称 present=1、但 blob 文件已丢失的行改回 0。
		// 只对“声称存在”的行做 N 次文件检查，不扫描整个 CAS 目录，启动成本与缓存条目数
		// 成正比；这能修复崩溃/手动清理磁盘后 GC 仍以为内容还在的账实不符。
		match reconcile_cache_presence(&store_arc, &cas, now_ms()) {
			Ok(0) => {
				let mut log_store = log_store_arc.lock().unwrap();
				let _ = log_store.log_debug(
					"Cache",
					"Startup cache reconciliation: no present row is missing its blob",
					Some("启动缓存对账：所有 present=1 的记录都能找到对应 blob"),
				);
			}
			Ok(count) => {
				let mut log_store = log_store_arc.lock().unwrap();
				let _ = log_store.log_warn(
					"Cache",
					&format!(
						"Startup cache reconciliation: {count} content_cache row(s) claimed present=1 \
                         but their blob is missing -> present=0"
					),
					Some(&format!(
						"启动缓存对账：{count} 条 content_cache 记录声称 present=1 但 blob 缺失，已改为 present=0"
					)),
				);
			}
			Err(e) => {
				let mut log_store = log_store_arc.lock().unwrap();
				let _ = log_store.log_error(
					"Cache",
					&format!("Startup cache reconciliation failed: {e}"),
					Some(&format!("启动缓存对账失败: {e}")),
					Some(&e.to_string()),
				);
			}
		}

		// --- M1 集成：启动网络管理器 ---
		// F5.2：启动时先做一次有界孤儿 blob 清扫。必须在 reconcile 之后、网络
		// 启动之前：此时 content_cache 已与磁盘对账，且还没有新的入站 commit 竞争。
		sweep_orphan_blobs_best_effort(&cas, &store_arc, &log_sink, "Startup");

		// 注意：这里假设 NetManager::spawn 是同步封装（内部 spawn 异步任务）
		// 如果是在 FFI 环境且有 Tokio Runtime，这将正常工作。
		// 测试/CI 可通过 disable_network=true 完全跳过网络启动，避免 bind socket 触发防火墙弹窗。
		let net_tx = if cfg.disable_network {
			let mut log_store = log_store_arc.lock().unwrap();
			let _ = log_store.log_info(
				"Init",
				"NetManager disabled by config (tests/CI)",
				Some("配置已禁用网络管理器（测试/CI）"),
			);
			None
		} else {
			match NetManager::spawn(
				cfg.clone(),
				sink.clone(),
				store_arc.clone(),
				cas.clone(),
				log_sink.clone(),
			) {
				Ok(tx) => {
					let mut log_store = log_store_arc.lock().unwrap();
					let _ = log_store.log_info(
						"Init",
						"NetManager started successfully",
						Some("网络管理器启动成功"),
					);
					Some(tx)
				}
				Err(e) => {
					let mut log_store = log_store_arc.lock().unwrap();
					let _ = log_store.log_warn(
						"Init",
						&format!("NetManager failed to start: {e} (continuing without network)"),
						Some(&format!("网络管理器启动失败: {e}（将在无网络模式下继续）")),
					);
					None
				}
			}
		};

		let inner = Inner {
			core_config: cfg,
			sink,
			is_shutdown: AtomicBool::new(false),
			store: store_arc,
			log_store: log_store_arc,
			log_sink,
			cas,
			net: net_tx,
		};
		let core = Self {
			inner: Arc::new(inner),
		};
		// 每 Core 一个后台 durability worker：启动时先扫一次，覆盖上次进程退出/
		// 崩溃遗留的 `durable=0` 行；随后由接收路径的 pending 标记唤醒，并每 30 s
		// 周期 sweep 兜底（队列满的行不会丢）。
		ensure_durability_worker(
			&core.inner.cas,
			core.inner.store.clone(),
			&core.inner.log_sink,
		);
		let _ = core.run_gc("Startup");
		core
	}

	/**
	 * 摄入本地剪贴板拷贝的内容。
	 *
	 * 这是一个组合操作，包含规划（plan）和应用（apply）两个阶段。
	 *
	 * @param snapshot 剪贴板内容快照
	 * @return 成功则返回新摄入项目的元数据
	 */
	#[allow(clippy::needless_pass_by_value)]
	pub fn ingest_local_copy(&self, snapshot: ClipboardSnapshot) -> anyhow::Result<ItemMeta> {
		// Step 1：只做计划，不碰 DB/CAS
		let plan = match self.plan_local_ingest(&snapshot, false) {
			Ok(p) => p,
			Err(e) => {
				self.inner.log_sink.log_error(
					"Ingest",
					&format!("Ingest planning failed: {e}"),
					Some(&format!("内容摄入规划失败: {e}")),
					Some(&e.to_string()),
				);
				return Err(e);
			}
		};
		// Step 2：真正落库 + CAS + 发事件
		match self.apply_ingest(&plan) {
			Ok(meta) => Ok(meta),
			Err(e) => {
				self.inner.log_sink.log_error(
					"Ingest",
					&format!("Ingest failed: {e}"),
					Some(&format!("内容摄入失败: {e}")),
					Some(&e.to_string()),
				);
				Err(e)
			}
		}
	}

	pub fn shutdown(&self) {
		self.inner.shutdown();
	}

	/// 清空所有缓存（CAS blobs、临时文件、视图等）
	///
	/// 只清“内容”、不清“元数据”：`content_cache` 行必须保留（只把 `present` 置 0），
	/// 因为 `items`/`history` 的读取都以 INNER JOIN `content_cache` 取 `total_bytes`；
	/// 删行会让整个历史记录从所有查询中消失。
	pub fn clear_cache(&self) -> anyhow::Result<()> {
		self.inner.cas.clear_all()?;
		// 内容已从磁盘删除，但“曾经缓存过这个 sha”的元数据继续保留，供 history 查询 JOIN。
		let mut store = self.inner.store.lock().unwrap();
		store.mark_all_cache_missing()?;
		Ok(())
	}

	/**
	 * 规划本地内容的摄入流程。
	 *
	 * 此方法会根据当前的设备信息、账户配置以及安全限制（Limits），
	 * 对剪贴板快照进行评估并生成摄入计划（IngestPlan），但不会执行实际的存储或 CAS 写入操作。
	 *
	 * @param snapshot 剪贴板快照数据
	 * @param force 是否强制摄入（若为 true，则可能跳过某些交互确认逻辑，例如大小限制警告）
	 * @return 返回生成的摄入计划，若核心已关闭则返回错误
	 */
	pub fn plan_local_ingest(
		&self,
		snapshot: &ClipboardSnapshot,
		force: bool,
	) -> anyhow::Result<IngestPlan> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}

		// 获取内容类型和大小
		let (content_type, size_bytes) = match snapshot {
			ClipboardSnapshot::Text { text_utf8, .. } => ("Text", text_utf8.len() as i64),
			ClipboardSnapshot::Image { bytes, .. } => ("Image", bytes.len() as i64),
			ClipboardSnapshot::FileList { files, .. } => {
				let total: i64 = files.iter().map(|f| f.size_bytes).sum();
				("FileList", total)
			}
		};

		// 记录规划日志（非阻塞入队，不占 FFI 线程的 SQLite 锁）
		self.inner.log_sink.log_info(
			"Ingest",
			&format!(
				"Planning ingest for clipboard content, type: {content_type}, size: {size_bytes} bytes"
			),
			Some(&format!(
				"正在规划剪贴板内容摄入，类型: {content_type}，大小: {size_bytes} 字节"
			)),
		);

		let deps = LocalIngestDeps {
			device_id: &self.inner.core_config.device_id,
			device_name: &self.inner.core_config.device_name,
			account_uid: &self.inner.core_config.account_uid,
		};

		let size_limits = &self.inner.core_config.app_config.size_limits;

		make_ingest_plan(&deps, snapshot, size_limits, force)
	}

	pub fn apply_ingest(&self, plan: &IngestPlan) -> anyhow::Result<ItemMeta> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}

		let now = now_ms();
		let meta_clone = plan.meta.clone();
		let sha = plan.meta.content.sha256.clone();
		let item_id = plan.meta.item_id.clone();

		// Phase A：落库（只在这个 block 里持锁）
		let cache = {
			let mut store = self.inner.store.lock().unwrap();
			store.insert_meta_and_history(&self.inner.core_config.account_uid, &plan.meta, now)?
		};

		// Phase B：CAS 去重写入（这里不持 store 锁）
		if !cache.present || !self.inner.cas.blob_exists(&sha) {
			let tmp_name = format!("{}.tmp", plan.meta.item_id);
			let _wrote = self
				.inner
				.cas
				.put_if_absent(&sha, &plan.content_bytes, &tmp_name)?;

			if self.inner.cas.blob_exists(&sha) {
				let mut store = self.inner.store.lock().unwrap();
				store.mark_cache_present(&sha, now)?;

				// 记录 CAS 写入成功
				self.inner.log_sink.log_info(
					"Ingest",
					&format!("CAS blob written successfully: {sha}"),
					Some(&format!("CAS 对象写入成功: {sha}")),
				);
			} else {
				self.inner.log_sink.log_error(
					"Ingest",
					"CAS write failed: blob missing after put_if_absent",
					Some("CAS 写入失败: put_if_absent 后对象缺失"),
					Some("CAS write failed: blob missing after put_if_absent"),
				);
				anyhow::bail!("CAS write failed: blob missing after put_if_absent");
			}
		} else {
			let mut store = self.inner.store.lock().unwrap();
			store.touch_cache(&sha, now)?;

			// 记录 CAS 去重命中
			self.inner.log_sink.log_debug(
				"Ingest",
				&format!("CAS blob already exists, skipping write: {sha}"),
				Some(&format!("CAS 对象已存在，跳过写入: {sha}")),
			);
		}

		// 事件（不需要 store 锁）
		let meta_evt = serde_json::json!({
		  "type": "ITEM_META_ADDED",
		  "meta": plan.meta,
		  "policy": {
			"needs_user_confirm": plan.needs_user_confirm,
			"strategy": format!("{:?}", plan.strategy),
		  }
		});
		self.inner.emit(meta_evt.to_string());

		// 记录摄入成功
		self.inner.log_sink.log_info(
			"Ingest",
			&format!("Ingest applied successfully, item_id: {item_id}"),
			Some(&format!("内容摄入成功，项目 ID: {item_id}")),
		);

		// --- M1 集成：广播元数据到网络 ---
		if let Some(net_tx) = &self.inner.net {
			// 使用 try_send 避免阻塞，如果通道满了或网络层挂了也不影响本地逻辑
			let _ = net_tx.try_send(NetCmd::BroadcastMeta(Box::new(plan.meta.clone())));
		}

		// GC（现在一定不会死锁）
		let _ = self.run_gc("AfterIngest");

		Ok(meta_clone)
	}

	/// 执行一次 GC：历史记录条数 + CAS 缓存容量。
	///
	/// `reason` 用于区分启动阶段：`"Startup"` 无条件做一次完整的 CAS 账实核算；
	/// 其他调用先查 SQL 账面估算，只有接近上限（≥ 90%）才遍历缓存目录。
	///
	/// 这样每次剪贴板摄入都不再为整个缓存目录付出 O(文件数) 的
	/// `total_size_bytes()` 成本，同时容量上限依然生效：SQL 估算只决定“要不要
	/// 做昂贵核算”，真实字节数仍由按 inode 去重硬链接的整树遍历决定。
	pub fn run_gc(&self, reason: &str) -> anyhow::Result<()> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}

		let now = now_ms();

		// 1) History GC
		let max_history = self.inner.core_config.app_config.gc_history_max_items;
		if max_history > 0 {
			let mut store = self.inner.store.lock().unwrap();
			let n = store.history_count_for_account(&self.inner.core_config.account_uid)?;
			if n > max_history {
				store.soft_delete_history_keep_latest(
					&self.inner.core_config.account_uid,
					max_history,
				)?;
			}
		}

		// 2) Cache GC（LRU）
		let max_cas = self.inner.core_config.app_config.gc_cas_max_bytes;
		if max_cas > 0 {
			// 廉价预检：`SELECT SUM(total_bytes) FROM content_cache WHERE present=1`
			// 走 idx_cache_size(present,total_bytes) 索引，不打开任何文件。它只是
			// “是否值得做整树核算”的触发器，不是权威值：
			// - 真实占用始终由 total_size_bytes() 的整树遍历（blobs/files/downloads，
			//   按 inode 去重硬链接）在真正需要核算时决定；
			// - 启动阶段无条件完整核算，同时兜底修复账实漂移；
			// - 非启动路径只有账面估算达到上限的 90% 才完整核算。
			// 漂移取舍：若 present=1 的行在磁盘上已丢失，估算偏高（多触发遍历，
			// 方向安全）；若磁盘上有 content_cache 未记录的残留（present=0 的历史
			// 行、孤儿 blob、无法追踪的复制视图），估算偏低，可能在本轮少触发一次
			// 遍历。后者由启动对账 reconcile_cache_presence + 启动 GC 的完整遍历兜底。
			let do_full_accounting = if reason == "Startup" {
				true
			} else {
				let estimate = {
					let store = self.inner.store.lock().unwrap();
					store.sum_present_bytes()?
				};
				// 留 10% 安全余量：低于上限的 90% 时不遍历磁盘。
				estimate >= max_cas - max_cas / GC_CAS_ACCOUNT_TRIGGER_DIVISOR
			};

			if do_full_accounting {
				#[cfg(test)]
				GC_CAS_FULL_WALKS.with(|n| n.set(n.get() + 1));

				// 权威值：真实占用 = blobs + files/ 视图 + downloads/ 视图（按 inode
				// 去重，硬链接共享的 inode 只计一次）。
				let mut cur = self.inner.cas.total_size_bytes()?;
				while cur > max_cas {
					let sha = {
						let store = self.inner.store.lock().unwrap();
						let mut cands = store.select_lru_present(1)?;
						let Some((sha, _bytes)) = cands.pop() else {
							break;
						};
						sha
					};
					// remove_blob 先删引用同一 inode 的 files/downloads 视图硬链接，
					// 再删 blob 本身；它返回的 `i64` 是该 blob 文件的字节数，也就是
					// 这个 inode 在所有链接消失时真正释放的字节数。因此可以直接从
					// `cur` 扣减，无需再走一次整树遍历。
					// 已知不足（只会让统计偏保守）：无法追踪的“复制回退”视图
					// （非硬链接）被删掉时释放的字节不含在返回值里，于是 `cur` 只会
					// 偏高、不会偏低 —— 最坏情况是多驱逐一点，而不会在真实占用仍超限
					// 时误判为“已够小”。
					// 删除报错（如 Windows 上视图被占用）时本轮直接放弃：不做第二次
					// 整树重算，下一次完整核算（启动或再次接近上限）会重新核准。
					let Ok(freed) = self.inner.cas.remove_blob(&sha) else {
						break;
					};
					{
						let mut store = self.inner.store.lock().unwrap();
						store.mark_cache_missing(&sha, now)?;
					}
					if freed <= 0 {
						// 账实漂移：blob 已不存在（或为空），没有可扣减的字节。
						// 不猜、不重算，交给下一次完整核算。
						break;
					}
					cur = cur.saturating_sub(freed);
				}
			}
		}
		Ok(())
	}

	pub fn plan_local_ingest_result(
		&self,
		snapshot: &ClipboardSnapshot,
		force: bool,
	) -> anyhow::Result<PlanResult> {
		let plan = self.plan_local_ingest(snapshot, force)?;
		Ok(PlanResult {
			meta: plan.meta,
			needs_user_confirm: plan.needs_user_confirm,
			strategy: format!("{:?}", plan.strategy),
		})
	}

	#[allow(clippy::needless_pass_by_value)]
	pub fn ingest_local_copy_with_force(
		&self,
		snapshot: ClipboardSnapshot,
		force: bool,
	) -> anyhow::Result<ItemMeta> {
		let plan = self.plan_local_ingest(&snapshot, force)?;
		self.apply_ingest(&plan)
	}

	/**
	 * 获取当前在线设备列表。
	 *
	 * 这是一个同步阻塞调用，适合 FFI 使用。
	 * 它会向 NetManager 发送查询指令并等待结果返回。
	 */
	pub fn list_peers(&self) -> anyhow::Result<Vec<PeerStatus>> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}

		// 检查网络层是否初始化
		let Some(net_tx) = &self.inner.net else {
			return Ok(vec![]); // 网络没起，返回空列表
		};

		// 1. 创建回传通道
		let (tx, rx) = tokio::sync::oneshot::channel();

		// 2. 发送命令 (使用 blocking_send 确保发送成功，或者 try_send)
		// 注意：net_tx 是 mpsc::Sender
		net_tx
			.blocking_send(NetCmd::GetPeers(tx))
			.map_err(|_| anyhow::anyhow!("NetManager channel closed"))?;

		// 3. 阻塞等待回包
		// 注意：因为我们是在 FFI 调用线程（非 Tokio 线程），使用 block_on 是安全的
		let peers = futures::executor::block_on(rx)
			.map_err(|_| anyhow::anyhow!("Failed to receive response from NetManager"))?;

		Ok(peers)
	}

	/**
	 * 获取 Core 运行状态。
	 */
	pub fn get_status(&self) -> anyhow::Result<serde_json::Value> {
		// M1 阶段简单返回配置信息和运行状态
		let is_shutdown = self.inner.is_shutdown.load(Ordering::Acquire);

		Ok(serde_json::json!({
			"status": if is_shutdown { "Shutdown" } else { "Running" },
			"device_id": self.inner.core_config.device_id,
			"account_uid": self.inner.core_config.account_uid,
			"net_enabled": self.inner.net.is_some(),
			"config": self.inner.core_config.app_config
		}))
	}

	/**
	 * 设置单个设备的共享策略。
	 *
	 * @param peer_id 对端设备 ID
	 * @param share_to 是否允许向该设备发送（None 表示不修改）
	 * @param accept_from 是否允许接收该设备的数据（None 表示不修改）
	 */
	/// 清除设备指纹（用于重新配对，解决 TLS_PIN_MISMATCH 问题）
	pub fn clear_peer_fingerprint(&self, peer_id: &str) -> anyhow::Result<()> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}

		let account_uid = &self.inner.core_config.account_uid;
		let mut store = self.inner.store.lock().unwrap();
		store.delete_peer_fingerprint(account_uid, peer_id)?;
		// 先释放 DB 锁，再回调壳层事件（避免重入死锁）
		drop(store);

		// 发送事件通知外壳
		self.inner.emit_json(serde_json::json!({
			"type": "peer_fingerprint_cleared",
			"ts_ms": now_ms(),
			"payload": {
				"device_id": peer_id
			}
		}));

		Ok(())
	}

	/// 清除本地证书（用于重新生成证书，需要重新配对所有设备）
	pub fn clear_local_cert(&self) -> anyhow::Result<()> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}

		let data_dir = &self.inner.core_config.data_dir;
		crate::transport::cert::clear_local_cert(data_dir)?;

		// 发送事件通知外壳
		self.inner.emit_json(serde_json::json!({
			"type": "local_cert_cleared",
			"ts_ms": now_ms(),
			"payload": {}
		}));

		Ok(())
	}

	pub fn set_peer_policy(
		&self,
		peer_id: &str,
		share_to: Option<bool>,
		accept_from: Option<bool>,
	) -> anyhow::Result<()> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}

		let now = now_ms();
		let account_uid = &self.inner.core_config.account_uid;

		// 获取或创建规则
		let mut store = self.inner.store.lock().unwrap();
		let mut rule = store.get_or_create_peer_rule(account_uid, peer_id, now)?;

		// 更新策略（如果提供了新值）
		if let Some(st) = share_to {
			rule.share_to_peer = st;
		}
		if let Some(af) = accept_from {
			rule.accept_from_peer = af;
		}
		rule.updated_at_ms = now;

		// 保存到数据库
		store.upsert_peer_rule(&rule)?;

		// 触发 peer_changed 事件通知外壳
		let event = serde_json::json!({
			"type": "peer_changed",
			"device_id": peer_id,
			"share_to_peer": rule.share_to_peer,
			"accept_from_peer": rule.accept_from_peer,
		});
		// 先释放 DB 锁，再回调壳层事件（避免重入死锁）
		drop(store);
		self.inner.emit(event.to_string());

		Ok(())
	}

	pub fn ensure_content_cached(
		&self,
		item_id: &str,
		file_id: Option<&str>,
	) -> anyhow::Result<String> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core shutdown");
		}

		// ---------- Fast path: 本机内容不走网络 ----------
		// 1) 查元数据（至少要拿到 source_device_id + content.sha256/bytes）
		if let Some(meta) = self.get_item_meta(item_id)? {
			// 你 init 里叫 device_id；这里按你真实字段改
			let my_device_id = &self.inner.core_config.device_id;

			// 如果内容来源就是本机：直接查本地缓存/CAS
			if meta.source_device_id == *my_device_id {
				// v1：只处理 file_id == None 的简单类型（text/image）
				if file_id.is_none() {
					let content = meta.content;
					// cas_has：判断 sha256 对应 blob 是否存在
					// 新逻辑：命中本机缓存也返回一个可等待的 transfer_id，并同步发 CONTENT_CACHED
					if self.inner.cas.blob_exists(&content.sha256) {
						let transfer_id = uuid::Uuid::new_v4().to_string();

						// kind：与接收侧一致的简单判定（你接收侧也是这么做的）
						let kind = if content.mime.starts_with("text/") {
							"text"
						} else if content.mime.starts_with("image/") {
							"image"
						} else {
							"file"
						};

						// local_path：text 直接给 CAS 路径；image 建议 materialize 带扩展名（可选）
						let local_path = if kind == "image" {
							let ext = if content.mime == "image/png" {
								Some("png")
							} else if content.mime == "image/jpeg" {
								Some("jpg")
							} else if content.mime == "image/gif" {
								Some("gif")
							} else {
								None
							};

							// cas.materialize_blob 已存在
							self.inner
								.cas
								.materialize_blob(&content.sha256, ext.unwrap_or(""))?
								.to_string_lossy()
								.to_string()
						} else {
							self.inner
								.cas
								.blob_path(&content.sha256)?
								.to_string_lossy()
								.to_string()
						};

						// local_ref 结构与接收侧保持一致
						let local_ref = serde_json::json!({
							"local_path": local_path,
							"item_id": item_id,
							"mime": content.mime,
							"kind": kind,
							"sha256": content.sha256,
							"total_bytes": content.total_bytes
						});

						// CONTENT_CACHED 结构与接收侧保持一致
						let evt = serde_json::json!({
								"type": "CONTENT_CACHED",
							"payload": {
								"transfer_id": transfer_id,
								"item_id": item_id,
								"file_id": file_id,
								"local_ref": local_ref
							}
						});

						self.inner.emit_json(evt);

						return Ok(transfer_id);
					}
				}
			}
		}

		// ---------- Slow path: 需要走网络 ----------
		let Some(net_tx) = &self.inner.net else {
			anyhow::bail!("network not initialized");
		};

		let (tx, rx) = tokio::sync::oneshot::channel();
		net_tx
			.blocking_send(NetCmd::EnsureContentCached {
				item_id: item_id.to_string(),
				file_id: file_id.map(ToString::to_string),
				force: false,
				reply: tx,
			})
			.map_err(|_| anyhow::anyhow!("NetManager closed"))?;

		match futures::executor::block_on(rx) {
			Ok(res) => res, // Ok(transfer_id) 或 Err(...)
			Err(_) => anyhow::bail!("Failed to get transfer_id"),
		}
	}

	/// M3: 取消传输
	pub fn cancel_transfer(&self, transfer_id: &str) {
		if let Some(net_tx) = &self.inner.net {
			let _ = net_tx.try_send(NetCmd::CancelTransfer {
				transfer_id: transfer_id.to_string(),
			});
		}
	}

	pub fn list_history(&self, limit: usize, cursor: Option<i64>) -> anyhow::Result<Vec<ItemMeta>> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}
		let store = self.inner.store.lock().unwrap();
		store.list_history_metas_paged(&self.inner.core_config.account_uid, limit, cursor)
	}

	/// 本地删除：软删除 + 从 CAS 移除 blob + 发出 item_deleted 事件
	pub fn delete_item_local(&self, item_id: &str) -> anyhow::Result<()> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}
		let account_uid = self.inner.core_config.account_uid.clone();
		let sha256_opt = {
			let mut store = self.inner.store.lock().unwrap();
			store.soft_delete_item(&account_uid, item_id)?;
			store.get_item_sha256(item_id)?
		};
		if let Some(sha) = &sha256_opt {
			let _ = self.inner.cas.remove_blob(sha);
			let now = now_ms();
			let mut store = self.inner.store.lock().unwrap();
			let _ = store.mark_cache_missing(sha, now);
		}
		let evt = serde_json::json!({
			"type": "ITEM_DELETED",
			"item_id": item_id,
		});
		self.inner.emit(evt.to_string());
		Ok(())
	}

	/// 全局删除：与本地删除相同，并广播给所有连接的设备
	pub fn delete_item_global(&self, item_id: &str) -> anyhow::Result<()> {
		self.delete_item_local(item_id)?;
		if let Some(tx) = &self.inner.net {
			let _ = tx.try_send(NetCmd::BroadcastDelete {
				item_id: item_id.to_string(),
			});
		}
		Ok(())
	}

	pub fn get_item_meta(&self, item_id: &str) -> anyhow::Result<Option<ItemMeta>> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}
		let store = self
			.inner
			.store
			.lock()
			.map_err(|_| anyhow::anyhow!("store mutex poisoned"))?;
		store.get_item_meta_by_id(item_id)
	}

	/// 统计窗口归一化，保持旧 FFI 行为：`end_ts_ms==0` 取当前时间；
	/// 只有 start/end 都为 0 时 start 才使用默认跨度（1h 或 24h）。
	fn stats_window(start_ts_ms: i64, end_ts_ms: i64, default_span_ms: i64) -> (i64, i64) {
		let use_default_start = start_ts_ms == 0 && end_ts_ms == 0;
		let end_ts = if end_ts_ms == 0 { now_ms() } else { end_ts_ms };
		let start_ts = if use_default_start {
			end_ts - default_span_ms
		} else {
			start_ts_ms
		};
		(start_ts, end_ts)
	}

	fn normalize_bucket_sec(bucket_sec: i32, default_sec: i32) -> i32 {
		if bucket_sec > 0 {
			bucket_sec
		} else {
			default_sec
		}
	}

	/// 缓存统计：从 `content_cache` 现算，不再依赖已删除的 `stats.db`。
	///
	/// - `current_cache_bytes` = `SUM(total_bytes) WHERE present=1`（与 GC 的账面口径一致）。
	/// - `series`：把 `present=1` 的行按 `created_ts_ms` 归桶、累加成“截至该桶的缓存占用”。
	///   窗口之前的行作为基线并入第一个点；没有历史指标表，所以这是能从现存数据
	///   诚实还原的全部信息（`created_ts_ms` 是内容创建时间，不是 blob 落盘时间）。
	pub fn query_cache_stats(
		&self,
		start_ts_ms: i64,
		end_ts_ms: i64,
		bucket_sec: i32,
	) -> anyhow::Result<serde_json::Value> {
		let (start_ts, end_ts) = Self::stats_window(start_ts_ms, end_ts_ms, 60 * 60 * 1000);
		let bucket_sec = Self::normalize_bucket_sec(bucket_sec, 10);
		let bucket_ms = i64::from(bucket_sec) * 1000;

		let (rows, current_cache_bytes) = {
			let store = self.inner.store.lock().unwrap();
			(store.list_present_cache_rows()?, store.sum_present_bytes()?)
		};

		let mut baseline = 0i64;
		let mut buckets: Vec<(i64, i64)> = Vec::new();
		for (created_ts_ms, bytes) in rows {
			if created_ts_ms > end_ts {
				// 创建时间在未来（时钟漂移）：不计入当前窗口。
				continue;
			}
			if created_ts_ms < start_ts {
				baseline += bytes;
				continue;
			}
			let bucket_ts = (created_ts_ms / bucket_ms) * bucket_ms;
			match buckets.last_mut() {
				Some((ts, sum)) if *ts == bucket_ts => *sum += bytes,
				_ => buckets.push((bucket_ts, bytes)),
			}
		}

		let mut series = Vec::with_capacity(buckets.len() + 1);
		if baseline > 0 {
			series.push(serde_json::json!({
				"ts_ms": start_ts,
				"cache_bytes": baseline
			}));
		}
		let mut cumulative = baseline;
		for (ts_ms, bytes) in buckets {
			cumulative += bytes;
			series.push(serde_json::json!({
				"ts_ms": ts_ms,
				"cache_bytes": cumulative
			}));
		}

		Ok(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": bucket_sec
			},
			"series": series,
			"current_cache_bytes": current_cache_bytes
		}))
	}

	/// 网络统计：核心从未记录线上字节计数器（`Session` 的 `bytes_sent` 只活在单次
	/// 传输对象里，传输结束即释放；也没有历史采样表），因此 `bytes_sent` /
	/// `bytes_recv` 无法被如实计算。
	///
	/// 这里只返回一个显式的 0 点（字段按契约给 0），不生成整段假的零曲线，也不把
	/// 设备数/在线数硬塞进字节字段——那会是伪造趋势。设备列表请走 `cb_list_peers`。
	pub fn query_net_stats(
		&self,
		start_ts_ms: i64,
		end_ts_ms: i64,
		bucket_sec: i32,
	) -> anyhow::Result<serde_json::Value> {
		let (start_ts, end_ts) = Self::stats_window(start_ts_ms, end_ts_ms, 60 * 60 * 1000);
		let bucket_sec = Self::normalize_bucket_sec(bucket_sec, 10);

		Ok(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": bucket_sec
			},
			"series": [{
				"ts_ms": end_ts,
				"bytes_sent": 0,
				"bytes_recv": 0
			}]
		}))
	}

	/// 活动统计：把 `history` 行按时间桶聚合，返回每个桶的 text/image/file_list 计数。
	///
	/// 默认窗口最近 24 小时、桶宽 60s（与旧 FFI 默认值一致）；图表因此能显示真实
	/// 摄入活动，而不是永远为空。
	pub fn query_activity_stats(
		&self,
		start_ts_ms: i64,
		end_ts_ms: i64,
		bucket_sec: i32,
	) -> anyhow::Result<serde_json::Value> {
		let (start_ts, end_ts) = Self::stats_window(start_ts_ms, end_ts_ms, 24 * 60 * 60 * 1000);
		let bucket_sec = Self::normalize_bucket_sec(bucket_sec, 60);
		let bucket_ms = i64::from(bucket_sec) * 1000;

		let buckets = {
			let store = self.inner.store.lock().unwrap();
			store.activity_buckets(start_ts, end_ts, bucket_ms)?
		};

		let series: Vec<serde_json::Value> = buckets
			.iter()
			.map(|b| {
				serde_json::json!({
					"ts_ms": b.ts_ms,
					"text_count": b.text_count,
					"image_count": b.image_count,
					"files_count": b.file_list_count
				})
			})
			.collect();

		Ok(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": bucket_sec
			},
			"series": series
		}))
	}
}

/// 启动对账：把“声称 present=1 但 blob 文件缺失”的 `content_cache` 行改回 0。
///
/// 只读取 present=1 的行并逐条 `exists` 检查；不扫描整个 cache 目录，也不删除
/// 任何 blob（没有元数据引用的孤儿由 schema 重建时的 CAS 清空处理）。
/// 返回实际更新（present=0）的行数。
fn reconcile_cache_presence(store: &Mutex<Store>, cas: &Cas, now_ms: i64) -> anyhow::Result<usize> {
	let hashes = store.lock().unwrap().list_present_cache_hashes()?;
	let missing: Vec<String> = hashes
		.into_iter()
		.filter(|sha| !cas.blob_exists(sha))
		.collect();
	if missing.is_empty() {
		return Ok(0);
	}

	// 文件检查在锁外完成，避免阻塞其他 store 调用；写回时再短暂持锁。
	let mut store = store.lock().unwrap();
	let mut updated = 0usize;
	for sha in &missing {
		updated += store.mark_cache_missing(sha, now_ms)?;
	}
	Ok(updated)
}

/// F5.2：孤儿 blob 清扫（best-effort，绝不因清扫失败影响启动/传输）。
///
/// 判据由 [`Store::list_referenced_content_hashes`] 提供（content_cache +
/// items + FileList 子文件）；只删除年龄超过
/// [`crate::cas::ORPHAN_BLOB_MIN_AGE_MS`] 且无任何引用的 blob，单次最多
/// [`crate::cas::ORPHAN_SWEEP_DELETE_BATCH`] 个。调用点：`Core::init` 启动时、
/// 以及每次入站传输最终化成功后（actor 在阻塞池里调用）。
pub(crate) fn sweep_orphan_blobs_best_effort(
	cas: &Cas,
	store: &Arc<Mutex<Store>>,
	log_sink: &LogSink,
	reason: &str,
) {
	let referenced = match store.lock().unwrap().list_referenced_content_hashes() {
		Ok(referenced) => referenced,
		Err(error) => {
			log_sink.log_warn(
				"Cache",
				&format!(
					"Orphan blob sweep skipped ({reason}): cannot list DB references: {error}"
				),
				Some(&format!(
					"孤儿 blob 清扫已跳过（{reason}）：无法读取数据库引用: {error}"
				)),
			);
			return;
		}
	};

	match cas.sweep_orphan_blobs(
		&referenced,
		now_ms(),
		crate::cas::ORPHAN_BLOB_MIN_AGE_MS,
		crate::cas::ORPHAN_SWEEP_DELETE_BATCH,
	) {
		Ok(stats) => {
			let detail = format!(
				"scanned={}, deleted={}, kept_referenced={}, kept_recent={}, kept_unknown_age={}, failed={}, freed_bytes={}",
				stats.scanned,
				stats.deleted,
				stats.kept_referenced,
				stats.kept_recent,
				stats.kept_unknown_age,
				stats.failed,
				stats.freed_bytes
			);
			log_sink.log_info(
				"Cache",
				&format!("Orphan blob sweep ({reason}): {detail}"),
				Some(&format!("孤儿 blob 清扫（{reason}）：{detail}")),
			);
		}
		Err(error) => {
			log_sink.log_warn(
				"Cache",
				&format!("Orphan blob sweep failed ({reason}): {error}"),
				Some(&format!("孤儿 blob 清扫失败（{reason}）: {error}")),
			);
		}
	}
}

#[cfg(test)]
impl Core {
	// 可以在这里添加针对 M1 的测试辅助方法
}

/// 确保本 Core 恰好启动一个后台 durability worker。
///
/// `Core::init` 与接收路径都会调用本函数；[`Cas::claim_durability_worker`] 用
/// 原子标记保证只有一个调用者真正启动线程（其余 clone 直接返回）。
///
/// 为什么是专用线程而不是 `tokio::spawn`：`Core::init` 运行在 FFI 线程上，
/// 此时 NetManager 的 runtime 还没建立，也没有任何 runtime 上下文。这里复刻
/// 日志 writer / NetManager 的“专用维护线程”模式：线程内建一个最小
/// current-thread runtime 驱动异步循环，真正的 `sync_data()` 再放进
/// `spawn_blocking`，因此同步磁盘 IO 永远不会占住 tokio worker。
pub(crate) fn ensure_durability_worker(cas: &Cas, store: Arc<Mutex<Store>>, log_sink: &LogSink) {
	let Some((rx, stop_rx)) = cas.claim_durability_worker() else {
		return;
	};
	let worker_cas = cas.clone();
	let worker_log = log_sink.clone();
	let spawned = std::thread::Builder::new()
		.name("cb-durability-sync".to_string())
		.spawn(move || {
			let rt = match tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
			{
				Ok(rt) => rt,
				Err(e) => {
					worker_log.log_error(
						"Durability",
						&format!("Failed to build durability sync runtime: {e}"),
						Some(&format!("无法创建后台持久化同步运行时: {e}")),
						Some(&e.to_string()),
					);
					// receiver 随闭包一起释放，队列会变成 closed；无法再安全地
					// 重建 channel，这里只记录错误（线程创建失败本身极罕见）。
					return;
				}
			};
			rt.block_on(durability_worker(
				rx, stop_rx, worker_cas, store, worker_log,
			));
		});
	if let Err(e) = spawned {
		log_sink.log_error(
			"Durability",
			&format!("Failed to spawn durability sync thread: {e}"),
			Some(&format!("无法启动后台持久化同步线程: {e}")),
			Some(&e.to_string()),
		);
	}
}

/// 后台 durability 主循环：处理队列 + 每 30 s 兜底 sweep。
async fn durability_worker(
	mut rx: mpsc::Receiver<PendingSync>,
	mut stop_rx: tokio::sync::watch::Receiver<bool>,
	cas: Cas,
	store: Arc<Mutex<Store>>,
	log_sink: LogSink,
) {
	let mut ticker = interval(DURABILITY_SWEEP_INTERVAL);
	ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
	// 立刻消费 interval 的第一次 tick，让第一次“周期”sweep 真正发生在 30 s 后；
	// 启动时的补同步由下面这次显式 sweep 完成。
	ticker.tick().await;

	// 启动先扫一次：上次进程在 `durable=0` 状态下退出/崩溃时，这些内容必须
	// 在本次启动后补同步，而不是等下一次大文件传输来“顺带”唤醒队列。
	sweep_durability_pending(&cas, &store, &log_sink).await;

	loop {
		if *stop_rx.borrow() {
			break;
		}
		tokio::select! {
			_ = stop_rx.changed() => break,
			request = rx.recv() => match request {
				Some(req) => {
					sync_pending_entry(&cas, &store, &log_sink, &req).await;
				}
				None => break,
			},
			_ = ticker.tick() => {
				sweep_durability_pending(&cas, &store, &log_sink).await;
			}
		}
	}
}

/// 对一条 pending 记录执行 `sync_data()`，成功后把 `durable` 翻回 1。
///
/// 返回 `true` 表示 blob 已同步且库里的标记已翻转。任何一步失败都只记录 warn，
/// 行保持 `durable=0`，由后续 sweep 重试（绝不因为同步失败回滚已完成的传输）。
pub(crate) async fn sync_pending_entry(
	cas: &Cas,
	store: &Arc<Mutex<Store>>,
	log_sink: &LogSink,
	request: &PendingSync,
) -> bool {
	let path = match cas.blob_path(&request.blob_sha256) {
		Ok(path) => path,
		Err(e) => {
			log_sink.log_warn(
				"Durability",
				&format!(
					"durable sync skipped: invalid blob sha256={}: {e}",
					request.blob_sha256
				),
				Some(&format!(
					"后台持久化同步跳过：blob SHA256 非法 {}: {e}",
					request.blob_sha256
				)),
			);
			return false;
		}
	};

	if let Err(e) = sync_data_blocking(cas, path.clone()).await {
		log_sink.log_warn(
			"Durability",
			&format!(
				"durable sync failed for blob {} ({}): {e}; leaving durable=0 for the 30s sweep",
				request.blob_sha256,
				path.display()
			),
			Some(&format!(
				"后台持久化同步失败: blob={}（{}）: {e}；保持 durable=0，等待 30s 周期 sweep 重试",
				request.blob_sha256,
				path.display()
			)),
		);
		return false;
	}

	let store_for_update = store.clone();
	let cache_sha = request.cache_sha256.clone();
	match tokio::task::spawn_blocking(move || {
		store_for_update
			.lock()
			.unwrap()
			.mark_cache_durable(&cache_sha)
	})
	.await
	{
		Ok(Ok(0)) => {
			// 行已被删除/GC：blob 同步本身成功，但没有标记可翻。
			log_sink.log_warn(
				"Durability",
				&format!(
					"durable sync succeeded but no content_cache row matched sha256={}",
					request.cache_sha256
				),
				Some(&format!(
					"后台持久化同步成功，但没有匹配的 content_cache 行: sha256={}",
					request.cache_sha256
				)),
			);
			false
		}
		Ok(Ok(_)) => true,
		Ok(Err(e)) => {
			log_sink.log_warn(
				"Durability",
				&format!(
					"durable sync succeeded but marking durable=1 failed for sha256={}: {e}",
					request.cache_sha256
				),
				Some(&format!(
					"后台持久化同步成功，但写回 durable=1 失败: sha256={}: {e}",
					request.cache_sha256
				)),
			);
			false
		}
		Err(e) => {
			log_sink.log_warn(
				"Durability",
				&format!(
					"durable sync marking task failed for sha256={}: {e}",
					request.cache_sha256
				),
				Some(&format!(
					"后台持久化标记任务失败: sha256={}: {e}",
					request.cache_sha256
				)),
			);
			false
		}
	}
}

/// 30 s 兜底 sweep：扫描 `content_cache.durable=0`，逐个补 `sync_data`。
///
/// 解析候选 blob 时使用 [`Store::sync_candidates_for_cache_sha`]：`content_cache`
/// 的键是 item content sha，而 FileList 传输的 blob 名是单个文件 sha，两者不同；
/// 候选列表同时覆盖两种情形。找不到任何 blob 时保持 pending 并记 warn。
pub(crate) async fn sweep_durability_pending(
	cas: &Cas,
	store: &Arc<Mutex<Store>>,
	log_sink: &LogSink,
) -> usize {
	let store_for_query = store.clone();
	let pending = match tokio::task::spawn_blocking(move || {
		store_for_query
			.lock()
			.unwrap()
			.select_durability_pending(DURABILITY_SWEEP_BATCH)
	})
	.await
	{
		Ok(Ok(rows)) => rows,
		Ok(Err(e)) => {
			log_sink.log_warn(
				"Durability",
				&format!("durability sweep query failed: {e}"),
				Some(&format!("后台持久化 sweep 查询失败: {e}")),
			);
			return 0;
		}
		Err(e) => {
			log_sink.log_warn(
				"Durability",
				&format!("durability sweep query task failed: {e}"),
				Some(&format!("后台持久化 sweep 查询任务失败: {e}")),
			);
			return 0;
		}
	};

	let mut synced = 0usize;
	for cache_sha in pending {
		let store_for_candidates = store.clone();
		let key = cache_sha.clone();
		let candidates = match tokio::task::spawn_blocking(move || {
			store_for_candidates
				.lock()
				.unwrap()
				.sync_candidates_for_cache_sha(&key)
		})
		.await
		{
			Ok(Ok(v)) => v,
			Ok(Err(e)) => {
				log_sink.log_warn(
					"Durability",
					&format!("durability sweep candidate lookup failed for {cache_sha}: {e}"),
					Some(&format!(
						"后台持久化 sweep 解析候选 blob 失败: {cache_sha}: {e}"
					)),
				);
				continue;
			}
			Err(e) => {
				log_sink.log_warn(
					"Durability",
					&format!("durability sweep candidate task failed for {cache_sha}: {e}"),
					Some(&format!("后台持久化 sweep 候选任务失败: {cache_sha}: {e}")),
				);
				continue;
			}
		};

		let mut synced_any = false;
		for blob_sha in &candidates {
			let Ok(path) = cas.blob_path(blob_sha) else {
				continue;
			};
			if !path.exists() {
				continue;
			}
			match sync_data_blocking(cas, path).await {
				Ok(()) => synced_any = true,
				Err(e) => log_sink.log_warn(
					"Durability",
					&format!(
						"durable sweep sync failed for blob {blob_sha} (cache sha {cache_sha}): {e}; leaving durable=0"
					),
					Some(&format!(
						"后台持久化 sweep 同步失败: blob={blob_sha}（缓存键 {cache_sha}）: {e}；保持 durable=0"
					)),
				),
			}
		}

		if !synced_any {
			log_sink.log_warn(
				"Durability",
				&format!(
					"durable sweep found no existing blob for cache sha {cache_sha}; leaving durable=0"
				),
				Some(&format!(
					"后台持久化 sweep 未找到缓存键 {cache_sha} 对应的磁盘 blob；保持 durable=0"
				)),
			);
			continue;
		}

		let store_for_update = store.clone();
		let key = cache_sha.clone();
		match tokio::task::spawn_blocking(move || {
			store_for_update.lock().unwrap().mark_cache_durable(&key)
		})
		.await
		{
			Ok(Ok(_)) => synced += 1,
			Ok(Err(e)) => log_sink.log_warn(
				"Durability",
				&format!("durable sweep failed to flip durable=1 for {cache_sha}: {e}"),
				Some(&format!(
					"后台持久化 sweep 写回 durable=1 失败: {cache_sha}: {e}"
				)),
			),
			Err(e) => log_sink.log_warn(
				"Durability",
				&format!("durable sweep update task failed for {cache_sha}: {e}"),
				Some(&format!("后台持久化 sweep 更新任务失败: {cache_sha}: {e}")),
			),
		}
	}
	synced
}

/// 在 `spawn_blocking` 里执行 `sync_data()`，绝不让磁盘 IO 占住 tokio worker。
async fn sync_data_blocking(cas: &Cas, path: std::path::PathBuf) -> std::io::Result<()> {
	let cas = cas.clone();
	tokio::task::spawn_blocking(move || cas.sync_data_file(&path))
		.await
		.map_err(std::io::Error::other)?
}

pub struct ContentFetchRequest {
	pub item_id: String,
	pub file_id: Option<String>,
	// pub prefer_peer: Option<String>, // 未来 M3 完整版添加
	// pub is_force: bool,
}

pub struct Inner {
	pub core_config: CoreConfig,
	pub sink: Arc<dyn CoreEventSink>,
	pub is_shutdown: AtomicBool,
	pub store: Arc<Mutex<Store>>,
	/// 查询/删除路径共享的同步 LogStore（FFI 线程使用，允许阻塞）。
	pub log_store: Arc<Mutex<LogStore>>,
	/// 运行时路径使用的非阻塞日志入口（内部有专用 writer 线程）。
	pub log_sink: LogSink,
	pub cas: Cas,
	pub net: Option<mpsc::Sender<NetCmd>>,
}

impl Inner {
	fn emit(&self, event_json: String) {
		// shutdown 后不允许再回调
		if self.is_shutdown.load(Ordering::Acquire) {
			return;
		}
		self.sink.emit(event_json);
	}

	#[allow(clippy::needless_pass_by_value)]
	pub(crate) fn emit_json(&self, v: serde_json::Value) {
		self.emit(v.to_string());
	}

	fn shutdown(&self) {
		let _already = self.is_shutdown.swap(true, Ordering::AcqRel);

		if let Some(tx) = &self.net {
			let _ = tx.try_send(NetCmd::Shutdown);
		}

		// 通知后台 durability worker 退出（不 join：与 NetManager 一样尽力而为，
		// 正在进行的单次 sync_data 会自然完成）。
		self.cas.stop_durability_worker();

		// 顺序：先通知 NetManager 停机（此后它多数日志会自然减少），再关闭 sink。
		// `flush_and_join` 先停止接收新记录，再让 writer 排空已入队的记录并
		// 在超时内 join。因此 shutdown 返回时，所有在关闭前成功入队的日志都已
		// 提交；NetManager 停机过程中仍可能并发调用 log_*，这些记录会被丢弃
		// 并计入 sink 的 dropped 计数，绝不会让关闭流程阻塞。
		//
		// 不能反过来先 flush 再通知 NetManager：那样停机期间的日志会落在
		// 已关闭的队列上，丢失更多。这里的取舍是“保证调用方已入队日志不丢”，
		// 而不是“等待 NetManager 完全停止”（它没有 join handle）。
		let _ = self
			.log_sink
			.flush_and_join(crate::logs::LOG_SINK_JOIN_TIMEOUT);
	}
}

impl Drop for Inner {
	fn drop(&mut self) {
		// 最后一个 `Core` 句柄释放时停止后台 durability worker：该线程持有
		// `Store` 的 Arc 克隆，不退出会延迟数据库/临时目录释放（Windows 上尤其明显）。
		// `Core::shutdown` 已经发过停止信号时，这里是幂等重复。
		self.cas.stop_durability_worker();
	}
}

#[cfg(test)]
mod tests;

/// `clear_cache` 回归测试：清缓存只能清“内容”，不能把历史元数据一起藏起来。
#[cfg(test)]
mod clear_cache_regression {
	use super::*;

	struct NoopSink;
	impl CoreEventSink for NoopSink {
		fn emit(&self, _event_json: String) {}
	}

	#[test]
	fn clear_cache_keeps_history_visible() {
		let root = tempfile::tempdir().unwrap();
		let cfg = CoreConfig {
			device_id: "dev-clear".to_string(),
			device_name: "dev".to_string(),
			account_uid: "acct-clear".to_string(),
			account_password: "test_password".to_string(),
			data_dir: root.path().join("data").to_string_lossy().into_owned(),
			cache_dir: root.path().join("cache").to_string_lossy().into_owned(),
			app_config: AppConfig::default(),
			disable_network: true,
		};
		let core = Core::init(cfg, Arc::new(NoopSink));

		let meta = core
			.ingest_local_copy(ClipboardSnapshot::Text {
				text_utf8: "history must survive clear_cache".to_string(),
				ts_ms: now_ms(),
			})
			.unwrap();
		let sha = meta.content.sha256.clone();
		assert!(core.inner.cas.blob_exists(&sha));

		core.clear_cache().unwrap();

		// 内容确实清掉了
		assert!(!core.inner.cas.blob_exists(&sha));
		let present = core
			.inner
			.store
			.lock()
			.unwrap()
			.get_cache_present(&sha)
			.unwrap();
		assert!(!present, "clear_cache 后 present 必须为 0");

		// 但历史/元数据查询必须仍能看到这条记录
		let list = core.list_history(10, None).unwrap();
		assert!(
			list.iter().any(|m| m.item_id == meta.item_id),
			"clear_cache 之后 list_history 不应丢失历史记录"
		);
		assert!(
			core.get_item_meta(&meta.item_id).unwrap().is_some(),
			"clear_cache 之后 get_item_meta 不应返回 None"
		);

		core.shutdown();
	}
}

/// 关闭顺序回归：sink 中已入队的日志必须在 `Core::shutdown` 返回前落库。
#[cfg(test)]
mod log_sink_shutdown {
	use super::*;

	struct NoopSink;
	impl CoreEventSink for NoopSink {
		fn emit(&self, _event_json: String) {}
	}

	#[test]
	fn shutdown_flushes_sink_queued_logs() {
		let root = tempfile::tempdir().unwrap();
		let data_dir = root.path().join("data");
		let cfg = CoreConfig {
			device_id: "dev-log-flush".to_string(),
			device_name: "dev".to_string(),
			account_uid: "acct-log-flush".to_string(),
			account_password: "test_password".to_string(),
			data_dir: data_dir.to_string_lossy().into_owned(),
			cache_dir: root.path().join("cache").to_string_lossy().into_owned(),
			app_config: AppConfig::default(),
			disable_network: true,
		};
		let core = Core::init(cfg, Arc::new(NoopSink));

		let marker = format!("shutdown-flush-{}", now_ms());
		core.inner.log_sink.log_info(
			"Test",
			&format!("record queued before shutdown: {marker}"),
			Some(&format!("关闭前入队的记录: {marker}")),
		);
		assert_eq!(
			core.inner.log_sink.dropped_count(),
			0,
			"小批量入队不应触发丢弃"
		);

		// shutdown 内部会关闭队列、排空并 join writer；返回即代表已落库。
		core.shutdown();
		drop(core);

		// writer 已退出、连接已释放，重新打开日志库必须能看到该记录。
		let store = LogStore::open(&data_dir).unwrap();
		let entries = store.query_latest(0, Some(&marker), 10, None).unwrap();
		assert_eq!(entries.len(), 1, "shutdown 前入队的日志必须已落库");
		assert!(entries[0].message.contains(&marker));
	}
}

/// 统计接口回归：`stats.db` 已移除，三个查询必须从现存数据现算出真实内容。
#[cfg(test)]
mod stats_reporting {
	use super::*;

	struct NoopSink;
	impl CoreEventSink for NoopSink {
		fn emit(&self, _event_json: String) {}
	}

	#[test]
	fn computed_stats_are_non_empty_and_no_stats_db_is_created() {
		let root = tempfile::tempdir().unwrap();
		let data_dir = root.path().join("data");
		std::fs::create_dir_all(&data_dir).unwrap();
		// 模拟旧版本遗留文件：启动时应被尽力删除。
		std::fs::write(data_dir.join("stats.db"), b"legacy").unwrap();

		let cfg = CoreConfig {
			device_id: "dev-stats".to_string(),
			device_name: "dev".to_string(),
			account_uid: "acct-stats".to_string(),
			account_password: "test_password".to_string(),
			data_dir: data_dir.to_string_lossy().into_owned(),
			cache_dir: root.path().join("cache").to_string_lossy().into_owned(),
			app_config: AppConfig::default(),
			disable_network: true,
		};
		let core = Core::init(cfg, Arc::new(NoopSink));

		let ts = now_ms();
		core.ingest_local_copy(ClipboardSnapshot::Text {
			text_utf8: "stats item one".to_string(),
			ts_ms: ts,
		})
		.unwrap();
		core.ingest_local_copy(ClipboardSnapshot::Text {
			text_utf8: "stats item two".to_string(),
			ts_ms: ts,
		})
		.unwrap();

		// cache_stats：当前占用非零，series 结构化且为累计字节。
		let cache = core.query_cache_stats(0, 0, 10).unwrap();
		assert!(
			cache["current_cache_bytes"].as_i64().unwrap() > 0,
			"ingest 之后 current_cache_bytes 必须大于 0"
		);
		let cache_series = cache["series"].as_array().unwrap();
		assert!(!cache_series.is_empty(), "cache series 不应为空");
		assert!(
			cache_series
				.iter()
				.all(|p| p["ts_ms"].is_i64() && p["cache_bytes"].as_i64().is_some_and(|b| b > 0)),
			"cache series 每个点都必须带 ts_ms 与正的 cache_bytes"
		);

		// activity_stats：两次 text ingest 的计数必须能对上。
		let activity = core.query_activity_stats(0, 0, 60).unwrap();
		let activity_series = activity["series"].as_array().unwrap();
		assert!(!activity_series.is_empty(), "activity series 不应为空");
		let text_total: i64 = activity_series
			.iter()
			.map(|p| p["text_count"].as_i64().unwrap())
			.sum();
		assert_eq!(text_total, 2);
		assert!(activity_series
			.iter()
			.all(|p| p["image_count"].is_i64() && p["files_count"].is_i64()));

		// net_stats：没有字节计数器，字段按契约给 0（不伪造趋势）。
		let net = core.query_net_stats(0, 0, 10).unwrap();
		let net_series = net["series"].as_array().unwrap();
		assert!(!net_series.is_empty(), "net series 应返回显式 0 点");
		assert_eq!(net_series[0]["bytes_sent"].as_i64(), Some(0));
		assert_eq!(net_series[0]["bytes_recv"].as_i64(), Some(0));

		// stats.db 不应再被创建，旧的也应被删除。
		assert!(!data_dir.join("stats.db").exists(), "stats.db 不应存在");
		assert!(!data_dir.join("stats.db-wal").exists());
		assert!(!data_dir.join("stats.db-shm").exists());

		core.shutdown();
	}
}

/// 启动对账回归：`present=1` 但 blob 丢失时，下次启动必须改回 `present=0`。
#[cfg(test)]
mod startup_reconciliation {
	use super::*;

	struct NoopSink;
	impl CoreEventSink for NoopSink {
		fn emit(&self, _event_json: String) {}
	}

	#[test]
	fn missing_blob_is_marked_absent_on_next_startup() {
		let root = tempfile::tempdir().unwrap();
		let cfg = CoreConfig {
			device_id: "dev-reconcile".to_string(),
			device_name: "dev".to_string(),
			account_uid: "acct-reconcile".to_string(),
			account_password: "test_password".to_string(),
			data_dir: root.path().join("data").to_string_lossy().into_owned(),
			cache_dir: root.path().join("cache").to_string_lossy().into_owned(),
			app_config: AppConfig::default(),
			disable_network: true,
		};

		let core = Core::init(cfg.clone(), Arc::new(NoopSink));
		let meta = core
			.ingest_local_copy(ClipboardSnapshot::Text {
				text_utf8: "reconcile me".to_string(),
				ts_ms: now_ms(),
			})
			.unwrap();
		let sha = meta.content.sha256.clone();
		let blob = core.inner.cas.blob_path(&sha).unwrap();
		assert!(blob.exists());
		// 模拟崩溃/外部清理：元数据还在，blob 已丢失。
		std::fs::remove_file(blob).unwrap();
		core.shutdown();
		drop(core);

		let reopened = Core::init(cfg, Arc::new(NoopSink));
		let present = reopened
			.inner
			.store
			.lock()
			.unwrap()
			.get_cache_present(&sha)
			.unwrap();
		assert!(!present, "blob 缺失的行必须在对账后 present=0");
		reopened.shutdown();
	}
}

/// 性能回归：摄入不再为每次拷贝做 O(缓存文件数) 的 CAS 整树遍历，
/// 但“超过上限必须驱逐”的语义不能丢。
#[cfg(test)]
mod ingest_gc_walk_regression {
	use super::*;

	struct NoopSink;
	impl CoreEventSink for NoopSink {
		fn emit(&self, _event_json: String) {}
	}

	fn make_core(root: &std::path::Path, gc_cas_max_bytes: i64) -> Core {
		let cfg = CoreConfig {
			device_id: "dev-ingest-gc-walk".to_string(),
			device_name: "dev".to_string(),
			account_uid: "acct-ingest-gc-walk".to_string(),
			account_password: "test_password".to_string(),
			data_dir: root.join("data").to_string_lossy().into_owned(),
			cache_dir: root.join("cache").to_string_lossy().into_owned(),
			app_config: AppConfig {
				gc_history_max_items: 50_000,
				gc_cas_max_bytes,
				..Default::default()
			},
			disable_network: true,
		};
		Core::init(cfg, Arc::new(NoopSink))
	}

	fn full_walk_count() -> usize {
		GC_CAS_FULL_WALKS.with(std::cell::Cell::get)
	}

	/// 缓存远低于上限时，N 次摄入不应触发任何一次 CAS 整树遍历。
	#[test]
	fn ingest_under_limit_skips_full_cas_walk() {
		let root = tempfile::tempdir().unwrap();
		// 1 GiB 上限；20 条 ~120 B 文本的账面占用远低于 90% 阈值。
		let core = make_core(root.path(), 1024 * 1024 * 1024);
		let walks_after_init = full_walk_count();

		for i in 0..20usize {
			core.ingest_local_copy(ClipboardSnapshot::Text {
				text_utf8: format!("gc-walk-under-limit {i:02} {}", "x".repeat(100)),
				ts_ms: now_ms() + i as i64,
			})
			.unwrap();
		}

		let present_bytes = core
			.inner
			.store
			.lock()
			.unwrap()
			.sum_present_bytes()
			.unwrap();
		assert!(present_bytes > 0, "ingest 之后缓存账面占用必须非零");
		assert_eq!(
			full_walk_count(),
			walks_after_init,
			"缓存远低于上限时 ingest 不应触发 run_gc 的整树遍历"
		);

		core.shutdown();
	}

	/// 上限很小、账面占用跨过 90% 阈值时，完整核算 + LRU 驱逐必须照常发生。
	#[test]
	fn tiny_cap_still_evicts_when_exceeded() {
		let root = tempfile::tempdir().unwrap();
		// 50 B 上限：第二条 40 B 内容摄入后账面 80 B >= 45 B（90%），必须触发
		// 完整核算并驱逐 LRU，真实磁盘占用最终回到上限以内。
		let core = make_core(root.path(), 50);
		let walks_after_init = full_walk_count();

		let b1 = vec![1u8; 40];
		let b2 = vec![2u8; 40];
		let sha1 = crate::util::sha256_hex(&b1);
		let sha2 = crate::util::sha256_hex(&b2);

		core.ingest_local_copy(ClipboardSnapshot::Image {
			bytes: b1,
			mime: "image/png".to_string(),
			ts_ms: now_ms(),
		})
		.unwrap();
		core.ingest_local_copy(ClipboardSnapshot::Image {
			bytes: b2,
			mime: "image/png".to_string(),
			ts_ms: now_ms() + 1,
		})
		.unwrap();

		assert!(
			full_walk_count() > walks_after_init,
			"接近/超过上限时 run_gc 必须做完整账实核算"
		);

		let real = core.inner.cas.total_size_bytes().unwrap();
		assert!(
			real <= 50,
			"驱逐后真实磁盘占用必须回到上限内，实际 {real} B"
		);

		// LRU 时间戳可能同毫秒，具体驱逐哪一条由 sha 兜底排序决定：只断言
		// 恰好留下一条，且 content_cache 的 present 与磁盘上的 blob 一致。
		let present = {
			let store = core.inner.store.lock().unwrap();
			(
				store.get_cache_present(&sha1).unwrap(),
				store.get_cache_present(&sha2).unwrap(),
			)
		};
		assert_eq!(
			usize::from(present.0) + usize::from(present.1),
			1,
			"超限时必须驱逐到只剩一条 present"
		);
		assert_eq!(
			core.inner.cas.blob_exists(&sha1),
			present.0,
			"sha1 的 present 必须与磁盘一致"
		);
		assert_eq!(
			core.inner.cas.blob_exists(&sha2),
			present.1,
			"sha2 的 present 必须与磁盘一致"
		);

		core.shutdown();
	}
}

/// 后台 durability worker 的无 socket 测试。
///
/// 覆盖：`sync_pending_entry` 成功后把 durable 翻回 1；30 s sweep 只处理
/// `durable=0` 的行，且对“缓存键 ≠ blob 名”的 FileList 形态（候选解析）也能工作。
#[cfg(test)]
mod durability_worker_tests {
	use super::*;
	use crate::cas::PendingSync;

	fn temp_log_sink(dir: &std::path::Path) -> LogSink {
		let store = LogStore::open(dir).expect("open log store");
		LogSink::new(Arc::new(Mutex::new(store)))
	}

	fn insert_cache_row(store: &mut Store, sha: &str, present: bool, durable: bool) {
		store
			.conn
			.execute(
				r"INSERT INTO content_cache
                   (sha256_hex, total_bytes, present, durable, last_access_ts_ms, created_ts_ms)
                   VALUES (?1, 16, ?2, ?3, 1, 1)",
				rusqlite::params![sha, i32::from(present), i32::from(durable)],
			)
			.unwrap();
	}

	fn write_blob(cas: &Cas, sha: &str) {
		let path = cas.blob_path(sha).unwrap();
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, b"durable-payload").unwrap();
	}

	#[tokio::test]
	async fn sync_pending_entry_flips_durable_on_success() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path().join("cache")).unwrap();
		let sha = "a".repeat(64);
		write_blob(&cas, &sha);

		let mut store = Store::open(dir.path().join("data")).unwrap();
		insert_cache_row(&mut store, &sha, true, false);
		let store = Arc::new(Mutex::new(store));
		let log = temp_log_sink(dir.path());

		let ok = sync_pending_entry(
			&cas,
			&store,
			&log,
			&PendingSync {
				blob_sha256: sha.clone(),
				cache_sha256: sha.clone(),
			},
		)
		.await;
		assert!(ok, "同步成功必须返回 true");
		assert!(store.lock().unwrap().get_cache_durable(&sha).unwrap());
		assert_eq!(
			store.lock().unwrap().select_durability_pending(10).unwrap(),
			Vec::<String>::new()
		);
	}

	#[tokio::test]
	async fn sync_pending_entry_leaves_pending_when_blob_missing() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path().join("cache")).unwrap();
		let sha = "b".repeat(64);
		// 不创建 blob：sync_data 必须失败，durable 保持 0。
		let mut store = Store::open(dir.path().join("data")).unwrap();
		insert_cache_row(&mut store, &sha, true, false);
		let store = Arc::new(Mutex::new(store));
		let log = temp_log_sink(dir.path());

		let ok = sync_pending_entry(
			&cas,
			&store,
			&log,
			&PendingSync {
				blob_sha256: sha.clone(),
				cache_sha256: sha.clone(),
			},
		)
		.await;
		assert!(!ok);
		assert!(!store.lock().unwrap().get_cache_durable(&sha).unwrap());
	}

	#[tokio::test]
	async fn sweep_syncs_only_pending_rows_and_resolves_file_list_blobs() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path().join("cache")).unwrap();

		let item_sha = "c".repeat(64); // content_cache 键（item content sha）
		let file_sha = "d".repeat(64); // FileList 实际 blob 名（单文件 sha）
		write_blob(&cas, &file_sha);

		let mut store = Store::open(dir.path().join("data")).unwrap();
		insert_cache_row(&mut store, &item_sha, true, false);
		// 已 durable 的行不能被 sweep 触碰。
		let done_sha = "e".repeat(64);
		write_blob(&cas, &done_sha);
		insert_cache_row(&mut store, &done_sha, true, true);
		// items 记录把缓存键映射到 FileList 的文件 sha。
		store
			.conn
			.execute(
				r"INSERT INTO items
                   (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, files_json)
                   VALUES ('item-1', 'file_list', 'dev-1', 1, 16, 'application/x-clipbridge-files', ?1, ?2)",
				rusqlite::params![
					item_sha,
					format!(
						r#"[{{"file_id":"f1","rel_name":"a.bin","size_bytes":16,"sha256":"{file_sha}","local_path":null}}]"#
					)
				],
			)
			.unwrap();
		let store = Arc::new(Mutex::new(store));
		let log = temp_log_sink(dir.path());

		let synced = sweep_durability_pending(&cas, &store, &log).await;
		assert_eq!(synced, 1, "只有 durable=0 的行应被 sweep");
		assert!(store.lock().unwrap().get_cache_durable(&item_sha).unwrap());
		assert!(store.lock().unwrap().get_cache_durable(&done_sha).unwrap());
		assert_eq!(
			store.lock().unwrap().select_durability_pending(10).unwrap(),
			Vec::<String>::new()
		);
	}
}
