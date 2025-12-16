use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{ItemKind, ItemMeta};

pub struct Store {
    conn: Connection,
}

pub struct CacheRow {
    pub present: bool,
}

impl Store {
    pub fn open(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir)?;
        let db_path: PathBuf = data_dir.join("core.db");

        let conn = Connection::open(db_path)?;
        Self::init_pragmas(&conn)?;
        Self::migrate_v1(&conn)?;
        Ok(Self { conn })
    }

    fn init_pragmas(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA foreign_keys=ON;
            "#,
        )?;
        Ok(())
    }

    fn migrate_v1(conn: &Connection) -> anyhow::Result<()> {
        let user_version: i64 = conn.query_row("PRAGMA user_version;", [], |r| r.get(0))?;
        if user_version >= 1 {
            return Ok(());
        }

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS items (
              item_id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              owner_device_id TEXT NOT NULL,
              created_ts_ms INTEGER NOT NULL,
              size_bytes INTEGER NOT NULL,
              mime TEXT NOT NULL,
              sha256_hex TEXT NOT NULL,
              preview_json TEXT,
              expires_ts_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_ts_ms);
            CREATE INDEX IF NOT EXISTS idx_items_sha256 ON items(sha256_hex);
            CREATE INDEX IF NOT EXISTS idx_items_owner_created ON items(owner_device_id, created_ts_ms);

            CREATE TABLE IF NOT EXISTS history (
              history_id INTEGER PRIMARY KEY AUTOINCREMENT,
              account_uid TEXT NOT NULL,
              item_id TEXT NOT NULL,
              sort_ts_ms INTEGER NOT NULL,
              source_device_id TEXT,
              is_deleted INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_history_account_sort ON history(account_uid, sort_ts_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_history_item ON history(item_id);

            CREATE TABLE IF NOT EXISTS content_cache (
              sha256_hex TEXT PRIMARY KEY,
              total_bytes INTEGER NOT NULL,
              present INTEGER NOT NULL,
              last_access_ts_ms INTEGER NOT NULL,
              created_ts_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cache_lru ON content_cache(present, last_access_ts_ms);
            CREATE INDEX IF NOT EXISTS idx_cache_size ON content_cache(present, total_bytes);

            PRAGMA user_version = 1;
            "#,
        )?;
        Ok(())
    }

    fn kind_to_str(k: &ItemKind) -> &'static str {
        match k {
            ItemKind::Text => "text",
            ItemKind::Image => "image",
            ItemKind::FileList => "file_list",
        }
    }

    /// Phase A：落库（cache 行 present 先按 0/已有值；items/history 插入）
    pub fn insert_meta_and_history(
        &mut self,
        account_uid: &str,
        meta: &ItemMeta,
        now_ms: i64,
    ) -> anyhow::Result<CacheRow> {
        let tx = self.conn.transaction()?;

        // 1) content_cache：先占位（present=0），同 sha 主键去重 :contentReference[oaicite:8]{index=8}
        tx.execute(
            r#"INSERT OR IGNORE INTO content_cache
               (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
               VALUES (?1, ?2, 0, ?3, ?4)"#,
            params![meta.content.sha256, meta.content.total_bytes, now_ms, meta.created_ts_ms],
        )?;

        // 2) items：允许“同 sha 不同 item_id”:contentReference[oaicite:9]{index=9}
        let preview_json = serde_json::to_string(&meta.preview)?;
        tx.execute(
            r#"INSERT INTO items
               (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, preview_json, expires_ts_ms)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                meta.item_id,
                Self::kind_to_str(&meta.kind),
                meta.source_device_id,
                meta.created_ts_ms,
                meta.size_bytes,
                meta.content.mime,
                meta.content.sha256,
                preview_json,
                meta.expires_ts_ms
            ],
        )?;

        // 3) history：sort_ts_ms 固定用 created_ts_ms :contentReference[oaicite:10]{index=10}
        tx.execute(
            r#"INSERT INTO history(account_uid, item_id, sort_ts_ms, source_device_id)
               VALUES (?1, ?2, ?3, ?4)"#,
            params![account_uid, meta.item_id, meta.created_ts_ms, meta.source_device_id],
        )?;

        // 读 present（可能已有=1）
        let present_i: i64 = tx
            .query_row(
                "SELECT present FROM content_cache WHERE sha256_hex=?1",
                params![meta.content.sha256],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);

        tx.commit()?;
        Ok(CacheRow { present: present_i != 0 })
    }

    /// Phase C：CAS 写完后把 present=1，并 touch LRU
    pub fn mark_cache_present(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<()> {
        self.conn.execute(
            r#"UPDATE content_cache
               SET present=1, last_access_ts_ms=?2
               WHERE sha256_hex=?1"#,
            params![sha256_hex, now_ms],
        )?;
        Ok(())
    }

    pub fn touch_cache(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<()> {
        self.conn.execute(
            r#"UPDATE content_cache
               SET last_access_ts_ms=?2
               WHERE sha256_hex=?1"#,
            params![sha256_hex, now_ms],
        )?;
        Ok(())
    }
    pub fn cache_row_count_for_sha(&self, sha256_hex: &str) -> anyhow::Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM content_cache WHERE sha256_hex=?1",
            params![sha256_hex],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn history_count_for_account(&self, account_uid: &str) -> anyhow::Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM history WHERE account_uid=?1 AND is_deleted=0",
            params![account_uid],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn list_history_metas(&self, account_uid: &str, limit: usize) -> anyhow::Result<Vec<ItemMeta>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT i.item_id, i.kind, i.owner_device_id, i.created_ts_ms,
                   i.size_bytes, i.mime, i.sha256_hex, i.preview_json, i.expires_ts_ms
            FROM history h
            JOIN items i ON h.item_id = i.item_id
            WHERE h.account_uid=?1 AND h.is_deleted=0
            ORDER BY h.sort_ts_ms DESC
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map(params![account_uid, limit as i64], |r| {
            let kind_s: String = r.get(1)?;
            let kind = match kind_s.as_str() {
                "text" => ItemKind::Text,
                "image" => ItemKind::Image,
                "file_list" => ItemKind::FileList,
                _ => ItemKind::Text,
            };

            let preview_json: String = r.get(7)?;
            let preview: crate::model::ItemPreview =
                serde_json::from_str(&preview_json).unwrap_or_default();

            Ok(ItemMeta {
                ty: "ItemMeta".to_string(),
                item_id: r.get(0)?,
                kind,
                source_device_id: r.get(2)?,
                source_device_name: None,
                created_ts_ms: r.get(3)?,
                size_bytes: r.get(4)?,
                preview,
                content: crate::model::ItemContent {
                    mime: r.get(5)?,
                    sha256: r.get(6)?,
                    total_bytes: r.get(4)?, // M0 里我们先让它等于 size_bytes
                },
                files: vec![],
                expires_ts_ms: r.get(8)?,
            })
        })?;

        let mut out = Vec::new();
        for it in rows {
            out.push(it?);
        }
        Ok(out)
    }
}
