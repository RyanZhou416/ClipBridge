// cb_core/src/api.rs

use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use tokio::sync::mpsc;

use crate::clipboard::{make_ingest_plan, ClipboardSnapshot, IngestPlan, LocalIngestDeps};
pub(crate) use crate::model::ItemMeta;
use crate::net::{NetCmd, NetManager};
use crate::{cas::Cas, store::Store, logs::LogStore, stats::StatsStore, util::now_ms};
pub use crate::policy::{AppConfig, GlobalPolicy};

/**
 * Core 的配置项。
 *
 * 定义了 Core 启动所需的设备 ID、名称、账户信息、存储路径以及各项限制策略。
 */
#[derive(Clone, Debug)]
#[derive(Default)]
pub struct CoreConfig {
    pub device_id: String,     // 本机 device_id（先由壳传入）
    pub device_name: String,   // 设备显示名
    pub account_uid: String,   // 本机当前账号域（history 分区键，账号明文）
    pub account_password: String,   // 账号密码（用于OPAQUE握手验证）
    pub data_dir: String,      // 持久：core.db
    pub cache_dir: String,     // 可清空：CAS blobs/tmp
	pub app_config: AppConfig,
}

/**
 * 摄入计划的评估结果。
 *
 * 用于告知调用方该次摄入的元数据信息、是否需要用户确认以及采取的摄入策略。
 */
#[derive(Clone, Debug)]
pub struct PlanResult {
    pub meta: crate::model::ItemMeta,
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
    pub device_name: Option<String>,      // 设备名称（从 HELLO 或 discovery 获取）
    pub state: PeerConnectionState,       // 连接状态
    pub last_seen_ts_ms: i64,            // 最后见到时间
    pub share_to_peer: bool,             // Outbound allow（策略状态）
    pub accept_from_peer: bool,          // Inbound allow（策略状态）
}

