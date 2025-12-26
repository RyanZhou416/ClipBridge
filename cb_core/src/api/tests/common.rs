// cb_core/src/api/tests/common.rs
use super::super::*;
use std::sync::Arc;

// 直接复用 testsupport 的目录夹具：
// - target/<profile>/clipbridge_tests/cb_core/<test_tag>_<uuid>/{data,cache}
// - 支持 CB_TEST_KEEP=1 保留现场
pub use crate::testsupport::dirs::TestDirs;

/// 兼容旧测试调用：sub 就当作 test_tag 用
pub fn unique_dirs(sub: &str) -> TestDirs {
	TestDirs::new("cb_core", sub)
}

struct PrintSink;
impl CoreEventSink for PrintSink {
	fn emit(&self, _event_json: String) {
		// 这些 api 单测暂时不依赖事件回调
	}
}

/// 兼容旧测试调用：保留 mk_core(sub, gc_history_max_items, gc_cas_max_bytes)
pub fn mk_core(sub: &str, gc_history_max_items: i64, gc_cas_max_bytes: i64) -> (Core, TestDirs) {
	let dirs = unique_dirs(sub);

	let cfg = CoreConfig {
		device_id: "dev-1".to_string(),
		device_name: "dev1".to_string(),
		account_uid: "acct-uid-1".to_string(),
		account_tag: "acctTag".to_string(),

		// 强制所有测试 DB/CAS 进入 testsupport 的测试目录（避免污染 repo 根目录）
		data_dir: dirs.data_dir.clone(),
		cache_dir: dirs.cache_dir.clone(),

		app_config: AppConfig {
			gc_history_max_items,
			gc_cas_max_bytes,
			..Default::default()
		},
	};

	let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
	let core = Core::init(cfg, sink);

	(core, dirs)
}
