use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{FileMeta, ItemKind, ItemMeta};

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
        Self::migrate_v2(&conn)?;
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

    fn migrate_v2(conn: &Connection) -> anyhow::Result<()> {
        let user_version: i64 = conn.query_row("PRAGMA user_version;", [], |r| r.get(0))?;
        if user_version >= 2 {
            return Ok(());
        }

        conn.execute_batch(
            r#"
            ALTER TABLE items ADD COLUMN files_json TEXT;

            -- 防止同一个 item 被重复写入 history（最简单：加 unique index）
            CREATE UNIQUE INDEX IF NOT EXISTS idx_history_account_item ON history(account_uid, item_id);

            PRAGMA user_version = 2;
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

        tx.execute(
            r#"INSERT OR IGNORE INTO content_cache
               (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
               VALUES (?1, ?2, 0, ?3, ?4)"#,
            params![meta.content.sha256, meta.content.total_bytes, now_ms, meta.created_ts_ms],
        )?;

        let preview_json = serde_json::to_string(&meta.preview)?;
        let files_json = serde_json::to_string(&meta.files)?;

        tx.execute(
            r#"INSERT INTO items
               (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, preview_json, files_json, expires_ts_ms)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
            params![
                meta.item_id,
                Self::kind_to_str(&meta.kind),
                meta.source_device_id,
                meta.created_ts_ms,
                meta.size_bytes,
                meta.content.mime,
                meta.content.sha256,
                preview_json,
                files_json,
                meta.expires_ts_ms
            ],
        )?;

        // history：sort_ts_ms 固定用 created_ts_ms
        // 如果同 (account_uid,item_id) 已经存在，unique index 会挡住；这里用 OR IGNORE 保证幂等
        tx.execute(
            r#"INSERT OR IGNORE INTO history(account_uid, item_id, sort_ts_ms, source_device_id)
               VALUES (?1, ?2, ?3, ?4)"#,
            params![account_uid, meta.item_id, meta.created_ts_ms, meta.source_device_id],
        )?;

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

    pub fn mark_cache_missing(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            r#"UPDATE content_cache
           SET present=0, last_access_ts_ms=?2
           WHERE sha256_hex=?1"#,
            params![sha256_hex, now_ms],
        )?;
        Ok(n)
    }

    /// Phase C：CAS 写完后把 present=1，并 touch LRU
    pub fn mark_cache_present(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            r#"UPDATE content_cache
           SET present=1, last_access_ts_ms=?2
           WHERE sha256_hex=?1"#,
            params![sha256_hex, now_ms],
        )?;
        Ok(n)
    }

    pub fn touch_cache(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            r#"UPDATE content_cache
           SET last_access_ts_ms=?2
           WHERE sha256_hex=?1"#,
            params![sha256_hex, now_ms],
        )?;
        Ok(n)
    }


    pub fn get_cache_present(&self, sha256_hex: &str) -> anyhow::Result<bool> {
        let v: i64 = self.conn.query_row(
            "SELECT present FROM content_cache WHERE sha256_hex=?1",
            params![sha256_hex],
            |r| r.get(0),
        )?;
        Ok(v != 0)
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
            SELECT
                i.item_id, i.kind, i.owner_device_id, i.created_ts_ms,
                i.size_bytes, i.mime, i.sha256_hex,
                i.preview_json, i.files_json, i.expires_ts_ms,
                cc.total_bytes
            FROM history h
            JOIN items i ON h.item_id = i.item_id
            JOIN content_cache cc ON i.sha256_hex = cc.sha256_hex
            WHERE h.account_uid=?1 AND h.is_deleted=0
            ORDER BY h.sort_ts_ms DESC, h.history_id DESC
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

            let files_json: Option<String> = r.get(8)?;
            let files: Vec<FileMeta> = files_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<FileMeta>>(s).ok())
                .unwrap_or_default();

            let total_bytes: i64 = r.get(10)?;

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
                    total_bytes,
                },
                files,
                expires_ts_ms: r.get(9)?,
            })
        })?;

        let mut out = Vec::new();
        for it in rows {
            out.push(it?);
        }
        Ok(out)
    }

    /// History GC：只保留最新 keep_latest 条，其余软删除
    pub fn soft_delete_history_keep_latest(&mut self, account_uid: &str, keep_latest: i64) -> anyhow::Result<i64> {
        if keep_latest < 0 {
            return Ok(0);
        }
        let before = self.conn.changes();
        self.conn.execute(
            r#"
            UPDATE history
            SET is_deleted=1
            WHERE history_id IN (
                SELECT history_id
                FROM history
                WHERE account_uid=?1 AND is_deleted=0
                ORDER BY sort_ts_ms DESC, history_id DESC
                LIMIT -1 OFFSET ?2
            )
            "#,
            params![account_uid, keep_latest],
        )?;
        let after = self.conn.changes();
        Ok((after - before) as i64)
    }

    /// Cache GC：挑 LRU（present=1）最旧的若干条
    pub fn select_lru_present(&self, limit: i64) -> anyhow::Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT sha256_hex, total_bytes
            FROM content_cache
            WHERE present=1
            ORDER BY last_access_ts_ms ASC, sha256_hex ASC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = vec![];
        for it in rows { out.push(it?); }
        Ok(out)
    }

    pub fn sum_present_bytes(&self) -> anyhow::Result<i64> {
        let s: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(total_bytes),0) FROM content_cache WHERE present=1",
            [],
            |r| r.get(0),
        )?;
        Ok(s)
    }

}
