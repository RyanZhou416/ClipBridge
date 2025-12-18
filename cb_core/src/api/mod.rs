use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use crate::{cas::Cas, store::Store, util::now_ms};
use crate::clipboard::{ClipboardSnapshot, LocalIngestDeps, IngestPlan, make_ingest_plan};
use crate::model::ItemMeta;

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
 */#[derive(Clone)]
pub struct Core {
    inner: Arc<Inner>,
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

        let inner = Inner {
            cfg,
            sink,
            is_shutdown: AtomicBool::new(false),
            store: Mutex::new(store),
            cas,
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

pub(crate) struct Inner {
    pub cfg: CoreConfig,
    pub sink: Arc<dyn CoreEventSink>,
    pub is_shutdown: AtomicBool,
    pub store: Mutex<Store>,
    pub cas: Cas,
}


impl Inner {
    fn emit(&self, event_json: String) {
        // shutdown 后不允许再回调
        if self.is_shutdown.load(Ordering::Acquire) {
            return;
        }
        self.sink.emit(event_json);
    }

    fn shutdown(&self) {
        // 幂等：多次调用也只会第一次生效
        let already = self.is_shutdown.swap(true, Ordering::AcqRel);
        if already {
            return;
        }
        // 第一次 shutdown 时你可以发一个事件（可选）
        // 注意：这里 emit 会被 is_shutdown 拦住，所以如果你想发“关闭事件”，要么先发再置位，
        // 要么专门允许这一个事件。这里我们先简单：不发。
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct PrintSink;
    impl CoreEventSink for PrintSink {
        fn emit(&self, event_json: String) {
        }
    }

    #[test]
    fn ingest_text_smoke() {
        let dirs = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-uid-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir: dirs.data_dir.clone(),
            cache_dir: dirs.cache_dir.clone(),
            limits: crate::policy::Limits::default(),
            gc_history_max_items: 1000000,
            gc_cas_max_bytes: 1_i64 << 60,
        };

        let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
        let core = Core::init(cfg, sink);

        let ts = crate::util::now_ms();
        let meta = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "hello world".to_string(),
            ts_ms: ts,
        }).unwrap();

        assert_eq!(meta.kind, crate::model::ItemKind::Text);
        core.shutdown();
        drop(core);
    }
    #[test]
    fn ingest_same_text_dedup_cache() {
        let dirs = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-uid-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir: dirs.data_dir.clone(),
            cache_dir: dirs.cache_dir.clone(),
            limits: crate::policy::Limits::default(),
            gc_history_max_items: 1000000,
            gc_cas_max_bytes: 1_i64 << 60,
        };

        let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
        let core = Core::init(cfg, sink);

