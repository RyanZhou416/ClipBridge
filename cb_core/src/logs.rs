use rusqlite::{params, Connection};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// [`LogSink`] 的默认队列容量（条）。
///
/// 4096 条诊断日志足够吸收一次突发（例如握手 + 传输状态密集切换），同时把
/// 最坏情况下的内存占用限制在几百 KB 量级。队列满时绝不阻塞调用方，只丢弃
/// 并计数（见 [`LogSink::dropped_count`]）。
pub const LOG_SINK_CAPACITY: usize = 4096;

/// 一次事务最多写入的记录数。
///
/// 批处理把「每条日志一次 INSERT + 一次 `LogStore` 锁」压低到「每 N 条一次
/// 事务 + 一次锁」，又不至于让查询路径等太久（单批是微秒级）。
const LOG_SINK_BATCH_MAX: usize = 256;

/// writer 线程空闲时轮询 drop 计数的间隔。
///
/// 有记录时 writer 阻塞在 `recv` 上立即被唤醒；该间隔只决定「空闲时多久
/// 检查一次丢弃率」，不引入任何调用方等待。
const LOG_SINK_IDLE_POLL: Duration = Duration::from_millis(500);

/// writer 线程向 stderr 汇报丢弃率的间隔。
const LOG_SINK_DROP_REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// F12：运行期日志保留策略的定时触发间隔（5 分钟）。
///
/// 旧实现在 `LogStore::open` 里只清理一次；长驻进程永远不再 prune，`logs.db`
/// 会一直涨到 [`LOG_MAX_ROWS`] 之上。现在由 `cb-log-writer` 线程按时间/计数
/// 触发（见 [`PruneSchedule`]），调用方仍然只做 `try_send`。
const LOG_PRUNE_INTERVAL: Duration = Duration::from_mins(5);

/// F12：writer 每成功提交多少条记录触发一次保留策略。
const LOG_PRUNE_WRITTEN_THRESHOLD: u64 = 10_000;

/// 日志保留策略的触发条件（纯数据，便于单测注入很小的阈值）。
///
/// 两个条件取“或”：无论持续写入还是长期空闲，最多在
/// `interval`/`written_threshold` 之后执行一次 `prune`。
#[derive(Clone, Copy, Debug)]
struct PruneSchedule {
	/// 累计提交多少条记录后触发；`u64::MAX` = 只按时间触发。
	written_threshold: u64,
	/// 距上次 prune 的最长时间；`Duration::MAX` = 只按计数触发。
	interval: Duration,
}

impl Default for PruneSchedule {
	fn default() -> Self {
		Self {
			written_threshold: LOG_PRUNE_WRITTEN_THRESHOLD,
			interval: LOG_PRUNE_INTERVAL,
		}
	}
}

impl PruneSchedule {
	/// 当前是否该执行 prune。只读本地计数/时钟，无锁、无 syscall。
	fn due(&self, written_since_prune: u64, last_prune: Instant) -> bool {
		written_since_prune >= self.written_threshold || last_prune.elapsed() >= self.interval
	}
}

