use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};

/// 多语言消息结构
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MultilingualMessage {
    #[serde(rename = "en")]
    en: String,
    #[serde(rename = "zh-CN")]
    zh_cn: Option<String>,
}

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

impl LogEntry {
    /// 从多语言 JSON 中提取指定语言的消息
    /// 回退逻辑：目标语言 → "en" → 原始 message 字段
    pub fn get_message_for_lang(&self, lang: Option<&str>) -> String {
        // 如果没有指定语言，直接返回原始 message（向后兼容）
        let lang = match lang {
            Some(l) => l,
            None => return self.message.clone(),
        };

        // 尝试解析 JSON
        let msg_json: Result<serde_json::Value, _> = serde_json::from_str(&self.message);
        if let Ok(json) = msg_json {
            // 规范化语言代码
            let normalized_lang = normalize_lang_code(lang);

            // 尝试获取目标语言
            if let Some(msg) = json.get(&normalized_lang).and_then(|v| v.as_str()) {
                return msg.to_string();
            }

            // 回退到 "en"
            if normalized_lang != "en" {
                if let Some(msg) = json.get("en").and_then(|v| v.as_str()) {
                    return msg.to_string();
                }
            }

            // 如果 JSON 格式正确但找不到对应语言，尝试使用第一个可用的值
            if let Some((_, v)) = json.as_object().and_then(|obj| obj.iter().next()) {
                if let Some(msg) = v.as_str() {
                    return msg.to_string();
                }
            }
        }

        // 如果解析失败或找不到对应语言，返回原始 message（向后兼容）
        self.message.clone()
    }
}

