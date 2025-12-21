use std::sync::Arc;

use crate::api::{Core, CoreConfig};
use crate::api::GlobalPolicy::AllowAll;
use crate::policy::Limits;

use super::dirs::TestDirs;
use super::events::{EventAsserter, EventCollector};

pub struct TestCore {
    pub core: Core,
    pub dirs: TestDirs,
    pub events: Arc<EventCollector>,
}

impl TestCore {
    pub fn with_cfg(crate_tag: &str, test_tag: &str, mut cfg: CoreConfig) -> Self {
        let dirs = TestDirs::new(crate_tag, test_tag);

        // 强制把 DB/CAS 放到测试目录（避免污染 repo 根目录）
        cfg.data_dir = dirs.data_dir.clone();
        cfg.cache_dir = dirs.cache_dir.clone();

        let events = Arc::new(EventCollector::new());
        let sink: Arc<dyn crate::api::CoreEventSink> = events.clone();

        let core = crate::api::Core::init(cfg, sink);

        Self { core, dirs, events }
    }

    pub fn new(
        crate_tag: &str,
        test_tag: &str,
        device_id: &str,
        device_name: &str,
        account_uid: &str,
        account_tag: &str,
    ) -> Self {
        let cfg = CoreConfig {
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            account_uid: account_uid.to_string(),
            account_tag: account_tag.to_string(),
            data_dir: String::new(),
            cache_dir: String::new(),
            limits: Limits::default(),
            gc_history_max_items: 50_000,
            gc_cas_max_bytes: 1_i64 << 60,
            global_policy: Default::default()
        };

        Self::with_cfg(crate_tag, test_tag, cfg)
    }

    pub fn asserter(&self) -> EventAsserter<'_> {
        EventAsserter::new(&self.events)
    }
}

impl Drop for TestCore {
    fn drop(&mut self) {
        self.core.shutdown();
    }
}
