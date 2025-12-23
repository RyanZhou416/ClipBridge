// cb_core/src/api/tests/m2_meta.rs

use std::time::Duration;
use crate::api::{PeerConnectionState};
use super::m1_net::{create_test_core, list_peers_async, wait_for};
use crate::clipboard::ClipboardSnapshot;
use crate::net::NetCmd;

#[tokio::test]
async fn test_m2_meta_sync_and_db_persistence() {
    let shared_uid = "m2_meta_sync";

    // 1. 启动两个 Core
    let (core_a, _rx_a, _dir_a) = create_test_core("m2_a", shared_uid, |_| {});
    let (core_b, mut rx_b, dir_b) = create_test_core("m2_b", shared_uid, |_| {});

    // 2. 等待互联
    let connected = wait_for(Duration::from_secs(5), || async {
        let peers = list_peers_async(&core_a).await;
        peers.iter().any(|p| p.device_id == "m2_b" && p.state == PeerConnectionState::Online)
    }).await;
    assert!(connected, "Peers not connected");

    // 3. A 产生数据
    let snapshot = ClipboardSnapshot::Text {
        text_utf8: "M2 Persistence Test".to_string(),
        ts_ms: crate::util::now_ms(),
    };
    let meta = core_a.ingest_local_copy(snapshot).expect("Ingest failed");
    let item_id = meta.item_id.clone();

    // 4. B 等待收到 ITEM_META_ADDED 事件 (M2-1 部分)
    let mut received_event = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(evt_json) = rx_b.try_recv() {
            if evt_json.contains("ITEM_META_ADDED") && evt_json.contains(&item_id) {
                received_event = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(received_event, "B should receive ITEM_META_ADDED event");

    // 5. [核心] 验证 B 的数据库状态 (M2-1 DB 部分)
    {
        let db_path = dir_b.path().join("core.db");
        // 使用 spawn_blocking 避免在该 async test 中阻塞
        let verify_db = tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(db_path).unwrap();

            // 验证 items 表
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM items WHERE item_id = ?",
                [&item_id], |r| r.get(0)
            ).unwrap();
            assert_eq!(count, 1, "Item should be in DB");

            // 验证 history 表
            let hist_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM history WHERE item_id = ?",
                [&item_id], |r| r.get(0)
            ).unwrap();
            assert_eq!(hist_count, 1, "History should be in DB");

            // 验证 content_cache 表 (Lazy Fetch: present 应该是 0)
            let (present, total): (i64, i64) = conn.query_row(
                "SELECT present, total_bytes FROM content_cache WHERE sha256_hex = ?",
                [&meta.content.sha256], |r| Ok((r.get(0)?, r.get(1)?))
            ).unwrap();
            assert_eq!(present, 0, "Content should NOT be present (Lazy Fetch)");
            assert!(total > 0, "Total bytes should be recorded");
        }).await.unwrap();
    }

    // 6. [核心] 验证幂等性 (M2-2)
    // A 再次广播同一条 meta (模拟重连或重发)
    // 这里我们直接通过 Internal API 或者 NetCmd 模拟发送有点复杂，
    // 最简单的方式是让 A 再次 ingest 同样的内容？不，那样会生成新的 item_id。
    // 我们手动构造 NetCmd::BroadcastMeta 发送给 A 的网络层让它重发 (需要一点 hack，或者直接让 A ingest 一个完全一样的 content 但 force item_id? 不行)

    // 我们采用直接观察 B 行为的方式：
    // B 如果收到重复的，Store::insert_remote_item 会返回 false，从而不触发事件。
    // 但是在这个集成测试里，很难强迫 A 重发旧的 item_id。
    // 替代方案：我们在 B 侧手动由 TestCore 注入一个重复的 Remote Meta (绕过网络，只测 SessionActor 逻辑不太容易，因为 Actor 私有)

    // 妥协方案：验证 DB 约束。
    // 既然我们在 Step 1 已经验证了 DB 有 Unique Index 和 INSERT OR IGNORE 逻辑，
    // 这里主要验证 "history count 不增加"。
    // 由于难以在集成层触发“重发旧 item_id”，我们将此部分留给 store 单元测试覆盖。
    // 或者，我们可以重启 B (simulation reconnection) 并检查历史记录是否重复。
}

