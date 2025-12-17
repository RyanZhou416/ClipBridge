use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use crate::{cas::Cas, store::Store, util::now_ms};
use crate::clipboard::{ClipboardSnapshot, LocalIngestDeps, IngestPlan, make_ingest_plan};
use crate::model::ItemMeta;

/**
Core 的门面，唯一稳定出口，定义了外界能看到的东西
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
}

#[derive(Clone, Debug)]
pub struct PlanResult {
    pub meta: crate::model::ItemMeta,
    pub needs_user_confirm: bool,
    pub strategy: String, // 用字符串，FFI/壳侧更省事
}


/// 事件回调接口（Core → 壳）
pub trait CoreEventSink: Send + Sync + 'static {
    fn emit(&self, event_json: String);
}

// Core 是“权威实例句柄”（后面 FFI 会把它包成不透明 handle）
#[derive(Clone)]
pub struct Core {
    inner: Arc<Inner>,
}

impl Core {
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
        Self { inner: Arc::new(inner) }
    }

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
        let meta_clone = plan.meta.clone(); // 准备给调用者返回

        // Phase A：落库
        let mut store = self.inner.store.lock().unwrap();
        let cache = store.insert_meta_and_history(&self.inner.cfg.account_uid, &plan.meta, now)?;

        // Phase B：CAS 去重写入
        let sha = plan.meta.content.sha256.clone();

        if !cache.present || !self.inner.cas.blob_exists(&sha) {
            let tmp_name = format!("{}.tmp", plan.meta.item_id);

            // 关键：并发情况下可能返回 Ok(false)，这不算错误
            let _wrote = self.inner.cas.put_if_absent(&sha, &plan.content_bytes, &tmp_name)?;

            // 只要最终 blob 存在，就认为 present=1
            if self.inner.cas.blob_exists(&sha) {
                store.mark_cache_present(&sha, now)?;
            } else {
                anyhow::bail!("CAS write failed: blob missing after put_if_absent");
            }
        } else {
            store.touch_cache(&sha, now)?;
        }




        // 事件 2：meta 已添加，可更新 UI
        let meta_evt = serde_json::json!({
          "type": "ITEM_META_ADDED",
          "meta": plan.meta,
          "policy": {
            "needs_user_confirm": plan.needs_user_confirm,
            "strategy": format!("{:?}", plan.strategy),
          }
        });
        self.inner.emit(meta_evt.to_string());

        Ok(meta_clone)
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
            println!("[core-event] {}", event_json);
        }
    }

    #[test]
    fn ingest_text_smoke() {
        let (data_dir, cache_dir) = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-uid-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir,
            cache_dir,
            limits: crate::policy::Limits::default(),

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
    }
    #[test]
    fn ingest_same_text_dedup_cache() {
        let (data_dir, cache_dir) = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-uid-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir,
            cache_dir,
            limits: crate::policy::Limits::default(),
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
        let (data_dir, cache_dir) = unique_dirs("dedup");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-uid-2".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir,
            cache_dir,
            limits: crate::policy::Limits::default(),

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
    }

    fn unique_dirs(tag: &str) -> (String, String) {
        let uid = uuid::Uuid::new_v4().to_string();
        let data = format!("./tmp_test/{}_data_{}", tag, uid);
        let cache = format!("./tmp_test/{}_cache_{}", tag, uid);
        (data, cache)
    }

    #[test]
    fn plan_requires_confirm_over_soft() {
        let (data_dir, cache_dir) = unique_dirs("plan_soft");
        let cfg = CoreConfig {
            device_id: "dev-1".to_string(),
            device_name: "dev1".to_string(),
            account_uid: "acct-plan-1".to_string(),
            account_tag: "acctTag".to_string(),
            data_dir,
            cache_dir,
            limits: crate::policy::Limits::default(),
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
    }

}

