// cb_core/src/api/tests/m3_content.rs

use std::time::Duration;
use crate::api::PeerConnectionState;
use crate::clipboard::ClipboardSnapshot;
// 复用 m1_net 的测试脚手架
use super::m1_net::{create_test_core, list_peers_async, wait_for};

#[tokio::test]
async fn test_m3_1_text_fetch_success() {
    let shared_uid = format!("m3_text_{}", uuid::Uuid::new_v4());

    // 1. 启动两个 Core (A 和 B)
    let (core_a, _rx_a, _dir_a) = create_test_core("m3_a", &shared_uid, |_| {});
    let (core_b, mut rx_b, dir_b) = create_test_core("m3_b", &shared_uid, |_| {});

    println!("Cores started. Waiting for discovery...");

    // 2. 等待互联
    let connected = wait_for(Duration::from_secs(5), || async {
        let peers = list_peers_async(&core_a).await;
        peers.iter().any(|p| p.device_id == "m3_b" && p.state == PeerConnectionState::Online)
    }).await;
    assert!(connected, "Peers failed to connect");

    // 3. A 产生数据
    let raw_text = "Hello ClipBridge M3! This is a test payload.";
    let snapshot = ClipboardSnapshot::Text {
        text_utf8: raw_text.to_string(),
        ts_ms: crate::util::now_ms(),
    };

    println!("A ingesting item...");
    let meta = core_a.ingest_local_copy(snapshot).expect("Ingest failed");
    let item_id = meta.item_id.clone();
    let sha256 = meta.content.sha256.clone();

    // 4. 等待 B 收到 Meta
    println!("Waiting for B to receive meta...");
    let mut received_meta = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if let Ok(evt_json) = rx_b.try_recv() {
            if evt_json.contains("ITEM_META_ADDED") && evt_json.contains(&item_id) {
                received_meta = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(received_meta, "B did not receive ITEM_META_ADDED");

    // 5. 验证 B 的 DB 状态 (Present=0)
    let db_path = dir_b.path().join("core.db");
    {
        let path = db_path.clone();
        let sha = sha256.clone();
        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(path).unwrap();
            let present: i64 = conn.query_row(
                "SELECT present FROM content_cache WHERE sha256_hex = ?",
                [&sha],
                |r| r.get(0)
            ).unwrap_or(0);
            assert_eq!(present, 0, "Content should be Lazy Fetched (present=0)");
        }).await.unwrap();
    }

    // 6. [核心] B 发起拉取请求 (修复点)
    println!("B calling ensure_content_cached...");

    // --- 修复：使用 spawn_blocking 包裹同步阻塞调用 ---
    let c_b = core_b.clone();
    let i_id = item_id.clone();
    let transfer_id = tokio::task::spawn_blocking(move || {
        c_b.ensure_content_cached(&i_id, None)
    }).await.unwrap().expect("ensure_content_cached failed");
    // ----------------------------------------------

    println!("Transfer initiated: {}", transfer_id);

    // 7. 等待传输完成
    let mut received_cached = false;
    let mut local_path_str = String::new();

    let start_fetch = std::time::Instant::now();
    while start_fetch.elapsed() < Duration::from_secs(5) {
        if let Ok(evt_json) = rx_b.try_recv() {
            if evt_json.contains("CONTENT_CACHED") && evt_json.contains(&transfer_id) {
                received_cached = true;
                let v: serde_json::Value = serde_json::from_str(&evt_json).unwrap();
                if let Some(path) = v["payload"]["local_ref"]["local_path"].as_str() {
                    local_path_str = path.to_string();
                }
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(received_cached, "B did not receive CONTENT_CACHED event");
    assert!(!local_path_str.is_empty(), "local_path should not be empty");

    // 8. 最终验证
    let file_content = std::fs::read_to_string(&local_path_str).expect("Failed to read downloaded file");
    assert_eq!(file_content, raw_text, "Content mismatch!");

    {
        let path = db_path.clone();
        let sha = sha256.clone();
        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(path).unwrap();
            let present: i64 = conn.query_row(
                "SELECT present FROM content_cache WHERE sha256_hex = ?",
                [&sha],
                |r| r.get(0)
            ).unwrap();
            assert_eq!(present, 1, "DB should be updated to present=1");
        }).await.unwrap();
    }

    println!("M3-1 Text Fetch Integration Test Passed!");

    core_a.shutdown();
    core_b.shutdown();
}