        let ts1 = crate::util::now_ms();
        let m1 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "hello world".to_string(),
            ts_ms: ts1,
        }).unwrap();

        let ts2 = ts1 + 1;
        let m2 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "hello world".to_string(),
            ts_ms: ts2,
        }).unwrap();

        assert_ne!(m1.item_id, m2.item_id);
        assert_eq!(m1.content.sha256, m2.content.sha256);

        let store = core.inner.store.lock().unwrap();
        let n_cache = store.cache_row_count_for_sha(&m1.content.sha256).unwrap();
        let n_hist = store.history_count_for_account(&core.inner.cfg.account_uid).unwrap();

        assert_eq!(n_cache, 1);
        assert_eq!(n_hist, 2);

        core.shutdown();
    }

    #[test]
    fn list_history_orders_newest_first() {
        let dirs = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-uid-2".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir: dirs.data_dir.clone(),
            cache_dir: dirs.cache_dir.clone(),
            limits: crate::policy::Limits::default(),
            gc_history_max_items: 1000000,
            gc_cas_max_bytes: 1_i64 << 60,
        };

        let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
        let core = Core::init(cfg, sink);

        let ts1 = crate::util::now_ms();
        let m1 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "first".to_string(),
            ts_ms: ts1,
        }).unwrap();

        let ts2 = ts1 + 10;
        let m2 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "second".to_string(),
            ts_ms: ts2,
        }).unwrap();

        let list = core.list_history(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].item_id, m2.item_id);
        assert_eq!(list[1].item_id, m1.item_id);

        core.shutdown();
        drop(core);
    }

    struct TestDirs {
        root: std::path::PathBuf,
        data_dir: String,
        cache_dir: String,
    }

    impl Drop for TestDirs {
        fn drop(&mut self) {
            // 如需保留现场排查：运行测试时加 CB_TEST_KEEP=1
            if std::env::var_os("CB_TEST_KEEP").is_some() {
                eprintln!("[test] keeping test dirs: {:?}", self.root);
                return;
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn unique_dirs(tag: &str) -> TestDirs {
        let uid = uuid::Uuid::new_v4().to_string();

        // workspace: cb_core 的 CARGO_MANIFEST_DIR 一般是 <repo>/cb_core
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // 优先尊重 CARGO_TARGET_DIR；否则用 <repo>/target
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| manifest.join("..").join("target"));

        // debug / release
        let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

        // target/<profile>/clipbridge_tests/cb_core/<tag>_<uuid>/{data,cache}
        let root = target
            .join(profile)
            .join("clipbridge_tests")
            .join("cb_core")
            .join(format!("{tag}_{uid}"));

        let data = root.join("data");
        let cache = root.join("cache");

        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&cache).unwrap();

        TestDirs {
            root,
            data_dir: data.to_string_lossy().to_string(),
            cache_dir: cache.to_string_lossy().to_string(),
        }
    }


    #[test]
    fn plan_requires_confirm_over_soft() {
        let dirs = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-plan-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir: dirs.data_dir.clone(),
            cache_dir: dirs.cache_dir.clone(),
            limits: crate::policy::Limits::default(),
            gc_history_max_items: 1000000,
            gc_cas_max_bytes: 1_i64 << 60,
        };
        let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
        let core = Core::init(cfg, sink);

        // 构造一个 > 1MB 的 text（soft=1MB）:contentReference[oaicite:1]{index=1}
        let big = "a".repeat((core.inner.cfg.limits.soft_text_bytes + 10) as usize);

        let snap = crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: big,
            ts_ms: crate::util::now_ms(),
        };

        let r = core.plan_local_ingest_result(&snap, false).unwrap();
        assert!(r.needs_user_confirm);

        // force=true 后就不需要 confirm
        let r2 = core.plan_local_ingest_result(&snap, true).unwrap();
        assert!(!r2.needs_user_confirm);

        core.shutdown();
        drop(core);
    }

    #[test]
    fn ingest_same_image_dedup_cache() {
        let dirs = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-img-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir: dirs.data_dir.clone(),
            cache_dir: dirs.cache_dir.clone(),
            limits: crate::policy::Limits::default(),
            gc_history_max_items: 1000000,
            gc_cas_max_bytes: 1_i64 << 60,
        };

        let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
        let core = Core::init(cfg, sink);

        let bytes = vec![7u8; 4096];
        let ts1 = crate::util::now_ms();
        let m1 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes: bytes.clone(),
            mime: "image/png".to_string(),
            ts_ms: ts1,
        }).unwrap();

        let ts2 = ts1 + 1;
        let m2 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes,
            mime: "image/png".to_string(),
            ts_ms: ts2,
        }).unwrap();

        assert_ne!(m1.item_id, m2.item_id);
        assert_eq!(m1.content.sha256, m2.content.sha256);

        let store = core.inner.store.lock().unwrap();
        let n_cache = store.cache_row_count_for_sha(&m1.content.sha256).unwrap();
        assert_eq!(n_cache, 1);
        assert!(core.inner.cas.blob_exists(&m1.content.sha256));

        core.shutdown();
    }

    #[test]
    fn gc_evicts_lru_when_over_cap() {
        use std::time::Duration;
        use std::thread::sleep;

        let dirs = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-gc-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir: dirs.data_dir.clone(),
            cache_dir: dirs.cache_dir.clone(),
            limits: crate::policy::Limits::default(),
            gc_history_max_items: 1000000,
            gc_cas_max_bytes: 1200, // 1KB，故意很小
        };

        let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
        let core = Core::init(cfg, sink);

        let ts = crate::util::now_ms();
        let m1 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes: vec![1u8; 800],
            mime: "image/png".to_string(),
            ts_ms: ts,
        }).unwrap();

        sleep(Duration::from_millis(5));

        let m2 = core.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes: vec![2u8; 800],
            mime: "image/png".to_string(),
            ts_ms: ts + 1,
        }).unwrap();

        // 触发后：更旧的 m1 应被淘汰（LRU）
        assert!(!core.inner.cas.blob_exists(&m1.content.sha256));

        let store = core.inner.store.lock().unwrap();
        assert!(!store.get_cache_present(&m1.content.sha256).unwrap());
        drop(store);
        // 历史仍可列出（meta 不丢）
        let list = core.list_history(10).unwrap();
        assert!(list.iter().any(|x| x.item_id == m1.item_id));
        assert!(list.iter().any(|x| x.item_id == m2.item_id));

        core.shutdown();
        drop(core);
    }

}