/// 详细的连接状态枚举
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub enum PeerConnectionState {
    Discovered,      // 知道地址，但在退避或未连接
    Connecting,      // 正在 TCP/QUIC 握手
    TransportReady,
    AccountVerifying,
    AccountVerified, // 账号已验证，正在查 Policy/TOFU
    Online,          // 完全可用
    Backoff,
    Offline,         // 彻底断开
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
        let log_store_arc = Arc::new(Mutex::new(log_store));
        
        // 记录初始化开始日志
        {
            let mut log_store = log_store_arc.lock().unwrap();
            let _ = log_store.log_info(
                "Init",
                &format!("Core initializing with device_id: {}", cfg.device_id),
                Some(&format!("核心正在初始化，设备 ID: {}", cfg.device_id)),
            );
        }

        // 初始化各个组件
        let store = match Store::open(&cfg.data_dir) {
            Ok(s) => {
                let mut log_store = log_store_arc.lock().unwrap();
                let _ = log_store.log_info(
                    "Init",
                    "Store initialized successfully",
                    Some("存储初始化成功"),
                );
                s
            }
            Err(e) => {
                let mut log_store = log_store_arc.lock().unwrap();
                let _ = log_store.log_error(
                    "Init",
                    &format!("Failed to initialize Store: {}", e),
                    Some(&format!("存储初始化失败: {}", e)),
                    Some(&e.to_string()),
                );
                panic!("Failed to open store: {}", e);
            }
        };
        let store_arc = Arc::new(Mutex::new(store));

        let stats_store = match StatsStore::open(&cfg.data_dir) {
            Ok(s) => {
                let mut log_store = log_store_arc.lock().unwrap();
                let _ = log_store.log_info(
                    "Init",
                    "StatsStore initialized successfully",
                    Some("统计存储初始化成功"),
                );
                s
            }
            Err(e) => {
                let mut log_store = log_store_arc.lock().unwrap();
                let _ = log_store.log_error(
                    "Init",
                    &format!("Failed to initialize StatsStore: {}", e),
                    Some(&format!("统计存储初始化失败: {}", e)),
                    Some(&e.to_string()),
                );
                panic!("Failed to open stats store: {}", e);
            }
        };
        let stats_store_arc = Arc::new(Mutex::new(stats_store));

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
                    &format!("Failed to initialize CAS: {}", e),
                    Some(&format!("CAS 初始化失败: {}", e)),
                    Some(&e.to_string()),
                );
                panic!("Failed to init cas: {}", e);
            }
        };

        // --- M1 集成：启动网络管理器 ---
        // 注意：这里假设 NetManager::spawn 是同步封装（内部 spawn 异步任务）
        // 如果是在 FFI 环境且有 Tokio Runtime，这将正常工作。
        let net_tx = match NetManager::spawn(cfg.clone(), sink.clone(), store_arc.clone(),cas.clone(), log_store_arc.clone()) {
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
                    &format!("NetManager failed to start: {} (continuing without network)", e),
                    Some(&format!("网络管理器启动失败: {}（将在无网络模式下继续）", e)),
                );
                None
            }
        };

        let inner = Inner {
			core_config: cfg,
            sink,
            is_shutdown: AtomicBool::new(false),
            store: store_arc,
            log_store: log_store_arc,
            stats_store: stats_store_arc,
            cas,
            net: net_tx,
        };
        let core = Self { inner: Arc::new(inner) };
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
    pub fn ingest_local_copy(&self, snapshot: ClipboardSnapshot) -> anyhow::Result<ItemMeta> {
        // Step 1：只做计划，不碰 DB/CAS
        let plan = match self.plan_local_ingest(&snapshot, false) {
            Ok(p) => p,
            Err(e) => {
                let mut log_store = self.inner.log_store.lock().unwrap();
                let _ = log_store.log_error(
                    "Ingest",
                    &format!("Ingest planning failed: {}", e),
                    Some(&format!("内容摄入规划失败: {}", e)),
                    Some(&e.to_string()),
                );
                return Err(e);
            }
        };
        // Step 2：真正落库 + CAS + 发事件
        match self.apply_ingest(plan) {
            Ok(meta) => Ok(meta),
            Err(e) => {
                let mut log_store = self.inner.log_store.lock().unwrap();
                let _ = log_store.log_error(
                    "Ingest",
                    &format!("Ingest failed: {}", e),
                    Some(&format!("内容摄入失败: {}", e)),
                    Some(&e.to_string()),
                );
                Err(e)
            }
        }
    }

    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// 清空所有缓存（CAS blobs、临时文件等）
    pub fn clear_cache(&self) -> anyhow::Result<()> {
        self.inner.cas.clear_all()?;
        // 同时清空数据库中的缓存记录
        let store = self.inner.store.lock().unwrap();
        store.conn.execute("DELETE FROM content_cache", [])?;
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
                &format!("Planning ingest for clipboard content, type: {}, size: {} bytes", content_type, size_bytes),
                Some(&format!("正在规划剪贴板内容摄入，类型: {}，大小: {} 字节", content_type, size_bytes)),
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

    pub fn apply_ingest(&self, plan: IngestPlan) -> anyhow::Result<ItemMeta> {
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
            let _wrote = self.inner.cas.put_if_absent(&sha, &plan.content_bytes, &tmp_name)?;

            if self.inner.cas.blob_exists(&sha) {
                let mut store = self.inner.store.lock().unwrap();
                store.mark_cache_present(&sha, now)?;
                
                // 记录 CAS 写入成功
                let mut log_store = self.inner.log_store.lock().unwrap();
                let _ = log_store.log_info(
                    "Ingest",
                    &format!("CAS blob written successfully: {}", sha),
                    Some(&format!("CAS 对象写入成功: {}", sha)),
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
                &format!("CAS blob already exists, skipping write: {}", sha),
                Some(&format!("CAS 对象已存在，跳过写入: {}", sha)),
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
                &format!("Ingest applied successfully, item_id: {}", item_id),
                Some(&format!("内容摄入成功，项目 ID: {}", item_id)),
            );
        }

        // --- M1 集成：广播元数据到网络 ---
        if let Some(net_tx) = &self.inner.net {
            // 使用 try_send 避免阻塞，如果通道满了或网络层挂了也不影响本地逻辑
            let _ = net_tx.try_send(NetCmd::BroadcastMeta(plan.meta.clone()));
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
				store.soft_delete_history_keep_latest(&self.inner.core_config.account_uid, max_history)?;
			}
		}

        // 2) Cache GC（LRU）
		let max_cas = self.inner.core_config.app_config.gc_cas_max_bytes;
		if max_cas > 0 {
			let mut cur = self.inner.cas.total_size_bytes()?;
			while cur > max_cas {
				let (sha, _expect_bytes) = {
					let store = self.inner.store.lock().unwrap();
					let cands = store.select_lru_present(1)?;
					if cands.is_empty() { break; }
					cands[0].clone()
				};
				let freed = self.inner.cas.remove_blob(&sha)?;
				{
					let mut store = self.inner.store.lock().unwrap();
					store.mark_cache_missing(&sha, now)?;
				}
				if freed > 0 {
					cur -= freed;
				} else {
					cur = self.inner.cas.total_size_bytes()?;
				}
			}
		}
		Ok(())
    }

    pub fn plan_local_ingest_result(
        &self,
        snapshot: &crate::clipboard::ClipboardSnapshot,
        force: bool,
    ) -> anyhow::Result<crate::api::PlanResult> {
        let plan = self.plan_local_ingest(snapshot, force)?;
        Ok(PlanResult {
            meta: plan.meta,
            needs_user_confirm: plan.needs_user_confirm,
            strategy: format!("{:?}", plan.strategy),
        })
    }

    pub fn ingest_local_copy_with_force(
        &self,
        snapshot: crate::clipboard::ClipboardSnapshot,
        force: bool,
    ) -> anyhow::Result<crate::model::ItemMeta> {
        let plan = self.plan_local_ingest(&snapshot, force)?;
        self.apply_ingest(plan)
    }

    /**
     * 获取当前在线设备列表。
     * * 这是一个同步阻塞调用，适合 FFI 使用。
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
        net_tx.blocking_send(NetCmd::GetPeers(tx))
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

    pub fn set_peer_policy(&self, peer_id: &str, share_to: Option<bool>, accept_from: Option<bool>) -> anyhow::Result<()> {
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
        self.inner.emit(event.to_string());

        Ok(())
    }

	pub fn ensure_content_cached(&self, item_id: &str, file_id: Option<&str>) -> anyhow::Result<String> {
		if self.inner.is_shutdown.load(std::sync::atomic::Ordering::Acquire) {
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
									.blob_path(&content.sha256)
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
			.blocking_send(crate::net::NetCmd::EnsureContentCached {
				item_id: item_id.to_string(),
				file_id: file_id.map(|s| s.to_string()),
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
            let _ = net_tx.try_send(crate::net::NetCmd::CancelTransfer {
                transfer_id: transfer_id.to_string(),
            });
        }
    }

	// [修改] 增强 list_history，虽然底层 store 可能只支持 limit，但 API 要预留 cursor 位置
	pub fn list_history(&self, limit: usize, _cursor: Option<i64>) -> anyhow::Result<Vec<crate::model::ItemMeta>> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}
		let store = self.inner.store.lock().unwrap();
		// 目前 store.rs 里的 list_history_metas 只接受 limit
		// 后续你需要去 store.rs 实现基于 cursor 的分页
		store.list_history_metas(&self.inner.core_config.account_uid, limit)
	}

	// [新增] 获取单条 Meta，供 FFI 调用
	pub fn get_item_meta(&self, item_id: &str) -> anyhow::Result<Option<crate::model::ItemMeta>> {
		if self.inner.is_shutdown.load(Ordering::Acquire) {
			anyhow::bail!("core already shutdown");
		}
		let store = self.inner.store.lock().unwrap();

		// 这里的 SQL 查询逻辑需要你确认 store.rs 里有没有。
		// 如果 store.rs 还没有 get_item_meta，你需要去加一个简单的 select。
		// 为了方便，这里暂时用 list_history 模拟（性能差，建议后续在 Store 实现专用查询）
		let list = store.list_history_metas(&self.inner.core_config.account_uid, 100)?;
		Ok(list.into_iter().find(|i| i.item_id == item_id))
	}
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
    pub stats_store: Arc<Mutex<StatsStore>>,
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

    pub(crate) fn emit_json(&self, v: serde_json::Value) {
        self.emit(v.to_string());
    }

    fn shutdown(&self) {
        // 幂等：多次调用也只会第一次生效
        let already = self.is_shutdown.swap(true, Ordering::AcqRel);

        // M1 集成：发送关闭信号
        if let Some(tx) = &self.net {
            // 尝试发送 Shutdown 命令，如果接收端已关闭则忽略错误
            let _ = tx.try_send(NetCmd::Shutdown);
        }

        if already {
            return;
        }
    }

}

#[cfg(test)]
mod tests;