/// 异步日志队列的关闭等待上限。
///
/// writer 可能卡在 SQLite 的 `busy_timeout`（5 s）上，因此等待上限必须大于它；
/// 超时后只记录错误并放弃 join（writer 会在 SQLite 释放后自行退出），
/// 绝不让关闭流程无限挂起。
pub const LOG_SINK_JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// `logs` 表 INSERT 语句（单语言与多语言共用的列）。
const INSERT_LOG_ROW_SQL: &str = r"
    INSERT INTO logs (ts_utc, level, component, category, message, exception, props_json)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
    ";

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
		insert_multilang_row(
			&self.conn,
			ts_utc,
			level,
			component,
			category,
			message_en,
			message_zh_cn,
			exception,
			props_json,
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

/// 把一条多语言日志编码成 `logs` 表行并插入。
///
/// [`LogStore::write_multilang`] 与异步 writer 线程共用这一段，保证两条写入
/// 路径生成的 `message` JSON 完全一致（日志 UI 依赖该格式）。
#[allow(clippy::too_many_arguments)]
fn insert_multilang_row(
	conn: &Connection,
	ts_utc: i64,
	level: i32,
	component: &str,
	category: &str,
	message_en: &str,
	message_zh_cn: Option<&str>,
	exception: Option<&str>,
	props_json: Option<&str>,
) -> anyhow::Result<()> {
	// 使用结构体构建多语言消息（类型安全，易于扩展）
	let msg = MultilingualMessage {
		en: message_en.to_string(),
		zh_cn: message_zh_cn.map(ToString::to_string),
	};
	let message_json = serde_json::to_string(&msg)?;
	conn.execute(
		INSERT_LOG_ROW_SQL,
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
	Ok(())
}

/// 队列中的一条待写日志。
///
/// 所有字段都是拥有所有权的 `String`：调用方（Tokio worker）只做字符串复制与
/// `try_send`，真正的 JSON 编码 / SQLite INSERT 全部发生在 `cb-log-writer` 线程。
#[derive(Debug)]
struct LogRecord {
	ts_utc: i64,
	level: i32,
	component: String,
	category: String,
	message_en: String,
	message_zh_cn: Option<String>,
	exception: Option<String>,
	props_json: Option<String>,
}

impl LogRecord {
	fn new(
		level: i32,
		component: &str,
		category: &str,
		message_en: &str,
		message_zh_cn: Option<&str>,
		exception: Option<&str>,
	) -> Self {
		Self {
			ts_utc: crate::util::now_ms(),
			level,
			component: component.to_string(),
			category: category.to_string(),
			message_en: message_en.to_string(),
			message_zh_cn: message_zh_cn.map(ToString::to_string),
			exception: exception.map(ToString::to_string),
			props_json: None,
		}
	}
}

/// `LogSink` 的内部共享状态：所有克隆共享同一个队列、计数器和 writer handle。
struct LogSinkInner {
	/// 有界队列的发送端。`shutdown` 会把它取走并 drop，writer 据此退出。
	tx: Mutex<Option<SyncSender<LogRecord>>>,
	/// 是否已停止接收（writer 退出后所有 `log*` 调用都只累加丢弃计数）。
	closed: AtomicBool,
	/// 因队列满 / 已关闭而丢弃的记录数（调用方只做原子递增，绝不再写 DB）。
	dropped: Arc<AtomicU64>,
	/// writer 已成功提交的记录数。
	written: Arc<AtomicU64>,
	/// `cb-log-writer` 的 join handle。`shutdown` 取走后等待其退出。
	writer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// 非阻塞日志接收端：异步运行时只调用 `try_send`，SQLite 写入由专用线程完成。
///
/// # 为什么需要它
///
/// P1 之后 NetManager 使用多线程 Tokio runtime，但会话路径仍直接
/// `log_store.lock()` + INSERT：一次慢 INSERT / WAL checkpoint / `busy_timeout`
/// 就能占住一个 worker，严重时连 2 s 心跳都发不出去。`LogSink` 把日志写入
/// 从运行时线程完全剥离。
///
/// # 设计
///
/// - 有界 `std::sync::mpsc::SyncSender<LogRecord>`（默认容量 [`LOG_SINK_CAPACITY`]）；
/// - 调用方只做 `try_send`：`Full` / `Disconnected` 时原子递增丢弃计数并立即返回；
/// - 专用 OS 线程 `cb-log-writer` 持有 `Arc<Mutex<LogStore>>`，按批（最多
///   [`LOG_SINK_BATCH_MAX`] 条）在**一个事务**里写入；该锁与 FFI 查询路径
///   （`cb_logs_query_*` / `cb_logs_stats`）共享，查询因此能看到已提交的日志；
/// - 顺序：队列 FIFO + 单 writer + 单事务，SQLite `id` 单调递增，提交顺序与
///   `log*` 调用顺序一致（跨批次也一致）；
/// - 丢弃率只在 writer 线程周期性写 stderr，调用方永远不会因为统计而写 DB。
#[derive(Clone)]
pub struct LogSink {
	inner: Arc<LogSinkInner>,
}

impl LogSink {
	/// 用默认容量创建 sink，并启动 `cb-log-writer` 线程。
	pub fn new(store: Arc<Mutex<LogStore>>) -> Self {
		Self::with_capacity(store, LOG_SINK_CAPACITY)
	}

	/// 用指定队列容量创建 sink。
	pub fn with_capacity(store: Arc<Mutex<LogStore>>, capacity: usize) -> Self {
		Self::with_capacity_and_schedule(store, capacity, PruneSchedule::default())
	}

	/// 用指定队列容量与保留策略调度创建 sink（生产默认值见 [`PruneSchedule`]）。
	fn with_capacity_and_schedule(
		store: Arc<Mutex<LogStore>>,
		capacity: usize,
		schedule: PruneSchedule,
	) -> Self {
		let capacity = capacity.max(1);
		let (tx, rx) = std::sync::mpsc::sync_channel(capacity);
		let dropped = Arc::new(AtomicU64::new(0));
		let written = Arc::new(AtomicU64::new(0));
		let writer_dropped = Arc::clone(&dropped);
		let writer_written = Arc::clone(&written);
		let writer = std::thread::Builder::new()
			.name("cb-log-writer".to_string())
			.spawn(move || writer_loop(&store, &rx, &writer_dropped, &writer_written, schedule))
			.expect("failed to spawn cb-log-writer thread");
		Self {
			inner: Arc::new(LogSinkInner {
				tx: Mutex::new(Some(tx)),
				closed: AtomicBool::new(false),
				dropped,
				written,
				writer: Mutex::new(Some(writer)),
			}),
		}
	}

	/// 测试专用：用小阈值调度保留策略，验证 writer 线程会按计数/定时触发。
	#[cfg(test)]
	fn with_policy_for_test(
		store: Arc<Mutex<LogStore>>,
		capacity: usize,
		schedule: PruneSchedule,
	) -> Self {
		Self::with_capacity_and_schedule(store, capacity, schedule)
	}

	/// 通用入口：指定 level / component，与 [`LogStore::write_multilang`] 等价。
	#[allow(clippy::too_many_arguments)]
	pub fn log(
		&self,
		level: i32,
		component: &str,
		category: &str,
		message_en: &str,
		message_zh_cn: Option<&str>,
		exception: Option<&str>,
	) {
		self.enqueue(LogRecord::new(
			level,
			component,
			category,
			message_en,
			message_zh_cn,
			exception,
		));
	}

	/// 非阻塞 Info 日志（component 固定为 `Core`，与 [`LogStore::log_info`] 一致）。
	pub fn log_info(&self, category: &str, message_en: &str, message_zh_cn: Option<&str>) {
		self.log(2, "Core", category, message_en, message_zh_cn, None);
	}

	/// 非阻塞 Warn 日志。
	pub fn log_warn(&self, category: &str, message_en: &str, message_zh_cn: Option<&str>) {
		self.log(3, "Core", category, message_en, message_zh_cn, None);
	}

	/// 非阻塞 Error 日志。
	pub fn log_error(
		&self,
		category: &str,
		message_en: &str,
		message_zh_cn: Option<&str>,
		error: Option<&str>,
	) {
		self.log(4, "Core", category, message_en, message_zh_cn, error);
	}

	/// 非阻塞 Debug 日志。
	pub fn log_debug(&self, category: &str, message_en: &str, message_zh_cn: Option<&str>) {
		self.log(1, "Core", category, message_en, message_zh_cn, None);
	}

	/// 已丢弃（队列满 / 已关闭）的记录数。
	pub fn dropped_count(&self) -> u64 {
		self.inner.dropped.load(Ordering::Relaxed)
	}

	/// writer 已成功提交的记录数。
	pub fn written_count(&self) -> u64 {
		self.inner.written.load(Ordering::Relaxed)
	}

	/// 入队；**任何情况下都不阻塞、不写 DB**。
	fn enqueue(&self, record: LogRecord) {
		if self.inner.closed.load(Ordering::Relaxed) {
			self.inner.dropped.fetch_add(1, Ordering::Relaxed);
			return;
		}
		let guard = match self.inner.tx.lock() {
			Ok(guard) => guard,
			Err(poisoned) => poisoned.into_inner(),
		};
		let Some(tx) = guard.as_ref() else {
			self.inner.dropped.fetch_add(1, Ordering::Relaxed);
			return;
		};
		match tx.try_send(record) {
			Ok(()) => {}
			Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
				self.inner.dropped.fetch_add(1, Ordering::Relaxed);
			}
		}
	}

	/// 停止接收、排空队列并等待 writer 退出（有界等待，返回是否成功 join）。
	///
	/// 顺序保证：
	/// 1. 先置 `closed`，之后所有 `log*` 调用直接计入丢弃计数（不碰队列）；
	/// 2. 取走并 drop `SyncSender`：writer 在收到 `Disconnected` 之前会先排空
	///    已入队的记录，因此不会丢失“关闭前成功入队”的日志；
	/// 3. 以 `timeout` 为上限轮询 writer 是否结束，结束后 `join` 回收线程。
	///
	/// 调用方是 `Core::shutdown`（FFI 线程），允许短暂阻塞；运行时线程从不调用。
	pub fn flush_and_join(&self, timeout: Duration) -> bool {
		self.inner.closed.store(true, Ordering::SeqCst);
		let sender = {
			let mut guard = match self.inner.tx.lock() {
				Ok(guard) => guard,
				Err(poisoned) => poisoned.into_inner(),
			};
			guard.take()
		};
		drop(sender);

		let writer = {
			let mut guard = match self.inner.writer.lock() {
				Ok(guard) => guard,
				Err(poisoned) => poisoned.into_inner(),
			};
			guard.take()
		};
		let Some(handle) = writer else {
			return true; // 已关闭或该 sink 没有 writer（测试构造）
		};

		let deadline = Instant::now() + timeout;
		while !handle.is_finished() && Instant::now() < deadline {
			std::thread::sleep(Duration::from_millis(2));
		}
		if handle.is_finished() {
			let _ = handle.join();
			true
		} else {
			eprintln!(
				"[logs] cb-log-writer did not exit within {timeout:?}; detaching \
                 (SQLite is likely blocked; queued logs may be lost)"
			);
			false
		}
	}

	/// 测试专用：构造一个**没有 writer** 的 sink（等效于 writer 永久停滞）。
	///
	/// 返回的 receiver 必须被测试持有，否则队列会立即变成 `Disconnected`；
	/// 持有它就能确定性地验证「第一次入队成功、后续入队因队列满而丢弃」。
	#[cfg(test)]
	fn new_stalled_for_test(capacity: usize) -> (Self, Receiver<LogRecord>) {
		let (tx, rx) = std::sync::mpsc::sync_channel(capacity.max(1));
		let sink = Self {
			inner: Arc::new(LogSinkInner {
				tx: Mutex::new(Some(tx)),
				closed: AtomicBool::new(false),
				dropped: Arc::new(AtomicU64::new(0)),
				written: Arc::new(AtomicU64::new(0)),
				writer: Mutex::new(None),
			}),
		};
		(sink, rx)
	}
}

/// `cb-log-writer` 主循环：批量取队列 → 一次事务写入 → 周期汇报丢弃率 →
/// 按 [`PruneSchedule`] 执行日志保留策略。
///
/// F12：`prune` 只在**本线程**、批量提交之后/空闲超时分支里执行，调用方
/// （Tokio worker）永远只做 `try_send`，不会因为保留策略被阻塞。
fn writer_loop(
	store: &Mutex<LogStore>,
	rx: &Receiver<LogRecord>,
	dropped: &AtomicU64,
	written: &AtomicU64,
	schedule: PruneSchedule,
) {
	let mut reported_dropped = 0u64;
	let mut last_report = Instant::now();
	let mut written_since_prune = 0u64;
	let mut last_prune = Instant::now();

	loop {
		let first = match rx.recv_timeout(LOG_SINK_IDLE_POLL) {
			Ok(record) => record,
			Err(RecvTimeoutError::Timeout) => {
				// 空闲时也按时间触发（长期低日志量的进程同样需要兜底）。
				if schedule.due(written_since_prune, last_prune) {
					written_since_prune = 0;
					last_prune = Instant::now();
					prune_now(store, "timer");
				}
				report_dropped(dropped, &mut reported_dropped, &mut last_report);
				continue;
			}
			Err(RecvTimeoutError::Disconnected) => break,
		};

		let mut batch = Vec::with_capacity(LOG_SINK_BATCH_MAX);
		batch.push(first);
		let mut disconnected = false;
		while batch.len() < LOG_SINK_BATCH_MAX {
			match rx.try_recv() {
				Ok(record) => batch.push(record),
				Err(TryRecvError::Empty) => break,
				Err(TryRecvError::Disconnected) => {
					disconnected = true;
					break;
				}
			}
		}

		let batch_len = batch.len() as u64;
		{
			// 与 FFI 查询路径共享同一把锁：查询最多等一个事务，writer 不会在
			// 持锁期间阻塞等待队列（队列由上面的循环排空）。
			let mut guard = match store.lock() {
				Ok(guard) => guard,
				Err(poisoned) => poisoned.into_inner(),
			};
			match write_batch(&guard, &batch) {
				Ok(()) => {
					written.fetch_add(batch_len, Ordering::Relaxed);
					written_since_prune = written_since_prune.saturating_add(batch_len);
				}
				Err(e) => {
					// 整批回滚：计入丢弃，绝不在 writer 里重试（避免永久卡死）。
					dropped.fetch_add(batch_len, Ordering::Relaxed);
					eprintln!("[logs] async log batch write failed ({batch_len} records): {e}");
				}
			}
			// 已经在持锁：直接在同一次临界区里做保留策略，避免二次加锁。
			if schedule.due(written_since_prune, last_prune) {
				written_since_prune = 0;
				last_prune = Instant::now();
				log_prune_locked(&mut guard, "counter");
			}
		}
		report_dropped(dropped, &mut reported_dropped, &mut last_report);

		if disconnected {
			break;
		}
	}

	// 退出前做最后一次汇报，避免最后一段丢弃静默丢失。
	let total = dropped.load(Ordering::Relaxed);
	if total > reported_dropped {
		eprintln!(
			"[logs] log sink dropped {} record(s) before shutdown (cumulative={total})",
			total - reported_dropped
		);
	}
}

/// 在 writer 线程的空闲分支里执行保留策略（自行加锁）。
fn prune_now(store: &Mutex<LogStore>, reason: &str) {
	let mut guard = match store.lock() {
		Ok(guard) => guard,
		Err(poisoned) => poisoned.into_inner(),
	};
	log_prune_locked(&mut guard, reason);
}

/// 执行一次保留策略并汇报结果；调用方必须已持有 `LogStore` 锁。
///
/// 只在 writer 线程调用。结果只打印计数（prune 失败也只打一次错误），不会回写
/// 日志表（避免“为了一条 prune 日志再触发一次 prune”的递归）。
fn log_prune_locked(store: &mut LogStore, reason: &str) {
	match store.prune(crate::util::now_ms()) {
		Ok(stats) => {
			if stats.total_deleted() > 0 || stats.vacuumed {
				eprintln!(
					"[logs] runtime prune ({reason}): deleted_by_age={}, deleted_by_count={}, vacuumed={}",
					stats.deleted_by_age, stats.deleted_by_count, stats.vacuumed
				);
			}
		}
		Err(e) => eprintln!("[logs] runtime prune ({reason}) failed: {e}"),
	}
}

/// 在一个事务里写入一批记录；失败时整批回滚。
fn write_batch(store: &LogStore, batch: &[LogRecord]) -> anyhow::Result<()> {
	let tx = store.conn.unchecked_transaction()?;
	for record in batch {
		insert_multilang_row(
			&tx,
			record.ts_utc,
			record.level,
			&record.component,
			&record.category,
			&record.message_en,
			record.message_zh_cn.as_deref(),
			record.exception.as_deref(),
			record.props_json.as_deref(),
		)?;
	}
	tx.commit()?;
	Ok(())
}

/// 周期性汇报被丢弃的日志条数；只在 writer 线程调用。
fn report_dropped(dropped: &AtomicU64, reported: &mut u64, last_report: &mut Instant) {
	if last_report.elapsed() < LOG_SINK_DROP_REPORT_INTERVAL {
		return;
	}
	let total = dropped.load(Ordering::Relaxed);
	if total > *reported {
		eprintln!(
			"[logs] log sink dropped {} record(s) in the last {:?} \
             (writer is behind; cumulative={total})",
			total - *reported,
			last_report.elapsed()
		);
	}
	*reported = total;
	*last_report = Instant::now();
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

	/// 队列满时 `log*` 必须立即返回并计入丢弃计数，绝不阻塞调用方。
	///
	/// 用一个没有 writer 的 sink（等价于 writer 永久停滞）把容量 1 的队列瞬间
	/// 填满：第一次入队成功，其余全部因 `Full` 被丢弃。只要实现里出现任何
	/// 阻塞式 `send` / SQLite 调用，1000 次循环就会卡住或超时。
	#[test]
	fn sink_never_blocks_when_writer_is_stalled() {
		let (sink, _rx) = LogSink::new_stalled_for_test(1);
		let started = Instant::now();
		for i in 0..1_000 {
			sink.log_info("SinkTest", &format!("queued message {i}"), None);
		}
		let elapsed = started.elapsed();

		assert_eq!(
			sink.dropped_count(),
			999,
			"capacity=1 只接收第一条，其余 999 条应被丢弃"
		);
		assert!(
			elapsed < Duration::from_millis(500),
			"log() 不得阻塞调用方，1000 次入队耗时 {elapsed:?}"
		);
	}

	/// 真实 writer：入队后 flush，记录必须全部落库且顺序/内容不变。
	#[test]
	fn sink_writer_commits_batches_and_flushes() {
		let dir = tempfile::tempdir().unwrap();
		let store = Arc::new(Mutex::new(LogStore::open(dir.path()).unwrap()));
		let sink = LogSink::with_capacity(Arc::clone(&store), 8);
		for i in 0..5 {
			sink.log_info("SinkTest", &format!("flush record {i}"), None);
		}

		assert!(
			sink.flush_and_join(Duration::from_secs(5)),
			"writer 必须正常退出"
		);
		assert!(
			sink.flush_and_join(Duration::from_secs(5)),
			"重复关闭必须幂等"
		);
		assert_eq!(sink.written_count(), 5);
		assert_eq!(sink.dropped_count(), 0);

		let entries = store
			.lock()
			.unwrap()
			.query_latest(0, Some("flush record"), 10, None)
			.unwrap();
		assert_eq!(entries.len(), 5, "flush 后 5 条记录都必须落库");
		for (i, entry) in entries.iter().rev().enumerate() {
			assert!(
				entry.message.contains(&format!("flush record {i}")),
				"记录顺序/内容必须保持: {}",
				entry.message
			);
		}
	}

	/// F12：writer 累计提交 N 条记录后必须自行触发一次 prune，且调用方
	/// （`log_info`）在触发前后都不阻塞、不碰 SQLite。
	#[test]
	fn writer_prunes_after_written_threshold_without_blocking_callers() {
		let dir = tempfile::tempdir().unwrap();
		let store = Arc::new(Mutex::new(LogStore::open(dir.path()).unwrap()));
		let now = crate::util::now_ms();
		let expired = now - (LOG_RETENTION_DAYS + 1) * MS_PER_DAY;
		{
			let mut guard = store.lock().unwrap();
			for _ in 0..3 {
				write_at(&mut guard, expired);
			}
		}
		assert_eq!(store.lock().unwrap().stats().unwrap().count, 3);

		// 阈值 5 条：写入 5 条后 writer 必须顺带把 3 条过期记录 prune 掉。
		let sink = LogSink::with_policy_for_test(
			Arc::clone(&store),
			16,
			PruneSchedule {
				written_threshold: 5,
				interval: Duration::from_secs(3_600),
			},
		);
		let started = Instant::now();
		for i in 0..5 {
			sink.log_info("PruneTest", &format!("record {i}"), None);
		}
		let elapsed = started.elapsed();
		assert!(
			elapsed < Duration::from_millis(500),
			"log() 不得因保留策略阻塞调用方，5 次入队耗时 {elapsed:?}"
		);

		assert!(sink.flush_and_join(Duration::from_secs(5)));
		let stats = store.lock().unwrap().stats().unwrap();
		assert_eq!(
			stats.count, 5,
			"5 条新写入保留、3 条过期记录必须被 writer 线程 prune"
		);
	}

	/// F12：长期空闲的进程也必须按时间触发 prune（不依赖写入计数）。
	#[test]
	fn writer_prunes_on_timer_while_idle() {
		let dir = tempfile::tempdir().unwrap();
		let store = Arc::new(Mutex::new(LogStore::open(dir.path()).unwrap()));
		let now = crate::util::now_ms();
		let expired = now - (LOG_RETENTION_DAYS + 1) * MS_PER_DAY;
		{
			let mut guard = store.lock().unwrap();
			write_at(&mut guard, expired);
		}
		assert_eq!(store.lock().unwrap().stats().unwrap().count, 1);

		let sink = LogSink::with_policy_for_test(
			Arc::clone(&store),
			8,
			PruneSchedule {
				written_threshold: u64::MAX,
				interval: Duration::from_millis(10),
			},
		);
		// writer 空闲轮询粒度是 LOG_SINK_IDLE_POLL（500 ms）；给足 3 个周期。
		let deadline = Instant::now() + Duration::from_secs(3);
		while store.lock().unwrap().stats().unwrap().count != 0 {
			assert!(
				Instant::now() < deadline,
				"空闲 writer 未在截止时间内执行定时 prune"
			);
			std::thread::sleep(Duration::from_millis(25));
		}
		assert!(sink.flush_and_join(Duration::from_secs(5)));
	}
}
