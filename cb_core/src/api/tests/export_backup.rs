// cb_core/src/api/tests/export_backup.rs
//
// 备份导出（单向，无恢复）的端到端测试：
// - 空/缺失目标目录可以导出，产出的 core.db / logs.db 可被 SQLite 正常打开；
// - 导出的行数与源库一致，manifest.json 字段完整；
// - 非空目标目录被拒绝；
// - include_content=true 会复制 CAS blob（且不复制 cache/tmp）；
// - 中途失败时 `<dest>.partial` 被清理，不会留下半个备份。

use super::common::*;
use crate::api::{export_test_hooks, BACKUP_FORMAT};
use crate::clipboard::ClipboardSnapshot;
use crate::store::SCHEMA_VERSION;
use crate::util::now_ms;
use rusqlite::Connection;
use std::path::Path;

fn count(path: &Path, table: &str) -> i64 {
	let conn = Connection::open(path).expect("open exported sqlite db");
	conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
		row.get(0)
	})
	.expect("count rows")
}

fn source_count(core: &crate::api::Core, table: &str) -> i64 {
	let store = core.inner.store.lock().unwrap();
	store
		.conn
		.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
			row.get(0)
		})
		.expect("count source rows")
}

fn partial_path(dest: &Path) -> std::path::PathBuf {
	dest.with_file_name(format!(
		"{}.partial",
		dest.file_name().unwrap().to_string_lossy()
	))
}

fn ingest_text(core: &crate::api::Core, text: &str) {
	core.ingest_local_copy(ClipboardSnapshot::Text {
		text_utf8: text.to_string(),
		ts_ms: now_ms(),
	})
	.expect("ingest text");
}

#[test]
fn export_to_empty_dir_creates_openable_snapshots_and_manifest() {
	let (core, dirs) = mk_core("export-backup-ok", 50_000, 1024 * 1024 * 1024);
	ingest_text(&core, "backup item one");
	ingest_text(&core, "backup item two");

	let dest = dirs.root.join("backup-ok");
	let info = core
		.export_backup(&dest, false)
		.expect("export into a missing directory");

	assert_eq!(info.dest, dest);
	assert_eq!(info.items, 2);
	assert_eq!(info.history, 2);
	assert!(info.core_db_bytes > 0, "core.db snapshot must not be empty");
	assert!(info.logs_db_bytes > 0, "logs.db snapshot must not be empty");
	assert_eq!(info.content_bytes, 0, "include_content=false copies no CAS");
	assert!(info.created_at_ms > 0);

	// 原子发布：最终目录存在，partial 不残留。
	assert!(dest.is_dir());
	assert!(!partial_path(&dest).exists());

	// 两个数据库都必须能被 SQLite 重新打开，且行数与源库一致。
	let exported_core = dest.join("core.db");
	let exported_logs = dest.join("logs.db");
	assert_eq!(count(&exported_core, "items"), source_count(&core, "items"));
	assert_eq!(
		count(&exported_core, "history"),
		source_count(&core, "history")
	);
	assert_eq!(
		count(&exported_core, "content_cache"),
		source_count(&core, "content_cache")
	);
	assert!(
		count(&exported_logs, "logs") > 0,
		"logs.db snapshot must contain the diagnostic log rows"
	);

	// manifest 必须自洽且能解析。
	let manifest: serde_json::Value =
		serde_json::from_slice(&std::fs::read(dest.join("manifest.json")).expect("read manifest"))
			.expect("manifest is valid JSON");
	assert_eq!(manifest["format"], BACKUP_FORMAT);
	assert_eq!(manifest["schema_version"], SCHEMA_VERSION);
	assert_eq!(manifest["app_version"], env!("CARGO_PKG_VERSION"));
	assert!(!manifest["include_content"].as_bool().unwrap());
	assert_eq!(manifest["created_at_ms"], info.created_at_ms);
	assert_eq!(manifest["counts"]["items"], 2);
	assert_eq!(manifest["counts"]["history"], 2);
	assert!(
		manifest["counts"]["logs"].as_i64().unwrap() > 0,
		"manifest must record the logs.db row count"
	);
	assert_eq!(
		count(&exported_logs, "logs"),
		manifest["counts"]["logs"].as_i64().unwrap()
	);
	assert_eq!(manifest["sizes"]["core_db_bytes"], info.core_db_bytes);
	assert_eq!(manifest["sizes"]["logs_db_bytes"], info.logs_db_bytes);
	assert_eq!(manifest["sizes"]["content_bytes"], 0);
	assert_eq!(
		manifest["sizes"]["total_bytes"],
		info.core_db_bytes + info.logs_db_bytes
	);

	// include_content=false 不应创建 cache/ 目录。
	assert!(!dest.join("cache").exists());

	core.shutdown();
}

