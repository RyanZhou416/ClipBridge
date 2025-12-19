use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct TestDirs {
    pub root: PathBuf,
    pub data_dir: String,
    pub cache_dir: String,
}

impl TestDirs {
    /// crate_tag 建议用 "cb_core" / "core_ffi"；test_tag 用具体用例名
    pub fn new(crate_tag: &str, test_tag: &str) -> Self {
        let uid = uuid::Uuid::new_v4().to_string();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // 1) 优先尊重 CARGO_TARGET_DIR
        // 2) 否则从 manifest 往上找第一个存在的 target/
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| find_target_dir(&manifest));

        let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

        // target/<profile>/clipbridge_tests/<crate_tag>/<test_tag>_<uuid>/{data,cache}
        let root = target
            .join(profile)
            .join("clipbridge_tests")
            .join(crate_tag)
            .join(format!("{test_tag}_{uid}"));

        let data = root.join("data");
        let cache = root.join("cache");

        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&cache).unwrap();

        Self {
            root,
            data_dir: data.to_string_lossy().to_string(),
            cache_dir: cache.to_string_lossy().to_string(),
        }
    }
}

impl Drop for TestDirs {
    fn drop(&mut self) {
        // 如需保留现场排查：运行测试时加 CB_TEST_KEEP=1
        if std::env::var_os("CB_TEST_KEEP").is_some() {
            eprintln!("[test] keeping test dirs: {:?}", self.root);
            return;
        }
        let _ = remove_dir_all_retry(&self.root, Duration::from_millis(1500));
    }
}

fn find_target_dir(from: &Path) -> PathBuf {
    // 最多向上找 8 层：适配 core-ffi 那种深目录
    let mut cur = from.to_path_buf();
    for _ in 0..8 {
        let cand = cur.join("target");
        if cand.exists() {
            return cand;
        }
        if let Some(p) = cur.parent() {
            cur = p.to_path_buf();
        } else {
            break;
        }
    }
    // 兜底：manifest/target（即使不存在也不 panic）
    from.join("target")
}

fn remove_dir_all_retry(path: &Path, max_wait: Duration) -> std::io::Result<()> {
    let start = Instant::now();
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if start.elapsed() < max_wait => {
                // 常见：Windows 上 sqlite/cas 文件句柄短时间未释放
                std::thread::sleep(Duration::from_millis(30));
                let _ = e; // 不打印，避免刷屏
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}
