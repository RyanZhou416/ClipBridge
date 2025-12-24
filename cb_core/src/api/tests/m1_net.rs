use std::path::PathBuf;
use crate::api::*;
use crate::clipboard::ClipboardSnapshot;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::sleep;

// --- 1. 本地测试辅助组件 ---

struct TestSink {
    tx: broadcast::Sender<String>,
}

impl TestSink {
    fn new() -> (Arc<Self>, broadcast::Receiver<String>) {
        let (tx, rx) = broadcast::channel(100);
        (Arc::new(Self { tx }), rx)
    }
}

impl CoreEventSink for TestSink {
    fn emit(&self, json_event: String) {
        let _ = self.tx.send(json_event);
    }
}

// 统一的 Core 创建函数
pub(crate) fn create_test_core<F>(device_id: &str, uid: &str, config_modifier: F) -> (Arc<Core>, broadcast::Receiver<String>, tempfile::TempDir)
where F: FnOnce(&mut CoreConfig)
{
    let mut base = workspace_target_dir();
    base.push("debug");
    base.push("clipbridge_tests");
    std::fs::create_dir_all(&base).unwrap();

    let dir = tempfile::Builder::new()
        .prefix(&format!("cb_{}_{}", device_id, uid))
        .tempdir_in(&base)
        .unwrap();
    let data_path = dir.path().to_string_lossy().to_string();

    let mut config = CoreConfig {
        device_id: device_id.to_string(),
        device_name: format!("Test Device {}", device_id),
        account_tag: uid.to_string(),
        account_uid: uid.to_string(),
        data_dir: data_path.clone(),
        cache_dir: data_path,
        ..Default::default()
    };

    config_modifier(&mut config);

    let (sink, rx) = TestSink::new();
    let core = Core::init(config, sink);

    (Arc::new(core), rx, dir)
}

// 异步包装：将阻塞调用移到 blocking thread
pub(crate) async fn list_peers_async(core: &Arc<Core>) -> Vec<PeerStatus> {
    let c = core.clone();
    tokio::task::spawn_blocking(move || {
        c.list_peers().unwrap_or_default()
    }).await.unwrap()
}

pub(crate) async fn wait_for<F, Fut>(timeout: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if condition().await {
            return true;
        }
        sleep(Duration::from_millis(200)).await;
    }
    false
}

fn workspace_target_dir() -> PathBuf {
    // 如果 cargo 明确给了 target dir（最标准）
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }

    // 否则：从当前 crate 的 manifest_dir 往上找 workspace 根
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // 假设 workspace 结构是：
    // <workspace>/cb_core
    // <workspace>/Cargo.toml
    manifest
        .parent()        // 回到 workspace 根
        .expect("cb_core should be inside workspace")
        .join("target")
}

// --- 2. 测试用例 ---

#[test]
fn test_core_api_basics() {
    // 基础测试使用独立的 uid
    let (core, _rx, _dir) = create_test_core("basics", "uid_basics", |_| {});

    let status_json = core.get_status().expect("get_status failed");
    assert_eq!(status_json["status"], "Running");

    let peers = core.list_peers().unwrap();
    assert!(peers.is_empty());

    core.shutdown();
}

#[tokio::test]
async fn test_m1_simulation_loopback() {
    let shared_uid = format!("m1_loopback_secret{}", uuid::Uuid::new_v4());
    let (core_a, _rx_a, _dir_a) = create_test_core("dev_a", &shared_uid, |_| {});
    let (core_b, _rx_b, _dir_b) = create_test_core("dev_b", &shared_uid, |_| {});

    println!("Core A and B started. Waiting for discovery...");

    let connected = wait_for(Duration::from_secs(5), || async {
        let peers_a = list_peers_async(&core_a).await;
        let peers_b = list_peers_async(&core_b).await;

        let a_sees_b = peers_a.iter().any(|p| p.device_id == "dev_b" && p.state == PeerConnectionState::Online);
        let b_sees_a = peers_b.iter().any(|p| p.device_id == "dev_a" && p.state == PeerConnectionState::Online);

        a_sees_b && b_sees_a
    }).await;

    assert!(connected, "Peers failed to connect via OPAQUE");

    core_a.shutdown();
    drop(core_a);
    sleep(Duration::from_millis(200)).await; // 给后台线程一点时间退出（尤其 Windows）
    core_b.shutdown();
    drop(core_b);
    sleep(Duration::from_millis(200)).await; // 给后台线程一点时间退出（尤其 Windows）


}

