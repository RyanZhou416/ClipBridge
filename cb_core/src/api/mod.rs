// cb_core/src/api/mod.rs

use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use tokio::sync::mpsc;

use crate::{cas::Cas, store::Store, util::now_ms};
use crate::clipboard::{make_ingest_plan, ClipboardSnapshot, IngestPlan, LocalIngestDeps};
use crate::model::ItemMeta;
use crate::net::{NetManager, NetCmd};

/**
 * Core 的配置项。
 *
 * 定义了 Core 启动所需的设备 ID、名称、账户信息、存储路径以及各项限制策略。
 */
#[derive(Clone, Debug)]
pub struct CoreConfig {
    pub device_id: String,     // 本机 device_id（先由壳传入）
    pub device_name: String,   // 设备显示名
    pub account_uid: String,   // 本机当前账号域（history 分区键）
    pub account_tag: String,   // 账号
    pub data_dir: String,      // 持久：core.db
    pub cache_dir: String,     // 可清空：CAS blobs/tmp
    pub limits: crate::policy::Limits,
    pub gc_history_max_items: i64, // 历史条数上限（超过就 GC）
    pub gc_cas_max_bytes: i64,     // blobs 总大小上限（超过就 GC）
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

/**
 * ClipBridge Core 的权威句柄。
 *
 * 这是整个核心库的唯一入口点，通过它来调用所有的业务逻辑。
 */
#[derive(Clone)]
pub struct Core {
    pub(crate) inner: Arc<Inner>,
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
        let store = Store::open(&cfg.data_dir).expect("open store");
        let cas = Cas::new(&cfg.cache_dir).expect("init cas");

        // --- M1 集成：启动网络管理器 ---
        // 注意：这里假设 NetManager::spawn 是同步封装（内部 spawn 异步任务）
        // 如果是在 FFI 环境且有 Tokio Runtime，这将正常工作。
        let net_tx = match NetManager::spawn(cfg.clone(), sink.clone()) {
            Ok(tx) => Some(tx),
            Err(e) => {
                eprintln!("[Core] Failed to start NetManager: {}", e);
                None
            }
        };

        let inner = Inner {
            cfg,
            sink,
            is_shutdown: AtomicBool::new(false),
            store: Mutex::new(store),
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
        let plan = self.plan_local_ingest(&snapshot, false)?;
        // Step 2：真正落库 + CAS + 发事件
        self.apply_ingest(plan)
    }

    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    pub fn list_history(&self, limit: usize) -> anyhow::Result<Vec<crate::model::ItemMeta>> {
        if self.inner.is_shutdown.load(Ordering::Acquire) {
            anyhow::bail!("core already shutdown");
        }
        let store = self.inner.store.lock().unwrap();
        store.list_history_metas(&self.inner.cfg.account_uid, limit)
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

        let deps = LocalIngestDeps {
            device_id: &self.inner.cfg.device_id,
            device_name: &self.inner.cfg.device_name,
            account_uid: &self.inner.cfg.account_uid,
        };

        let limits = &self.inner.cfg.limits;

        make_ingest_plan(&deps, snapshot, limits, force)
    }

    pub fn apply_ingest(&self, plan: IngestPlan) -> anyhow::Result<ItemMeta> {
        if self.inner.is_shutdown.load(Ordering::Acquire) {
            anyhow::bail!("core already shutdown");
        }

        let now = now_ms();
        let meta_clone = plan.meta.clone();
        let sha = plan.meta.content.sha256.clone();

        // Phase A：落库（只在这个 block 里持锁）
        let cache = {
            let mut store = self.inner.store.lock().unwrap();
            store.insert_meta_and_history(&self.inner.cfg.account_uid, &plan.meta, now)?
        };

        // Phase B：CAS 去重写入（这里不持 store 锁）
        if !cache.present || !self.inner.cas.blob_exists(&sha) {
            let tmp_name = format!("{}.tmp", plan.meta.item_id);
            let _wrote = self.inner.cas.put_if_absent(&sha, &plan.content_bytes, &tmp_name)?;

            if self.inner.cas.blob_exists(&sha) {
                let mut store = self.inner.store.lock().unwrap();
                store.mark_cache_present(&sha, now)?;
            } else {
                anyhow::bail!("CAS write failed: blob missing after put_if_absent");
            }
        } else {
            let mut store = self.inner.store.lock().unwrap();
            store.touch_cache(&sha, now)?;
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
        {
            let mut store = self.inner.store.lock().unwrap();
            let n = store.history_count_for_account(&self.inner.cfg.account_uid)?;
            if self.inner.cfg.gc_history_max_items > 0 && n > self.inner.cfg.gc_history_max_items {
                store.soft_delete_history_keep_latest(&self.inner.cfg.account_uid, self.inner.cfg.gc_history_max_items)?;
            }
        }

        // 2) Cache GC（LRU）
        if self.inner.cfg.gc_cas_max_bytes > 0 {
            let mut cur = self.inner.cas.total_size_bytes()?;
            while cur > self.inner.cfg.gc_cas_max_bytes {
                let (sha, _expect_bytes) = {
                    let store = self.inner.store.lock().unwrap();
                    let cands = store.select_lru_present(1)?;
                    if cands.is_empty() {
                        break;
                    }
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
                    // 文件不存在/统计不准：重新扫一次，避免死循环
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
    ) -> anyhow::Result<PlanResult> {
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
}

// 移除旧的测试代码，以适应 M1 新架构
#[cfg(test)]
impl Core {
    // 可以在这里添加针对 M1 的测试辅助方法
}

pub(crate) struct Inner {
    pub cfg: CoreConfig,
    pub sink: Arc<dyn CoreEventSink>,
    pub is_shutdown: AtomicBool,
    pub store: Mutex<Store>,
    pub cas: Cas,
    // M1 集成：改为 NetCmd 发送端
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