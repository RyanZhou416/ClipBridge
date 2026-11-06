use rusqlite::{params, Connection};
use std::fs;
use std::path::{Path, PathBuf};

/// 日志保留天数：打开数据库时删除早于该天数的记录。
const LOG_RETENTION_DAYS: i64 = 7;

/// `logs` 表允许的最大行数：超出部分按时间从最旧的开始删除。
const LOG_MAX_ROWS: i64 = 200_000;

/// 单次清理删除超过该行数时才执行 VACUUM。
///
/// VACUUM 会重写整个数据库文件（可能数百 MB），期间持有独占锁，代价远高于普通
/// DELETE；只有释放的页足够多时才值得做，避免每次打开都拖慢启动。
const LOG_VACUUM_MIN_DELETED: i64 = 10_000;

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// 日志保留策略的执行结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneStats {
	/// 因超过保留天数而删除的行数
	pub deleted_by_age: i64,
	/// 因超过行数上限而删除的行数
	pub deleted_by_count: i64,
	/// 是否执行了 VACUUM
	pub vacuumed: bool,
}

impl PruneStats {
	pub fn total_deleted(&self) -> i64 {
		self.deleted_by_age + self.deleted_by_count
	}
}

/// 多语言消息结构
/// 设计为可扩展：可以轻松添加新语言字段
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MultilingualMessage {
	/// 英文消息（必需，作为回退语言）
	#[serde(rename = "en")]
	en: String,
	/// 简体中文消息（可选）
	#[serde(rename = "zh-CN")]
	zh_cn: Option<String>,
	// 未来可以添加更多语言：
	// #[serde(rename = "ja")]
	// ja: Option<String>,  // 日语
	// #[serde(rename = "fr")]
	// fr: Option<String>,  // 法语
	// 等等...
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
		let Some(lang) = lang else {
			return self.message.clone();
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
	} else if lang.eq_ignore_ascii_case("zh")
		|| lang.eq_ignore_ascii_case("zh-Hans")
		|| lang.starts_with("zh-")
	{
		"zh-CN".to_string()
	} else {
		lang.to_string()
	}
}

/// 当前 `logs.db` 的 schema 版本。
///
/// 日志只是诊断数据，没有兼容负担：版本不匹配时整表删除重建即可。
/// 与 `core.db` 一样，未来任何 DDL 变化都必须递增本常量。
pub const LOG_SCHEMA_VERSION: i64 = 2;

/// 重建日志库时删除的表（历史上一直叫 `logs`，未发现其它拼写）。
const DROP_LOG_TABLES_DDL: &str = "DROP TABLE IF EXISTS logs;";

/// v2 的 `logs` 表 DDL。
const LOG_SCHEMA_DDL: &str = r"
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
";

pub struct LogStore {
	pub(crate) conn: Connection,
	/// 本次 `open` 是否因为 schema 版本变化而整库重建（日志被清空）。
	pub rebuilt: bool,
}

impl LogStore {
	/// 打开日志数据库
	pub fn open(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
		let data_dir = data_dir.as_ref();
		fs::create_dir_all(data_dir)?;
		let db_path: PathBuf = data_dir.join("logs.db");

		let conn = Connection::open(db_path)?;
		Self::init_pragmas(&conn)?;
		let rebuilt = Self::ensure_schema(&conn)?;
		let mut store = Self { conn, rebuilt };
		// 启动时执行一次保留策略：日志维护失败不应让核心无法启动，只记录错误。
		if let Err(e) = store.prune(crate::util::now_ms()) {
			eprintln!("[logs] prune on open failed: {e}");
		}
		Ok(store)
	}

	fn init_pragmas(conn: &Connection) -> anyhow::Result<()> {
		conn.execute_batch(
			r"
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA foreign_keys=ON;
            PRAGMA busy_timeout=5000;
            ",
		)?;
		Ok(())
	}

	/// 按 [`LOG_SCHEMA_VERSION`] 校验并（必要时）重建日志库。
	///
	/// 日志是诊断数据：版本不匹配时直接 `DROP TABLE` 重建，不做迁移。
	///
	/// # 返回值
	/// `Ok(true)` 表示本次执行了 drop-and-rebuild（旧日志已全部丢弃）。
	fn ensure_schema(conn: &Connection) -> anyhow::Result<bool> {
		let old_version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
		if old_version == LOG_SCHEMA_VERSION {
			return Ok(false);
		}

		let tx = conn.unchecked_transaction()?;
		tx.execute_batch(DROP_LOG_TABLES_DDL)?;
		tx.execute_batch(LOG_SCHEMA_DDL)?;
		tx.pragma_update(None, "user_version", LOG_SCHEMA_VERSION)?;
		tx.commit()?;

		eprintln!(
			"[logs] logs.db schema changed ({old_version} -> {LOG_SCHEMA_VERSION}): \
             diagnostic logs dropped and recreated"
		);
		Ok(true)
	}

