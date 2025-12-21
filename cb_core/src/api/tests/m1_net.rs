use std::sync::{Arc, Mutex};
use std::time::Duration;
use crate::api::{Core, CoreConfig, CoreEventSink, PeerConnectionState}; // [修改] 引入 PeerConnectionState

// Mock 事件接收器
struct TestSink {
    events: Arc<Mutex<Vec<String>>>,
}

impl TestSink {
    fn new() -> (Arc<Self>, Arc<Mutex<Vec<String>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self { events: events.clone() }),
            events
        )
    }
}

impl CoreEventSink for TestSink {
    fn emit(&self, event_json: String) {
        self.events.lock().unwrap().push(event_json);
    }
}

fn create_test_config(port_offset: u16, unique_tag: &str) -> CoreConfig {
    let mut dir = std::env::temp_dir();
    dir.push(format!("cb_test_m1_{}", port_offset));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    CoreConfig {
        device_id: format!("test_device_{}", port_offset),
        device_name: format!("Test Device {}", port_offset),
        account_uid: "test_account_uid".to_string(),
        account_tag: unique_tag.to_string(),
        data_dir: dir.to_string_lossy().to_string(),
        cache_dir: dir.to_string_lossy().to_string(),
        limits: crate::policy::Limits::default(),
        gc_history_max_items: 100,
        gc_cas_max_bytes: 1024 * 1024,
        // [修复] 增加 policy 字段
        global_policy: crate::api::GlobalPolicy::AllowAll,
    }
}

#[test]
fn test_core_api_basics() {
    let (sink, _events) = TestSink::new();
    let config = create_test_config(1, "tag_basics");

    println!("Init Core...");
    let core = Core::init(config.clone(), sink);

    let status_json = core.get_status().expect("get_status failed");
    println!("Status: {}", status_json);

    assert_eq!(status_json["status"], "Running");
    assert_eq!(status_json["device_id"], config.device_id);
    assert_eq!(status_json["net_enabled"], true);

    let peers = core.list_peers().expect("list_peers failed");
    assert!(peers.is_empty(), "Should have no peers initially");

    core.shutdown();
    let status_after = core.get_status().expect("get_status after shutdown");
    assert_eq!(status_after["status"], "Shutdown");
}

#[tokio::test]
async fn test_m1_simulation_loopback() {
    let tag = "tag_loopback";

    // --- Device A ---
    let (sink_a, _events_a) = TestSink::new();
    let config_a = create_test_config(100, tag);
    let core_a = Core::init(config_a.clone(), sink_a);

    // --- Device B ---
    let (sink_b, _events_b) = TestSink::new();
    let config_b = create_test_config(101, tag);
    let core_b = Core::init(config_b.clone(), sink_b);

    println!("Core A and B started. Waiting for discovery (approx 2-3s)...");

    tokio::time::sleep(Duration::from_secs(3)).await;

    let core_a_clone = core_a.clone();
    let peers_a = tokio::task::spawn_blocking(move || core_a_clone.list_peers())
        .await.unwrap().unwrap();
    println!("Core A see peers: {:?}", peers_a);

    let core_b_clone = core_b.clone();
    let peers_b = tokio::task::spawn_blocking(move || core_b_clone.list_peers())
        .await.unwrap().unwrap();
    println!("Core B see peers: {:?}", peers_b);

    // [修复] 验证逻辑更新：使用 state == Online
    assert!(peers_a.iter().any(|p| p.device_id == config_b.device_id && p.state == PeerConnectionState::Online));
    assert!(peers_b.iter().any(|p| p.device_id == config_a.device_id && p.state == PeerConnectionState::Online));

    core_a.shutdown();
    core_b.shutdown();
}

