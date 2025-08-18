// cb_core/src/storage.rs
//! SQLite 存储 + CAS(内容寻址存储) 封装（v1 最小实现）
//!
//! 目标：
//! - 维护 items（元数据表）、history（历史表）；
//! - 提供去重窗口判断（复用既有 item_id）；
//! - 提供 CAS 路径计算与写入/删除；
//! - 提供简单的缓存清理（按体积与条数上限近似 LRU）；
//!
//! 依赖：rusqlite（轻量稳定），serde_json（存储 JSON 字符串）。

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as Json;

use crate::api::{CacheLimits, HistoryEntry, HistoryKind, ItemRecord};
use crate::proto::ItemMeta;

/// CAS 路径工具：根据 sha256 计算落盘路径（避免大目录）。
#[derive(Clone)]
pub struct CasPaths {
    root: PathBuf,
}

impl CasPaths {
    /// 新建路径计算器。
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// 根据 sha256 计算文件绝对路径（`<root>/<aa>/<sha256>`）。
    #[must_use]
    pub fn path_for_sha256(&self, sha256_hex: &str) -> PathBuf {
        let shard = &sha256_hex[0..2.min(sha256_hex.len())];
        self.root.join(shard).join(sha256_hex)
    }
}

/// v1 的 SQLite 封装。一个进程建议只持有一个实例。
pub struct Storage {
    conn: Connection,
}

// 去重窗口（毫秒）。窗口内，同一来源设备 + 同一 sha256 视为重复复制。
const DEDUP_WINDOW_MS: i64 = 1_500;

// 历史默认保留条数（用于 `prune_history` 的占位策略）
const HISTORY_KEEP: i64 = 5_000;

impl Storage {
    /// 打开或创建数据库，并确保表结构就绪。
    pub fn open(data_dir: PathBuf) -> rusqlite::Result<Self> {
        let db_path = data_dir.join("cb_core.sqlite3");
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", &"WAL")?;
        conn.pragma_update(None, "synchronous", &"NORMAL")?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
        // items：存储元数据 + 是否已缓存正文（present）
        // 注意：mimes_json 与 preview_json 用 TEXT 存储 JSON 字符串
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS items (
              item_id           TEXT PRIMARY KEY,
              owner_device_id   TEXT NOT NULL,
              owner_device_name TEXT,
              mimes_json        TEXT NOT NULL,
              size_bytes        INTEGER NOT NULL,
              sha256_hex        TEXT NOT NULL,
              expires_at        INTEGER,
              preview_json      TEXT NOT NULL,
              uri               TEXT NOT NULL,
              created_at        INTEGER NOT NULL,
              present           INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_items_owner_sha ON items(owner_device_id, sha256_hex);
            CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_at);

            -- history：顺序日志，按自增 seq_id
            CREATE TABLE IF NOT EXISTS history (
              seq_id      INTEGER PRIMARY KEY AUTOINCREMENT,
              item_id     TEXT NOT NULL,
              kind        TEXT NOT NULL,  -- 'copy_local' | 'recv_remote' | 'paste_local'
              created_at  INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_history_item ON history(item_id);
            CREATE INDEX IF NOT EXISTS idx_history_created ON history(created_at);
        "#,
        )?;
        Ok(())
    }

    /// 写入或更新一条元数据，`present` 表示本地是否已具备正文。
    pub fn upsert_item(&self, meta: &ItemMeta, present: bool) -> rusqlite::Result<()> {
        let mimes_json = serde_json::to_string(&meta.mimes).unwrap_or("[]".into());
        let preview_json = meta.preview_json.to_string();
        self.conn.execute(
            r#"
            INSERT INTO items (
              item_id, owner_device_id, owner_device_name, mimes_json,
              size_bytes, sha256_hex, expires_at, preview_json, uri,
              created_at, present
            ) VALUES (
              ?1, ?2, ?3, ?4,
              ?5, ?6, ?7, ?8, ?9,
              ?10, ?11
            )
            ON CONFLICT(item_id) DO UPDATE SET
              owner_device_id=excluded.owner_device_id,
              owner_device_name=excluded.owner_device_name,
              mimes_json=excluded.mimes_json,
              size_bytes=excluded.size_bytes,
              sha256_hex=excluded.sha256_hex,
              expires_at=excluded.expires_at,
              preview_json=excluded.preview_json,
              uri=excluded.uri,
              created_at=excluded.created_at,
              present=excluded.present
            "#,
            params![
                meta.item_id,
                meta.owner_device_id,
                meta.owner_device_name,
                mimes_json,
                meta.size_bytes as i64,
                meta.sha256_hex,
                meta.expires_at,
                preview_json,
                meta.uri,
                meta.created_at,
                if present { 1 } else { 0 },
            ],
        )?;
        Ok(())
    }