	/// 写入日志（单语言，向后兼容）
	#[allow(clippy::too_many_arguments)]
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
			r"
            INSERT INTO logs (ts_utc, level, component, category, message, exception, props_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
			params![ts_utc, level, component, category, message, exception, props_json],
		)?;
		Ok(self.conn.last_insert_rowid())
	}

	/// 写入多语言日志
	#[allow(clippy::too_many_arguments)]
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
		// 使用结构体构建多语言消息（类型安全，易于扩展）
		let msg = MultilingualMessage {
			en: message_en.to_string(),
			zh_cn: message_zh_cn.map(ToString::to_string),
		};
		let message_json = serde_json::to_string(&msg)?;

		let _id = self.conn.execute(
			r"
            INSERT INTO logs (ts_utc, level, component, category, message, exception, props_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
			params![
				ts_utc,
				level,
				component,
				category,
				message_json,
				exception,
				props_json
			],
		)?;
		Ok(self.conn.last_insert_rowid())
	}

	/// 记录 Info 级别的多语言日志
	pub fn log_info(
		&mut self,
		category: &str,
		message_en: &str,
		message_zh_cn: Option<&str>,
	) -> anyhow::Result<i64> {
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
	pub fn log_warn(
		&mut self,
		category: &str,
		message_en: &str,
		message_zh_cn: Option<&str>,
	) -> anyhow::Result<i64> {
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
	pub fn log_error(
		&mut self,
		category: &str,
		message_en: &str,
		message_zh_cn: Option<&str>,
		error: Option<&str>,
	) -> anyhow::Result<i64> {
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
	pub fn log_debug(
		&mut self,
		category: &str,
		message_en: &str,
		message_zh_cn: Option<&str>,
	) -> anyhow::Result<i64> {
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
			let like_pattern = format!("%{like_str}%");
			let mut stmt = self.conn.prepare(
                r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id > ?1 AND level >= ?2 AND (message LIKE ?3 OR category LIKE ?3 OR exception LIKE ?3)
                ORDER BY ts_utc ASC, id ASC
                LIMIT ?4
                ",
            )?;
			let rows = stmt.query_map(params![after_id, level_min, like_pattern, limit], |r| {
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
			})?;
			let mut out = Vec::new();
			for row in rows {
				out.push(row?);
			}
			Ok(out)
		} else {
			let mut stmt = self.conn.prepare(
				r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id > ?1 AND level >= ?2
                ORDER BY ts_utc ASC, id ASC
                LIMIT ?3
                ",
			)?;
			let rows = stmt.query_map(params![after_id, level_min, limit], |r| {
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
			})?;
			let mut out = Vec::new();
			for row in rows {
				out.push(row?);
			}
			Ok(out)
		}
	}

	/// 查询时间范围内的日志
	#[allow(clippy::too_many_arguments)]
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
			let like_pattern = format!("%{like_str}%");
			let mut stmt = self.conn.prepare(
				r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE ts_utc >= ?1 AND ts_utc <= ?2 AND level >= ?3
                    AND (message LIKE ?4 OR category LIKE ?4 OR exception LIKE ?4)
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?5 OFFSET ?6
                ",
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
				r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE ts_utc >= ?1 AND ts_utc <= ?2 AND level >= ?3
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?4 OFFSET ?5
                ",
			)?;
			let rows =
				stmt.query_map(params![start_ms, end_ms, level_min, limit, offset], |r| {
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
				})?;
			let mut out = Vec::new();
			for row in rows {
				out.push(row?);
			}
			Ok(out)
		}
	}

	/// 获取日志统计
	pub fn stats(&self) -> anyhow::Result<LogStats> {
		let count: i64 = self
			.conn
			.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?;

		// 空表时 MIN/MAX 返回 NULL：按可空 i64 读取，不能假设一定有一行非空值。
		let first_ms: Option<i64> =
			self.conn
				.query_row("SELECT MIN(ts_utc) FROM logs", [], |r| r.get(0))?;

		let last_ms: Option<i64> =
			self.conn
				.query_row("SELECT MAX(ts_utc) FROM logs", [], |r| r.get(0))?;

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

	/// 按保留策略清理日志：先删早于 [`LOG_RETENTION_DAYS`] 天的记录，再把总行数
	/// 压到 [`LOG_MAX_ROWS`] 以内（从最旧的开始）；删除量达到
	/// [`LOG_VACUUM_MIN_DELETED`] 时执行一次 VACUUM 归还磁盘页。
	pub fn prune(&mut self, now_ms: i64) -> anyhow::Result<PruneStats> {
		self.prune_with_limits(
			now_ms,
			LOG_RETENTION_DAYS,
			LOG_MAX_ROWS,
			LOG_VACUUM_MIN_DELETED,
		)
	}

	/// 保留策略的实际实现，参数化边界便于单元测试。
	fn prune_with_limits(
		&mut self,
		now_ms: i64,
		retention_days: i64,
		max_rows: i64,
		vacuum_min_deleted: i64,
	) -> anyhow::Result<PruneStats> {
		let cutoff_ms = now_ms.saturating_sub(retention_days.saturating_mul(MS_PER_DAY));
		let deleted_by_age =
			self.conn
				.execute("DELETE FROM logs WHERE ts_utc < ?1", params![cutoff_ms])? as i64;

		let total: i64 = self
			.conn
			.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?;
		let deleted_by_count = if total > max_rows {
			let excess = total - max_rows;
			self.conn.execute(
				r"DELETE FROM logs WHERE id IN (
                    SELECT id FROM logs ORDER BY ts_utc ASC, id ASC LIMIT ?1
                )",
				params![excess],
			)? as i64
		} else {
			0
		};

		let mut stats = PruneStats {
			deleted_by_age,
			deleted_by_count,
			vacuumed: false,
		};
		// VACUUM 是重操作（重写整库 + 独占锁）：只有删除量足够大才值得做。
		// 这里都是自动提交的独立语句，不会在事务中执行 VACUUM。
		if vacuum_min_deleted > 0 && stats.total_deleted() >= vacuum_min_deleted {
			self.conn.execute_batch("VACUUM")?;
			stats.vacuumed = true;
		}
		Ok(stats)
	}

	/// 删除指定时间之前的日志
	pub fn delete_before(&mut self, cutoff_ms: i64) -> anyhow::Result<i64> {
		let deleted = self
			.conn
			.execute("DELETE FROM logs WHERE ts_utc < ?1", params![cutoff_ms])?;
		Ok(deleted as i64)
	}

	/// 按 ID 列表删除日志
	pub fn delete_by_ids(&mut self, ids: &[i64]) -> anyhow::Result<i64> {
		if ids.is_empty() {
			return Ok(0);
		}
		// 构建 IN 子句的占位符
		let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
		let query = format!("DELETE FROM logs WHERE id IN ({})", placeholders.join(","));

		// 使用 rusqlite::params! 宏需要编译时知道参数数量，所以使用 execute_batch
		// 但我们需要返回值，所以使用 prepare + execute
		let mut stmt = self.conn.prepare(&query)?;

		// 构建参数数组
		let param_values: Vec<rusqlite::types::Value> = ids
			.iter()
			.map(|&id| rusqlite::types::Value::Integer(id))
			.collect();
		let params: Vec<&dyn rusqlite::ToSql> = param_values
			.iter()
			.map(|v| v as &dyn rusqlite::ToSql)
			.collect();

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
			let like_pattern = format!("%{like_str}%");
			let mut stmt = self.conn.prepare(
                r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id < ?1 AND level >= ?2 AND (message LIKE ?3 OR category LIKE ?3 OR exception LIKE ?3)
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?4
                ",
            )?;
			let rows = stmt.query_map(params![before_id, level_min, like_pattern, limit], |r| {
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
			})?;
			let mut out = Vec::new();
			for row in rows {
				out.push(row?);
			}
			// 反转结果，使其按时间升序排列（从旧到新）
			out.reverse();
			Ok(out)
		} else {
			let mut stmt = self.conn.prepare(
				r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE id < ?1 AND level >= ?2
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?3
                ",
			)?;
			let rows = stmt.query_map(params![before_id, level_min, limit], |r| {
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
			})?;
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
		let mut stmt = self
			.conn
			.prepare("SELECT component, COUNT(*) as cnt FROM logs GROUP BY component")?;
		let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
		for row in rows {
			let (component, count) = row?;
			component_map.insert(component, count);
		}

		// 获取所有 category 及其计数
		let mut category_map = std::collections::HashMap::new();
		let mut stmt = self
			.conn
			.prepare("SELECT category, COUNT(*) as cnt FROM logs GROUP BY category")?;
		let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
		for row in rows {
			let (category, count) = row?;
			category_map.insert(category, count);
		}

		Ok(SourceStats {
			components: component_map,
			categories: category_map,
		})
	}

	/// 查询最新的日志（从新到旧排序，用于初始加载）
	pub fn query_latest(
		&self,
		level_min: i32,
		like: Option<&str>,
		limit: i32,
		lang: Option<&str>,
	) -> anyhow::Result<Vec<LogEntry>> {
		if let Some(like_str) = like {
			let like_pattern = format!("%{like_str}%");
			let mut stmt = self.conn.prepare(
				r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE level >= ?1 AND (message LIKE ?2 OR category LIKE ?2 OR exception LIKE ?2)
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?3
                ",
			)?;
			let rows = stmt.query_map(params![level_min, like_pattern, limit], |r| {
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
				entry.message = entry.get_message_for_lang(lang);
				Ok(entry)
			})?;
			let mut out = Vec::new();
			for row in rows {
				out.push(row?);
			}
			Ok(out)
		} else {
			let mut stmt = self.conn.prepare(
				r"
                SELECT id, ts_utc, level, component, category, message, exception, props_json
                FROM logs
                WHERE level >= ?1
                ORDER BY ts_utc DESC, id DESC
                LIMIT ?2
                ",
			)?;
			let rows = stmt.query_map(params![level_min, limit], |r| {
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
				entry.message = entry.get_message_for_lang(lang);
				Ok(entry)
			})?;
			let mut out = Vec::new();
			for row in rows {
				out.push(row?);
			}
			Ok(out)
		}
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

#[cfg(test)]
mod tests {
	use super::*;

	fn write_at(store: &mut LogStore, ts_utc: i64) {
		store
			.write(ts_utc, 2, "Test", "Retention", "message", None, None)
			.unwrap();
	}

	#[test]
	fn prune_enforces_age_and_row_limits() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = LogStore::open(dir.path()).unwrap();
		let now = 1_700_000_000_000i64;
		let expired = now - (LOG_RETENTION_DAYS + 1) * MS_PER_DAY;
		let fresh = now - 1_000;

		for _ in 0..5 {
			write_at(&mut store, expired);
		}
		for _ in 0..20 {
			write_at(&mut store, fresh);
		}

		// 保留 7 天 / 只留 10 条；15 条删除量低于 VACUUM 阈值，不应触发 VACUUM。
		let stats = store
			.prune_with_limits(now, LOG_RETENTION_DAYS, 10, 10_000)
			.unwrap();
		assert_eq!(stats.deleted_by_age, 5);
		assert_eq!(stats.deleted_by_count, 10);
		assert_eq!(stats.total_deleted(), 15);
		assert!(!stats.vacuumed);

		let remaining = store.stats().unwrap();
		assert_eq!(remaining.count, 10);
		assert!(remaining.first_ms.unwrap() >= fresh - 1);
		assert!(remaining.last_ms.unwrap() >= fresh - 1);
	}

	#[test]
	fn prune_vacuums_only_above_threshold() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = LogStore::open(dir.path()).unwrap();
		let now = crate::util::now_ms();
		for _ in 0..6 {
			write_at(&mut store, now - MS_PER_DAY);
		}

		// 行数上限设为 0（删掉全部 6 条），阈值 5：删除量达到阈值应触发 VACUUM。
		let stats = store
			.prune_with_limits(now, LOG_RETENTION_DAYS, 0, 5)
			.unwrap();
		assert_eq!(stats.deleted_by_age, 0);
		assert_eq!(stats.total_deleted(), 6);
		assert!(stats.vacuumed);
		assert_eq!(store.stats().unwrap().count, 0);
	}

	#[test]
	fn schema_is_stamped_and_only_rebuilt_on_version_change() {
		let dir = tempfile::tempdir().unwrap();

		let store = LogStore::open(dir.path()).unwrap();
		assert!(store.rebuilt, "新文件 user_version=0，必须重建一次");
		let version: i64 = store
			.conn
			.pragma_query_value(None, "user_version", |r| r.get(0))
			.unwrap();
		assert_eq!(version, LOG_SCHEMA_VERSION);
		drop(store);

		let reopened = LogStore::open(dir.path()).unwrap();
		assert!(!reopened.rebuilt, "版本一致时不应清空日志");
		drop(reopened);

		// 人为写旧版本号，模拟下一次 schema 变更：必须整表重建并重新打戳。
		{
			let conn = Connection::open(dir.path().join("logs.db")).unwrap();
			conn.pragma_update(None, "user_version", LOG_SCHEMA_VERSION - 1)
				.unwrap();
		}
		let rebuilt = LogStore::open(dir.path()).unwrap();
		assert!(rebuilt.rebuilt);
		let version: i64 = rebuilt
			.conn
			.pragma_query_value(None, "user_version", |r| r.get(0))
			.unwrap();
		assert_eq!(version, LOG_SCHEMA_VERSION);
	}

	#[test]
	fn open_prunes_expired_rows() {
		let dir = tempfile::tempdir().unwrap();
		let now = crate::util::now_ms();
		{
			let mut store = LogStore::open(dir.path()).unwrap();
			write_at(&mut store, now - (LOG_RETENTION_DAYS + 1) * MS_PER_DAY);
			write_at(&mut store, now);
		}

		// 重新打开时应自动清理过期记录，仅保留新鲜记录。
		let reopened = LogStore::open(dir.path()).unwrap();
		assert_eq!(reopened.stats().unwrap().count, 1);
	}
}