#[test]
fn export_refuses_non_empty_destination() {
	let (core, dirs) = mk_core("export-backup-nonempty", 50_000, 1024 * 1024 * 1024);
	let dest = dirs.root.join("backup-nonempty");
	std::fs::create_dir_all(&dest).unwrap();
	std::fs::write(dest.join("keep.txt"), b"user data").unwrap();

	let error = core
		.export_backup(&dest, false)
		.expect_err("non-empty destination must be refused");
	assert!(
		error.to_string().contains("not empty"),
		"unexpected error: {error:#}"
	);

	// 用户文件原封不动，partial 也不存在。
	assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"user data");
	assert!(!partial_path(&dest).exists());
	assert!(!dest.join("core.db").exists());

	core.shutdown();
}

#[test]
fn include_content_copies_cas_blobs_but_not_tmp() {
	let (core, dirs) = mk_core("export-backup-content", 50_000, 1024 * 1024 * 1024);
	let text = "content that must survive in the backup";
	let meta = core
		.ingest_local_copy(ClipboardSnapshot::Text {
			text_utf8: text.to_string(),
			ts_ms: now_ms(),
		})
		.expect("ingest text");
	let sha = meta.content.sha256.clone();
	let src_blob = core.inner.cas.blob_path(&sha).expect("blob path");
	assert!(src_blob.exists());

	let dest = dirs.root.join("backup-content");
	let info = core
		.export_backup(&dest, true)
		.expect("export with content");
	assert!(info.content_bytes > 0, "CAS bytes must be reported");

	let relative = src_blob
		.strip_prefix(core.inner.cas.cache_dir())
		.expect("blob lives under cache dir");
	let dst_blob = dest.join("cache").join(relative);
	assert!(dst_blob.exists(), "CAS blob must be copied");
	assert_eq!(
		std::fs::read(&dst_blob).unwrap(),
		std::fs::read(&src_blob).unwrap()
	);

	// cache/tmp 永远不复制。
	assert!(!dest.join("cache").join("tmp").exists());

	let manifest: serde_json::Value =
		serde_json::from_slice(&std::fs::read(dest.join("manifest.json")).expect("read manifest"))
			.expect("manifest is valid JSON");
	assert!(manifest["include_content"].as_bool().unwrap());
	assert!(manifest["sizes"]["content_bytes"].as_u64().unwrap() > 0);

	core.shutdown();
}

#[test]
fn failed_export_cleans_partial_directory() {
	let (core, dirs) = mk_core("export-backup-cleanup", 50_000, 1024 * 1024 * 1024);
	ingest_text(&core, "rollback me");
	let dest = dirs.root.join("backup-cleanup");

	export_test_hooks::set_fail_after_snapshot(true);
	let result = core.export_backup(&dest, false);
	export_test_hooks::set_fail_after_snapshot(false);

	let error = result.expect_err("injected failure must fail the export");
	assert!(
		error.to_string().contains("injected backup failure"),
		"unexpected error: {error:#}"
	);

	// 不能留下半备份：dest 与 `<dest>.partial` 都不存在。
	assert!(!dest.exists(), "failed export must not publish dest");
	assert!(!partial_path(&dest).exists(), "partial must be cleaned up");

	core.shutdown();
}