    /// 追加一条历史记录。
    pub fn append_history(&self, meta: &ItemMeta, kind: HistoryKind) -> rusqlite::Result<()> {
        self.conn.execute(
            r#"INSERT INTO history(item_id, kind, created_at) VALUES(?1, ?2, ?3)"#,
            params![meta.item_id, kind_as_str(&kind), now_ms()],
        )?;
        Ok(())
    }

    /// 如果在去重窗口内已经存在“同来源 + 同 sha256”的条目，返回**已有的 item_id**。
    pub fn dedup_recent(&self, meta: &ItemMeta) -> rusqlite::Result<Option<String>> {
        // 找最近一条匹配 owner+sha 的记录
        let mut stmt = self.conn.prepare(
            r#"
            SELECT item_id, created_at
              FROM items
             WHERE owner_device_id = ?1
               AND sha256_hex = ?2
             ORDER BY created_at DESC
             LIMIT 1
            "#,
        )?;
        let row_opt = stmt
            .query_row(params![meta.owner_device_id, meta.sha256_hex], |row| {
                let id: String = row.get(0)?;
                let ts: i64 = row.get(1)?;
                Ok((id, ts))
            })
            .optional()?;

        if let Some((id, ts)) = row_opt {
            if (meta.created_at - ts).abs() <= DEDUP_WINDOW_MS {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// 标记条目正文已在本地缓存。
    pub fn mark_present(&self, item_id: &str, _sha256: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            r#"UPDATE items SET present=1 WHERE item_id=?1"#,
            params![item_id],
        )?;
        Ok(())
    }

    /// 获取条目详情（元数据 + present）。
    pub fn get_item(&self, item_id: &str) -> rusqlite::Result<Option<ItemRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT item_id, owner_device_id, owner_device_name, mimes_json,
                   size_bytes, sha256_hex, expires_at, preview_json, uri,
                   created_at, present
              FROM items
             WHERE item_id = ?1
            "#,
        )?;

        let row_opt = stmt
            .query_row(params![item_id], |row| {
                let item_id: String = row.get(0)?;
                let owner_device_id: String = row.get(1)?;
                let owner_device_name: Option<String> = row.get(2)?;
                let mimes_json: String = row.get(3)?;
                let size_bytes: i64 = row.get(4)?;
                let sha256_hex: String = row.get(5)?;
                let expires_at: Option<i64> = row.get(6)?;
                let preview_json: String = row.get(7)?;
                let uri: String = row.get(8)?;
                let created_at: i64 = row.get(9)?;
                let present: i64 = row.get(10)?;

                let mimes: Vec<String> =
                    serde_json::from_str(&mimes_json).unwrap_or_default();
                let preview: Json = serde_json::from_str(&preview_json).unwrap_or(Json::Null);

                let meta = ItemMeta {
                    protocol_version: crate::proto::PROTOCOL_VERSION,
                    item_id,
                    owner_device_id,
                    owner_device_name,
                    mimes,
                    size_bytes: size_bytes as u64,
                    sha256_hex,
                    expires_at,
                    preview_json: preview,
                    uri,
                    created_at,
                };
                Ok(ItemRecord {
                    meta,
                    present: present != 0,
                })
            })
            .optional()?;

        Ok(row_opt)
    }

    /// 列出历史（分页）。为简化 UI，返回一个扁平的视图。
    pub fn list_history(
        &self,
        limit: u32,
        offset: u32,
        kind: Option<&crate::api::HistoryKind>,
    ) -> rusqlite::Result<Vec<HistoryEntry>> {
        let mut out = Vec::new();

        let (sql, params_any): (&str, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(k) = kind {
            (
                r#"
                SELECT h.seq_id, i.item_id, i.preview_json, i.created_at,
                       i.owner_device_id, i.mimes_json
                  FROM history h
                  JOIN items i ON i.item_id = h.item_id
                 WHERE h.kind = ?1
                 ORDER BY h.seq_id DESC
                 LIMIT ?2 OFFSET ?3
                "#,
                vec![
                    Box::new(kind_as_str(k)),
                    Box::new(limit as i64),
                    Box::new(offset as i64),
                ],
            )
        } else {
            (
                r#"
                SELECT h.seq_id, i.item_id, i.preview_json, i.created_at,
                       i.owner_device_id, i.mimes_json
                  FROM history h
                  JOIN items i ON i.item_id = h.item_id
                 ORDER BY h.seq_id DESC
                 LIMIT ?1 OFFSET ?2
                "#,
                vec![Box::new(limit as i64), Box::new(offset as i64)],
            )
        };

        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params_any.iter()))?;
        while let Some(row) = rows.next()? {
            let seq_id: i64 = row.get(0)?;
            let item_id: String = row.get(1)?;
            let preview_json: String = row.get(2)?;
            let created_at: i64 = row.get(3)?;
            let owner_device_id: String = row.get(4)?;
            let mimes_json: String = row.get(5)?;

            let preview: Json = serde_json::from_str(&preview_json).unwrap_or(Json::Null);
            let mimes: Vec<String> =
                serde_json::from_str(&mimes_json).unwrap_or_default();

            out.push(HistoryEntry {
                seq_id,
                item_id,
                summary: summarize_preview(&preview),
                created_at,
                owner_device_id,
                mimes,
            });
        }

