// cb_core/src/api.rs

use std::sync::{
	atomic::{AtomicBool, Ordering},
	Arc, Mutex,
};
use tokio::sync::mpsc;

use crate::clipboard::{make_ingest_plan, ClipboardSnapshot, IngestPlan, LocalIngestDeps};
pub(crate) use crate::model::ItemMeta;
use crate::net::{NetCmd, NetManager};
pub use crate::policy::{AppConfig, GlobalPolicy};
use crate::{cas::Cas, logs::LogStore, store::Store, util::now_ms};

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

		let cas = match Cas::new(&cfg.cache_dir) {
			Ok(c) => {
				let mut log_store = log_store_arc.lock().unwrap();
				let _ = log_store.log_info(
					"Init",
					"CAS initialized successfully",
					Some("CAS 初始化成功"),
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
				log_store_arc.clone(),
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
			cas,
			net: net_tx,
		};
		let core = Self {
			inner: Arc::new(inner),
		};
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
				if let Ok(mut log_store) = self.inner.log_store.lock() {
					let _ = log_store.log_error(
						"Ingest",
						&format!("Ingest planning failed: {e}"),
						Some(&format!("内容摄入规划失败: {e}")),
						Some(&e.to_string()),
					);
				}
				return Err(e);
			}
		};
		// Step 2：真正落库 + CAS + 发事件
		match self.apply_ingest(&plan) {
			Ok(meta) => Ok(meta),
			Err(e) => {
				if let Ok(mut log_store) = self.inner.log_store.lock() {
					let _ = log_store.log_error(
						"Ingest",
						&format!("Ingest failed: {e}"),
						Some(&format!("内容摄入失败: {e}")),
						Some(&e.to_string()),
					);
				}
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

		// 记录规划日志
		{
			let mut log_store = self.inner.log_store.lock().unwrap();
			let _ = log_store.log_info(
				"Ingest",
				&format!(
					"Planning ingest for clipboard content, type: {content_type}, size: {size_bytes} bytes"
				),
				Some(&format!(
					"正在规划剪贴板内容摄入，类型: {content_type}，大小: {size_bytes} 字节"
				)),
			);
		}

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
				let mut log_store = self.inner.log_store.lock().unwrap();
				let _ = log_store.log_info(
					"Ingest",
					&format!("CAS blob written successfully: {sha}"),
					Some(&format!("CAS 对象写入成功: {sha}")),
				);
			} else {
				let mut log_store = self.inner.log_store.lock().unwrap();
				let _ = log_store.log_error(
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
			let mut log_store = self.inner.log_store.lock().unwrap();
			let _ = log_store.log_debug(
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
		{
			let mut log_store = self.inner.log_store.lock().unwrap();
			let _ = log_store.log_info(
				"Ingest",
				&format!("Ingest applied successfully, item_id: {item_id}"),
				Some(&format!("内容摄入成功，项目 ID: {item_id}")),
			);
		}

		// --- M1 集成：广播元数据到网络 ---
		if let Some(net_tx) = &self.inner.net {
			// 使用 try_send 避免阻塞，如果通道满了或网络层挂了也不影响本地逻辑
			let _ = net_tx.try_send(NetCmd::BroadcastMeta(Box::new(plan.meta.clone())));
		}

		// GC（现在一定不会死锁）
		let _ = self.run_gc("AfterIngest");

		Ok(meta_clone)
	}

	pub fn run_gc(&self, _reason: &str) -> anyhow::Result<()> {
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
			// 真实占用 = blobs + files/ 视图 + downloads/ 视图（按 inode 去重，硬链接只计一次）
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
				// 删除 blob 时同步删除引用它的视图链接；视图被占用而删不掉时
				// remove_blob 会报错并保留 blob，本轮直接放弃，避免死循环。
				if self.inner.cas.remove_blob(&sha).is_err() {
					break;
				}
				{
					let mut store = self.inner.store.lock().unwrap();
					store.mark_cache_missing(&sha, now)?;
				}
				// 删除后重新核算真实占用，而不是相信一个“已释放字节数”的预估值：
				// 硬链接共享 inode，只有最后一个链接消失时磁盘空间才真正下降。
				let after = self.inner.cas.total_size_bytes()?;
				if after >= cur {
					break;
				}
				cur = after;
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

#[cfg(test)]
impl Core {
	// 可以在这里添加针对 M1 的测试辅助方法
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
	pub log_store: Arc<Mutex<LogStore>>,
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