#[tokio::test]
async fn test_m1_data_broadcast() {
    let shared_uid = "m1broadcast";

    let (core_a, _rx_a, _dir_a) = create_test_core("broada", shared_uid, |_| {});
    let (_core_b, mut rx_b, _dir_b) = create_test_core("broadb", shared_uid, |_| {});

    println!("Waiting for connection...");
    let connected = wait_for(Duration::from_secs(5), || async {
        let peers = list_peers_async(&core_a).await;
        peers.iter().any(|p| p.device_id == "broadb" && p.state == PeerConnectionState::Online)
    }).await;
    assert!(connected, "Peers not connected");

    let snapshot = ClipboardSnapshot::Text {
        text_utf8: "Hello M1 Network".to_string(),
        ts_ms: crate::util::now_ms(),
    };

    println!("A ingesting data...");
    let meta = core_a.ingest_local_copy(snapshot).expect("Ingest failed");
    println!("Generated item: {}", meta.item_id);

    println!("Waiting for B to receive meta...");

    let start = std::time::Instant::now();
    let mut received = false;
    while start.elapsed() < Duration::from_secs(3) {
        while let Ok(evt_json) = rx_b.try_recv() {
            if evt_json.contains("ITEM_META_ADDED") && evt_json.contains(&meta.item_id) {
                received = true;
                break;
            }
        }
        if received { break; }
        sleep(Duration::from_millis(100)).await;
    }

    assert!(received, "Device B did not receive the broadcast metadata");
    core_a.shutdown();
    drop(core_a);
    sleep(Duration::from_millis(200)).await; // 给后台线程一点时间退出（尤其 Windows）
    _core_b.shutdown();
    drop(_core_b);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn test_m1_reconnection() {
    let shared_uid = format!("m1_recon_secret{}", uuid::Uuid::new_v4());
    let (core_a, _rx_a, dir_a) = create_test_core("recon_a", &shared_uid, |_| {});
    let (core_b, _rx_b, _dir_b) = create_test_core("recon_b", &shared_uid, |_| {});

    println!("1. Waiting for initial connection...");
    let connected = wait_for(Duration::from_secs(5), || async {
        let peers = list_peers_async(&core_a).await;
        peers.iter().any(|p| p.device_id == "recon_b" && p.state == PeerConnectionState::Online)
    }).await;
    assert!(connected, "Initial connection failed");

    println!("2. Killing B...");
    core_b.shutdown();
    drop(core_b);

    println!("Waiting for A to detect disconnect...");
    let disconnected = wait_for(Duration::from_secs(15), || async {
        let peers = list_peers_async(&core_a).await;
        !peers.iter().any(|p| p.device_id == "recon_b" && p.state == PeerConnectionState::Online)
    }).await;
    assert!(disconnected, "A should have detected B offline");

    println!("2.5 Clearning TOFU record for B in A's DB...");
    {
        let db_path = dir_a.path().join("core.db");
        let conn = rusqlite::Connection::open(db_path).expect("Failed to open DB");
        conn.execute(
            "DELETE FROM trusted_peers WHERE device_id = ?",
            ["recon_b"],
        ).expect("Failed to delete TOFU record");
    }

    println!("3. Reviving B...");
    let (_core_b_new, _rx_b_new, _dir_b_new) = create_test_core("recon_b", &shared_uid, |_| {});

    println!("Waiting for reconnection...");
    let reconnected = wait_for(Duration::from_secs(10), || async {
        let peers = list_peers_async(&core_a).await;
        peers.iter().any(|p| p.device_id == "recon_b" && p.state == PeerConnectionState::Online)
    }).await;

    assert!(reconnected, "A failed to reconnect to B");
    core_a.shutdown();
    drop(core_a);
    sleep(Duration::from_millis(200)).await; // 给后台线程一点时间退出（尤其 Windows）
}

#[tokio::test]
async fn test_m1_policy_deny() {
    let shared_uid = "m1_policy_secret";

    let (core_a, _rx_a, _dir_a) = create_test_core("deny_a", shared_uid, |c| {
        c.global_policy = crate::api::GlobalPolicy::DenyAll;
    });

    let (_core_b, mut rx_b, _dir_b) = create_test_core("deny_b", shared_uid, |_| {});

    let connected = wait_for(Duration::from_secs(5), || async {
        let peers = list_peers_async(&core_a).await;
        peers.iter().any(|p| p.device_id == "deny_b" && p.state == PeerConnectionState::Online)
    }).await;
    assert!(connected, "Peers should connect even with DenyAll policy");

    let snapshot = ClipboardSnapshot::Text {
        text_utf8: "Should NOT be sent".to_string(),
        ts_ms: crate::util::now_ms(),
    };
    let _ = core_a.ingest_local_copy(snapshot);

    println!("Checking if B receives data (expecting NO)...");
    sleep(Duration::from_secs(1)).await;

    let mut received = false;
    while let Ok(evt) = rx_b.try_recv() {
        if evt.contains("Should NOT be sent") {
            received = true;
            break;
        }
    }

    assert!(!received, "Policy DenyAll failed! B received data.");
    println!("Policy check passed: Data was blocked.");
    core_a.shutdown();
    drop(core_a);
    sleep(Duration::from_millis(200)).await; // 给后台线程一点时间退出（尤其 Windows）
}