        Ok(out)
    }

    /// 写入 CAS：按 sha256 路径写入内容（原子重命名避免部分写入）。
    pub fn write_blob(
        &self,
        cas: &CasPaths,
        sha256_hex: &str,
        content: &[u8],
    ) -> rusqlite::Result<PathBuf> {
        let final_path = cas.path_for_sha256(sha256_hex);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).map_err(sqlerr)?;
        }

        // 已存在则直接返回
        if final_path.exists() {
            return Ok(final_path);
        }

        // 写临时文件再重命名
        let tmp_path = final_path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp_path).map_err(sqlerr)?;
            f.write_all(content).map_err(sqlerr)?;
            f.sync_all().ok(); // best-effort
        }
        fs::rename(&tmp_path, &final_path).map_err(sqlerr)?;
        Ok(final_path)
    }

    /// 近似 LRU 的缓存清理（按 `CacheLimits`）：超限则从最旧的 present 开始删。
    pub fn prune_cache(
        &self,
        cas: &CasPaths,
        limits: &CacheLimits,
    ) -> rusqlite::Result<()> {
        // 估算当前 present 总体积与条数（用元数据 size_bytes）
        let (mut total_bytes, mut total_items) = self.cache_counters()?;

        if total_bytes <= limits.max_bytes && total_items as u32 <= limits.max_items {
            return Ok(());
        }

        // 找出 present 的条目，按 created_at 升序删除（最旧的先删）
        let mut stmt = self.conn.prepare(
            r#"
            SELECT item_id, sha256_hex, created_at, size_bytes
              FROM items
             WHERE present = 1
             ORDER BY created_at ASC
            "#,
        )?;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let item_id: String = row.get(0)?;
            let sha256_hex: String = row.get(1)?;
            let _created_at: i64 = row.get(2)?;
            let size_bytes: i64 = row.get(3)?;

            // 删除文件（忽略错误），然后把 present 置 0
            let p = cas.path_for_sha256(&sha256_hex);
            let _ = fs::remove_file(&p);
            self.conn.execute(
                r#"UPDATE items SET present=0 WHERE item_id=?1"#,
                params![item_id],
            )?;

            if total_bytes > 0 {
                total_bytes = total_bytes.saturating_sub(size_bytes as u64);
            }
            if total_items > 0 {
                total_items -= 1;
            }
            if total_bytes <= limits.max_bytes && (total_items as u32) <= limits.max_items {
                break;
            }
        }

        Ok(())
    }

    /// 清理历史（占位策略：只保留最近 HISTORY_KEEP 条）
    pub fn prune_history(&self) -> rusqlite::Result<()> {
        // 找到要保留的最大 seq_id 下界
        let keep = HISTORY_KEEP;
        let max_seq: Option<i64> = self
            .conn
            .query_row(
                r#"SELECT MAX(seq_id) FROM history"#,
                [],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(max_seq) = max_seq {
            let threshold = max_seq - keep;
            if threshold > 0 {
                self.conn.execute(
                    r#"DELETE FROM history WHERE seq_id <= ?1"#,
                    params![threshold],
                )?;
            }
        }
        Ok(())
    }

    // ---- 内部小工具 ----

    fn cache_counters(&self) -> rusqlite::Result<(u64, u64)> {
        let mut stmt = self.conn.prepare(
            r#"SELECT IFNULL(SUM(size_bytes),0), COUNT(1) FROM items WHERE present=1"#,
        )?;
        let (sum_bytes, cnt): (i64, i64) = stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok((sum_bytes as u64, cnt as u64))
    }
}

// --------------------------- 工具函数与映射 -----------------------------------------

fn now_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    dur.as_millis() as i64
}

fn kind_as_str(k: &HistoryKind) -> &'static str {
    match k {
        HistoryKind::CopyLocal => "copy_local",
        HistoryKind::RecvRemote => "recv_remote",
        HistoryKind::PasteLocal => "paste_local",
    }
}

fn summarize_preview(p: &Json) -> String {
    // 约定：如果 preview 里有 "head" 或 "text" 字段，就截取前 120 字符。
    if let Some(head) = p.get("head").and_then(|v| v.as_str()) {
        return truncate(head, 120);
    }
    if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
        return truncate(text, 120);
    }
    // 兜底：JSON 压缩成一行再截断
    truncate(&p.to_string(), 120)
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut out = s[..n].to_string();
        out.push('…');
        out
    }
}

fn sqlerr(e: io::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}
