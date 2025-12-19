use super::super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct TestDirs {
    pub root: PathBuf,
    pub data_dir: String,
    pub cache_dir: String,
}

impl Drop for TestDirs {
    fn drop(&mut self) {
        if std::env::var_os("CB_TEST_KEEP").is_some() {
            return;
        }
        // SQLite/WAL 句柄可能延迟释放：重试更稳
        for _ in 0..10 {
            if !self.root.exists() {
                return;
            }
            if fs::remove_dir_all(&self.root).is_ok() {
                return;
            }
            sleep(Duration::from_millis(20));
        }
    }
}

fn test_target_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(p);
    }
    // 兜底：workspace 根的 target
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target")
}

pub fn unique_dirs(sub: &str) -> TestDirs {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let root = test_target_dir()
        .join("debug")
        .join("clipbridge_tests")
        .join("cb_core")
        .join(sub)
        .join(format!("run_{nanos}"));

    let data_path = root.join("data");
    let cache_path = root.join("cache");
    let _ = fs::create_dir_all(&data_path);
    let _ = fs::create_dir_all(&cache_path);

    TestDirs {
        root,
        data_dir: data_path.to_string_lossy().into_owned(),
        cache_dir: cache_path.to_string_lossy().into_owned(),
    }
}

struct PrintSink;
impl CoreEventSink for PrintSink {
    fn emit(&self, _event_json: String) {
        // 先不处理事件
    }
}

pub fn mk_core(sub: &str, gc_history_max_items: i64, gc_cas_max_bytes: i64) -> (Core, TestDirs) {
    let dirs = unique_dirs(sub);

    let cfg = CoreConfig {
        device_id: "dev-1".to_string(),
        device_name: "dev1".to_string(),
        account_uid: "acct-uid-1".to_string(),
        account_tag: "acctTag".to_string(),
        data_dir: dirs.data_dir.clone(),
        cache_dir: dirs.cache_dir.clone(),
        limits: crate::policy::Limits::default(),
        gc_history_max_items,
        gc_cas_max_bytes,
    };

    let sink: Arc<dyn CoreEventSink> = Arc::new(PrintSink);
    let core = Core::init(cfg, sink);

    (core, dirs)
}
