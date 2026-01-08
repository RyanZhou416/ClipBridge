use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};

/// 日志条目
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    pub id: i64,
    pub ts_utc: i64,
    pub level: i32,
    pub component: String,
    pub category: String,
    pub message: String,
    pub exception: Option<String>,
    pub props_json: Option<String>,
}

pub struct LogStore {
    pub(crate) conn: Connection,
}

impl LogStore {
    /// 打开日志数据库
    pub fn open(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir)?;
        let db_path: PathBuf = data_dir.join("logs.db");

        let conn = Connection::open(db_path)?;
        Self::init_pragmas(&conn)?;
        Self::init_schema(&conn)?;
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

    fn init_schema(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_utc INTEGER NOT NULL,
                level INTEGER NOT NULL,
                component TEXT NOT NULL,
                category TEXT NOT NULL,
                message TEXT NOT NULL,
                exception TEXT,
                props_json TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_logs_ts ON logs(ts_utc);
            CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(level);
            CREATE INDEX IF NOT EXISTS idx_logs_category ON logs(category);
            "#,
        )?;
        Ok(())
    }

    /// 写入日志
    pub fn write(
        &mut self,
        ts_utc: i64,
        level: i32,
        component: &str,
        category: &str,
        message: &str,
        exception: Option<&str>,
        props_json: Option<&str>,
    ) -> anyhow::Result<i64> {
        let id = self.conn.execute(
            r#"
            INSERT INTO logs (ts_utc, level, component, category, message, exception, props_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![ts_utc, level, component, category, message, exception, props_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 查询 after_id 之后的日志（用于 tail）
    pub fn query_after_id(
        &self,
        after_id: i64,
        level_min: i32,
        like: Option<&str>,
        limit: i32,
    ) -> anyhow::Result<Vec<LogEntry>> {
        if let Some(like_str) = like {
            let like_pattern = format!("%{}%", like_str);
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id > ?1 AND level >= ?2 AND (message LIKE ?3 OR category LIKE ?3 OR exception LIKE ?3)
                ORDER BY ts_utc ASC, id ASC
                LIMIT ?4
                "#,
            )?;
            let rows = stmt.query_map(
                params![after_id, level_min, like_pattern, limit],
                |r| {
                    Ok(LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        } else {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id > ?1 AND level >= ?2
                ORDER BY ts_utc ASC, id ASC
                LIMIT ?3
                "#,
            )?;
            let rows = stmt.query_map(
                params![after_id, level_min, limit],
                |r| {
                    Ok(LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        }
    }

    /// 查询时间范围内的日志
    pub fn query_range(
        &self,
        start_ms: i64,
        end_ms: i64,
        level_min: i32,
        like: Option<&str>,
        limit: i32,
        offset: i32,
    ) -> anyhow::Result<Vec<LogEntry>> {
        if let Some(like_str) = like {
            let like_pattern = format!("%{}%", like_str);
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE ts_utc >= ?1 AND ts_utc <= ?2 AND level >= ?3 
                    AND (message LIKE ?4 OR category LIKE ?4 OR exception LIKE ?4)
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?5 OFFSET ?6
                "#,
            )?;
            let rows = stmt.query_map(
                params![start_ms, end_ms, level_min, like_pattern, limit, offset],
                |r| {
                    Ok(LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        } else {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE ts_utc >= ?1 AND ts_utc <= ?2 AND level >= ?3
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?4 OFFSET ?5
                "#,
            )?;
            let rows = stmt.query_map(
                params![start_ms, end_ms, level_min, limit, offset],
                |r| {
                    Ok(LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    })
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        }
    }

    /// 获取日志统计
    pub fn stats(&self) -> anyhow::Result<LogStats> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?;

        let first_ms: Option<i64> = self
            .conn
            .query_row("SELECT MIN(ts_utc) FROM logs", [], |r| r.get(0))
            .optional()?;

        let last_ms: Option<i64> = self
            .conn
            .query_row("SELECT MAX(ts_utc) FROM logs", [], |r| r.get(0))
            .optional()?;

        let mut by_level = [0i64; 7];
        for level in 0..7 {
            let count: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM logs WHERE level = ?1",
                    params![level],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            by_level[level as usize] = count;
        }

        Ok(LogStats {
            count,
            first_ms,
            last_ms,
            by_level,
        })
    }

    /// 删除指定时间之前的日志
    pub fn delete_before(&mut self, cutoff_ms: i64) -> anyhow::Result<i64> {
        let deleted = self.conn.execute(
            "DELETE FROM logs WHERE ts_utc < ?1",
            params![cutoff_ms],
        )?;
        Ok(deleted as i64)
    }

    /// 清空日志数据库
    pub fn clear_logs_db(&mut self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM logs", [])?;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogStats {
    pub count: i64,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub by_level: [i64; 7],
}
