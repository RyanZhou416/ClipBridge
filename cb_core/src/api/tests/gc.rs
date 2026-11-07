use super::common::*;

use crate::model::{ItemContent, ItemKind, ItemMeta, ItemPreview};

fn gc_text_meta(i: i64) -> ItemMeta {
	ItemMeta {
		ty: "ItemMeta".to_string(),
		item_id: format!("item-gc-{i:03}"),
		kind: ItemKind::Text,
		created_ts_ms: i,
		source_device_id: "peer-a".to_string(),
		source_device_name: None,
		size_bytes: 8,
		preview: ItemPreview {
			text: Some(format!("row {i}")),
			..Default::default()
		},
		content: ItemContent {
			mime: "text/plain".to_string(),
			sha256: format!("sha-gc-{i:03}"),
			total_bytes: 8,
		},
		files: Vec::new(),
		expires_ts_ms: None,
	}
}

#[test]
fn gc_evicts_lru_when_over_cap() {
	// cap = 50 bytes
	let (core, _dirs) = mk_core("gc", 1_000_000, 50);

	let ts1 = crate::util::now_ms();
	let b1 = vec![1u8; 40];
	let sha1 = crate::util::sha256_hex(&b1);

	let _ = core
		.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
			bytes: b1,
			mime: "image/png".to_string(),
			ts_ms: ts1,
		})
		.unwrap();

	let ts2 = ts1 + 1;
	let b2 = vec![2u8; 40];
	let sha2 = crate::util::sha256_hex(&b2);

	let _ = core
		.ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
			bytes: b2,
			mime: "image/png".to_string(),
			ts_ms: ts2,
		})
		.unwrap();

	// ingest 第二个后会触发 AfterIngest GC（你的 apply_ingest 里会 run_gc）:contentReference[oaicite:4]{index=4}
	// cap=50，总量=80，应该踢掉 LRU（第一个）
	assert!(!core.inner.cas.blob_exists(&sha1));
	assert!(core.inner.cas.blob_exists(&sha2));

	let (p1, p2) = {
		let store = core.inner.store.lock().unwrap();
		(
			store.get_cache_present(&sha1).unwrap_or(false),
			store.get_cache_present(&sha2).unwrap_or(false),
		)
	};

	assert!(!p1);
	assert!(p2);
}

/// P3：历史行被硬删除（保留策略）之后，`run_gc` 的有界空间回收必须删掉
/// 变成孤儿的 items 与无引用的 content_cache 行，同时保留仍有 history 引用的行。
#[test]
fn run_gc_reclaims_orphans_after_history_hard_delete() {
	let (core, _dirs) = mk_core("gc-space-reclaim", 50_000, 1_i64 << 40);
	let store = &core.inner.store;

	{
		let mut guard = store.lock().unwrap();
		for i in 0..20 {
			guard
				.insert_remote_item("acct-uid-1", &gc_text_meta(i), i)
				.unwrap();
		}
		// 模拟历史保留：硬删除最旧的 15 行 history（items 暂成孤儿）。
		assert_eq!(
			guard.purge_history_oldest_excess("acct-uid-1", 15).unwrap(),
			15
		);
	}
	let count = |sql: &str| -> i64 {
		store
			.lock()
			.unwrap()
			.conn
			.query_row(sql, [], |r| r.get(0))
			.unwrap()
	};
	assert_eq!(count("SELECT COUNT(*) FROM items"), 20);

	// 启动路径（reason == "Startup"）触发有界空间回收；生产环境由启动 /
	// 历史硬删除 / CAS 驱逐触发，周期兜底在后台维护线程。
	core.run_gc("Startup").unwrap();

	assert_eq!(
		count("SELECT COUNT(*) FROM items"),
		5,
		"只保留仍有 history 行引用的 5 条 items"
	);
	assert_eq!(
		count("SELECT COUNT(*) FROM content_cache"),
		5,
		"孤儿 items 的 present=0 cache 行必须一并回收"
	);
	// 存活历史仍可读（读路径走只读连接池）。
	let visible = core.list_history(100, None).unwrap();
	assert_eq!(visible.len(), 5, "存活历史不得被空间回收误删");

	core.shutdown();
}
