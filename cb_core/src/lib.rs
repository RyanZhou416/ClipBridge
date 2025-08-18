//cb_core/src/lib.rs

pub mod models;
pub mod ingest;
pub mod api;

// 放在其它 mod 前也可
pub use models::*;      // 便于 core-ffi 直接用
pub use ingest::*;
pub use api::*;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub struct Device {
    pub device_id: String,
    pub account_id: Option<String>,
    pub name: String,
    pub pubkey_fpr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClipMeta {
    pub item_id: String,
    pub source_device_id: String,
    pub owner_account_id: Option<String>,
    pub kinds_json: String,         // JSON 数组
    pub mimes_json: String,         // JSON 数组
    pub preferred_mime: String,
    pub size_bytes: i64,
    pub sha256: Option<String>,
    pub created_at: i64,            // epoch seconds
    pub expires_at: Option<i64>,
    pub preview_text: Option<String>,
    pub files_json: Option<String>, // 多文件清单（JSON）
    pub seen_ts: i64,               // 本机看到/产生时间
}

struct CoreState {
    db_path: PathBuf,
    conn: Connection,
}

static CORE: OnceLock<Mutex<CoreState>> = OnceLock::new();

fn app_data_dir() -> Result<PathBuf> {
    // org = ClipBridge, app = ClipBridge
    let pd = ProjectDirs::from("dev", "ClipBridge", "ClipBridge")
        .ok_or_else(|| anyhow::anyhow!("Cannot resolve app data dir"))?;
    Ok(pd.data_dir().to_path_buf())
}

fn ensure_parent_dir(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {:?}", parent))?;
    }
    Ok(())
}

fn open_db(db_path: &Path) -> Result<Connection> {
    ensure_parent_dir(db_path)?;
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("open {:?}", db_path))?;
    // 并发/可靠性建议
    conn.pragma_update(None, "journal_mode", &"WAL")?;
    conn.pragma_update(None, "synchronous", &"NORMAL")?;
    conn.pragma_update(None, "foreign_keys", &"ON")?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v == 0 {
        conn.execute_batch(
            r#"
            BEGIN;
            CREATE TABLE IF NOT EXISTS device (
              device_id      TEXT PRIMARY KEY,
              account_id     TEXT,
              name           TEXT,
              pubkey_fpr     TEXT,
              last_seen_ts   INTEGER
            );

            CREATE TABLE IF NOT EXISTS clip_meta (
              item_id            TEXT PRIMARY KEY,
              source_device_id   TEXT NOT NULL,
              owner_account_id   TEXT,
              kinds              TEXT NOT NULL,
              mimes              TEXT NOT NULL,
              preferred_mime     TEXT NOT NULL,
              size_bytes         INTEGER NOT NULL,
              sha256             TEXT,
              preview_text       TEXT,
              files_json         TEXT,
              created_at         INTEGER NOT NULL,
              expires_at         INTEGER,
              ttl_seconds        INTEGER,
              policy_version     INTEGER,
              seen_ts            INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE INDEX IF NOT EXISTS idx_clip_meta_seen ON clip_meta(seen_ts DESC);
            CREATE INDEX IF NOT EXISTS idx_clip_meta_src  ON clip_meta(source_device_id, seen_ts DESC);
            CREATE INDEX IF NOT EXISTS idx_clip_meta_sha  ON clip_meta(sha256);

            PRAGMA user_version=1;
            COMMIT;
            "#,
        )?;
    }
    Ok(())
}

/// 初始化核心（创建/打开数据库）
/// `storage_dir` 为空则用平台默认目录。
pub fn init(storage_dir: Option<&Path>) -> Result<()> {
    let base = match storage_dir {
        Some(p) => p.to_path_buf(),
        None => app_data_dir()?,
    };
    let db_path = base.join("db").join("clipbridge.sqlite");
    let conn = open_db(&db_path)?;
    CORE.get_or_init(|| Mutex::new(CoreState { db_path, conn }));
    Ok(())
}

fn with_conn<R>(f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
    let st = CORE.get().ok_or_else(|| anyhow::anyhow!("core not initialized"))?;
    let guard = st.lock().unwrap();
    f(&guard.conn)
}

/// upsert 设备（可选）
pub fn upsert_device(dev: &Device) -> Result<()> {
    with_conn(|c| {
        c.execute(
            r#"
            INSERT INTO device(device_id, account_id, name, pubkey_fpr, last_seen_ts)
            VALUES(?1, ?2, ?3, ?4, strftime('%s','now'))
            ON CONFLICT(device_id) DO UPDATE SET
              account_id=excluded.account_id,
              name=excluded.name,
              pubkey_fpr=excluded.pubkey_fpr,
              last_seen_ts=strftime('%s','now')
            "#,
            params![
                dev.device_id,
                dev.account_id,
                dev.name,
                dev.pubkey_fpr
            ],
        )?;
        Ok(())
    })
}

/// 存一条元数据（本机或远端）
pub fn store_meta(m: &ClipMeta) -> Result<()> {
    with_conn(|c| {
        c.execute(
            r#"
            INSERT INTO clip_meta(
              item_id, source_device_id, owner_account_id,
              kinds, mimes, preferred_mime, size_bytes, sha256,
              preview_text, files_json, created_at, expires_at, seen_ts
            )
            VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, strftime('%s','now'))
            ON CONFLICT(item_id) DO UPDATE SET
              source_device_id=excluded.source_device_id,
              owner_account_id=excluded.owner_account_id,
              kinds=excluded.kinds,
              mimes=excluded.mimes,
              preferred_mime=excluded.preferred_mime,
              size_bytes=excluded.size_bytes,
              sha256=excluded.sha256,
              preview_text=excluded.preview_text,
              files_json=excluded.files_json,
              created_at=excluded.created_at,
              expires_at=excluded.expires_at,
              seen_ts=strftime('%s','now')
            "#,
            params![
                m.item_id,
                m.source_device_id,
                m.owner_account_id,
                m.kinds_json,
                m.mimes_json,
                m.preferred_mime,
                m.size_bytes,
                m.sha256,
                m.preview_text,
                m.files_json,
                m.created_at,
                m.expires_at
            ],
        )?;
        Ok(())
    })
}

/// 拉取历史（按时间倒序）
pub fn history_since(since_ts: i64, limit: u32) -> Result<Vec<ClipMeta>> {
    with_conn(|c| {
        let mut stmt = c.prepare(
            r#"
            SELECT item_id, source_device_id, owner_account_id,
                   kinds, mimes, preferred_mime, size_bytes, sha256,
                   preview_text, files_json, created_at, expires_at, seen_ts
            FROM clip_meta
            WHERE seen_ts >= ?1
            ORDER BY seen_ts DESC
            LIMIT ?2
            "#,
        )?;
        let rows = stmt
            .query_map(params![since_ts, limit as i64], |r| {
                Ok(ClipMeta {
                    item_id: r.get(0)?,
                    source_device_id: r.get(1)?,
                    owner_account_id: r.get::<_, Option<String>>(2)?,
                    kinds_json: r.get(3)?,
                    mimes_json: r.get(4)?,
                    preferred_mime: r.get(5)?,
                    size_bytes: r.get::<_, i64>(6)?,
                    sha256: r.get::<_, Option<String>>(7)?,
                    preview_text: r.get::<_, Option<String>>(8)?,
                    files_json: r.get::<_, Option<String>>(9)?,
                    created_at: r.get(10)?,
                    expires_at: r.get::<_, Option<i64>>(11)?,
                    seen_ts: r.get(12)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}