#[test]
fn test_m2_store_idempotency() {
    // 这是一个单元测试，专门测 Step 1 的 Store 逻辑
    use crate::store::Store;
    use crate::model::{ItemMeta, ItemKind, ItemContent, ItemPreview};

    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let acct = "test_acct";
    let now = crate::util::now_ms();

    let meta = ItemMeta {
        ty: "ItemMeta".to_string(),
        item_id: "duplicate_id".to_string(),
        kind: ItemKind::Text,
        created_ts_ms: now,
        source_device_id: "src_dev".to_string(),
        source_device_name: None,
        size_bytes: 100,
        preview: ItemPreview::default(),
        content: ItemContent { mime: "text/plain".to_string(), sha256: "abc".to_string(), total_bytes: 100 },
        files: vec![],
        expires_ts_ms: None,
    };

    // 第一次插入
    let is_new = store.insert_remote_item(acct, &meta, now).unwrap();
    assert!(is_new, "First insert should be new");

    // 第二次插入 (模拟重放)
    let is_new_2 = store.insert_remote_item(acct, &meta, now + 1000).unwrap();
    assert!(!is_new_2, "Second insert should be ignored (idempotent)");
}

#[tokio::test]
async fn test_m2_robustness_network_replay() {
    let shared_uid = format!("m2_robustness_replay_{}", uuid::Uuid::new_v4());

    // 1. 启动环境
    let (core_a, _rx_a, _dir_a) = create_test_core("m2_rob_a", &shared_uid, |_| {});
    let (core_b, mut rx_b, dir_b) = create_test_core("m2_rob_b", &shared_uid, |_| {});

    // 2. 建立连接
    let connected = wait_for(Duration::from_secs(5), || async {
        let peers = list_peers_async(&core_a).await;
        peers.iter().any(|p| p.device_id == "m2_rob_b" && p.state == PeerConnectionState::Online)
    }).await;
    assert!(connected, "Peers not connected");

    // 3. A 产生第一条数据
    let snapshot = ClipboardSnapshot::Text {
        text_utf8: "Replay Attack Test".to_string(),
        ts_ms: crate::util::now_ms(),
    };
    let meta = core_a.ingest_local_copy(snapshot).expect("Ingest failed");
    let item_id = meta.item_id.clone();

    // 4. 等待 B 首次接收并处理
    let mut first_receive = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(evt_json) = rx_b.try_recv() {
            if evt_json.contains("ITEM_META_ADDED") && evt_json.contains(&item_id) {
                first_receive = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(first_receive, "B should receive the first event");

    // 验证 B 的 DB 初始状态：应该有 1 条记录
    let db_path = dir_b.path().join("core.db");
    {
        let path = db_path.clone();
        let iid = item_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(path).unwrap();
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM history WHERE item_id = ?",
                [&iid], |r| r.get(0)
            ).unwrap();
            assert_eq!(count, 1, "Initial history count should be 1");
        }).await.unwrap();
    }

    // --- 关键步骤：模拟网络重放 ---
    println!("Simulating network replay: A resends the SAME meta...");

    // 我们直接获取 A 的内部 NetManager 通道，手动发送一个 BroadcastMeta 命令
    // 这完全模拟了 A 决定重发旧数据的场景
    if let Some(net_tx) = &core_a.inner.net {
        let _ = net_tx.try_send(NetCmd::BroadcastMeta(meta.clone()));
    } else {
        panic!("Core A net is missing");
    }

    // 5. 等待一小段时间，确保 B 收到了这个包并进行了处理
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 6. 验证健壮性：B 不应再发出 ITEM_META_ADDED 事件
    let mut duplicate_event = false;
    while let Ok(evt_json) = rx_b.try_recv() {
        if evt_json.contains("ITEM_META_ADDED") && evt_json.contains(&item_id) {
            duplicate_event = true;
            println!("Fail: Received duplicate event: {}", evt_json);
        }
    }
    assert!(!duplicate_event, "B should NOT emit ITEM_META_ADDED for duplicate meta");

    // 7. 验证数据库：History 数量仍应为 1 (DB 层的幂等性)
    {
        let path = db_path.clone();
        let iid = item_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(path).unwrap();
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM history WHERE item_id = ?",
                [&iid], |r| r.get(0)
            ).unwrap();
            assert_eq!(count, 1, "History count should remain 1 after replay");
        }).await.unwrap();
    }

    println!("SUCCESS: Network replay robustness test passed.");
}