#[tokio::test]
async fn test_m1_data_broadcast() {
    let tag = "tag_broadcast";

    let (sink_a, _events_a) = TestSink::new();
    let config_a = create_test_config(200, tag);
    let core_a = Core::init(config_a.clone(), sink_a);

    let (sink_b, events_b) = TestSink::new();
    let config_b = create_test_config(201, tag);
    let core_b = Core::init(config_b.clone(), sink_b);

    println!("Waiting for connection...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let core_a_clone = core_a.clone();
    let peers = tokio::task::spawn_blocking(move || core_a_clone.list_peers())
        .await.unwrap().unwrap();
    assert!(!peers.is_empty(), "Peers not connected");

    use crate::clipboard::ClipboardSnapshot;
    let snapshot = ClipboardSnapshot::Text {
        text_utf8: "Hello M1 Network".to_string(),
        ts_ms: crate::util::now_ms(),
    };

    println!("A ingesting data...");
    let meta = core_a.ingest_local_copy_with_force(snapshot, true).expect("Ingest failed");
    println!("A generated meta: {}", meta.item_id);

    println!("Waiting for B to receive meta...");
    let start = std::time::Instant::now();
    let mut received = false;

    while start.elapsed() < Duration::from_secs(3) {
        {
            let evts = events_b.lock().unwrap();
            if let Some(evt) = evts.iter().find(|e| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(e) {
                    v["type"] == "ITEM_META_ADDED" &&
                        v["payload"]["meta"]["item_id"] == meta.item_id
                } else {
                    false
                }
            }) {
                println!("B received event: {}", evt);
                received = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(received, "Device B did not receive the broadcast metadata");

    core_a.shutdown();
    core_b.shutdown();
}

#[tokio::test]
async fn test_m1_reconnection() {
    let tag = "tag_reconnect";

    let (sink_a, _events_a) = TestSink::new();
    let config_a = create_test_config(300, tag);
    let core_a = Core::init(config_a.clone(), sink_a);

    let (sink_b, _events_b) = TestSink::new();
    let config_b = create_test_config(301, tag);
    let core_b = Core::init(config_b.clone(), sink_b);

    println!("1. Waiting for initial connection...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let core_a_clone = core_a.clone();
    let peers = tokio::task::spawn_blocking(move || core_a_clone.list_peers()).await.unwrap().unwrap();
    // [修复] 使用 state == Online
    assert!(peers.iter().any(|p| p.state == PeerConnectionState::Online), "Initial connection failed");

    println!("2. Killing B...");
    core_b.shutdown();
    drop(core_b);

    println!("Waiting for A to detect disconnect (approx 6-8s)...");
    tokio::time::sleep(Duration::from_secs(8)).await;

    // 验证 A 认为 B 已离线
    let core_a_clone = core_a.clone();
    let peers_offline = tokio::task::spawn_blocking(move || core_a_clone.list_peers()).await.unwrap().unwrap();
    // [修复] 检查是否还在线
    let is_still_online = peers_offline.iter().any(|p| p.device_id == config_b.device_id && p.state == PeerConnectionState::Online);
    assert!(!is_still_online, "A should have detected B offline");

    println!("2.5 Clearning TOFU record for B...");
    {
        let db_path = std::path::Path::new(&config_a.data_dir).join("core.db");
        let conn = rusqlite::Connection::open(db_path).expect("Failed to open DB for test cleanup");
        conn.execute(
            "DELETE FROM trusted_peers WHERE device_id = ?",
            [&config_b.device_id],
        ).expect("Failed to delete TOFU record");
    }

    println!("3. Reviving B...");
    let (sink_b_new, _) = TestSink::new();
    let core_b_new = Core::init(config_b.clone(), sink_b_new);

    println!("Waiting for reconnection...");

    let start = std::time::Instant::now();
    let mut reconnected = false;
    while start.elapsed() < Duration::from_secs(15) {
        let core_a_clone = core_a.clone();
        let peers = tokio::task::spawn_blocking(move || core_a_clone.list_peers()).await.unwrap().unwrap();

        // [修复] 使用 state == Online
        if peers.iter().any(|p| p.device_id == config_b.device_id && p.state == PeerConnectionState::Online) {
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    assert!(reconnected, "A failed to reconnect to B after B restarted");

    core_a.shutdown();
    core_b_new.shutdown();
}

#[tokio::test]
async fn test_m1_policy_deny() {
    let tag = "tag_deny";

    // 1. 创建 Config，强制 DenyAll
    let mut config_a = create_test_config(400, tag);
    config_a.global_policy = crate::api::GlobalPolicy::DenyAll; // <--- 关键

    let (sink_a, _) = TestSink::new();
    let core_a = Core::init(config_a, sink_a);

    let config_b = create_test_config(401, tag);
    let (sink_b, events_b) = TestSink::new();
    let core_b = Core::init(config_b, sink_b);

    // 2. 等待连接
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 3. 尝试广播
    use crate::clipboard::ClipboardSnapshot;
    let snapshot = ClipboardSnapshot::Text {
        text_utf8: "Should NOT be sent".to_string(),
        ts_ms: crate::util::now_ms(),
    };
    let _ = core_a.ingest_local_copy_with_force(snapshot, true);

    // 4. 验证 B **没有**收到消息
    tokio::time::sleep(Duration::from_millis(500)).await;
    let evts = events_b.lock().unwrap();
    let received = evts.iter().any(|e| e.contains("Should NOT be sent"));

    assert!(!received, "Policy DenyAll failed! B received data.");
    println!("Policy check passed: Data was blocked.");
}