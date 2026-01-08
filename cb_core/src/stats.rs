use rusqlite::{params, Connection};
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// 统计数据条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsEntry {
    pub ts_ms: i64,
    pub bucket_sec: i32,
    pub stat_type: String, // 'cache', 'network', 'activity'
    pub data_json: String, // JSON格式的统计数据
}

pub struct StatsStore {
    pub(crate) conn: Connection,
}

impl StatsStore {
    /// 打开统计数据库
    pub fn open(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir)?;
        let db_path: PathBuf = data_dir.join("stats.db");

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
            CREATE TABLE IF NOT EXISTS stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_ms INTEGER NOT NULL,
                bucket_sec INTEGER NOT NULL,
                stat_type TEXT NOT NULL,
                data_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stats_ts ON stats(ts_ms);
            CREATE INDEX IF NOT EXISTS idx_stats_type ON stats(stat_type);
            "#,
        )?;
        Ok(())
    }

    /// 写入统计数据
    pub fn write(
        &mut self,
        ts_ms: i64,
        bucket_sec: i32,
        stat_type: &str,
        data_json: &str,
    ) -> anyhow::Result<i64> {
        let _id = self.conn.execute(
            r#"
            INSERT INTO stats (ts_ms, bucket_sec, stat_type, data_json)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![ts_ms, bucket_sec, stat_type, data_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 查询缓存统计
    pub fn query_cache_stats(
        &self,
        start_ts_ms: i64,
        end_ts_ms: i64,
        bucket_sec: i32,
    ) -> anyhow::Result<Vec<StatsEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT ts_ms, bucket_sec, stat_type, data_json
            FROM stats
            WHERE stat_type = 'cache' AND ts_ms >= ?1 AND ts_ms <= ?2 AND bucket_sec = ?3
            ORDER BY ts_ms ASC
            "#,
        )?;
        let rows = stmt.query_map(
            params![start_ts_ms, end_ts_ms, bucket_sec],
            |r| {
                Ok(StatsEntry {
                    ts_ms: r.get(0)?,
                    bucket_sec: r.get(1)?,
                    stat_type: r.get(2)?,
                    data_json: r.get(3)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 查询网络统计
    pub fn query_net_stats(
        &self,
        start_ts_ms: i64,
        end_ts_ms: i64,
        bucket_sec: i32,
    ) -> anyhow::Result<Vec<StatsEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT ts_ms, bucket_sec, stat_type, data_json
            FROM stats
            WHERE stat_type = 'network' AND ts_ms >= ?1 AND ts_ms <= ?2 AND bucket_sec = ?3
            ORDER BY ts_ms ASC
            "#,
        )?;
        let rows = stmt.query_map(
            params![start_ts_ms, end_ts_ms, bucket_sec],
            |r| {
                Ok(StatsEntry {
                    ts_ms: r.get(0)?,
                    bucket_sec: r.get(1)?,
                    stat_type: r.get(2)?,
                    data_json: r.get(3)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 查询活动统计
    pub fn query_activity_stats(
        &self,
        start_ts_ms: i64,
        end_ts_ms: i64,
        bucket_sec: i32,
    ) -> anyhow::Result<Vec<StatsEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT ts_ms, bucket_sec, stat_type, data_json
            FROM stats
            WHERE stat_type = 'activity' AND ts_ms >= ?1 AND ts_ms <= ?2 AND bucket_sec = ?3
            ORDER BY ts_ms ASC
            "#,
        )?;
        let rows = stmt.query_map(
            params![start_ts_ms, end_ts_ms, bucket_sec],
            |r| {
                Ok(StatsEntry {
                    ts_ms: r.get(0)?,
                    bucket_sec: r.get(1)?,
                    stat_type: r.get(2)?,
                    data_json: r.get(3)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 清空统计数据库
    pub fn clear_stats_db(&mut self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM stats", [])?;
        Ok(())
    }
}
