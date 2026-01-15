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

	// [修复点] B 端禁用自动预取，以确保能测试 "Lazy Fetch" -> "Manual Fetch" 的流程
	// 否则小文本会自动下载，导致步骤 5 的 present=0 断言失败
	let (core_b, mut rx_b, dir_b) = create_test_core("m3_b", &shared_uid, |c| {
		c.app_config.size_limits.text_auto_prefetch_bytes = 0;
	});

	println!("Cores started. Waiting for discovery...");
	// 2. 等待互联
	let connected = wait_for(Duration::from_secs(15), || async {
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
	while start.elapsed() < Duration::from_secs(15) {
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

	// 6. [核心] B 发起拉取请求
	println!("B calling ensure_content_cached...");
	let c_b = core_b.clone();
	let i_id = item_id.clone();
	let transfer_id = tokio::task::spawn_blocking(move || {
		c_b.ensure_content_cached(&i_id, None)
	}).await.unwrap().expect("ensure_content_cached failed");

	println!("Transfer initiated: {}", transfer_id);

	// 7. 等待传输完成
	let mut received_cached = false;
	let mut local_path_str = String::new();

	let start_fetch = std::time::Instant::now();
	while start_fetch.elapsed() < Duration::from_secs(15) {
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

#[tokio::test]
async fn test_m3_2_image_fetch_success() {
	let shared_uid = format!("m3_img_{}", uuid::Uuid::new_v4());
	let (core_a, _rx_a, _dir_a) = create_test_core("m3_img_a", &shared_uid, |_| {});
	let (core_b, mut rx_b, _dir_b) = create_test_core("m3_img_b", &shared_uid, |_| {});

	wait_for(Duration::from_secs(5), || async {
		let peers = list_peers_async(&core_a).await;
		peers.iter().any(|p| p.device_id == "m3_img_b" && p.state == PeerConnectionState::Online)
	}).await;

	// 1. A 产生图片数据 (模拟 PNG)
	let png_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x01];
	let snapshot = ClipboardSnapshot::Image {
		bytes: png_bytes.clone(),
		mime: "image/png".to_string(),
		ts_ms: crate::util::now_ms(),
	};
	let meta = core_a.ingest_local_copy(snapshot).expect("Ingest failed");
	let item_id = meta.item_id.clone();

	println!("Waiting for B to receive meta...");
	let start_meta = std::time::Instant::now();
	let mut received_meta = false;
	while start_meta.elapsed() < Duration::from_secs(15) {
		if let Ok(evt_json) = rx_b.try_recv() {
			if evt_json.contains("ITEM_META_ADDED") && evt_json.contains(&item_id) {
				received_meta = true;
				break;
			}
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	assert!(received_meta, "B did not receive ITEM_META_ADDED for image");

	// 2. B 发起请求
	println!("B requesting image...");
	let c_b = core_b.clone();
	let i_id = item_id.clone();
	let transfer_id = tokio::task::spawn_blocking(move || {
		c_b.ensure_content_cached(&i_id, None)
	}).await.unwrap().expect("ensure failed");

	// 3. 等待传输完成并验证后缀
	let start = std::time::Instant::now();
	let mut local_path_str = String::new();
	while start.elapsed() < Duration::from_secs(15) {
		if let Ok(evt_json) = rx_b.try_recv() {
			if evt_json.contains("CONTENT_CACHED") && evt_json.contains(&transfer_id) {
				let v: serde_json::Value = serde_json::from_str(&evt_json).unwrap();
				local_path_str = v["payload"]["local_ref"]["local_path"].as_str().unwrap().to_string();
				break;
			}
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}

	assert!(!local_path_str.is_empty(), "Image transfer timed out");
	assert!(local_path_str.ends_with(".png"), "Local path must have .png extension: {}", local_path_str);

	let content = std::fs::read(&local_path_str).unwrap();
	assert_eq!(content, png_bytes, "Image content mismatch");

	core_a.shutdown();
	core_b.shutdown();
}

#[tokio::test]
async fn test_m3_3_file_fetch_success() {
	let shared_uid = format!("m3_file_{}", uuid::Uuid::new_v4());
	let (core_a, _rx_a, _dir_a) = create_test_core("m3_file_a", &shared_uid, |_| {});
	let (core_b, mut rx_b, _dir_b) = create_test_core("m3_file_b", &shared_uid, |_| {});

	wait_for(Duration::from_secs(15), || async {
		let peers = list_peers_async(&core_a).await;
		peers.iter().any(|p| p.device_id == "m3_file_b" && p.state == PeerConnectionState::Online)
	}).await;

	// 1. A Ingest FileList
	let file_content = b"Content of report.pdf";
	let sha = crate::util::sha256_hex(file_content);

	// A. 写入 CAS (测试直接写 CAS，不依赖本地路径)
	core_a.inner.cas.put_blob(file_content).unwrap();

	let file_id = uuid::Uuid::new_v4().to_string();
	let meta = crate::model::ItemMeta {
		ty: "ItemMeta".to_string(),
		item_id: "item_files_001".to_string(),
		kind: crate::model::ItemKind::FileList,
		created_ts_ms: crate::util::now_ms(),
		source_device_id: "m3_file_a".to_string(),
		source_device_name: None,
		size_bytes: file_content.len() as i64,
		preview: Default::default(),
		content: crate::model::ItemContent {
			mime: "application/x-clipbridge-filelist+json".to_string(),
			sha256: "manifest_sha_ignored_in_this_test".to_string(),
			total_bytes: 100,
		},
		files: vec![
			crate::model::FileMeta {
				file_id: file_id.clone(),
				rel_name: "report.pdf".to_string(),
				size_bytes: file_content.len() as i64,
				sha256: Some(sha.clone()),
				local_path: None, // 模拟只有 CAS 的情况
			}
		],
		expires_ts_ms: None,
	};

	// B. 必须写入 A 的 DB
	{
		let store = core_a.inner.store.lock().unwrap();
		let files_json = serde_json::to_string(&meta.files).unwrap();

		store.conn.execute(
			"INSERT INTO items (
                item_id, kind, owner_device_id, created_ts_ms, size_bytes,
                mime, sha256_hex, files_json, expires_ts_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
			(
				&meta.item_id,
				"file_list",
				&meta.source_device_id,
				meta.created_ts_ms,
				meta.size_bytes,
				&meta.content.mime,
				&meta.content.sha256,
				&files_json,
				Option::<i64>::None
			)
		).expect("Failed to insert meta into A's DB");
	}

	// C. A 广播 Meta
	if let Some(net) = &core_a.inner.net {
		net.send(crate::net::NetCmd::BroadcastMeta(meta.clone())).await.unwrap();
	}

	// 2. B 等待 Meta
	println!("Waiting for B to receive file list meta...");
	let start_meta = std::time::Instant::now();
	let mut received = false;
	while start_meta.elapsed() < Duration::from_secs(15) {
		if let Ok(evt) = rx_b.try_recv() {
			if evt.contains("ITEM_META_ADDED") && evt.contains("item_files_001") {
				received = true;
				break;
			}
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	assert!(received, "B missing meta");

	// 3. B 发起拉取
	println!("B requesting specific file...");
	let c_b = core_b.clone();
	let i_id = "item_files_001".to_string();
	let f_id = file_id.clone();
	let transfer_id = tokio::task::spawn_blocking(move || {
		c_b.ensure_content_cached(&i_id, Some(&f_id))
	}).await.unwrap().expect("ensure failed");

	// 4. 验证结果
	let start = std::time::Instant::now();
	let mut local_path_str = String::new();
	while start.elapsed() < Duration::from_secs(15) {
		if let Ok(evt_json) = rx_b.try_recv() {
			if evt_json.contains("CONTENT_CACHED") && evt_json.contains(&transfer_id) {
				let v: serde_json::Value = serde_json::from_str(&evt_json).unwrap();
				local_path_str = v["payload"]["local_ref"]["local_path"].as_str().unwrap().to_string();
				break;
			}
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}

	assert!(!local_path_str.is_empty(), "File transfer timed out");
	assert!(local_path_str.ends_with("report.pdf"), "Filename mismatch: {}", local_path_str);

	let got = std::fs::read(local_path_str).unwrap();
	assert_eq!(got, file_content);
	println!("M3-3 File Fetch Success!");

	core_a.shutdown();
	core_b.shutdown();
}