/// 规范化语言代码
/// "en-US" -> "en", "zh-CN" -> "zh-CN", "zh" -> "zh-CN"
fn normalize_lang_code(lang: &str) -> String {
    let lang = lang.trim();
    if lang.eq_ignore_ascii_case("en") || lang.starts_with("en-") {
        "en".to_string()
    } else if lang.eq_ignore_ascii_case("zh") || lang.eq_ignore_ascii_case("zh-Hans") || lang.starts_with("zh-") {
        "zh-CN".to_string()
    } else {
        lang.to_string()
    }
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

    /// 写入日志（单语言，向后兼容）
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
        let _id = self.conn.execute(
            r#"
            INSERT INTO logs (ts_utc, level, component, category, message, exception, props_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![ts_utc, level, component, category, message, exception, props_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 写入多语言日志
    pub fn write_multilang(
        &mut self,
        ts_utc: i64,
        level: i32,
        component: &str,
        category: &str,
        message_en: &str,
        message_zh_cn: Option<&str>,
        exception: Option<&str>,
        props_json: Option<&str>,
    ) -> anyhow::Result<i64> {
        // 构建多语言 JSON
        let mut msg_obj = serde_json::json!({
            "en": message_en
        });
        if let Some(zh_cn) = message_zh_cn {
            msg_obj["zh-CN"] = serde_json::Value::String(zh_cn.to_string());
        }
        let message_json = serde_json::to_string(&msg_obj)?;

        let _id = self.conn.execute(
            r#"
            INSERT INTO logs (ts_utc, level, component, category, message, exception, props_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![ts_utc, level, component, category, message_json, exception, props_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 记录 Info 级别的多语言日志
    pub fn log_info(&mut self, category: &str, message_en: &str, message_zh_cn: Option<&str>) -> anyhow::Result<i64> {
        self.write_multilang(
            crate::util::now_ms(),
            2, // Info level
            "Core",
            category,
            message_en,
            message_zh_cn,
            None,
            None,
        )
    }

    /// 记录 Warn 级别的多语言日志
    pub fn log_warn(&mut self, category: &str, message_en: &str, message_zh_cn: Option<&str>) -> anyhow::Result<i64> {
        self.write_multilang(
            crate::util::now_ms(),
            3, // Warn level
            "Core",
            category,
            message_en,
            message_zh_cn,
            None,
            None,
        )
    }

    /// 记录 Error 级别的多语言日志
    pub fn log_error(&mut self, category: &str, message_en: &str, message_zh_cn: Option<&str>, error: Option<&str>) -> anyhow::Result<i64> {
        self.write_multilang(
            crate::util::now_ms(),
            4, // Error level
            "Core",
            category,
            message_en,
            message_zh_cn,
            error,
            None,
        )
    }

    /// 记录 Debug 级别的多语言日志
    pub fn log_debug(&mut self, category: &str, message_en: &str, message_zh_cn: Option<&str>) -> anyhow::Result<i64> {
        self.write_multilang(
            crate::util::now_ms(),
            1, // Debug level
            "Core",
            category,
            message_en,
            message_zh_cn,
            None,
            None,
        )
    }

    /// 查询 after_id 之后的日志（用于 tail）
    pub fn query_after_id(
        &self,
        after_id: i64,
        level_min: i32,
        like: Option<&str>,
        limit: i32,
        lang: Option<&str>,
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
                    let mut entry = LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    };
                    // 根据语言提取消息
                    entry.message = entry.get_message_for_lang(lang);
                    Ok(entry)
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
                    let mut entry = LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    };
                    // 根据语言提取消息
                    entry.message = entry.get_message_for_lang(lang);
                    Ok(entry)
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
        lang: Option<&str>,
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
                    let mut entry = LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    };
                    // 根据语言提取消息
                    entry.message = entry.get_message_for_lang(lang);
                    Ok(entry)
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
                    let mut entry = LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    };
                    // 根据语言提取消息
                    entry.message = entry.get_message_for_lang(lang);
                    Ok(entry)
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

    /// 按 ID 列表删除日志
    pub fn delete_by_ids(&mut self, ids: &[i64]) -> anyhow::Result<i64> {
        if ids.is_empty() {
            return Ok(0);
        }
        // 构建 IN 子句的占位符
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
        let query = format!("DELETE FROM logs WHERE id IN ({})", placeholders.join(","));

        // 使用 rusqlite::params! 宏需要编译时知道参数数量，所以使用 execute_batch
        // 但我们需要返回值，所以使用 prepare + execute
        let mut stmt = self.conn.prepare(&query)?;

        // 构建参数数组
        let param_values: Vec<rusqlite::types::Value> = ids.iter().map(|&id| rusqlite::types::Value::Integer(id)).collect();
        let params: Vec<&dyn rusqlite::ToSql> = param_values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

        let deleted = stmt.execute(&params[..])?;
        Ok(deleted as i64)
    }

    /// 查询 before_id 之前的日志（用于向上滚动加载更早的日志）
    pub fn query_before_id(
        &self,
        before_id: i64,
        level_min: i32,
        like: Option<&str>,
        limit: i32,
        lang: Option<&str>,
    ) -> anyhow::Result<Vec<LogEntry>> {
        if let Some(like_str) = like {
            let like_pattern = format!("%{}%", like_str);
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id < ?1 AND level >= ?2 AND (message LIKE ?3 OR category LIKE ?3 OR exception LIKE ?3)
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?4
                "#,
            )?;
            let rows = stmt.query_map(
                params![before_id, level_min, like_pattern, limit],
                |r| {
                    let mut entry = LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    };
                    // 根据语言提取消息
                    entry.message = entry.get_message_for_lang(lang);
                    Ok(entry)
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            // 反转结果，使其按时间升序排列（从旧到新）
            out.reverse();
            Ok(out)
        } else {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id < ?1 AND level >= ?2
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?3
                "#,
            )?;
            let rows = stmt.query_map(
                params![before_id, level_min, limit],
                |r| {
                    let mut entry = LogEntry {
                        id: r.get(0)?,
                        ts_utc: r.get(1)?,
                        level: r.get(2)?,
                        component: r.get(3)?,
                        category: r.get(4)?,
                        message: r.get(5)?,
                        exception: r.get(6)?,
                        props_json: r.get(7)?,
                    };
                    // 根据语言提取消息
                    entry.message = entry.get_message_for_lang(lang);
                    Ok(entry)
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            // 反转结果，使其按时间升序排列（从旧到新）
            out.reverse();
            Ok(out)
        }
    }

    /// 获取来源统计（component 和 category 的列表及计数）
    pub fn get_source_stats(&self) -> anyhow::Result<SourceStats> {
        // 获取所有 component 及其计数
        let mut component_map = std::collections::HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT component, COUNT(*) as cnt FROM logs GROUP BY component",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (component, count) = row?;
            component_map.insert(component, count);
        }

        // 获取所有 category 及其计数
        let mut category_map = std::collections::HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT category, COUNT(*) as cnt FROM logs GROUP BY category",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (category, count) = row?;
            category_map.insert(category, count);
        }

        Ok(SourceStats {
            components: component_map,
            categories: category_map,
        })
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceStats {
    pub components: std::collections::HashMap<String, i64>,
    pub categories: std::collections::HashMap<String, i64>,
}
