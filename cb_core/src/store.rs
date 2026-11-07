use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use crate::model::{FileMeta, ItemKind, ItemMeta, ItemPreview};

pub struct Store {
	pub(crate) conn: Connection,
	/// 本次 `open` 是否执行了 drop-and-rebuild（数据丢失路径）。
	///
	/// 全新数据库与损坏兜底重建为 `true`；**逐版本迁移成功后为 `false`**（数据保留，
	/// 调用方不应清空 CAS）。见 [`SCHEMA_VERSION`] 与 `ensure_schema`。
	pub rebuilt: bool,
}

/// 当前 `core.db` 的 schema **目标**版本。
///
/// **v4 起采用逐版本增量迁移，不再“版本不匹配即整库重建”（drop-and-rebuild）。**
/// 打开已有库时按 `MIGRATIONS` 表从当前 `PRAGMA user_version` 逐级升级到本常量：
/// 每一步都在**独立事务**里执行 DDL 与版本号更新，任一步失败则整体回滚，库停留在
/// 失败前的版本并在下次启动重试。`items` / `history` / `content_cache` /
/// `trusted_peers` / `peer_rules` 的数据全部保留，用户无需重新配对。
///
/// 版本高于本常量（旧 app 打开新库）直接返回错误，绝不降级、绝不清库。
/// 只有在**目标版本**的结构指纹（`schema_ready`）不一致时才回退到旧的
/// drop-and-rebuild——那是损坏兜底，不是正常升级路径，且会显式写日志。
///
/// 任何未来的 DDL 变更都必须同时：递增本常量、更新 [`SCHEMA_DDL`]（新建库的完整
/// 目标结构）、在 `MIGRATIONS` 追加一条 `旧版本 -> 旧版本 + 1` 的迁移。只改
/// `SCHEMA_DDL` 不改版本号会让已有库继续使用旧结构（v1 时代
/// `CREATE TABLE IF NOT EXISTS` 踩过的坑）。
///
/// 版本历史：
/// - v3（ADR-002 §3.7）：`content_cache.durable INTEGER NOT NULL DEFAULT 1`。
/// - v4：`idx_items_kind_created(kind, created_ts_ms)`（见 `MIGRATIONS`）。
/// - v5（P3 空间回收）：`PRAGMA auto_vacuum=INCREMENTAL`（升级后由一次性 `VACUUM`
///   生效）；维护路径据此用 `PRAGMA incremental_vacuum` 增量归还空闲页。表结构本身
///   不变，版本号是这次一次性转换的守卫（见 `migrate_v4_to_v5`）。
pub const SCHEMA_VERSION: i64 = 5;

// ---------------------------------------------------------------------------
// F12：入站 `ItemMeta` 的逻辑上限
//
// 对端可以在 10 MiB 控制帧内塞入任意长/任意多的元数据字段。这些字段会原样
// 进入 `items` / `history` / `files_json`，并在每个进程生命周期里被反复读取，
// 因此必须在**落库之前**用 O(1) 的字符串/计数检查挡掉，而不是依赖帧大小上限。
//
// 这里定义的是“逻辑上限”，全部是常数，不读取环境变量、不做正则、不分配与
// 字段总长度无关的内存；校验函数只遍历一遍文件列表。
// ---------------------------------------------------------------------------

/// `item_id` / `source_device_id` / `FileMeta.file_id` 允许的最大字节数。
pub const MAX_ITEM_META_ID_BYTES: usize = 128;

/// 单条 `ItemMeta` 允许携带的 `files[]` 数量。
pub const MAX_ITEM_META_FILES: usize = 256;

/// `FileMeta.rel_name` 允许的最大字节数。
pub const MAX_ITEM_META_REL_NAME_BYTES: usize = 512;

/// `ItemPreview` 序列化后的最大字节数（64 KiB）。
pub const MAX_ITEM_META_PREVIEW_BYTES: usize = 64 * 1024;

/// 单条 `ItemMeta` 所有字段长度求和后的逻辑上限（256 KiB）。
///
/// 10 MiB 帧上限只是传输层防线；这里把“一条元数据真正值得保存的大小”压到
/// 256 KiB，使单条恶意元数据无法长期占据数据库/内存的大头。
pub const MAX_ITEM_META_TOTAL_BYTES: usize = 256 * 1024;

/// F12 历史保留：每个账号默认保留的历史行数（维护路径按此硬删除最旧行）。
pub const DEFAULT_HISTORY_RETENTION_ROWS_PER_ACCOUNT: i64 = 10_000;

/// F12 历史保留：覆盖默认保留行数的环境变量（`0` = 关闭保留，非法值回退默认）。
pub const HISTORY_RETENTION_ENV: &str = "CB_HISTORY_RETENTION_ROWS";

/// 入站元数据被拒绝的具体原因（只用于计数日志，不包含任何对端字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaLimitKind {
	ItemIdLength,
	ItemIdChars,
	DeviceIdLength,
	DeviceIdChars,
	FileIdLength,
	FileIdChars,
	TooManyFiles,
	RelNameLength,
	PreviewTooLarge,
	TotalTooLarge,
}

impl MetaLimitKind {
	/// 稳定的短原因串（用于诊断日志 / 事件 payload，绝不拼接对端字符串）。
	pub fn as_str(self) -> &'static str {
		match self {
			Self::ItemIdLength => "item_id_length",
			Self::ItemIdChars => "item_id_chars",
			Self::DeviceIdLength => "device_id_length",
			Self::DeviceIdChars => "device_id_chars",
			Self::FileIdLength => "file_id_length",
			Self::FileIdChars => "file_id_chars",
			Self::TooManyFiles => "too_many_files",
			Self::RelNameLength => "rel_name_length",
			Self::PreviewTooLarge => "preview_too_large",
			Self::TotalTooLarge => "total_too_large",
		}
	}
}

/// 一次元数据校验的计数快照 + 被触发的限制。
///
/// 所有字段都是长度/计数，**没有任何对端可控字符串**，因此可以安全地写入日志
/// 或事件（避免攻击者用超长/控制字符污染诊断输出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaLimitViolation {
	pub kind: MetaLimitKind,
	pub item_id_len: usize,
	pub device_id_len: usize,
	pub files: usize,
	pub max_rel_name_len: usize,
	pub preview_bytes: usize,
	pub total_meta_bytes: usize,
}

/// ID 类字段的白名单：可打印 ASCII（0x20-0x7E），且不含路径分隔符。
///
/// `:` 也一并拒绝：它是 Windows ADS/盘符分隔符，而 `file_id` 一旦被用于构造
/// 路径就必须先通过同样的白名单（见 F14 的 `cas::validate_component`）。
fn is_safe_meta_id(value: &str) -> bool {
	value
		.bytes()
		.all(|b| (0x20..=0x7E).contains(&b) && !matches!(b, b'/' | b'\\' | b':'))
}

/// `preview` 的序列化大小；`text` 长度超过上限时直接返回 `usize::MAX`，
/// 避免为一条注定被拒绝的元数据做最多 6 倍膨胀的 JSON 转义。
fn preview_serialized_len(preview: &ItemPreview) -> usize {
	if preview
		.text
		.as_ref()
		.is_some_and(|text| text.len() > MAX_ITEM_META_PREVIEW_BYTES)
	{
		return usize::MAX;
	}
	serde_json::to_vec(preview).map_or(usize::MAX, |bytes| bytes.len())
}

/// 元数据逻辑大小：所有字符串字段长度 + 序列化后的 `preview` 大小。
///
/// 这是“保存到数据库里的有效负载”的上界（数组/JSON 标点开销很小，不单独计量），
/// 用饱和加法避免恶意长度造成整数溢出。
fn meta_logical_size(meta: &ItemMeta, preview_bytes: usize) -> usize {
	let mut total = preview_bytes
		.saturating_add(meta.item_id.len())
		.saturating_add(meta.source_device_id.len())
		.saturating_add(meta.source_device_name.as_deref().map_or(0, str::len))
		.saturating_add(meta.content.mime.len())
		.saturating_add(meta.content.sha256.len());
	for file in &meta.files {
		total = total
			.saturating_add(file.file_id.len())
			.saturating_add(file.rel_name.len())
			.saturating_add(file.sha256.as_deref().map_or(0, str::len))
			.saturating_add(file.local_path.as_deref().map_or(0, str::len));
	}
	total
}

/// 校验一条**远端** `ItemMeta` 是否在 F12 的逻辑上限内。
///
/// 调用方：入站 `CtrlMsg::ItemMeta` 路径（先校验再落库/发事件）与
/// [`Store::insert_remote_item`]（纵深防御）。函数是纯函数、只遍历一次文件列表，
/// 不触碰 DB/磁盘，因此可以在解析完消息后立刻调用。
///
/// 返回 `Err(MetaLimitViolation)` 时调用方必须**非致命**地丢弃该元数据
/// （`ContentDenied`/`CORE_ERROR` 事件 + 计数 warn），不能终止会话。
pub fn validate_inbound_item_meta(meta: &ItemMeta) -> Result<(), MetaLimitViolation> {
	let item_id_len = meta.item_id.len();
	let device_id_len = meta.source_device_id.len();
	let files = meta.files.len();
	let max_rel_name_len = meta
		.files
		.iter()
		.map(|file| file.rel_name.len())
		.max()
		.unwrap_or(0);
	let preview_bytes = preview_serialized_len(&meta.preview);
	let total_meta_bytes = meta_logical_size(meta, preview_bytes);

	let stats = MetaLimitViolation {
		kind: MetaLimitKind::ItemIdLength,
		item_id_len,
		device_id_len,
		files,
		max_rel_name_len,
		preview_bytes,
		total_meta_bytes,
	};
	let fail = |kind: MetaLimitKind| Err(MetaLimitViolation { kind, ..stats });

	if item_id_len > MAX_ITEM_META_ID_BYTES {
		return fail(MetaLimitKind::ItemIdLength);
	}
	if !is_safe_meta_id(&meta.item_id) {
		return fail(MetaLimitKind::ItemIdChars);
	}
	if device_id_len > MAX_ITEM_META_ID_BYTES {
		return fail(MetaLimitKind::DeviceIdLength);
	}
	if !is_safe_meta_id(&meta.source_device_id) {
		return fail(MetaLimitKind::DeviceIdChars);
	}
	if files > MAX_ITEM_META_FILES {
		return fail(MetaLimitKind::TooManyFiles);
	}
	for file in &meta.files {
		if file.file_id.len() > MAX_ITEM_META_ID_BYTES {
			return fail(MetaLimitKind::FileIdLength);
		}
		if !is_safe_meta_id(&file.file_id) {
			return fail(MetaLimitKind::FileIdChars);
		}
	}
	if max_rel_name_len > MAX_ITEM_META_REL_NAME_BYTES {
		return fail(MetaLimitKind::RelNameLength);
	}
	if preview_bytes > MAX_ITEM_META_PREVIEW_BYTES {
		return fail(MetaLimitKind::PreviewTooLarge);
	}
	if total_meta_bytes > MAX_ITEM_META_TOTAL_BYTES {
		return fail(MetaLimitKind::TotalTooLarge);
	}
	Ok(())
}

/// 重建时先删除的表：本模块拥有的全部内容/身份表。
///
/// 顺序上 `history` 在 `items` 之前（子表先删，避免外键检查打扰 DROP）；
/// `DROP TABLE` 会一并删除该表上的索引。v1（含更早的 migrate_v1..v3 时代）
/// 用的就是这些表名，没有发现其它历史拼写/别名，因此列表不需要兼容旧名字。
const DROP_OWNED_TABLES_DDL: &str = r"
    DROP TABLE IF EXISTS history;
    DROP TABLE IF EXISTS items;
    DROP TABLE IF EXISTS content_cache;
    DROP TABLE IF EXISTS trusted_peers;
    DROP TABLE IF EXISTS peer_rules;
";

/// 目标版本（[`SCHEMA_VERSION`]）的完整 DDL：新建库与损坏兜底重建都执行它。
///
/// 已有库的升级由 `MIGRATIONS` 逐版本完成，不执行本 DDL。因此每次 schema 变更
/// 必须同时更新这里和迁移表，二者描述的是同一个目标结构。
///
/// 表职责：
/// - `items`：每个剪贴板条目的元数据（内容本体在 CAS，`sha256_hex` 关联 `content_cache`）。
/// - `history`：按账号的历史视图；软删除只置 `is_deleted=1`，不删行。
/// - `content_cache`：SHA-256 内容的体积、`present` 状态（LRU/GC 依据）与
///   `durable` 持久化标记（1 = 已 fsync 或按策略无需 fsync；0 = 后台同步待完成）。
/// - `trusted_peers`：TOFU 设备指纹（配对信任）。
/// - `peer_rules`：每个设备的共享/接收策略。
///
/// 约束说明（v3 起）：
/// - `items.kind` 用 CHECK 限定为 `kind_to_str` 产出的三个值，防止脏数据。
/// - `history.item_id` 外键到 `items.item_id` 并 `ON DELETE CASCADE`。现有删除路径
///   都是软删除（只置 `is_deleted`），因此 cascade 只在未来做硬删除时兜底：
///   删 items 行时不会留下悬空 history。`clear_core_db` 先删 history 再删 items，
///   不依赖 cascade。
/// - `items.expires_ts_ms` 已删除：全仓库没有任何 TTL 清理逻辑读取它，模型里的
///   `ItemMeta.expires_ts_ms` 仍保留（对外 JSON 形状），读取时恒为 `None`。
/// - `content_cache.durable` 默认 1：本列之前的所有行（以及本地 ingest 写入）都按
///   “已持久化”处理；只有接收路径在 `large-only` 且 ≥ 8 MiB 时才显式写 0。
const SCHEMA_DDL: &str = r"
    -- items 表：存储项的元数据
    CREATE TABLE IF NOT EXISTS items (
      item_id TEXT PRIMARY KEY,
      kind TEXT NOT NULL CHECK (kind IN ('text','image','file_list')),
      owner_device_id TEXT NOT NULL,
      created_ts_ms INTEGER NOT NULL,
      size_bytes INTEGER NOT NULL,
      mime TEXT NOT NULL,
      sha256_hex TEXT NOT NULL,
      preview_json TEXT,
      files_json TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_ts_ms);
    CREATE INDEX IF NOT EXISTS idx_items_sha256 ON items(sha256_hex);
    CREATE INDEX IF NOT EXISTS idx_items_owner_created ON items(owner_device_id, created_ts_ms);
    -- v4：按 kind + 时间范围筛选/统计条目（活动统计、按类型的历史视图）；
    -- 有序前缀 kind 也服务单类型查询，写入成本远低于全表扫描。
    CREATE INDEX IF NOT EXISTS idx_items_kind_created ON items(kind, created_ts_ms);

    -- history 表：追踪项的历史数据；item 被硬删除时级联清掉历史
    CREATE TABLE IF NOT EXISTS history (
      history_id INTEGER PRIMARY KEY AUTOINCREMENT,
      account_uid TEXT NOT NULL,
      item_id TEXT NOT NULL REFERENCES items(item_id) ON DELETE CASCADE,
      sort_ts_ms INTEGER NOT NULL,
      source_device_id TEXT,
      is_deleted INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_history_account_sort ON history(account_uid, sort_ts_ms DESC);
    CREATE INDEX IF NOT EXISTS idx_history_item ON history(item_id);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_history_account_item ON history(account_uid, item_id);

    -- content_cache 表：存储内容缓存信息
    -- durable：1 = 内容已 fsync 或按策略无需 fsync；0 = 已上报完成但后台 sync_data 待完成
    CREATE TABLE IF NOT EXISTS content_cache (
      sha256_hex TEXT PRIMARY KEY,
      total_bytes INTEGER NOT NULL,
      present INTEGER NOT NULL,
      durable INTEGER NOT NULL DEFAULT 1,
      last_access_ts_ms INTEGER NOT NULL,
      created_ts_ms INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_cache_lru ON content_cache(present, last_access_ts_ms);
    CREATE INDEX IF NOT EXISTS idx_cache_size ON content_cache(present, total_bytes);
    CREATE INDEX IF NOT EXISTS idx_cache_durable ON content_cache(durable);

    -- trusted_peers 表：记录已信任的设备指纹
    CREATE TABLE IF NOT EXISTS trusted_peers (
        account_uid TEXT NOT NULL,
        device_id TEXT NOT NULL,
        fingerprint_sha256 TEXT NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (account_uid, device_id)
    );

    -- peer_rules 表：存储设备共享策略
    CREATE TABLE IF NOT EXISTS peer_rules (
        account_uid TEXT NOT NULL,
        device_id TEXT NOT NULL,
        share_to_peer INTEGER NOT NULL DEFAULT 1,
        accept_from_peer INTEGER NOT NULL DEFAULT 1,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (account_uid, device_id)
    );
";

// ---------------------------------------------------------------------------
// P1：增量迁移框架
//
// 约定：迁移表按 `from` 升序排列，每条只负责 `from -> from + 1`。打开旧库时从
// `PRAGMA user_version` 起逐条执行，每条的 DDL/DML 与版本号更新在**同一个事务**
// 里提交；任何一步失败都会回滚该步，库停留在上一步的版本，数据不丢、也不半迁移。
//
// 迁移实现应使用 `IF NOT EXISTS` 等幂等写法：旧库可能已被手工修补过，或在
// 提交前崩溃后重试。迁移函数只拿到 `&Transaction`，无法自行提交或开新事务。
// ---------------------------------------------------------------------------

/// 单条迁移的实现：把库从 `from` 升到 `from + 1`，只允许操作传入的事务。
type MigrationStep = fn(&Transaction<'_>) -> anyhow::Result<()>;

/// 一条注册在 `MIGRATIONS` 表里的迁移。
struct Migration {
	/// 迁移的源版本；成功后 `user_version` 变为 `from + 1`。
	from: i64,
	/// 迁移实现（见 [`MigrationStep`]）。
	apply: MigrationStep,
	/// 单行说明，成功日志会带上。
	description: &'static str,
}

/// 迁移表：唯一的“版本 -> 操作”注册点。
///
/// 新增 schema 版本时：递增 [`SCHEMA_VERSION`]、更新 [`SCHEMA_DDL`]，并在本表
/// 末尾追加一条 `from = 旧目标版本` 的迁移。版本链必须连续，`ensure_schema`
/// 会检查 `from` 到 `SCHEMA_VERSION` 的每一级都有迁移，缺一级就走显式的
/// drop-and-rebuild 兜底。
const MIGRATIONS: &[Migration] = &[
	Migration {
		from: 3,
		apply: migrate_v3_to_v4,
		description: "create items(kind, created_ts_ms) index for kind/time queries",
	},
	Migration {
		from: 4,
		apply: migrate_v4_to_v5,
		description: "enable incremental auto-vacuum (one-time VACUUM after commit)",
	},
];

/// v3 -> v4：为 `items` 增加 `(kind, created_ts_ms)` 组合索引。
///
/// 用途：按类型 + 时间范围筛选或统计条目时避免全表扫描 `items`；索引的有序前缀
/// `kind` 对单类型查询同样有效。索引名/列不改变任何行数据，是纯增量变更。
/// `IF NOT EXISTS` 保证对手工补过索引的库可重复执行。
fn migrate_v3_to_v4(tx: &Transaction<'_>) -> anyhow::Result<()> {
	tx.execute_batch(
		r"
        CREATE INDEX IF NOT EXISTS idx_items_kind_created ON items(kind, created_ts_ms);
        ",
	)?;
	#[cfg(test)]
	if test_hooks::fail_v4_migration() {
		// 测试注入点：DDL 已执行但提交前失败，用来验证事务回滚 + 版本号不前进。
		anyhow::bail!("injected failure in v3 -> v4 migration (test-only)");
	}
	Ok(())
}

/// v4 -> v5：把数据库切到 `auto_vacuum=INCREMENTAL`。
///
/// `PRAGMA auto_vacuum` 的 set 形式在这里只设置连接级的“期望模式”
/// （`nextAutovac`）；已有数据库要把文件真正转换成 auto-vacuum 结构，必须在
/// 事务之外做一次 `VACUUM`。因此本步骤只负责设置期望值（事务内合法），真正的
/// `VACUUM` 由 [`Store::ensure_schema`] 在本事务提交后、用**同一条连接**执行；
/// `VACUUM` 读取的正是 `nextAutovac`。
fn migrate_v4_to_v5(tx: &Transaction<'_>) -> anyhow::Result<()> {
	tx.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
	Ok(())
}

/// 仅测试使用的迁移注入钩子。
///
/// 用 thread-local 而不是全局 static：`cargo test` 默认并发跑测试，测试线程各自
/// 独立，钩子不会串到其它用例的 `Store::open`。
#[cfg(test)]
mod test_hooks {
	use std::cell::Cell;

	thread_local! {
		/// 让 v3 -> v4 迁移在 DDL 之后故意失败；只在设置它的测试线程上生效。
		static FAIL_V4_MIGRATION: Cell<bool> = const { Cell::new(false) };
	}

	pub(super) fn set_fail_v4_migration(fail: bool) {
		FAIL_V4_MIGRATION.with(|flag| flag.set(fail));
	}

	pub(super) fn fail_v4_migration() -> bool {
		FAIL_V4_MIGRATION.with(Cell::get)
	}
}

pub struct CacheRow {
	pub present: bool,
}

/// 活动统计的一个时间桶（`history` 按 `sort_ts_ms` 归桶后的条目数）。
#[derive(Debug, Clone, Copy)]
pub struct ActivityBucket {
	/// 桶起点（毫秒时间戳，向下取整到 `bucket_ms` 的整数倍）。
	pub ts_ms: i64,
	pub text_count: i64,
	pub image_count: i64,
	pub file_list_count: i64,
}

/// PeerRule 结构体：设备共享策略规则
pub struct PeerRule {
	pub account_uid: String,
	pub device_id: String,
	pub share_to_peer: bool,
	pub accept_from_peer: bool,
	pub updated_at_ms: i64,
}

impl Store {
	pub fn open(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
		let data_dir = data_dir.as_ref();
		fs::create_dir_all(data_dir)?;
		let db_path: PathBuf = data_dir.join("core.db");

		let conn = Connection::open(db_path)?;
		Self::init_pragmas(&conn)?;
		let rebuilt = Self::ensure_schema(&conn)?;
		Ok(Self { conn, rebuilt })
	}

	fn init_pragmas(conn: &Connection) -> anyhow::Result<()> {
		conn.execute_batch(
			r"
            -- P3：增量 auto-vacuum。**必须排在 journal_mode=WAL 与建表之前**：
            -- 全新数据库只有在首页写入前设置，才会以 auto-vacuum 结构创建；
            -- 已有库这里只记录连接级的 nextAutovac，真正的文件结构转换由
            -- v4 -> v5 迁移提交后的一次性 VACUUM 完成（见 ensure_schema）。
            PRAGMA auto_vacuum = INCREMENTAL;
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA foreign_keys=ON;
            PRAGMA busy_timeout=5000;

            -- P4 性能调优（core.db 与 logs.db 使用同一组参数）：
            -- cache_size 为负数时单位是 KiB：-16000 ≈ 16 MB 页缓存，减少读路径换页；
            -- mmap_size = 64 MiB：读多写少的查询走内存映射，省一次内核到用户态拷贝
            --   （个别 SQLite 构建/平台禁用 mmap，读回 0 属正常，见测试）；
            -- temp_store = MEMORY：ORDER BY / 临时索引（如 history 排序）不落临时文件；
            -- journal_size_limit = 8 MiB：checkpoint 后把 -wal 文件压回该上限，
            --   防止批量写入把 WAL 撑大后长期占用磁盘。
            -- 内存代价约 16 + 64 MB；相对 app 约 200 MB 的常驻占用很小，且 mmap 是
            -- 只读、由 OS 按需换页的地址空间（不计入堆），收益（少 syscall/少写盘）
            -- 明显大于这点成本。
            PRAGMA cache_size = -16000;
            PRAGMA mmap_size = 67108864;
            PRAGMA temp_store = MEMORY;
            PRAGMA journal_size_limit = 8388608;
            ",
		)?;
		Ok(())
	}

	/// 按 [`SCHEMA_VERSION`] 迁移或校验数据库模式。
	///
	/// 决策顺序（从上到下，先命中先返回）：
	/// 1. `user_version > SCHEMA_VERSION`（旧 app 打开新库）→ 明确报错，不降级、
	///    不清库；
	/// 2. `user_version == SCHEMA_VERSION`：指纹一致直接使用；不一致才走旧的
	///    drop-and-rebuild（损坏兜底，日志写明原因）；
	/// 3. `user_version == 0`（全新文件）→ 直接创建目标结构；
	/// 4. `0 < user_version < SCHEMA_VERSION`：源结构指纹（表/列/外键）不成立
	///    （旧 v1/v2 形态或已损坏）时无迁移价值，沿用旧的 drop-and-rebuild 并显式
	///    记录；迁移表缺少 `old -> ... -> target` 完整链路时同样回退；否则逐级迁移
	///    （每级一个事务，**任一级失败直接返回 Err**，库停留在该级之前的版本，
	///    不 wipe、不半迁移，下次启动重试）。迁移完成后目标指纹仍不一致 →
	///    损坏兜底 drop-and-rebuild。
	///
	/// # 返回值
	/// `Ok(true)` 表示执行了 drop-and-rebuild（数据丢失）；`Ok(false)` 表示结构已
	/// 是最新，或已通过迁移升级（数据保留）。
	fn ensure_schema(conn: &Connection) -> anyhow::Result<bool> {
		let old_version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;

		if old_version > SCHEMA_VERSION {
			anyhow::bail!(
				"core.db schema version {old_version} is newer than this build supports \
                 ({SCHEMA_VERSION}); refusing to open or modify it. Downgrading the app is \
                 not supported: restore the matching app version, or move the database \
                 aside to start fresh (this build will not delete existing data)"
			);
		}

		if old_version == SCHEMA_VERSION {
			if Self::schema_ready(conn)? {
				return Ok(false);
			}
			Self::rebuild_schema(conn)?;
			eprintln!(
				"[store] core.db structural fingerprint mismatch at v{SCHEMA_VERSION}: \
                 dropped and recreated all content/identity tables (corruption fallback; \
                 trusted_peers/peer_rules are lost by design)"
			);
			return Ok(true);
		}

		if old_version == 0 {
			Self::rebuild_schema(conn)?;
			eprintln!(
				"[store] core.db created at v{SCHEMA_VERSION} (fresh database; empty tables)"
			);
			return Ok(true);
		}

		// 0 < old_version < SCHEMA_VERSION：只有结构可信且迁移链完整才迁移。
		if !Self::schema_core_ready(conn)? {
			Self::rebuild_schema(conn)?;
			eprintln!(
				"[store] core.db v{old_version} does not match any supported source shape \
                 (missing tables/columns/foreign keys): dropped and recreated at \
                 v{SCHEMA_VERSION} (legacy/corruption fallback; trusted_peers/peer_rules lost)"
			);
			return Ok(true);
		}
		if !Self::migration_chain_available(old_version, SCHEMA_VERSION) {
			Self::rebuild_schema(conn)?;
			eprintln!(
				"[store] core.db v{old_version} has no migration path to v{SCHEMA_VERSION}: \
                 dropped and recreated (legacy fallback; trusted_peers/peer_rules lost)"
			);
			return Ok(true);
		}

		Self::apply_migrations(conn, old_version, SCHEMA_VERSION)?;

		// v4 -> v5（以及更早版本经迁移链）把 auto_vacuum 切到 INCREMENTAL。
		// 迁移事务里的 PRAGMA 只设置连接级的 nextAutovac；把已有文件转换成
		// auto-vacuum 结构需要一次 VACUUM，而 VACUUM 不能跑在事务里。因此这里在
		// 迁移提交之后、仍使用**同一条连接**执行一次；`user_version` 已前进到 v5，
		// 正常重启不会再跑（版本变化本身就是“只跑一次”的守卫）。
		Self::convert_auto_vacuum_after_migration(conn);

		if !Self::schema_ready(conn)? {
			Self::rebuild_schema(conn)?;
			eprintln!(
				"[store] core.db structural fingerprint mismatch at v{SCHEMA_VERSION} after \
                 migration: dropped and recreated all content/identity tables \
                 (corruption fallback; trusted_peers/peer_rules are lost by design)"
			);
			return Ok(true);
		}
		Ok(false)
	}

	/// 损坏/旧版兜底：删除本模块拥有的全部表，按 [`SCHEMA_DDL`] 重建并打版本号。
	///
	/// DDL 与版本号在同一事务里：要么整套新结构 + 新版本号一起生效，要么保持旧库
	/// （下次启动重试），不会留下“表删了、版本没写”的中间态。
	///
	/// 注意：这是**唯一**会丢数据的路径，只在全新库或指纹损坏时调用。正常版本升级
	/// 走 [`Store::apply_migrations`]。
	fn rebuild_schema(conn: &Connection) -> anyhow::Result<()> {
		let tx = conn.unchecked_transaction()?;
		tx.execute_batch(DROP_OWNED_TABLES_DDL)?;
		tx.execute_batch(SCHEMA_DDL)?;
		tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
		tx.commit()?;
		Ok(())
	}

	/// 查找 `version -> version + 1` 的迁移（迁移表按 `from` 升序）。
	fn migration_from(version: i64) -> Option<&'static Migration> {
		MIGRATIONS.iter().find(|m| m.from == version)
	}

	/// 迁移表是否覆盖 `from` 到 `target` 的每一级（缺一级就不能迁移）。
	fn migration_chain_available(mut version: i64, target: i64) -> bool {
		while version < target {
			let Some(step) = Self::migration_from(version) else {
				return false;
			};
			version = step.from + 1;
		}
		true
	}

	/// 逐级执行迁移：每级一个事务，DDL/DML 与 `user_version` 同事务提交。
	///
	/// 返回 `Err` 时当前这一级已回滚，库停留在上一级版本；不会清库、也不会留下
	/// 部分应用的结构。错误原样向上传递（[`Store::open`] 失败），下次启动重试。
	fn apply_migrations(conn: &Connection, from: i64, target: i64) -> anyhow::Result<()> {
		let mut version = from;
		while version < target {
			let step = Self::migration_from(version).ok_or_else(|| {
				anyhow::anyhow!(
					"no migration registered for core.db v{version} -> v{}",
					version + 1
				)
			})?;

			let tx = conn.unchecked_transaction()?;
			(step.apply)(&tx)?;
			tx.pragma_update(None, "user_version", step.from + 1)?;
			tx.commit()?;

			eprintln!(
				"[store] core.db migrated v{} -> v{}: {}",
				step.from,
				step.from + 1,
				step.description
			);
			version = step.from + 1;
		}
		Ok(())
	}

	/// 迁移到 v5 之后的一次性 auto-vacuum 转换（`VACUUM`）。
	///
	/// 成本与库大小成正比：升级到本版本后的**第一次启动**会多一次整库重写
	/// （几百 MB 的库可能是秒级，见报告中的 residual risk）。之后由维护路径的
	/// `PRAGMA incremental_vacuum` 增量归还空间，不再做全量 VACUUM。
	///
	/// 失败策略：VACUUM 是空间回收优化而不是数据迁移；磁盘满/IO 错误时记录警告
	/// 并继续启动（库保持原来的 auto-vacuum 模式，功能不受影响）。版本号已经在
	/// 迁移事务里提交，因此本函数**不会**在下次启动重试。
	fn convert_auto_vacuum_after_migration(conn: &Connection) {
		let started = std::time::Instant::now();
		match conn.execute_batch("VACUUM") {
			Ok(()) => match conn.pragma_query_value(None, "auto_vacuum", |r| r.get::<_, i64>(0)) {
				Ok(mode) => eprintln!(
					"[store] core.db v{} auto_vacuum={mode} (2 = INCREMENTAL) after one-time \
                     VACUUM in {:?}",
					SCHEMA_VERSION,
					started.elapsed()
				),
				Err(error) => eprintln!(
					"[store] core.db v{SCHEMA_VERSION} auto_vacuum read-back failed after VACUUM: \
                     {error}"
				),
			},
			Err(error) => eprintln!(
				"[store] core.db v{} auto_vacuum conversion VACUUM failed after {:?}: {error}; \
                 continuing with the previous auto-vacuum mode (space reclamation degraded)",
				SCHEMA_VERSION,
				started.elapsed()
			),
		}
	}

	/// 目标版本（v5）的完整结构指纹。
	///
	/// 在 [`Store::schema_core_ready`] 的表/列/外键检查之上，再要求 v4 新增索引
	/// `idx_items_kind_created` 存在：v3 库没有它（迁移会建），v4/v5 库缺它则说明
	/// 被外部改坏或迁移未生效，按损坏走 drop-and-rebuild。v5 只改 auto-vacuum
	/// 模式（不是 sqlite_master 结构），因此指纹与 v4 相同，版本号单独由
	/// `PRAGMA user_version` 守卫。
	fn schema_ready(conn: &Connection) -> anyhow::Result<bool> {
		if !Self::schema_core_ready(conn)? {
			return Ok(false);
		}
		let has_kind_created_idx: i64 = conn.query_row(
			"SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_items_kind_created'",
			[],
			|r| r.get(0),
		)?;
		Ok(has_kind_created_idx > 0)
	}

	/// v3 与 v4 共有的结构指纹：表、关键列与外键。
	///
	/// 迁移前的源结构检查与目标版本检查都复用它；v4 新增的索引由
	/// [`Store::schema_ready`] 单独追加。旧 migrate_v2 时代 `user_version=2` 但
	/// `items` 没有 `files_json`，v1 时代也没有 `trusted_peers` / `peer_rules`，
	/// 更早的 `content_cache` 没有 `durable`——只要关键表或关键列缺失就视为
	/// 无法迁移的旧库/损坏库。
	fn schema_core_ready(conn: &Connection) -> anyhow::Result<bool> {
		for table in [
			"items",
			"history",
			"content_cache",
			"trusted_peers",
			"peer_rules",
		] {
			let exists: i64 = conn.query_row(
				"SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
				[table],
				|r| r.get(0),
			)?;
			if exists == 0 {
				return Ok(false);
			}
		}

		// 关键列指纹：旧 v2 没有 files_json；本代 v2 已删除 expires_ts_ms。
		// 这样即使某个中间版本也写过 user_version=2，最终代码仍会重建它。
		let mut stmt = conn.prepare("PRAGMA table_info(items)")?;
		let mut rows = stmt.query([])?;
		let mut has_files_json = false;
		let mut has_expires = false;
		while let Some(row) = rows.next()? {
			let name: String = row.get(1)?;
			match name.as_str() {
				"files_json" => has_files_json = true,
				"expires_ts_ms" => has_expires = true,
				_ => {}
			}
		}
		if !has_files_json || has_expires {
			return Ok(false);
		}

		// v3 列指纹：内容表的 durable 标记必须存在。
		let has_durable = conn
			.prepare("PRAGMA table_info(content_cache)")?
			.query_map([], |r| r.get::<_, String>(1))?
			.collect::<rusqlite::Result<Vec<_>>>()?
			.iter()
			.any(|c| c == "durable");
		if !has_durable {
			return Ok(false);
		}

		// 外键指纹：v2 起要求 history.item_id -> items.item_id ON DELETE CASCADE。
		let fk_count: i64 = conn.query_row(
			r#"SELECT COUNT(*) FROM pragma_foreign_key_list('history')
               WHERE "table"='items' AND "from"='item_id' AND on_delete='CASCADE'"#,
			[],
			|r| r.get(0),
		)?;
		Ok(fk_count > 0)
	}

	fn kind_to_str(k: &ItemKind) -> &'static str {
		match k {
			ItemKind::Text => "text",
			ItemKind::Image => "image",
			ItemKind::FileList => "file_list",
		}
	}

	/// 向数据库插入元数据和历史信息。
	///
	/// 此函数在单个事务中执行以下操作：
	/// 1. 如果内容元数据在 `content_cache` 表中尚不存在，则插入该元数据。
	/// 2. 向 `items` 表插入项的元数据，包括项 ID、类型、所有者设备、时间戳、大小、
	///    MIME 类型，以及序列化后的预览和文件列表 JSON 数据。
	/// 3. 向 `history` 表追加一条历史记录，通过使用 `INSERT OR IGNORE` 确保操作幂等。
	/// 4. 从 `content_cache` 表中获取所提供元数据的 `present`（是否存在）状态。
	///
	/// # 参数
	/// * `account_uid` - 一个字符串切片，表示拥有该项的账号用户 ID。
	/// * `meta` - 指向包含该项元数据的 `ItemMeta` 结构的引用。
	/// * `now_ms` - 当前时间戳（毫秒）。
	///
	/// # 返回值
	/// 返回一个包含 `CacheRow` 结构的 `Result`，该结构指示该项是否已存在于缓存中；
	/// 如果操作失败，则返回错误。
	///
	/// # 错误
	/// 如果发生以下任何操作失败，将返回 `anyhow::Error`：
	/// - 数据库事务创建失败。
	/// - 执行 SQL 插入语句失败。
	/// - `preview` 或 `files` 的 JSON 序列化失败。
	/// - 从 `content_cache` 获取 `present` 状态的 SQL 查询失败。
	///
	/// # SQL 细节
	/// - `content_cache` 表会更新内容信息，如 SHA-256 哈希、大小和时间戳。
	/// - `items` 表会更新每个项的详细元数据。
	/// - `history` 表记录特定账号的项历史，确保 `(account_uid, item_id)` 没有重复条目。
	///
	/// # 示例
	/// ```rust,ignore
	/// let mut database = Database::new(); // 假设 `Database` 是一个带有 `conn` 字段的结构体。
	/// let now_ms = 1684567890123; // 当前毫秒级时间戳。
	///
	/// let meta = ItemMeta {
	///     item_id: "item123".to_string(),
	///     kind: ItemKind::File,
	///     source_device_id: "device456".to_string(),
	///     created_ts_ms: 1684560000000,
	///     size_bytes: 1024,
	///     expires_ts_ms: 1687567890123,
	///     content: ContentMeta {
	///         sha256: "abcd1234".to_string(),
	///         total_bytes: 1024,
	///         mime: "application/pdf".to_string(),
	///     },
	///     preview: Some(Preview { thumbnail_url: "https://example.com/thumbnail.png".to_string() }),
	///     files: vec![File { name: "example.pdf".to_string(), size: 1024 }],
	/// };
	///
	/// let result = database.insert_meta_and_history("user789", &meta, now_ms);
	/// match result {
	///     Ok(cache_row) => println!("缓存中是否存在该项: {}", cache_row.present),
	///     Err(e) => eprintln!("插入元数据和历史记录失败: {}", e),
	/// }
	/// ```
	///
	/// # 备注
	/// - 此函数通过使用 `INSERT OR IGNORE` 确保 `content_cache` 和 `history` 条目的幂等性。
	/// - 历史记录中的 `sort_ts_ms` 被固定为创建时间戳 (`created_ts_ms`)。
	pub fn insert_meta_and_history(
		&mut self,
		account_uid: &str,
		meta: &ItemMeta,
		now_ms: i64,
	) -> anyhow::Result<CacheRow> {
		let tx = self.conn.transaction()?;

		tx.execute(
			r"INSERT OR IGNORE INTO content_cache
               (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
               VALUES (?1, ?2, 0, ?3, ?4)",
			params![
				meta.content.sha256,
				meta.content.total_bytes,
				now_ms,
				meta.created_ts_ms
			],
		)?;

		let preview_json = serde_json::to_string(&meta.preview)?;
		let files_json = serde_json::to_string(&meta.files)?;

		tx.execute(
            r"INSERT INTO items
               (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, preview_json, files_json)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                meta.item_id,
                Self::kind_to_str(&meta.kind),
                meta.source_device_id,
                meta.created_ts_ms,
                meta.size_bytes,
                meta.content.mime,
                meta.content.sha256,
                preview_json,
                files_json
            ],
        )?;

		// history：sort_ts_ms 固定用 created_ts_ms
		// 如果同 (account_uid, item_id) 已经存在，unique index 会挡住；这里用 OR IGNORE 保证幂等
		tx.execute(
			r"INSERT OR IGNORE INTO history(account_uid, item_id, sort_ts_ms, source_device_id)
               VALUES (?1, ?2, ?3, ?4)",
			params![
				account_uid,
				meta.item_id,
				meta.created_ts_ms,
				meta.source_device_id
			],
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
		Ok(CacheRow {
			present: present_i != 0,
		})
	}

	pub fn mark_cache_missing(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<usize> {
		let n = self.conn.execute(
			r"UPDATE content_cache
           SET present=0, last_access_ts_ms=?2
           WHERE sha256_hex=?1",
			params![sha256_hex, now_ms],
		)?;
		Ok(n)
	}

	/// 清空缓存实现：保留 `content_cache` 行，只把 `present` 置 0。
	///
	/// 不能直接 `DELETE FROM content_cache`：`items`/`history` 的读取都 INNER JOIN
	/// `content_cache`，删行会让用户的历史记录从所有查询里消失。
	pub fn mark_all_cache_missing(&mut self) -> anyhow::Result<usize> {
		let n = self
			.conn
			.execute("UPDATE content_cache SET present=0", [])?;
		Ok(n)
	}

	/// Phase C：CAS 写完后把 present=1，并 touch LRU。
	///
	/// 默认按 `durable=1` 处理（本地 ingest 路径与旧行为一致）。接收路径必须用
	/// [`Store::mark_cache_present_with_durability`] 明确告诉库这次是否还要后台
	/// `sync_data`，否则“已上报完成但尚未落盘”的窗口就无法被 sweep 发现。
	pub fn mark_cache_present(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<usize> {
		self.mark_cache_present_with_durability(sha256_hex, now_ms, true)
	}

	/// Phase C（接收路径）：CAS 写完、按 fsync 策略决定 durable 后，一并更新
	/// `present` / `durable` / LRU。
	///
	/// `durable=false` 表示：blob 已 rename 到最终路径（rename 成功后才调用），
	/// 但 `sync_data` 还没完成，行需要由后台任务或 30s sweep 补同步。
	pub fn mark_cache_present_with_durability(
		&mut self,
		sha256_hex: &str,
		now_ms: i64,
		durable: bool,
	) -> anyhow::Result<usize> {
		let n = self.conn.execute(
			r"UPDATE content_cache
           SET present=1, durable=?3, last_access_ts_ms=?2
           WHERE sha256_hex=?1",
			params![sha256_hex, now_ms, i32::from(durable)],
		)?;
		Ok(n)
	}

	/// 后台同步成功后把 durable 翻回 1。返回受影响行数（0 表示行已被删除）。
	pub fn mark_cache_durable(&mut self, sha256_hex: &str) -> anyhow::Result<usize> {
		let n = self.conn.execute(
			"UPDATE content_cache SET durable=1 WHERE sha256_hex=?1",
			params![sha256_hex],
		)?;
		Ok(n)
	}

	/// 读取 durable 标记（诊断 / 测试用；本列不进入 FFI 返回值）。
	pub fn get_cache_durable(&self, sha256_hex: &str) -> anyhow::Result<bool> {
		let v: i64 = self.conn.query_row(
			"SELECT durable FROM content_cache WHERE sha256_hex=?1",
			params![sha256_hex],
			|r| r.get(0),
		)?;
		Ok(v != 0)
	}

	/// 后台同步队列满/进程重启后，sweep 用这个查询找回所有待同步行。
	///
	/// 只返回 `present=1 AND durable=0`：`present=0` 表示内容已被 GC/删除，
	/// 没有 blob 可同步；若仍按 durable=0 处理会让 sweep 每 30 s 对同一批
	/// 已删除内容重复记警告。`present` 与 `durable` 由接收完成路径在同一条
	/// UPDATE 里写入，正常 pending 行必然 present=1。
	pub fn select_durability_pending(&self, limit: i64) -> anyhow::Result<Vec<String>> {
		let mut stmt = self.conn.prepare(
			r"
            SELECT sha256_hex
            FROM content_cache
            WHERE present=1 AND durable=0
            ORDER BY last_access_ts_ms ASC, sha256_hex ASC
            LIMIT ?1
            ",
		)?;
		let rows = stmt.query_map(params![limit], |r| r.get(0))?;
		let mut out = Vec::new();
		for it in rows {
			out.push(it?);
		}
		Ok(out)
	}

	/// 为 sweep 解析一个 `content_cache` 键可能对应的磁盘 blob 列表。
	///
	/// `content_cache` 以 **item 的** `content.sha256` 为键，但 FileList 传输的
	/// `ContentBegin.sha256` 是**单个文件**的 hash（见 actor.rs 的
	/// `mark_item_content_cached`），两者并不相等。因此 sweep 不能假设 blob 名
	/// 就是行主键：先取 item 自身 content sha 作为候选，再从 `files_json` 里
	/// 补上每个文件的 sha，逐个尝试 `sync_data`。返回空列表表示库里没有对应
	/// item（例如元数据已删除），调用方只记警告、保持 durable=0。
	pub fn sync_candidates_for_cache_sha(&self, sha256_hex: &str) -> anyhow::Result<Vec<String>> {
		let mut out = vec![sha256_hex.to_string()];
		let mut stmt = self
			.conn
			.prepare("SELECT files_json FROM items WHERE sha256_hex=?1")?;
		let mut rows = stmt.query(params![sha256_hex])?;
		while let Some(row) = rows.next()? {
			let files_json: Option<String> = row.get(0)?;
			let Some(files_json) = files_json else {
				continue;
			};
			let Ok(files) = serde_json::from_str::<Vec<FileMeta>>(&files_json) else {
				continue;
			};
			for f in files {
				if let Some(sha) = f.sha256 {
					if !sha.is_empty() && !out.iter().any(|s| s == &sha) {
						out.push(sha);
					}
				}
			}
		}
		Ok(out)
	}

	pub fn touch_cache(&mut self, sha256_hex: &str, now_ms: i64) -> anyhow::Result<usize> {
		let n = self.conn.execute(
			r"UPDATE content_cache
           SET last_access_ts_ms=?2
           WHERE sha256_hex=?1",
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

	/// 从具有指定账号 UID 的用户历史记录中获取项元数据列表。
	///
	/// 此函数查询数据库，以检索由给定 `account_uid` 标识的用户历史中存在的项元数据。
	/// 将数据库行映射为 ItemMeta（SELECT 列顺序需与此函数一致）。
	///
	/// 期望的列顺序：
	/// 0: item_id, 1: kind, 2: owner_device_id, 3: created_ts_ms,
	/// 4: size_bytes, 5: mime, 6: sha256_hex,
	/// 7: preview_json, 8: files_json, 9: total_bytes
	fn row_to_item_meta(r: &Row) -> rusqlite::Result<ItemMeta> {
		let kind_s: String = r.get(1)?;
		let kind = match kind_s.as_str() {
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

		let total_bytes: i64 = r.get(9)?;

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
			// 列已从 v2 DDL 删除；模型字段保留用于对外 JSON 形状，恒为 None。
			expires_ts_ms: None,
		})
	}

	/// 按 item_id 直查单条元数据（主键查询，O(1)）。
	pub fn get_item_meta_by_id(&self, item_id: &str) -> anyhow::Result<Option<ItemMeta>> {
		query_item_meta_by_id(&self.conn, item_id)
	}

	/// 查询受指定的数量限制 (`limit`)。返回的项按历史排序时间戳 (`h.sort_ts_ms`) 倒序排列，
	/// 如果时间戳相同，则按 `history_id` 倒序排列。
	///
	/// # 参数
	///
	/// - `account_uid`: 指向表示用户账号唯一标识符的字符串切片的引用。
	/// - `limit`: 要从历史记录中检索的项元数据的最大数量。
	///
	/// # 返回值
	///
	/// 成功时返回一个包裹在 `anyhow::Result` 中的 `ItemMeta` 对象向量，
	/// 如果在数据库查询或数据解析过程中发生任何失败，则返回错误。
	///
	/// `ItemMeta` 对象包含关于项的元数据，包括：
	/// - `item_id`: 项的唯一标识符。
	/// - `kind`: 项的类型（例如：`Text`, `Image`, `FileList`）。
	/// - `source_device_id`: 创建该项的设备 ID。
	/// - `source_device_name`: (可选) 源设备的名称（当前实现中始终为 `None`）。
	/// - `created_ts_ms`: 自 Unix 纪元以来的创建时间戳（毫秒）。
	/// - `size_bytes`: 项的大小（字节）。
	/// - `preview`: 表示项内容预览的数据结构。
	/// - `content`: 关联的 `ItemContent` 对象，包含 MIME 类型、SHA-256 哈希和总字节数等额外元数据。
	/// - `files`: 关联文件元数据的向量 (`FileMeta`)。
	/// - `expires_ts_ms`: 保留字段；v2 已删除该列，恒为 `None`。
	///
	/// # 数据库模式
	///
	/// 该查询通过连接三张表来获取数据：
	/// - `history`: 追踪项的历史引用。
	/// - `items`: 包含单个项的详细信息。
	/// - `content_cache`: 存储缓存内容的聚合元数据。
	///
	/// 仅包含与指定账号关联且未删除 (`h.is_deleted=0`) 的历史记录。
	/// 检索以下列：
	/// - `item_id`, `kind`, `owner_device_id`, `created_ts_ms`, `size_bytes`, `mime`, `sha256_hex`,
	///   `preview_json`, `files_json` 和 `total_bytes`。
	///
	/// # 内部处理
	///
	/// - 项的类型（`text`, `image`, `file_list`）被映射到 `ItemKind` 枚举。
	/// - 项预览 (`preview_json`) 和文件列表 (`files_json`) 的 JSON 字符串被解析为
	///   相应的数据结构（分别是 `ItemPreview` 和 `Vec<FileMeta>`）。
	/// - 任何 JSON 解析失败都会优雅地回退到默认值。
	///
	/// # 错误
	///
	/// 在以下任何情况下，此函数都会返回错误：
	/// - 准备或执行 SQL 查询失败。
	/// - 数据库行中缺失或包含无效数据。
	/// - 解析 `preview_json` 或 `files_json` 时发生 JSON 错误。
	///
	/// # 示例
	///
	/// ```rust,ignore
	/// let account_uid = "user123";
	/// let limit = 10;
	/// match db.list_history_metas(account_uid, limit) {
	///     Ok(item_metas) => {
	///         for item_meta in item_metas {
	///             println!("{:?}", item_meta);
	///         }
	///     }
	///     Err(err) => eprintln!("获取历史元数据失败: {:?}", err),
	/// }
	/// ```
	pub fn list_history_metas(
		&self,
		account_uid: &str,
		limit: usize,
	) -> anyhow::Result<Vec<ItemMeta>> {
		query_list_history_metas(&self.conn, account_uid, limit)
	}

	/// 游标分页查询历史元数据。
	///
	/// 当 `cursor` 为 `Some(ts)` 时，只返回 `sort_ts_ms < ts` 的记录（即比游标更旧的）；
	/// 当 `cursor` 为 `None` 时，从最新记录开始返回（等价于无 cursor 的首页查询）。
	pub fn list_history_metas_paged(
		&self,
		account_uid: &str,
		limit: usize,
		cursor: Option<i64>,
	) -> anyhow::Result<Vec<ItemMeta>> {
		query_list_history_metas_paged(&self.conn, account_uid, limit, cursor)
	}

	/// History GC：只保留最新 keep_latest 条，其余软删除
	pub fn soft_delete_history_keep_latest(
		&mut self,
		account_uid: &str,
		keep_latest: i64,
	) -> anyhow::Result<i64> {
		if keep_latest < 0 {
			return Ok(0);
		}
		// execute() 返回本条 UPDATE 实际影响的行数（changes() 只反映最近一条语句，
		// 用前后差值相减会下溢 panic）。
		let affected = self.conn.execute(
			r"
            UPDATE history
            SET is_deleted=1
            WHERE history_id IN (
                SELECT history_id
                FROM history
                WHERE account_uid=?1 AND is_deleted=0
                ORDER BY sort_ts_ms DESC, history_id DESC
                LIMIT -1 OFFSET ?2
            )
            ",
			params![account_uid, keep_latest],
		)?;
		Ok(affected as i64)
	}

	/// 软删除指定 item 的 history 记录
	pub fn soft_delete_item(&mut self, account_uid: &str, item_id: &str) -> anyhow::Result<bool> {
		let n = self.conn.execute(
			"UPDATE history SET is_deleted=1 WHERE account_uid=?1 AND item_id=?2",
			params![account_uid, item_id],
		)?;
		Ok(n > 0)
	}

	/// F12：账号的历史总行数（含已软删除的行）。
	///
	/// 维护路径只做这一条 `COUNT(*)`（走 `(account_uid, sort_ts_ms)` 索引前缀），
	/// 不再额外叠加 SUM/COUNT，避免在每次 ingest 后的 GC 里多付一次扫描。
	pub fn history_total_count_for_account(&self, account_uid: &str) -> anyhow::Result<i64> {
		let total: i64 = self.conn.query_row(
			"SELECT COUNT(*) FROM history WHERE account_uid=?1",
			params![account_uid],
			|r| r.get(0),
		)?;
		Ok(total)
	}

	/// F12：硬删除账号最旧的 `excess` 行历史（维护路径专用）。
	///
	/// 只按 `sort_ts_ms`/`history_id` 删除最旧的行；用户发起的删除仍然是
	/// [`Store::soft_delete_item`] 的软删除，不会被这里的维护逻辑替代。
	/// `history.item_id` 的外键 `ON DELETE CASCADE` 只影响 items 硬删除，本函数
	/// 删的是子表行，不触碰 `items` / `content_cache`。
	pub fn purge_history_oldest_excess(
		&mut self,
		account_uid: &str,
		excess: i64,
	) -> anyhow::Result<i64> {
		if excess <= 0 {
			return Ok(0);
		}
		let deleted = self.conn.execute(
			r"
            DELETE FROM history
            WHERE history_id IN (
                SELECT history_id
                FROM history
                WHERE account_uid=?1
                ORDER BY sort_ts_ms ASC, history_id ASC
                LIMIT ?2
            )
            ",
			params![account_uid, excess],
		)?;
		Ok(deleted as i64)
	}

	/// F12：把账号的历史行压到最新 `keep_latest` 行以内，返回硬删除的行数。
	///
	/// 保留策略是按行数（count）的硬删除：先数总行数，超出部分交给
	/// [`Store::purge_history_oldest_excess`]。`keep_latest < 0` 表示不限制；
	/// `keep_latest == 0` 删除该账号全部历史行（测试/显式清空用）。
	pub fn purge_history_beyond_retention(
		&mut self,
		account_uid: &str,
		keep_latest: i64,
	) -> anyhow::Result<i64> {
		if keep_latest < 0 {
			return Ok(0);
		}
		let total = self.history_total_count_for_account(account_uid)?;
		self.purge_history_oldest_excess(account_uid, total.saturating_sub(keep_latest))
	}

	/// Cache GC：挑 LRU（present=1）最旧的若干条
	pub fn select_lru_present(&self, limit: i64) -> anyhow::Result<Vec<(String, i64)>> {
		let mut stmt = self.conn.prepare(
			r"
            SELECT sha256_hex, total_bytes
            FROM content_cache
            WHERE present=1
            ORDER BY last_access_ts_ms ASC, sha256_hex ASC
            LIMIT ?1
            ",
		)?;
		let rows = stmt.query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
		let mut out = vec![];
		for it in rows {
			out.push(it?);
		}
		Ok(out)
	}

	pub fn sum_present_bytes(&self) -> anyhow::Result<i64> {
		query_sum_present_bytes(&self.conn)
	}

	/// 缓存统计：`present=1` 的 (created_ts_ms, total_bytes)，按时间升序。
	///
	/// 只返回“声称仍存在”的行；`present=0` 的内容已被 GC/删除，不算当前占用。
	pub fn list_present_cache_rows(&self) -> anyhow::Result<Vec<(i64, i64)>> {
		query_list_present_cache_rows(&self.conn)
	}

	/// 启动对账：列出所有声称 `present=1` 的 sha256。
	///
	/// 调用方再逐个检查 blob 是否存在，缺文件的用 [`Store::mark_cache_missing`] 改回 0；
	/// 只遍历“声称存在”的行，不扫描整个 CAS 目录。
	pub fn list_present_cache_hashes(&self) -> anyhow::Result<Vec<String>> {
		let mut stmt = self.conn.prepare(
			r"
            SELECT sha256_hex
            FROM content_cache
            WHERE present=1
            ORDER BY sha256_hex ASC
            ",
		)?;
		let rows = stmt.query_map([], |r| r.get(0))?;
		let mut out = Vec::new();
		for row in rows {
			out.push(row?);
		}
		Ok(out)
	}

	/// 活动统计：把 `history` 行按 `bucket_ms` 归桶，统计各 kind 的条目数。
	///
	/// 用 `history.sort_ts_ms`（入历史的时间）而不是 `items.created_ts_ms`：
	/// 远端条目可能由对端更早创建，但“本机何时看到它”才是本机活动。
	/// 已软删除的行也计入——活动确实发生过，图表反映的是历史流量而非当前列表。
	pub fn activity_buckets(
		&self,
		start_ts_ms: i64,
		end_ts_ms: i64,
		bucket_ms: i64,
	) -> anyhow::Result<Vec<ActivityBucket>> {
		query_activity_buckets(&self.conn, start_ts_ms, end_ts_ms, bucket_ms)
	}

	/// 获取已保存的设备指纹
	pub fn get_peer_fingerprint(
		&self,
		account_uid: &str,
		device_id: &str,
	) -> anyhow::Result<Option<String>> {
		let res: Option<String> = self.conn.query_row(
            "SELECT fingerprint_sha256 FROM trusted_peers WHERE account_uid=?1 AND device_id=?2",
            params![account_uid, device_id],
            |r| r.get(0),
        ).optional()?;
		Ok(res)
	}

	/// 保存/更新设备指纹 (TOFU pinning)
	pub fn save_peer_fingerprint(
		&mut self,
		account_uid: &str,
		device_id: &str,
		fingerprint: &str,
		now_ms: i64,
	) -> anyhow::Result<()> {
		self.conn.execute(
            "INSERT OR REPLACE INTO trusted_peers (account_uid, device_id, fingerprint_sha256, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![account_uid, device_id, fingerprint, now_ms]
        )?;
		Ok(())
	}

	/// 删除设备指纹（用于重新配对）
	pub fn delete_peer_fingerprint(
		&mut self,
		account_uid: &str,
		device_id: &str,
	) -> anyhow::Result<()> {
		self.conn.execute(
			"DELETE FROM trusted_peers WHERE account_uid=?1 AND device_id=?2",
			params![account_uid, device_id],
		)?;
		Ok(())
	}

	/// 获取或创建默认的 PeerRule（如果不存在则创建默认值）
	pub fn get_or_create_peer_rule(
		&mut self,
		account_uid: &str,
		device_id: &str,
		now_ms: i64,
	) -> anyhow::Result<PeerRule> {
		// 先尝试查询
		let existing: Option<(bool, bool, i64)> = self.conn.query_row(
            "SELECT share_to_peer, accept_from_peer, updated_at_ms FROM peer_rules WHERE account_uid=?1 AND device_id=?2",
            params![account_uid, device_id],
            |r| Ok((r.get::<_, i32>(0)? != 0, r.get::<_, i32>(1)? != 0, r.get(2)?)),
        ).optional()?;

		if let Some((share_to, accept_from, updated_at)) = existing {
			Ok(PeerRule {
				account_uid: account_uid.to_string(),
				device_id: device_id.to_string(),
				share_to_peer: share_to,
				accept_from_peer: accept_from,
				updated_at_ms: updated_at,
			})
		} else {
			// 创建默认规则（默认都允许）
			let rule = PeerRule {
				account_uid: account_uid.to_string(),
				device_id: device_id.to_string(),
				share_to_peer: true,
				accept_from_peer: true,
				updated_at_ms: now_ms,
			};
			self.upsert_peer_rule(&rule)?;
			Ok(rule)
		}
	}

	/// 更新或插入 PeerRule
	pub fn upsert_peer_rule(&mut self, rule: &PeerRule) -> anyhow::Result<()> {
		self.conn.execute(
            "INSERT OR REPLACE INTO peer_rules (account_uid, device_id, share_to_peer, accept_from_peer, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                rule.account_uid,
                rule.device_id,
                i32::from(rule.share_to_peer),
                i32::from(rule.accept_from_peer),
                rule.updated_at_ms
            ]
        )?;
		Ok(())
	}

	/// 获取 PeerRule（如果不存在返回 None）
	pub fn get_peer_rule(
		&self,
		account_uid: &str,
		device_id: &str,
	) -> anyhow::Result<Option<PeerRule>> {
		let res: Option<(bool, bool, i64)> = self.conn.query_row(
            "SELECT share_to_peer, accept_from_peer, updated_at_ms FROM peer_rules WHERE account_uid=?1 AND device_id=?2",
            params![account_uid, device_id],
            |r| Ok((r.get::<_, i32>(0)? != 0, r.get::<_, i32>(1)? != 0, r.get(2)?)),
        ).optional()?;

		Ok(res.map(|(share_to, accept_from, updated_at)| PeerRule {
			account_uid: account_uid.to_string(),
			device_id: device_id.to_string(),
			share_to_peer: share_to,
			accept_from_peer: accept_from,
			updated_at_ms: updated_at,
		}))
	}

	/// 列出所有 PeerRule（用于调试或迁移）
	pub fn list_peer_rules(&self, account_uid: &str) -> anyhow::Result<Vec<PeerRule>> {
		let mut stmt = self.conn.prepare(
            "SELECT device_id, share_to_peer, accept_from_peer, updated_at_ms FROM peer_rules WHERE account_uid=?1"
        )?;
		let rows = stmt.query_map([account_uid], |r| {
			Ok(PeerRule {
				account_uid: account_uid.to_string(),
				device_id: r.get(0)?,
				share_to_peer: r.get::<_, i32>(1)? != 0,
				accept_from_peer: r.get::<_, i32>(2)? != 0,
				updated_at_ms: r.get(3)?,
			})
		})?;

		let mut rules = Vec::new();
		for row in rows {
			rules.push(row?);
		}
		Ok(rules)
	}

	// [新增] M2: 处理远端广播来的元数据
	// 返回 true 表示这是新数据（需要触发事件），false 表示已存在（忽略）
	pub fn insert_remote_item(
		&mut self,
		account_uid: &str,
		meta: &ItemMeta,
		now_ms: i64,
	) -> anyhow::Result<bool> {
		// 安全（F1）：远端 ItemMeta 是不可信输入。`files[].local_path` 一旦入库，
		// 对端只要再发 `ContentGet{item_id,file_id}` 就能让本机把任意本地文件
		// 当作内容流回去（UNC 路径还会触发 outbound SMB 认证）。因此入库前
		// 必须剥离，且明确记录对端尝试注入的 hostile input（不记录路径本身，
		// 避免攻击者控制的内容污染日志）。
		let mut sanitized_meta = meta.clone();
		let stripped_local_paths = sanitized_meta.strip_local_paths();
		if stripped_local_paths > 0 {
			// 只记计数：此时 item_id 尚未通过 F12 校验，可能是超长/控制字符，
			// 绝不能写进日志。
			eprintln!(
				"[Store] WARNING: dropped {stripped_local_paths} peer-supplied local_path value(s) from remote metadata (untrusted input)"
			);
		}
		let meta = &sanitized_meta;

		// 安全（F12）：纵深防御。入站 actor 已经先校验过一次，这里再校验一次，
		// 保证任何直接调用 `insert_remote_item` 的路径（未来的转发/重放代码）
		// 也无法把超长/带路径分隔符的 peer-controlled 元数据写进库。
		// 错误信息只带长度/计数，不拼接对端字符串。
		if let Err(violation) = validate_inbound_item_meta(meta) {
			anyhow::bail!(
				"remote ItemMeta rejected by metadata limits: reason={}, item_id_len={}, device_id_len={}, files={}, max_rel_name_len={}, preview_bytes={}, total_meta_bytes={}",
				violation.kind.as_str(),
				violation.item_id_len,
				violation.device_id_len,
				violation.files,
				violation.max_rel_name_len,
				violation.preview_bytes,
				violation.total_meta_bytes
			);
		}

		let tx = self.conn.transaction()?;

		// 1. content_cache: 远端来的默认 present=0 (Lazy Fetch)
		// 如果本地已存在(可能曾经下载过)，IGNORE 会保留原有状态
		tx.execute(
			r"INSERT OR IGNORE INTO content_cache
               (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
               VALUES (?1, ?2, 0, ?3, ?4)",
			params![
				meta.content.sha256,
				meta.content.total_bytes,
				now_ms,
				meta.created_ts_ms
			],
		)?;

		// 2. items: 插入元数据
		let preview_json = serde_json::to_string(&meta.preview)?;
		let files_json = serde_json::to_string(&meta.files)?;

		tx.execute(
            r"INSERT OR IGNORE INTO items
               (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, preview_json, files_json)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                meta.item_id,
                Self::kind_to_str(&meta.kind),
                meta.source_device_id,
                meta.created_ts_ms,
                meta.size_bytes,
                meta.content.mime,
                meta.content.sha256,
                preview_json,
                files_json
            ],
        )?;

		// 3. history: 插入历史
		// 使用 INSERT OR IGNORE。如果 (account_uid, item_id) 已存在，则不执行插入，changes() 为 0
		let changes = tx.execute(
			r"INSERT OR IGNORE INTO history(account_uid, item_id, sort_ts_ms, source_device_id)
               VALUES (?1, ?2, ?3, ?4)",
			params![
				account_uid,
				meta.item_id,
				meta.created_ts_ms,
				meta.source_device_id
			],
		)?;

		tx.commit()?;

		// 如果 history 表发生了写入，说明这是一条新记录
		Ok(changes > 0)
	}

	/// M3: 获取 item 的 owner (用于 NetManager 路由)
	pub fn get_item_owner(&self, item_id: &str) -> anyhow::Result<Option<String>> {
		let mut stmt = self
			.conn
			.prepare("SELECT owner_device_id FROM items WHERE item_id = ?")?;
		let mut rows = stmt.query([item_id])?;
		if let Some(row) = rows.next()? {
			let owner: String = row.get(0)?;
			Ok(Some(owner))
		} else {
			Ok(None)
		}
	}

	/// M3: 获取 item 的 sha256 (用于 Session 发送文件)
	pub fn get_item_sha256(&self, item_id: &str) -> anyhow::Result<Option<String>> {
		let mut stmt = self
			.conn
			.prepare("SELECT sha256_hex FROM items WHERE item_id = ?")?;
		let mut rows = stmt.query([item_id])?;
		if let Some(row) = rows.next()? {
			let sha: String = row.get(0)?;
			Ok(Some(sha))
		} else {
			Ok(None)
		}
	}

	/// M3: 获取 item 的 mime (用于 Session 发送 ContentBegin)
	pub fn get_item_mime(&self, item_id: &str) -> anyhow::Result<Option<String>> {
		let mut stmt = self
			.conn
			.prepare("SELECT mime FROM items WHERE item_id = ?")?;
		let mut rows = stmt.query([item_id])?;
		if let Some(row) = rows.next()? {
			let mime: String = row.get(0)?;
			Ok(Some(mime))
		} else {
			Ok(None)
		}
	}

	/// M3-3: 获取 FileList 中指定 file_id 的详细信息 (sha256, rel_name, size)
	pub fn get_file_meta(&self, item_id: &str, file_id: &str) -> anyhow::Result<Option<FileMeta>> {
		let mut stmt = self
			.conn
			.prepare("SELECT files_json FROM items WHERE item_id = ?")?;
		let mut rows = stmt.query([item_id])?;

		if let Some(row) = rows.next()? {
			let json_str: Option<String> = row.get(0)?;
			if let Some(s) = json_str {
				let files: Vec<FileMeta> = serde_json::from_str(&s).unwrap_or_default();
				// 找到对应的 file_id
				let target = files.into_iter().find(|f| f.file_id == file_id);
				return Ok(target);
			}
		}
		Ok(None)
	}

	/// 返回**所有仍可能被引用的内容 sha256**（F5.2 孤儿 blob 清扫的判据）。
	///
	/// 并集：
	/// - `content_cache.sha256_hex`（含 `present=0` 的历史行——行还在就说明这个
	///   sha 曾被登记过，清扫阶段保守保留）；
	/// - `items.sha256_hex`（Text/Image 内容、FileList manifest）；
	/// - `items.files_json[].sha256`（FileList 子文件；这些 blob 没有独立的
	///   `content_cache` 行，只看 content_cache 会把合法文件误判成孤儿）。
	///
	/// 注释里虽然以“content_cache 行”为审计口径，但只用它做判据会误删
	/// FileList 子文件，因此这里把 `items` 的显式引用一并纳入。
	pub fn list_referenced_content_hashes(&self) -> anyhow::Result<HashSet<String>> {
		let mut out = HashSet::new();

		{
			let mut stmt = self.conn.prepare("SELECT sha256_hex FROM content_cache")?;
			let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
			for row in rows {
				let sha = row?;
				if !sha.is_empty() {
					out.insert(sha);
				}
			}
		}

		{
			let mut stmt = self
				.conn
				.prepare("SELECT sha256_hex, files_json FROM items")?;
			let mut rows = stmt.query([])?;
			while let Some(row) = rows.next()? {
				let item_sha: String = row.get(0)?;
				if !item_sha.is_empty() {
					out.insert(item_sha);
				}
				let files_json: Option<String> = row.get(1)?;
				if let Some(json) = files_json {
					if let Ok(files) = serde_json::from_str::<Vec<FileMeta>>(&json) {
						for file in files {
							if let Some(sha) = file.sha256.filter(|sha| !sha.is_empty()) {
								out.insert(sha);
							}
						}
					}
				}
			}
		}

		Ok(out)
	}

	/// F8：除 `exclude_item_id` 外，是否还有**未删除**的 item 引用该 sha。
	///
	/// “未删除”以 `history` 为准（items 本身只做软删除，行不会被移除）：
	/// - 完全没有 history 行的 item 视为仍存在（保守保留 blob）；
	/// - 有 history 行且至少一行 `is_deleted=0` 的 item 视为未删除；
	/// - 有 history 行但全部已删除的 item 不算引用。
	///
	/// 被检查的引用包括 item 自身的 `sha256_hex` 与 FileList 的
	/// `files_json[].sha256`（同一份内容可能被多个 item 共享）。
	pub fn has_other_live_content_reference(
		&self,
		sha256_hex: &str,
		exclude_item_id: &str,
	) -> anyhow::Result<bool> {
		if sha256_hex.is_empty() {
			return Ok(false);
		}
		let mut stmt = self.conn.prepare(
			r"
            SELECT i.sha256_hex, i.files_json
            FROM items i
            WHERE i.item_id <> ?1
              AND NOT (
                  EXISTS (SELECT 1 FROM history h WHERE h.item_id = i.item_id)
                  AND NOT EXISTS (
                      SELECT 1 FROM history h
                      WHERE h.item_id = i.item_id AND h.is_deleted = 0
                  )
              )
            ",
		)?;
		let mut rows = stmt.query(params![exclude_item_id])?;
		while let Some(row) = rows.next()? {
			let item_sha: String = row.get(0)?;
			if item_sha == sha256_hex {
				return Ok(true);
			}
			let files_json: Option<String> = row.get(1)?;
			if let Some(json) = files_json {
				if let Ok(files) = serde_json::from_str::<Vec<FileMeta>>(&json) {
					if files
						.iter()
						.any(|file| file.sha256.as_deref() == Some(sha256_hex))
					{
						return Ok(true);
					}
				}
			}
		}
		Ok(false)
	}

	/// 清空核心数据库的所有表
	pub fn clear_core_db(&mut self) -> anyhow::Result<()> {
		let tx = self.conn.transaction()?;
		tx.execute("DELETE FROM history", [])?;
		tx.execute("DELETE FROM items", [])?;
		tx.execute("DELETE FROM content_cache", [])?;
		tx.execute("DELETE FROM trusted_peers", [])?;
		tx.execute("DELETE FROM peer_rules", [])?;
		tx.commit()?;
		Ok(())
	}

	/// P3：删除**没有任何 `history` 行引用**的孤儿 `items` 行（单次最多 `limit` 行）。
	///
	/// 只按“是否存在 history 行”判定，不看 `is_deleted`：显式软删除的行仍保留，
	/// 只有历史保留策略（[`Store::purge_history_oldest_excess`]）真正硬删掉历史行
	/// 之后，对应 items 才会变成孤儿。`items` 只被 `history` 引用（FileList 子文件
	/// 是 `files_json` 内容而不是独立表行），因此不存在“其它 live 引用”需要额外检查。
	///
	/// 返回删除的行数；有 `limit` 上界，避免一次维护窗口做无界删除。
	pub fn compact_orphan_items(&mut self, limit: i64) -> anyhow::Result<i64> {
		if limit <= 0 {
			return Ok(0);
		}
		let deleted = self.conn.execute(
			r"
            DELETE FROM items
            WHERE item_id IN (
                SELECT i.item_id
                FROM items i
                WHERE NOT EXISTS (SELECT 1 FROM history h WHERE h.item_id = i.item_id)
                LIMIT ?1
            )
            ",
			params![limit],
		)?;
		Ok(deleted as i64)
	}

	/// P3：删除“`present=0` 且没有任何 `items` 引用”的 `content_cache` 行。
	///
	/// 引用判定覆盖两类（与 [`Store::list_referenced_content_hashes`] 同口径）：
	/// - `items.sha256_hex`（Text/Image 内容或 FileList manifest 本体）；
	/// - `items.files_json[].sha256`（FileList 子文件；它们没有独立 cache 行，
	///   但共享同一份 blob）。
	///
	/// 实现上**不把全库引用集合读进内存**：先按 `limit` 取一批候选（`present=0`
	/// 且无 `sha256_hex` 引用，最多 512 个），候选集本身就是内存上界；再流式扫描
	/// `items.files_json`（逐行处理，`"[]"` 直接跳过），命中即从候选集移除。
	/// 最后只删除剩下的（确实无任何引用）行。因此内存是 O(limit)，不随库增长。
	///
	/// 调用方应先跑 [`Store::compact_orphan_items`]，否则刚变成孤儿的 items
	/// （下一批才会被删）会保守地保住它们的 cache 行——安全但会多留一轮。
	pub fn compact_unreferenced_cache_rows(&mut self, limit: i64) -> anyhow::Result<i64> {
		if limit <= 0 {
			return Ok(0);
		}
		let mut candidates = {
			let mut stmt = self.conn.prepare(
				r"
                SELECT cc.sha256_hex
                FROM content_cache cc
                WHERE cc.present = 0
                  AND NOT EXISTS (SELECT 1 FROM items i WHERE i.sha256_hex = cc.sha256_hex)
                LIMIT ?1
                ",
			)?;
			let rows = stmt.query_map(params![limit], |r| r.get::<_, String>(0))?;
			let mut out = Vec::new();
			for row in rows {
				out.push(row?);
			}
			out
		};
		if candidates.is_empty() {
			return Ok(0);
		}

		// 候选集是硬上界（<= limit）；files_json 逐行流式解析，不建全库集合。
		let mut unreferenced: HashSet<String> = candidates.drain(..).collect();
		{
			let mut stmt = self
				.conn
				.prepare("SELECT files_json FROM items WHERE files_json IS NOT NULL")?;
			let mut rows = stmt.query([])?;
			while let Some(row) = rows.next()? {
				if unreferenced.is_empty() {
					break;
				}
				let files_json: String = row.get(0)?;
				// 绝大多数 text/image 行是 "[]"：无需 JSON 解析。
				if files_json.len() <= 2 {
					continue;
				}
				let Ok(files) = serde_json::from_str::<Vec<FileMeta>>(&files_json) else {
					// 解析失败按“可能有引用”保守处理：跳过本行，候选行保留。
					continue;
				};
				for file in files {
					if let Some(sha) = file.sha256.filter(|sha| !sha.is_empty()) {
						unreferenced.remove(&sha);
					}
				}
			}
		}

		if unreferenced.is_empty() {
			return Ok(0);
		}
		let tx = self.conn.transaction()?;
		let mut deleted = 0usize;
		{
			// `present=0` 再校验一次：只删“已不在缓存里”的行。
			let mut stmt =
				tx.prepare("DELETE FROM content_cache WHERE sha256_hex=?1 AND present=0")?;
			for sha in &unreferenced {
				deleted += stmt.execute(params![sha])?;
			}
		}
		tx.commit()?;
		Ok(deleted as i64)
	}

	/// P3：把 `freelist` 上最多 `max_pages` 页归还给文件系统
	/// （`PRAGMA incremental_vacuum(N)`），返回实际回收页数。
	///
	/// 只有数据库处于 `auto_vacuum=INCREMENTAL` 且 freelist 非空时才有行返回；
	/// 每个返回行对应回收的一页，因此行数就是回收页数（不依赖 pragma 的列值，
	/// 该 pragma 的结果列数在不同构建上并不稳定）。非 auto-vacuum 库是 no-op。
	pub fn incremental_vacuum(&mut self, max_pages: i64) -> anyhow::Result<i64> {
		if max_pages <= 0 {
			return Ok(0);
		}
		// PRAGMA 的参数不支持绑定变量；max_pages 是调用方提供的 i64，无注入面。
		let mut stmt = self
			.conn
			.prepare(&format!("PRAGMA incremental_vacuum({max_pages})"))?;
		let mut rows = stmt.query([])?;
		let mut reclaimed = 0i64;
		while rows.next()?.is_some() {
			reclaimed += 1;
		}
		Ok(reclaimed)
	}
}

// ---------------------------------------------------------------------------
// Task B：只读连接池
//
// Task A 的实测（cb_core/tests/db_contention_probe.rs，release）：写线程按
// ~5 ms 节奏 insert_remote_item、读线程按 ~5 ms 节奏 list_history(100) 时，
// 共享 Mutex 的读线程 p99 ≈ 10.8–11.5 ms、max ≈ 16–87 ms（多次运行）—— 远高于
// “p99 < 5 ms 且 max < 20 ms”的无池阈值；同期 10k 行历史硬删除（单条 DELETE
// 约 43–87 ms）会让共享 Mutex 的读线程出现同样量级的停顿。尾延迟来源是写连接
// 在 WAL 自动 checkpoint/落盘期间长时间持有 `Mutex<Store>`：所有查询都排在
// 同一个互斥锁后面。修复后同一探针的读线程 p99 ≈ 1.0 ms、max 4.7–11.4 ms
// （单次系统抖动）；10k 行硬删除期间从 42.6–87.2 ms 降到 0.7–9.7 ms。
//
// WAL 本身允许读事务与写事务并发（且 PASSIVE checkpoint 不阻塞读快照），所以
// 把**查询**改到独立只读连接上即可消除这段排队；写路径继续使用 Store 的唯一
// 写连接，避免多写者/锁升级带来的复杂度。
// ---------------------------------------------------------------------------

/// 只读查询连接池的默认大小（任务要求 2–4）。
pub const READ_POOL_SIZE: usize = 2;

/// 查询专用的只读连接池。
///
/// 每个连接都带 `PRAGMA query_only=ON`，误用写语句只会得到 SQLite 错误而不
/// 会破坏数据。池内连接数固定（`size` 是硬上界），但连接是**惰性**打开的：
/// 构造池不付 SQLite open 的成本（避免拖慢 `Core::init`），首次查询时才打开，
/// 之后复用。借出用 `Condvar` 等待，不会无界增长；`ReadConnGuard` 保证正常返回
/// 与 panic 展开路径都把连接还回池里。
///
/// 内存：每条已打开连接沿用与写连接一致的读侧调优（16 MB 页缓存 + 64 MB
/// mmap 地址空间，后者由 OS 按需换页）。默认 2 条连接，实际按需打开。
pub struct StoreReader {
	db_path: PathBuf,
	size: usize,
	/// 已实际打开的连接数（<= `size`）；用于惰性创建与泄漏诊断。
	created: AtomicUsize,
	idle: Mutex<Vec<Connection>>,
	available: Condvar,
}

impl StoreReader {
	/// 构造默认大小（[`READ_POOL_SIZE`]）的只读连接池（惰性打开连接）。
	pub fn open(data_dir: impl AsRef<Path>) -> Self {
		Self::with_size(data_dir, READ_POOL_SIZE)
	}

	/// 构造指定大小的只读连接池（`size` 至少 1）。供测试/诊断使用。
	pub fn with_size(data_dir: impl AsRef<Path>, size: usize) -> Self {
		Self {
			db_path: data_dir.as_ref().join("core.db"),
			size: size.max(1),
			created: AtomicUsize::new(0),
			idle: Mutex::new(Vec::new()),
			available: Condvar::new(),
		}
	}

	/// 池内连接数硬上界。
	pub fn size(&self) -> usize {
		self.size
	}

	/// 已经实际打开的连接数（<= [`StoreReader::size`]）。
	pub fn created(&self) -> usize {
		self.created.load(Ordering::Acquire)
	}

	/// 借出一条只读连接：优先复用空闲连接，名额未满时惰性打开，否则等待归还。
	fn acquire(&self) -> anyhow::Result<ReadConnGuard<'_>> {
		let mut idle = self
			.idle
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		loop {
			if let Some(conn) = idle.pop() {
				return Ok(ReadConnGuard {
					reader: self,
					conn: Some(conn),
				});
			}

			let created = self.created.load(Ordering::Acquire);
			if created < self.size
				&& self
					.created
					.compare_exchange(created, created + 1, Ordering::AcqRel, Ordering::Acquire)
					.is_ok()
			{
				// 先释放池锁再打开 SQLite 连接：open 可能耗时数 ms，不能占着锁。
				drop(idle);
				return match open_read_only_connection(&self.db_path) {
					Ok(conn) => Ok(ReadConnGuard {
						reader: self,
						conn: Some(conn),
					}),
					Err(error) => {
						// 打开失败：退回名额并唤醒一个等待者重试/报错。
						self.created.fetch_sub(1, Ordering::AcqRel);
						self.available.notify_one();
						Err(error)
					}
				};
			}

			// 名额已满且没有空闲连接：等待借出方归还（或打开失败后让出名额）。
			idle = self
				.available
				.wait(idle)
				.unwrap_or_else(std::sync::PoisonError::into_inner);
		}
	}

	fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> anyhow::Result<T>) -> anyhow::Result<T> {
		let guard = self.acquire()?;
		f(guard.conn())
	}

	pub fn list_history_metas(
		&self,
		account_uid: &str,
		limit: usize,
	) -> anyhow::Result<Vec<ItemMeta>> {
		self.with_conn(|conn| query_list_history_metas(conn, account_uid, limit))
	}

	pub fn list_history_metas_paged(
		&self,
		account_uid: &str,
		limit: usize,
		cursor: Option<i64>,
	) -> anyhow::Result<Vec<ItemMeta>> {
		self.with_conn(|conn| query_list_history_metas_paged(conn, account_uid, limit, cursor))
	}

	pub fn get_item_meta_by_id(&self, item_id: &str) -> anyhow::Result<Option<ItemMeta>> {
		self.with_conn(|conn| query_item_meta_by_id(conn, item_id))
	}

	pub fn list_present_cache_rows(&self) -> anyhow::Result<Vec<(i64, i64)>> {
		self.with_conn(query_list_present_cache_rows)
	}

	pub fn sum_present_bytes(&self) -> anyhow::Result<i64> {
		self.with_conn(query_sum_present_bytes)
	}

	pub fn activity_buckets(
		&self,
		start_ts_ms: i64,
		end_ts_ms: i64,
		bucket_ms: i64,
	) -> anyhow::Result<Vec<ActivityBucket>> {
		self.with_conn(|conn| query_activity_buckets(conn, start_ts_ms, end_ts_ms, bucket_ms))
	}
}

/// 借出的只读连接；`Drop` 时归还池并唤醒一个等待者。
struct ReadConnGuard<'a> {
	reader: &'a StoreReader,
	conn: Option<Connection>,
}

impl ReadConnGuard<'_> {
	fn conn(&self) -> &Connection {
		self.conn.as_ref().expect("leased read connection")
	}
}

impl Drop for ReadConnGuard<'_> {
	fn drop(&mut self) {
		let Some(conn) = self.conn.take() else {
			return;
		};
		{
			let mut idle = self
				.reader
				.idle
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			idle.push(conn);
		}
		self.reader.available.notify_one();
	}
}

/// 打开一条查询专用连接：只读 + 与写连接一致的读侧调优。
fn open_read_only_connection(db_path: &Path) -> anyhow::Result<Connection> {
	let conn = Connection::open(db_path)?;
	conn.execute_batch(
		r"
        PRAGMA busy_timeout=5000;
        PRAGMA cache_size=-16000;
        PRAGMA mmap_size=67108864;
        PRAGMA temp_store=MEMORY;
        PRAGMA query_only=ON;
        ",
	)?;
	// 纵深防御：确认 query_only 真的生效，否则宁可打不开也不让查询连接可写。
	let query_only: i64 = conn.pragma_query_value(None, "query_only", |r| r.get(0))?;
	anyhow::ensure!(
		query_only == 1,
		"read-only connection did not accept PRAGMA query_only=ON"
	);
	Ok(conn)
}

// ---------------------------------------------------------------------------
// 查询实现（连接无关）：写连接 Store 与只读池 StoreReader 共用同一份 SQL，
// 避免两条路径的列顺序/JSON 解析漂移。
// ---------------------------------------------------------------------------

fn query_item_meta_by_id(conn: &Connection, item_id: &str) -> anyhow::Result<Option<ItemMeta>> {
	let mut stmt = conn.prepare(
		r"
        SELECT
            i.item_id, i.kind, i.owner_device_id, i.created_ts_ms,
            i.size_bytes, i.mime, i.sha256_hex,
            i.preview_json, i.files_json,
            cc.total_bytes
        FROM items i
        JOIN content_cache cc ON i.sha256_hex = cc.sha256_hex
        WHERE i.item_id = ?1
        ",
	)?;
	let result = stmt
		.query_row(params![item_id], Store::row_to_item_meta)
		.optional()?;
	Ok(result)
}

fn query_list_history_metas(
	conn: &Connection,
	account_uid: &str,
	limit: usize,
) -> anyhow::Result<Vec<ItemMeta>> {
	let mut stmt = conn.prepare(
		r"
        SELECT
            i.item_id, i.kind, i.owner_device_id, i.created_ts_ms,
            i.size_bytes, i.mime, i.sha256_hex,
            i.preview_json, i.files_json,
            cc.total_bytes
        FROM history h
        JOIN items i ON h.item_id = i.item_id
        JOIN content_cache cc ON i.sha256_hex = cc.sha256_hex
        WHERE h.account_uid=?1 AND h.is_deleted=0
        ORDER BY h.sort_ts_ms DESC, h.history_id DESC
        LIMIT ?2
        ",
	)?;

	let rows = stmt.query_map(params![account_uid, limit as i64], Store::row_to_item_meta)?;

	let mut out = Vec::new();
	for it in rows {
		out.push(it?);
	}
	Ok(out)
}

fn query_list_history_metas_paged(
	conn: &Connection,
	account_uid: &str,
	limit: usize,
	cursor: Option<i64>,
) -> anyhow::Result<Vec<ItemMeta>> {
	let (sql, has_cursor) = match cursor {
		Some(_) => (
			r"
            SELECT
                i.item_id, i.kind, i.owner_device_id, i.created_ts_ms,
                i.size_bytes, i.mime, i.sha256_hex,
                i.preview_json, i.files_json,
                cc.total_bytes
            FROM history h
            JOIN items i ON h.item_id = i.item_id
            JOIN content_cache cc ON i.sha256_hex = cc.sha256_hex
            WHERE h.account_uid=?1 AND h.is_deleted=0 AND h.sort_ts_ms < ?3
            ORDER BY h.sort_ts_ms DESC, h.history_id DESC
            LIMIT ?2
            ",
			true,
		),
		None => (
			r"
            SELECT
                i.item_id, i.kind, i.owner_device_id, i.created_ts_ms,
                i.size_bytes, i.mime, i.sha256_hex,
                i.preview_json, i.files_json,
                cc.total_bytes
            FROM history h
            JOIN items i ON h.item_id = i.item_id
            JOIN content_cache cc ON i.sha256_hex = cc.sha256_hex
            WHERE h.account_uid=?1 AND h.is_deleted=0
            ORDER BY h.sort_ts_ms DESC, h.history_id DESC
            LIMIT ?2
            ",
			false,
		),
	};

	let mut stmt = conn.prepare(sql)?;

	let rows = if has_cursor {
		stmt.query_map(
			params![account_uid, limit as i64, cursor.unwrap()],
			Store::row_to_item_meta,
		)?
	} else {
		stmt.query_map(params![account_uid, limit as i64], Store::row_to_item_meta)?
	};

	let mut out = Vec::new();
	for it in rows {
		out.push(it?);
	}
	Ok(out)
}

fn query_sum_present_bytes(conn: &Connection) -> anyhow::Result<i64> {
	let s: i64 = conn.query_row(
		"SELECT COALESCE(SUM(total_bytes),0) FROM content_cache WHERE present=1",
		[],
		|r| r.get(0),
	)?;
	Ok(s)
}

fn query_list_present_cache_rows(conn: &Connection) -> anyhow::Result<Vec<(i64, i64)>> {
	let mut stmt = conn.prepare(
		r"
        SELECT created_ts_ms, total_bytes
        FROM content_cache
        WHERE present=1
        ORDER BY created_ts_ms ASC, sha256_hex ASC
        ",
	)?;
	let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
	let mut out = Vec::new();
	for row in rows {
		out.push(row?);
	}
	Ok(out)
}

fn query_activity_buckets(
	conn: &Connection,
	start_ts_ms: i64,
	end_ts_ms: i64,
	bucket_ms: i64,
) -> anyhow::Result<Vec<ActivityBucket>> {
	let bucket_ms = bucket_ms.max(1);
	let mut stmt = conn.prepare(
		r"
        SELECT
            (h.sort_ts_ms / ?3) * ?3 AS bucket_ts,
            SUM(CASE WHEN i.kind='text' THEN 1 ELSE 0 END),
            SUM(CASE WHEN i.kind='image' THEN 1 ELSE 0 END),
            SUM(CASE WHEN i.kind='file_list' THEN 1 ELSE 0 END)
        FROM history h
        JOIN items i ON h.item_id = i.item_id
        WHERE h.sort_ts_ms >= ?1 AND h.sort_ts_ms <= ?2
        GROUP BY bucket_ts
        ORDER BY bucket_ts ASC
        ",
	)?;
	let rows = stmt.query_map(params![start_ts_ms, end_ts_ms, bucket_ms], |r| {
		Ok(ActivityBucket {
			ts_ms: r.get(0)?,
			text_count: r.get(1)?,
			image_count: r.get(2)?,
			file_list_count: r.get(3)?,
		})
	})?;
	let mut out = Vec::new();
	for row in rows {
		out.push(row?);
	}
	Ok(out)
}

#[cfg(test)]
mod schema_tests {
	use super::*;

	fn user_version(conn: &Connection) -> i64 {
		conn.pragma_query_value(None, "user_version", |r| r.get(0))
			.unwrap()
	}

	fn item_columns(conn: &Connection) -> Vec<String> {
		let mut stmt = conn.prepare("PRAGMA table_info(items)").unwrap();
		stmt.query_map([], |r| r.get::<_, String>(1))
			.unwrap()
			.map(Result::unwrap)
			.collect()
	}

	fn count_rows(conn: &Connection, table: &str) -> i64 {
		conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
			.unwrap()
	}

	fn index_exists(conn: &Connection, name: &str) -> bool {
		let n: i64 = conn
			.query_row(
				"SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
				params![name],
				|r| r.get(0),
			)
			.unwrap();
		n > 0
	}

	/// v3 的完整 DDL（测试专用）：用于构造“升级前”的旧库。
	///
	/// 与 v4 的 [`SCHEMA_DDL`] 唯一区别是没有 `idx_items_kind_created`——这正是
	/// v3 -> v4 迁移要补上的增量。
	const V3_SCHEMA_DDL: &str = r"
        CREATE TABLE items (
          item_id TEXT PRIMARY KEY,
          kind TEXT NOT NULL CHECK (kind IN ('text','image','file_list')),
          owner_device_id TEXT NOT NULL,
          created_ts_ms INTEGER NOT NULL,
          size_bytes INTEGER NOT NULL,
          mime TEXT NOT NULL,
          sha256_hex TEXT NOT NULL,
          preview_json TEXT,
          files_json TEXT
        );
        CREATE INDEX idx_items_created ON items(created_ts_ms);
        CREATE INDEX idx_items_sha256 ON items(sha256_hex);
        CREATE INDEX idx_items_owner_created ON items(owner_device_id, created_ts_ms);

        CREATE TABLE history (
          history_id INTEGER PRIMARY KEY AUTOINCREMENT,
          account_uid TEXT NOT NULL,
          item_id TEXT NOT NULL REFERENCES items(item_id) ON DELETE CASCADE,
          sort_ts_ms INTEGER NOT NULL,
          source_device_id TEXT,
          is_deleted INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX idx_history_account_sort ON history(account_uid, sort_ts_ms DESC);
        CREATE INDEX idx_history_item ON history(item_id);
        CREATE UNIQUE INDEX idx_history_account_item ON history(account_uid, item_id);

        CREATE TABLE content_cache (
          sha256_hex TEXT PRIMARY KEY,
          total_bytes INTEGER NOT NULL,
          present INTEGER NOT NULL,
          durable INTEGER NOT NULL DEFAULT 1,
          last_access_ts_ms INTEGER NOT NULL,
          created_ts_ms INTEGER NOT NULL
        );
        CREATE INDEX idx_cache_lru ON content_cache(present, last_access_ts_ms);
        CREATE INDEX idx_cache_size ON content_cache(present, total_bytes);
        CREATE INDEX idx_cache_durable ON content_cache(durable);

        CREATE TABLE trusted_peers (
            account_uid TEXT NOT NULL,
            device_id TEXT NOT NULL,
            fingerprint_sha256 TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY (account_uid, device_id)
        );

        CREATE TABLE peer_rules (
            account_uid TEXT NOT NULL,
            device_id TEXT NOT NULL,
            share_to_peer INTEGER NOT NULL DEFAULT 1,
            accept_from_peer INTEGER NOT NULL DEFAULT 1,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY (account_uid, device_id)
        );
    ";

	/// 建一个结构完整、版本号为 3、且每张表都有数据的旧库。
	fn create_v3_db(dir: &std::path::Path) {
		let conn = Connection::open(dir.join("core.db")).unwrap();
		conn.execute_batch(V3_SCHEMA_DDL).unwrap();
		conn.execute_batch(
			r#"
            INSERT INTO content_cache
                (sha256_hex, total_bytes, present, durable, last_access_ts_ms, created_ts_ms)
                VALUES ('sha-v3-a', 10, 1, 1, 100, 90),
                       ('sha-v3-b', 20, 0, 1, 200, 190);
            INSERT INTO items
                (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, preview_json, files_json)
                VALUES ('item-v3-a', 'text', 'dev-v3', 1000, 10, 'text/plain', 'sha-v3-a', '{"text":"hello-v3"}', '[]'),
                       ('item-v3-b', 'image', 'dev-v3', 2000, 20, 'image/png', 'sha-v3-b', '{}', '[]');
            INSERT INTO history(account_uid, item_id, sort_ts_ms, source_device_id)
                VALUES ('acct-v3', 'item-v3-a', 1000, 'dev-v3');
            INSERT INTO trusted_peers(account_uid, device_id, fingerprint_sha256, updated_at_ms)
                VALUES ('acct-v3', 'peer-v3', 'fingerprint-v3', 1234);
            INSERT INTO peer_rules(account_uid, device_id, share_to_peer, accept_from_peer, updated_at_ms)
                VALUES ('acct-v3', 'peer-v3', 0, 1, 1234);
            PRAGMA user_version = 3;
            "#,
		)
		.unwrap();
		drop(conn);
	}

	/// 建一个结构完整、版本号为 4（v3 结构 + `idx_items_kind_created`）的旧库，
	/// 用于验证 P3 的 v4 -> v5 auto-vacuum 迁移。
	fn create_v4_db(dir: &std::path::Path) {
		let conn = Connection::open(dir.join("core.db")).unwrap();
		conn.execute_batch(V3_SCHEMA_DDL).unwrap();
		conn.execute_batch(
			r#"
            CREATE INDEX idx_items_kind_created ON items(kind, created_ts_ms);
            INSERT INTO content_cache
                (sha256_hex, total_bytes, present, durable, last_access_ts_ms, created_ts_ms)
                VALUES ('sha-v4-a', 10, 1, 1, 100, 90),
                       ('sha-v4-b', 20, 0, 1, 200, 190);
            INSERT INTO items
                (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, preview_json, files_json)
                VALUES ('item-v4-a', 'text', 'dev-v4', 1000, 10, 'text/plain', 'sha-v4-a', '{"text":"hello-v4"}', '[]'),
                       ('item-v4-b', 'file_list', 'dev-v4', 2000, 20,
                        'application/x-clipbridge-filelist+json', 'sha-v4-b', '{}',
                        '[{"file_id":"f1","rel_name":"a.bin","size_bytes":5,"sha256":"child-sha-v4"}]');
            INSERT INTO history(account_uid, item_id, sort_ts_ms, source_device_id)
                VALUES ('acct-v4', 'item-v4-a', 1000, 'dev-v4'),
                       ('acct-v4', 'item-v4-b', 2000, 'dev-v4');
            INSERT INTO trusted_peers(account_uid, device_id, fingerprint_sha256, updated_at_ms)
                VALUES ('acct-v4', 'peer-v4', 'fingerprint-v4', 1234);
            INSERT INTO peer_rules(account_uid, device_id, share_to_peer, accept_from_peer, updated_at_ms)
                VALUES ('acct-v4', 'peer-v4', 1, 0, 1234);
            PRAGMA user_version = 4;
            "#,
		)
		.unwrap();
		drop(conn);
	}

	/// P3 核心用例：一个 v4 库（含行数据）打开后必须迁移到 v5、开启
	/// `auto_vacuum=INCREMENTAL`（2）并保留全部数据；再打开不会重复迁移或重建。
	#[test]
	fn v4_db_migrates_to_v5_sets_auto_vacuum_and_preserves_all_rows() {
		let dir = tempfile::tempdir().unwrap();
		create_v4_db(dir.path());

		let store = Store::open(dir.path()).unwrap();
		assert!(!store.rebuilt, "v4 -> v5 是迁移，不得 drop-and-rebuild");
		assert_eq!(user_version(&store.conn), 5);
		assert_eq!(
			store
				.conn
				.pragma_query_value::<i64, _>(None, "auto_vacuum", |r| r.get(0))
				.unwrap(),
			2,
			"one-time VACUUM after the v4 -> v5 migration must enable INCREMENTAL"
		);
		assert!(index_exists(&store.conn, "idx_items_kind_created"));
		assert_eq!(count_rows(&store.conn, "items"), 2);
		assert_eq!(count_rows(&store.conn, "content_cache"), 2);
		assert_eq!(count_rows(&store.conn, "history"), 2);
		assert_eq!(count_rows(&store.conn, "trusted_peers"), 1);
		assert_eq!(count_rows(&store.conn, "peer_rules"), 1);

		// 行内容（含 FileList 的 files_json 子文件 sha）必须原样保留。
		let (mime, files): (String, String) = store
			.conn
			.query_row(
				"SELECT mime, files_json FROM items WHERE item_id='item-v4-b'",
				[],
				|r| Ok((r.get(0)?, r.get(1)?)),
			)
			.unwrap();
		assert_eq!(mime, "application/x-clipbridge-filelist+json");
		assert!(
			files.contains("child-sha-v4"),
			"files_json must survive the VACUUM: {files}"
		);
		assert_eq!(
			store
				.conn
				.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
				.unwrap(),
			5
		);
		drop(store);

		// 第二次打开：v5 已是最新，不得再 VACUUM/迁移；auto_vacuum 保持 2。
		let reopened = Store::open(dir.path()).unwrap();
		assert!(!reopened.rebuilt);
		assert_eq!(user_version(&reopened.conn), 5);
		assert_eq!(
			reopened
				.conn
				.pragma_query_value::<i64, _>(None, "auto_vacuum", |r| r.get(0))
				.unwrap(),
			2
		);
		assert_eq!(count_rows(&reopened.conn, "items"), 2);
	}

	#[test]
	fn fresh_db_is_v5_with_all_tables_and_indices_and_auto_vacuum() {
		let dir = tempfile::tempdir().unwrap();
		let store = Store::open(dir.path()).unwrap();
		assert_eq!(SCHEMA_VERSION, 5);
		assert_eq!(user_version(&store.conn), SCHEMA_VERSION);
		// P3：新库在 journal_mode=WAL 之前设置 auto_vacuum，必须以 auto-vacuum
		// 结构创建（无需再 VACUUM）。
		assert_eq!(
			store
				.conn
				.pragma_query_value::<i64, _>(None, "auto_vacuum", |r| r.get(0))
				.unwrap(),
			2,
			"fresh v5 db must be created in INCREMENTAL auto-vacuum mode"
		);
		for table in [
			"items",
			"history",
			"content_cache",
			"trusted_peers",
			"peer_rules",
		] {
			let exists: i64 = store
				.conn
				.query_row(
					"SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
					params![table],
					|r| r.get(0),
				)
				.unwrap();
			assert_eq!(exists, 1, "missing table {table}");
		}
		for index in [
			"idx_items_created",
			"idx_items_sha256",
			"idx_items_owner_created",
			"idx_items_kind_created",
			"idx_history_account_sort",
			"idx_history_item",
			"idx_history_account_item",
			"idx_cache_lru",
			"idx_cache_size",
			"idx_cache_durable",
		] {
			assert!(
				index_exists(&store.conn, index),
				"fresh v5 db is missing index {index}"
			);
		}
	}

	/// P1 核心用例：一个 v3 库（每张表都有行）打开后必须**迁移**到 v5，而不是重建；
	/// 行数据、身份/策略表全部保留，新索引存在，且再打开不会重复迁移。
	/// （v3 -> v4 建索引，v4 -> v5 开 auto_vacuum 并做一次性 VACUUM。）
	#[test]
	fn v3_db_migrates_to_v5_and_preserves_all_rows() {
		let dir = tempfile::tempdir().unwrap();
		create_v3_db(dir.path());

		let store = Store::open(dir.path()).unwrap();
		assert!(!store.rebuilt, "v3 -> v5 是迁移，绝不能走 drop-and-rebuild");
		assert_eq!(user_version(&store.conn), SCHEMA_VERSION);
		assert!(index_exists(&store.conn, "idx_items_kind_created"));
		// P3：迁移链经过 v4 -> v5，一次性 VACUUM 后必须是 INCREMENTAL。
		assert_eq!(
			store
				.conn
				.pragma_query_value::<i64, _>(None, "auto_vacuum", |r| r.get(0))
				.unwrap(),
			2,
			"v4 -> v5 migration must convert the db to INCREMENTAL auto-vacuum"
		);
		assert_eq!(count_rows(&store.conn, "items"), 2);
		assert_eq!(count_rows(&store.conn, "content_cache"), 2);
		assert_eq!(count_rows(&store.conn, "history"), 1);
		assert_eq!(count_rows(&store.conn, "trusted_peers"), 1);
		assert_eq!(count_rows(&store.conn, "peer_rules"), 1);

		// 保留的不只是行数：列内容也必须原样可读。
		let (mime, preview, files): (String, String, String) = store
			.conn
			.query_row(
				"SELECT mime, preview_json, files_json FROM items WHERE item_id='item-v3-a'",
				[],
				|r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
			)
			.unwrap();
		assert_eq!(mime, "text/plain");
		assert_eq!(preview, r#"{"text":"hello-v3"}"#);
		assert_eq!(files, "[]");
		drop(store);

		// 第二次打开：版本/指纹都匹配，不应再迁移或重建；auto_vacuum 保持 2。
		let reopened = Store::open(dir.path()).unwrap();
		assert!(!reopened.rebuilt);
		assert_eq!(user_version(&reopened.conn), SCHEMA_VERSION);
		assert_eq!(count_rows(&reopened.conn, "items"), 2);
		assert_eq!(
			reopened
				.conn
				.pragma_query_value::<i64, _>(None, "auto_vacuum", |r| r.get(0))
				.unwrap(),
			2
		);
	}

	/// P1 失败语义：迁移步骤报错时 `open` 返回 Err，事务回滚，库停留在旧版本且
	/// 数据完好；清除注入后同一库可以正常迁移。
	#[test]
	fn failed_migration_returns_err_and_leaves_db_at_previous_version() {
		let dir = tempfile::tempdir().unwrap();
		create_v3_db(dir.path());

		test_hooks::set_fail_v4_migration(true);
		let result = Store::open(dir.path());
		test_hooks::set_fail_v4_migration(false);

		let err = result
			.err()
			.expect("migration failure must make Store::open fail");
		assert!(
			format!("{err:#}").contains("injected failure"),
			"unexpected error: {err:#}"
		);

		// 直接读原库：版本号仍为 3、行数据完好、失败步骤的索引已回滚。
		let conn = Connection::open(dir.path().join("core.db")).unwrap();
		assert_eq!(user_version(&conn), 3, "失败的迁移不得推进 user_version");
		assert_eq!(count_rows(&conn, "items"), 2);
		assert_eq!(count_rows(&conn, "trusted_peers"), 1);
		assert!(
			!index_exists(&conn, "idx_items_kind_created"),
			"失败步骤的 DDL 必须随事务回滚"
		);
		drop(conn);

		let store = Store::open(dir.path()).unwrap();
		assert!(!store.rebuilt);
		assert_eq!(user_version(&store.conn), SCHEMA_VERSION);
		assert!(index_exists(&store.conn, "idx_items_kind_created"));
		assert_eq!(count_rows(&store.conn, "items"), 2);
		assert_eq!(
			store
				.conn
				.pragma_query_value::<i64, _>(None, "auto_vacuum", |r| r.get(0))
				.unwrap(),
			2,
			"重试成功后同样要完成 v4 -> v5 的 auto-vacuum 转换"
		);
	}

	/// P1 降级保护：`user_version` 高于当前构建时返回清晰错误，绝不写库/清库。
	#[test]
	fn newer_schema_version_is_rejected_without_data_loss() {
		let dir = tempfile::tempdir().unwrap();
		{
			let store = Store::open(dir.path()).unwrap();
			store
				.conn
				.execute(
					r"INSERT INTO items
                       (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex)
                       VALUES ('item-future', 'text', 'dev', 1, 1, 'text/plain', 'sha-future')",
					[],
				)
				.unwrap();
		}
		{
			let conn = Connection::open(dir.path().join("core.db")).unwrap();
			conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
				.unwrap();
		}

		let err = Store::open(dir.path())
			.err()
			.expect("newer schema version must be rejected");
		let msg = format!("{err:#}");
		assert!(
			msg.contains("newer than this build supports"),
			"error must explain the downgrade: {msg}"
		);
		assert!(
			msg.contains(&(SCHEMA_VERSION + 1).to_string()),
			"error must include the found version: {msg}"
		);

		let conn = Connection::open(dir.path().join("core.db")).unwrap();
		assert_eq!(
			user_version(&conn),
			SCHEMA_VERSION + 1,
			"不得改写更高版本号"
		);
		assert_eq!(count_rows(&conn, "items"), 1, "拒绝打开时不得清库");
	}

	/// P1 损坏兜底：版本号已是目标版本、但指纹（v4 索引）缺失时，仍按旧策略
	/// drop-and-rebuild，而不是带着坏结构运行。
	#[test]
	fn target_version_fingerprint_mismatch_rebuilds() {
		let dir = tempfile::tempdir().unwrap();
		{
			let store = Store::open(dir.path()).unwrap();
			store
				.conn
				.execute(
					r"INSERT INTO items
                       (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex)
                       VALUES ('item-corrupt', 'text', 'dev', 1, 1, 'text/plain', 'sha-corrupt')",
					[],
				)
				.unwrap();
			store
				.conn
				.execute_batch("DROP INDEX idx_items_kind_created;")
				.unwrap();
		}

		let reopened = Store::open(dir.path()).unwrap();
		assert!(reopened.rebuilt, "目标版本缺索引时必须走损坏兜底重建");
		assert_eq!(user_version(&reopened.conn), SCHEMA_VERSION);
		assert!(index_exists(&reopened.conn, "idx_items_kind_created"));
		assert_eq!(
			count_rows(&reopened.conn, "items"),
			0,
			"损坏兜底会清空内容表（这是唯一允许丢数据的路径）"
		);
	}

	/// P4：core.db 连接必须带上调优后的 PRAGMA。
	#[test]
	fn init_pragmas_are_applied() {
		let dir = tempfile::tempdir().unwrap();
		let store = Store::open(dir.path()).unwrap();

		let cache_size: i64 = store
			.conn
			.pragma_query_value(None, "cache_size", |r| r.get(0))
			.unwrap();
		assert_eq!(cache_size, -16000, "cache_size 必须为 16 MB（负数 = KiB）");

		let journal_size_limit: i64 = store
			.conn
			.pragma_query_value(None, "journal_size_limit", |r| r.get(0))
			.unwrap();
		assert_eq!(journal_size_limit, 8_388_608);

		let temp_store: i64 = store
			.conn
			.pragma_query_value(None, "temp_store", |r| r.get(0))
			.unwrap();
		assert_eq!(temp_store, 2, "temp_store=2 表示 MEMORY");

		let journal_mode: String = store
			.conn
			.pragma_query_value(None, "journal_mode", |r| r.get(0))
			.unwrap();
		assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

		// P3：新库必须在 WAL/建表之前设置 auto_vacuum，读回应为 2（INCREMENTAL）。
		let auto_vacuum: i64 = store
			.conn
			.pragma_query_value(None, "auto_vacuum", |r| r.get(0))
			.unwrap();
		assert_eq!(auto_vacuum, 2, "auto_vacuum=2 表示 INCREMENTAL");
		assert_eq!(
			store
				.conn
				.pragma_query_value::<i64, _>(None, "synchronous", |r| r.get(0))
				.unwrap(),
			1,
			"synchronous=NORMAL(1)"
		);

		// mmap_size 在禁用 mmap 的 SQLite 构建（SQLITE_MAX_MMAP_SIZE=0）上会读回 0；
		// 本机 bundled 构建应等于设置值。两种都接受，但绝不接受其它数值。
		let mmap_size: i64 = store
			.conn
			.pragma_query_value(None, "mmap_size", |r| r.get(0))
			.unwrap();
		assert!(
			mmap_size == 0 || mmap_size == 67_108_864,
			"unexpected mmap_size {mmap_size}"
		);
	}

	#[test]
	fn fresh_db_is_stamped_and_reopened_without_rebuild() {
		let dir = tempfile::tempdir().unwrap();

		let store = Store::open(dir.path()).unwrap();
		assert!(store.rebuilt, "新文件 user_version=0，必须走一次重建");
		assert_eq!(user_version(&store.conn), SCHEMA_VERSION);
		drop(store);

		let reopened = Store::open(dir.path()).unwrap();
		assert!(!reopened.rebuilt, "版本一致且结构完整时不应重建");
	}

	#[test]
	fn legacy_v2_without_files_json_is_rebuilt() {
		let dir = tempfile::tempdir().unwrap();
		let db_path = dir.path().join("core.db");
		{
			// 模拟 Dec 2025 的 migrate_v1..v3 时代：user_version=2，但 items 还没有
			// files_json，也没有 peer_rules。只信版本号会漏掉这种库。
			let conn = Connection::open(&db_path).unwrap();
			conn.execute_batch(
				r"
                CREATE TABLE items (
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
                CREATE TABLE history (
                  history_id INTEGER PRIMARY KEY AUTOINCREMENT,
                  account_uid TEXT NOT NULL,
                  item_id TEXT NOT NULL,
                  sort_ts_ms INTEGER NOT NULL,
                  source_device_id TEXT,
                  is_deleted INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE content_cache (
                  sha256_hex TEXT PRIMARY KEY,
                  total_bytes INTEGER NOT NULL,
                  present INTEGER NOT NULL,
                  last_access_ts_ms INTEGER NOT NULL,
                  created_ts_ms INTEGER NOT NULL
                );
                CREATE TABLE trusted_peers (
                  account_uid TEXT NOT NULL,
                  device_id TEXT NOT NULL,
                  fingerprint_sha256 TEXT NOT NULL,
                  updated_at_ms INTEGER NOT NULL,
                  PRIMARY KEY (account_uid, device_id)
                );
                PRAGMA user_version = 2;
                ",
			)
			.unwrap();
		}

		let store = Store::open(dir.path()).unwrap();
		assert!(store.rebuilt, "旧 v2 结构指纹不符，必须整库重建");
		assert!(item_columns(&store.conn).iter().any(|c| c == "files_json"));
		assert_eq!(user_version(&store.conn), SCHEMA_VERSION);
	}

	#[test]
	fn expires_column_is_gone_and_intermediate_v2_is_rebuilt() {
		let dir = tempfile::tempdir().unwrap();
		{
			// 先让新代码建出最终 v2 结构。
			let store = Store::open(dir.path()).unwrap();
			assert!(!item_columns(&store.conn)
				.iter()
				.any(|c| c == "expires_ts_ms"));
		}
		{
			// 模拟“Task 1 时代”的中间 v2：版本号相同，但 items 还留着 expires_ts_ms。
			let conn = Connection::open(dir.path().join("core.db")).unwrap();
			conn.execute_batch("ALTER TABLE items ADD COLUMN expires_ts_ms INTEGER;")
				.unwrap();
		}

		let reopened = Store::open(dir.path()).unwrap();
		assert!(reopened.rebuilt, "版本相同但结构指纹不符，仍必须重建");
		assert!(!item_columns(&reopened.conn)
			.iter()
			.any(|c| c == "expires_ts_ms"));
	}

	#[test]
	fn kind_check_and_history_cascade_are_enforced() {
		let dir = tempfile::tempdir().unwrap();
		let store = Store::open(dir.path()).unwrap();

		store
			.conn
			.execute(
				r"INSERT INTO items
                   (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex)
                   VALUES ('item-1', 'text', 'dev-1', 1, 1, 'text/plain', 'sha-1')",
				[],
			)
			.unwrap();
		// CHECK 只允许 kind_to_str 产出的三个值。
		assert!(store
			.conn
			.execute(
				r"INSERT INTO items
                   (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex)
                   VALUES ('item-2', 'video', 'dev-1', 1, 1, 'video/mp4', 'sha-2')",
				[],
			)
			.is_err());

		store
			.conn
			.execute(
				"INSERT INTO history(account_uid, item_id, sort_ts_ms) VALUES ('acct', 'item-1', 1)",
				[],
			)
			.unwrap();
		// 现有删除路径都是软删除；这里验证硬删除时 FK cascade 不会留下悬空 history。
		store
			.conn
			.execute("DELETE FROM items WHERE item_id='item-1'", [])
			.unwrap();
		let remaining: i64 = store
			.conn
			.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
			.unwrap();
		assert_eq!(remaining, 0);
	}

	/// v3 的 durable 列：默认 1（旧行/本地 ingest 视为已持久化），可以置 0/1，
	/// sweep 查询只返回 durable=0 的行。
	#[test]
	fn durable_defaults_to_one_and_sweep_lists_only_pending_rows() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		// 不写 durable 列，模拟“本列出现之前插入的行”：DEFAULT 1 必须生效。
		for (sha, ts) in [("a".repeat(64), 1_i64), ("b".repeat(64), 2_i64)] {
			store
				.conn
				.execute(
					r"INSERT INTO content_cache
                       (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
                       VALUES (?1, 10, 0, ?2, ?2)",
					params![sha, ts],
				)
				.unwrap();
		}

		let a = "a".repeat(64);
		let b = "b".repeat(64);
		assert!(
			store.get_cache_durable(&a).unwrap(),
			"未显式写入 durable 的行必须默认 1"
		);
		assert_eq!(
			store.select_durability_pending(10).unwrap(),
			Vec::<String>::new()
		);

		// 接收路径：present=1 + durable=0 必须能在同一条 UPDATE 里落库。
		assert_eq!(
			store
				.mark_cache_present_with_durability(&a, 100, false)
				.unwrap(),
			1
		);
		assert!(store.get_cache_present(&a).unwrap());
		assert!(!store.get_cache_durable(&a).unwrap());
		assert_eq!(
			store.select_durability_pending(10).unwrap(),
			vec![a.clone()]
		);

		// sweep 只处理 pending：durable=1 的行不会出现。
		assert_eq!(
			store.select_durability_pending(10).unwrap(),
			vec![a.clone()]
		);

		// 后台同步成功：翻回 1 后不再出现在 sweep 结果里。
		assert_eq!(store.mark_cache_durable(&a).unwrap(), 1);
		assert!(store.get_cache_durable(&a).unwrap());
		assert_eq!(
			store.select_durability_pending(10).unwrap(),
			Vec::<String>::new(),
			"同步完成后必须离开 pending 列表"
		);

		// 再置 0 可以重复进入 pending（后台失败重试 / 再次接收）。
		assert_eq!(
			store
				.mark_cache_present_with_durability(&b, 200, false)
				.unwrap(),
			1
		);
		assert_eq!(store.select_durability_pending(10).unwrap(), vec![b]);
	}

	/// 同一版本号、但 content_cache 缺 durable 列的旧库（v2 结构）必须重建，
	/// 否则接收路径的 durable=0 写入会在老库上直接失败。
	#[test]
	fn missing_durable_column_forces_rebuild() {
		let dir = tempfile::tempdir().unwrap();
		{
			// 先建出 v3 结构，再把版本号伪装成 v3、删掉 durable 列。
			let _store = Store::open(dir.path()).unwrap();
			let conn = Connection::open(dir.path().join("core.db")).unwrap();
			conn.execute_batch(
				"DROP INDEX idx_cache_durable;
                 ALTER TABLE content_cache DROP COLUMN durable;
                 PRAGMA user_version = 3;",
			)
			.unwrap();
		}

		let reopened = Store::open(dir.path()).unwrap();
		assert!(reopened.rebuilt, "缺 durable 列时不能只信版本号");
		let has_durable = reopened
			.conn
			.prepare("PRAGMA table_info(content_cache)")
			.unwrap()
			.query_map([], |r| r.get::<_, String>(1))
			.unwrap()
			.map(Result::unwrap)
			.any(|c| c == "durable");
		assert!(has_durable);
		assert_eq!(user_version(&reopened.conn), SCHEMA_VERSION);
	}

	/// 回归（F1）：远端 `ItemMeta.files[].local_path` 是 hostile input，入库前
	/// 必须被剥离；否则对端只需 `ContentGet` 就能读到任意本地文件。
	#[test]
	fn insert_remote_item_strips_peer_supplied_local_path() {
		use crate::model::{ItemContent, ItemPreview};

		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		// 真实存在的本地文件：若 local_path 被保留，ContentGet 会把它流给对端。
		let secret = dir.path().join("victim-secret.docx");
		std::fs::write(&secret, b"top secret").unwrap();
		let secret_str = secret.to_string_lossy().to_string();
		let file_sha = "a".repeat(64);

		let meta = ItemMeta {
			ty: "ItemMeta".to_string(),
			item_id: "item-evil".to_string(),
			kind: ItemKind::FileList,
			created_ts_ms: 1,
			source_device_id: "peer-evil".to_string(),
			source_device_name: None,
			size_bytes: 10,
			preview: ItemPreview::default(),
			content: ItemContent {
				mime: "application/x-clipbridge-files".to_string(),
				sha256: "b".repeat(64),
				total_bytes: 10,
			},
			files: vec![FileMeta {
				file_id: "file-evil".to_string(),
				rel_name: "secret.docx".to_string(),
				size_bytes: 10,
				sha256: Some(file_sha),
				local_path: Some(secret_str.clone()),
			}],
			expires_ts_ms: None,
		};

		assert!(store.insert_remote_item("acct", &meta, 1).unwrap());
		// 调用方传入的原始 DTO 不应被修改（调用方还要用它发事件）。
		assert_eq!(
			meta.files[0].local_path.as_deref(),
			Some(secret_str.as_str())
		);

		let files_json: String = store
			.conn
			.query_row(
				"SELECT files_json FROM items WHERE item_id = 'item-evil'",
				[],
				|r| r.get(0),
			)
			.unwrap();
		let persisted: Vec<serde_json::Value> = serde_json::from_str(&files_json).unwrap();
		assert!(
			persisted[0].get("local_path").is_none(),
			"local_path must be stripped before persistence: {files_json}"
		);
		assert_eq!(
			store
				.get_file_meta("item-evil", "file-evil")
				.unwrap()
				.unwrap()
				.local_path,
			None,
			"store accessor must never return a peer-supplied local path"
		);
		// owner 仍保持远端身份：防守逻辑（local_path 仅本地 owner 可用）依赖它。
		assert_eq!(
			store.get_item_owner("item-evil").unwrap().as_deref(),
			Some("peer-evil")
		);
	}

	fn test_item_meta(item_id: &str, sha: &str) -> ItemMeta {
		use crate::model::{ItemContent, ItemPreview};
		ItemMeta {
			ty: "ItemMeta".to_string(),
			item_id: item_id.to_string(),
			kind: ItemKind::Text,
			created_ts_ms: 1,
			source_device_id: "peer-a".to_string(),
			source_device_name: None,
			size_bytes: 10,
			preview: ItemPreview::default(),
			content: ItemContent {
				mime: "text/plain".to_string(),
				sha256: sha.to_string(),
				total_bytes: 10,
			},
			files: vec![],
			expires_ts_ms: None,
		}
	}

	/// F5.2：引用集合必须同时覆盖 content_cache、items 本体与 FileList 子文件
	/// （子文件 blob 没有独立的 content_cache 行，漏掉就会被孤儿清扫误删）。
	#[test]
	fn referenced_hashes_include_cache_items_and_file_blobs() {
		use crate::model::{ItemContent, ItemPreview};

		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		let item_sha = "1".repeat(64);
		let file_sha = "2".repeat(64);
		let cache_only_sha = "3".repeat(64);

		let meta = ItemMeta {
			ty: "ItemMeta".to_string(),
			item_id: "item-files".to_string(),
			kind: ItemKind::FileList,
			created_ts_ms: 1,
			source_device_id: "peer-a".to_string(),
			source_device_name: None,
			size_bytes: 10,
			preview: ItemPreview::default(),
			content: ItemContent {
				mime: "application/x-clipbridge-filelist+json".to_string(),
				sha256: item_sha.clone(),
				total_bytes: 10,
			},
			files: vec![FileMeta {
				file_id: "file-1".to_string(),
				rel_name: "a.bin".to_string(),
				size_bytes: 10,
				sha256: Some(file_sha.clone()),
				local_path: None,
			}],
			expires_ts_ms: None,
		};
		store.insert_remote_item("acct", &meta, 1).unwrap();
		store
			.conn
			.execute(
				r"INSERT OR IGNORE INTO content_cache
                   (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
                   VALUES (?1, 1, 0, 1, 1)",
				params![cache_only_sha],
			)
			.unwrap();

		let referenced = store.list_referenced_content_hashes().unwrap();
		assert!(referenced.contains(&item_sha), "items.sha256_hex 必须计入");
		assert!(
			referenced.contains(&file_sha),
			"files_json[].sha256 必须计入（否则清扫会误删 FileList 子文件）"
		);
		assert!(
			referenced.contains(&cache_only_sha),
			"content_cache 行必须计入"
		);
	}

	/// F8：删除共享 blob 前必须确认没有其他“未删除”item 引用同一 sha。
	#[test]
	fn other_live_content_reference_respects_soft_delete() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();
		let shared = "4".repeat(64);

		store
			.insert_remote_item("acct", &test_item_meta("item-a", &shared), 1)
			.unwrap();
		store
			.insert_remote_item("acct", &test_item_meta("item-b", &shared), 2)
			.unwrap();

		assert!(
			store
				.has_other_live_content_reference(&shared, "item-a")
				.unwrap(),
			"item-b 仍存活时 item-a 的删除不得清掉共享 blob"
		);

		assert!(store.soft_delete_item("acct", "item-b").unwrap());
		assert!(
			!store
				.has_other_live_content_reference(&shared, "item-a")
				.unwrap(),
			"除被删 item 之外没有存活引用时才允许清理 blob"
		);

		// item-b 视角：item-a 仍存活，必须保留。
		assert!(store
			.has_other_live_content_reference(&shared, "item-b")
			.unwrap());
		// 两个 item 都软删除后，谁再删都不应保留 blob。
		assert!(store.soft_delete_item("acct", "item-a").unwrap());
		assert!(!store
			.has_other_live_content_reference(&shared, "item-b")
			.unwrap());
	}

	// -----------------------------------------------------------------------
	// F12：入站元数据上限 + 历史行数保留
	// -----------------------------------------------------------------------

	fn limit_meta(item_id: &str) -> ItemMeta {
		use crate::model::{ItemContent, ItemPreview};
		ItemMeta {
			ty: "ItemMeta".to_string(),
			item_id: item_id.to_string(),
			kind: ItemKind::FileList,
			created_ts_ms: 1,
			source_device_id: "dev-a".to_string(),
			source_device_name: None,
			size_bytes: 10,
			preview: ItemPreview::default(),
			content: ItemContent {
				mime: "application/x-clipbridge-filelist+json".to_string(),
				sha256: "a".repeat(64),
				total_bytes: 10,
			},
			files: Vec::new(),
			expires_ts_ms: None,
		}
	}

	fn limit_file(file_id: &str, rel_name: &str) -> FileMeta {
		FileMeta {
			file_id: file_id.to_string(),
			rel_name: rel_name.to_string(),
			size_bytes: 1,
			sha256: None,
			local_path: None,
		}
	}

	/// 边界：恰好等于上限必须接受，超过 1 字节必须拒绝；ID 白名单只允许
	/// 可打印 ASCII 且不含路径分隔符。
	#[test]
	fn inbound_meta_limits_accept_boundaries_and_reject_oversize() {
		// 恰好达到所有单字段上限：128 / 128 / 128 / 512 / 1 个文件。
		let mut meta = limit_meta(&"i".repeat(MAX_ITEM_META_ID_BYTES));
		meta.source_device_id = "d".repeat(MAX_ITEM_META_ID_BYTES);
		meta.files = vec![limit_file(
			&"f".repeat(MAX_ITEM_META_ID_BYTES),
			&"r".repeat(MAX_ITEM_META_REL_NAME_BYTES),
		)];
		assert!(validate_inbound_item_meta(&meta).is_ok());

		// files.len() 恰好 256 可以接受。
		meta.files = (0..MAX_ITEM_META_FILES)
			.map(|i| limit_file(&format!("f{i}"), "r.txt"))
			.collect();
		assert!(validate_inbound_item_meta(&meta).is_ok());
		meta.files.push(limit_file("f-extra", "r.txt"));
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::TooManyFiles
		);

		// 单字段 +1 字节必须拒绝。
		let mut meta = limit_meta(&"i".repeat(MAX_ITEM_META_ID_BYTES + 1));
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::ItemIdLength
		);
		meta.item_id = "ok".to_string();
		meta.source_device_id = "d".repeat(MAX_ITEM_META_ID_BYTES + 1);
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::DeviceIdLength
		);
		meta.source_device_id = "dev".to_string();
		meta.files = vec![limit_file(&"f".repeat(MAX_ITEM_META_ID_BYTES + 1), "r")];
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::FileIdLength
		);
		meta.files = vec![limit_file(
			"f",
			&"r".repeat(MAX_ITEM_META_REL_NAME_BYTES + 1),
		)];
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::RelNameLength
		);
	}

	/// ID 白名单：控制字符 / 路径分隔符 / `:` 都拒绝（不记录对端字符串，只报原因）。
	#[test]
	fn inbound_meta_id_whitelist_rejects_control_and_path_chars() {
		for bad in ["a/b", "a\\b", "C:evil", "evil\u{0}", "evil\u{1f}", "联想"] {
			let mut meta = limit_meta(bad);
			let err = validate_inbound_item_meta(&meta).unwrap_err();
			assert_eq!(err.kind, MetaLimitKind::ItemIdChars, "item_id={bad:?}");
			meta.item_id = "ok".to_string();
			meta.source_device_id = bad.to_string();
			assert_eq!(
				validate_inbound_item_meta(&meta).unwrap_err().kind,
				MetaLimitKind::DeviceIdChars,
				"device_id={bad:?}"
			);
			meta.source_device_id = "dev".to_string();
			meta.files = vec![limit_file(bad, "r.txt")];
			assert_eq!(
				validate_inbound_item_meta(&meta).unwrap_err().kind,
				MetaLimitKind::FileIdChars,
				"file_id={bad:?}"
			);
		}
	}

	/// `preview` 的序列化大小边界：恰好 64 KiB 接受，+1 字节拒绝。
	#[test]
	fn inbound_meta_preview_serialized_size_boundary() {
		use crate::model::ItemPreview;

		let overhead = serde_json::to_vec(&ItemPreview {
			text: Some(String::new()),
			..Default::default()
		})
		.unwrap()
		.len();
		let exact = MAX_ITEM_META_PREVIEW_BYTES - overhead;

		let mut meta = limit_meta("item-preview");
		meta.preview.text = Some("x".repeat(exact));
		assert!(validate_inbound_item_meta(&meta).is_ok());

		meta.preview.text = Some("x".repeat(exact + 1));
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::PreviewTooLarge
		);

		// 远大于上限的 text 走“长度下界”快速拒绝，不做 JSON 转义。
		meta.preview.text = Some("x".repeat(MAX_ITEM_META_PREVIEW_BYTES * 2));
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::PreviewTooLarge
		);
	}

	/// 总逻辑大小：每个字段都在上限内，但合起来超过 256 KiB 仍必须拒绝。
	#[test]
	fn inbound_meta_total_size_cap_rejects_oversize_aggregate() {
		let mut meta = limit_meta("item-total");
		meta.preview.text = Some("p".repeat(60 * 1024));
		meta.source_device_name = Some("n".repeat(200 * 1024));
		// 单字段都合法：text < 64 KiB、device_name 无独立上限、文件数为 0。
		assert_eq!(
			validate_inbound_item_meta(&meta).unwrap_err().kind,
			MetaLimitKind::TotalTooLarge
		);
	}

	/// 纵深防御：即使绕过 actor 直接调用 `insert_remote_item`，超限元数据也不得落库。
	#[test]
	fn insert_remote_item_rejects_oversize_meta() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();
		let meta = limit_meta(&"i".repeat(MAX_ITEM_META_ID_BYTES + 1));

		let err = store.insert_remote_item("acct", &meta, 1).unwrap_err();
		assert!(
			err.to_string().contains("item_id_length"),
			"unexpected error: {err:#}"
		);

		let items: i64 = store
			.conn
			.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
			.unwrap();
		let history: i64 = store
			.conn
			.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
			.unwrap();
		assert_eq!((items, history), (0, 0), "被拒绝的元数据不得写入任何表");
	}

	/// F12：按行数保留最新 N 条，且只影响目标账号；用户软删除的用户操作不变。
	#[test]
	fn history_retention_purges_oldest_rows_and_keeps_newest() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		for i in 0..25_i64 {
			let mut meta = test_item_meta(&format!("item-{i:03}"), &"5".repeat(64));
			meta.created_ts_ms = i;
			store.insert_remote_item("acct", &meta, i).unwrap();
		}
		// 另一个账号的数据绝不能被误删。
		for i in 0..3_i64 {
			let mut meta = test_item_meta(&format!("other-{i}"), &"6".repeat(64));
			meta.created_ts_ms = i;
			store.insert_remote_item("acct-other", &meta, i).unwrap();
		}
		assert_eq!(store.history_total_count_for_account("acct").unwrap(), 25);

		// 保留最新 10 条：删除最旧 15 条。
		let deleted = store.purge_history_beyond_retention("acct", 10).unwrap();
		assert_eq!(deleted, 15);
		assert_eq!(store.history_total_count_for_account("acct").unwrap(), 10);
		assert_eq!(
			store.history_total_count_for_account("acct-other").unwrap(),
			3,
			"其他账号的历史不得被保留策略删除"
		);

		// 留下的是最新的 10 条（item-015..item-024），按 sort_ts 倒序返回。
		let metas = store.list_history_metas("acct", 100).unwrap();
		assert_eq!(metas.len(), 10);
		assert_eq!(metas.first().unwrap().item_id, "item-024");
		assert_eq!(metas.last().unwrap().item_id, "item-015");

		// 幂等：已经在上限以内时不再删除。
		assert_eq!(store.purge_history_beyond_retention("acct", 10).unwrap(), 0);

		// 软删除的行也属于“总行数”，同样会被最旧优先的保留策略清掉。
		assert!(store.soft_delete_item("acct", "item-015").unwrap());
		for i in 25..30_i64 {
			let mut meta = test_item_meta(&format!("item-{i:03}"), &"7".repeat(64));
			meta.created_ts_ms = i;
			store.insert_remote_item("acct", &meta, i).unwrap();
		}
		assert_eq!(store.history_total_count_for_account("acct").unwrap(), 15);
		assert_eq!(store.purge_history_beyond_retention("acct", 10).unwrap(), 5);
		assert_eq!(store.history_total_count_for_account("acct").unwrap(), 10);
	}

	// -----------------------------------------------------------------------
	// P3：孤儿行压缩 / incremental_vacuum
	// -----------------------------------------------------------------------

	fn file_list_meta(item_id: &str, manifest_sha: &str, child_sha: &str) -> ItemMeta {
		use crate::model::{ItemContent, ItemPreview};
		ItemMeta {
			ty: "ItemMeta".to_string(),
			item_id: item_id.to_string(),
			kind: ItemKind::FileList,
			created_ts_ms: 2,
			source_device_id: "peer-a".to_string(),
			source_device_name: None,
			size_bytes: 10,
			preview: ItemPreview::default(),
			content: ItemContent {
				mime: "application/x-clipbridge-filelist+json".to_string(),
				sha256: manifest_sha.to_string(),
				total_bytes: 10,
			},
			files: vec![FileMeta {
				file_id: "file-1".to_string(),
				rel_name: "a.bin".to_string(),
				size_bytes: 10,
				sha256: Some(child_sha.to_string()),
				local_path: None,
			}],
			expires_ts_ms: None,
		}
	}

	fn insert_cache_row(store: &Store, sha: &str, present: i64, ts: i64) {
		store
			.conn
			.execute(
				r"INSERT OR IGNORE INTO content_cache
                   (sha256_hex, total_bytes, present, durable, last_access_ts_ms, created_ts_ms)
                   VALUES (?1, 10, ?2, 1, ?3, ?3)",
				params![sha, present, ts],
			)
			.unwrap();
	}

	fn cache_row_exists(store: &Store, sha: &str) -> bool {
		store
			.conn
			.query_row(
				"SELECT COUNT(*) FROM content_cache WHERE sha256_hex=?1",
				params![sha],
				|r| r.get::<_, i64>(0),
			)
			.unwrap() > 0
	}

	/// 插入一条没有 history 行的孤儿 item（直接 SQL；正常写入路径都会建 history）。
	fn insert_orphan_item(store: &Store, item_id: &str, sha: &str, files_json: &str) {
		store
			.conn
			.execute(
				r"INSERT INTO items
                   (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex, preview_json, files_json)
                   VALUES (?1, 'file_list', 'peer-a', 1, 10,
                           'application/x-clipbridge-filelist+json', ?2, '{}', ?3)",
				params![item_id, sha, files_json],
			)
			.unwrap();
	}

	/// P3：压缩只删“没有 history 引用的 items”和“present=0 且无任何 items 引用
	/// （本体 sha 或 files_json 子文件 sha）的 content_cache 行”，绝不误删：
	/// - 有 history 行（哪怕 is_deleted=1）的 item；
	/// - 存活 FileList item 的 files_json 子文件 sha；
	/// - present=1 的行（不在回收范围）。
	#[test]
	fn compaction_deletes_only_unreferenced_items_and_cache_rows() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		// 存活 text item：自身 sha 引用 cache 行。
		store
			.insert_remote_item(
				"acct",
				&test_item_meta("item-live-text", "sha-live-text"),
				1,
			)
			.unwrap();
		// 存活 FileList item：manifest sha + files_json 子文件 sha 都必须保护 cache 行。
		store
			.insert_remote_item(
				"acct",
				&file_list_meta("item-live-files", "sha-live-manifest", "sha-live-child"),
				2,
			)
			.unwrap();
		// 软删除 item：history 行仍在（is_deleted=1），不得删 item / 其 cache 行。
		store
			.insert_remote_item("acct", &test_item_meta("item-soft-deleted", "sha-soft"), 3)
			.unwrap();
		store.soft_delete_item("acct", "item-soft-deleted").unwrap();

		// 孤儿 item（无 history 行），自身与子文件各引用一个 sha。
		insert_orphan_item(
			&store,
			"item-orphan",
			"sha-orphan-manifest",
			r#"[{"file_id":"f1","rel_name":"b.bin","size_bytes":1,"sha256":"sha-orphan-child"}]"#,
		);

		// 候选 cache 行。
		insert_cache_row(&store, "sha-live-child", 0, 10); // FileList 子文件引用：保留
		insert_cache_row(&store, "sha-orphan-manifest", 0, 11); // 孤儿 item 本体：item 删后可删
		insert_cache_row(&store, "sha-orphan-child", 0, 12); // 孤儿 item 子文件：item 删后可删
		insert_cache_row(&store, "sha-unreferenced", 0, 13); // 无任何引用：删
		insert_cache_row(&store, "sha-present-unref", 1, 14); // present=1：不删

		assert_eq!(count_rows(&store.conn, "items"), 4);
		// 有界删除：limit=1 只删一条孤儿。
		assert_eq!(store.compact_orphan_items(1).unwrap(), 1);
		assert_eq!(count_rows(&store.conn, "items"), 3);
		assert_eq!(store.compact_orphan_items(512).unwrap(), 0);

		let deleted = store.compact_unreferenced_cache_rows(512).unwrap();
		assert_eq!(
			deleted, 3,
			"只删孤儿 manifest/child 与完全无引用的 present=0 行"
		);

		for kept in [
			"sha-live-text",
			"sha-live-manifest",
			"sha-live-child",
			"sha-soft",
			"sha-present-unref",
		] {
			assert!(cache_row_exists(&store, kept), "must keep {kept}");
		}
		for gone in [
			"sha-orphan-manifest",
			"sha-orphan-child",
			"sha-unreferenced",
		] {
			assert!(!cache_row_exists(&store, gone), "must delete {gone}");
		}

		// 软删除的 item 仍是存活行（history 只置 is_deleted）。
		assert_eq!(
			store
				.conn
				.query_row(
					"SELECT COUNT(*) FROM items WHERE item_id='item-soft-deleted'",
					[],
					|r| r.get::<_, i64>(0),
				)
				.unwrap(),
			1
		);
	}

	/// P3：两个压缩入口都遵守每次调用的行数上限（所以大批孤儿会分多轮清掉）。
	#[test]
	fn compaction_respects_per_call_row_bounds() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		for i in 0..3 {
			insert_orphan_item(
				&store,
				&format!("item-orphan-{i}"),
				&format!("sha-orphan-{i}"),
				"[]",
			);
			insert_cache_row(&store, &format!("sha-orphan-{i}"), 0, i);
		}
		insert_cache_row(&store, "sha-unreferenced-extra", 0, 9);

		assert_eq!(store.compact_orphan_items(2).unwrap(), 2);
		assert_eq!(store.compact_orphan_items(2).unwrap(), 1);
		assert_eq!(store.compact_orphan_items(2).unwrap(), 0);

		assert_eq!(store.compact_unreferenced_cache_rows(2).unwrap(), 2);
		assert_eq!(store.compact_unreferenced_cache_rows(2).unwrap(), 2);
		assert_eq!(store.compact_unreferenced_cache_rows(2).unwrap(), 0);
	}

	/// P3：删除大量行后，`PRAGMA incremental_vacuum(N)` 必须真的回收 freelist 页；
	/// 这是 auto_vacuum=INCREMENTAL 生效的回归（非 auto-vacuum 库只会返回 0 行）。
	#[test]
	fn incremental_vacuum_reclaims_freelist_pages_after_deletes() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		// 撑到几百页，再删掉绝大多数行制造 freelist。
		for i in 0..3_000_i64 {
			let mut meta = test_item_meta(&format!("item-{i:05}"), &format!("sha-{i:05}"));
			meta.created_ts_ms = i;
			meta.preview.text = Some(format!("{}{:05}", "x".repeat(200), i));
			store.insert_meta_and_history("acct", &meta, i).unwrap();
		}
		assert_eq!(
			store.purge_history_oldest_excess("acct", 2_800).unwrap(),
			2_800
		);
		assert_eq!(store.compact_orphan_items(10_000).unwrap(), 2_800);

		let freelist_before: i64 = store
			.conn
			.pragma_query_value(None, "freelist_count", |r| r.get(0))
			.unwrap();
		assert!(freelist_before > 0, "删除大量行后必须有 freelist 页");

		let reclaimed = store.incremental_vacuum(64).unwrap();
		assert!(
			reclaimed > 0,
			"auto_vacuum=INCREMENTAL 下 incremental_vacuum 必须回收页"
		);
		let freelist_after: i64 = store
			.conn
			.pragma_query_value(None, "freelist_count", |r| r.get(0))
			.unwrap();
		assert!(
			freelist_after < freelist_before,
			"回收后 freelist 必须下降（{freelist_before} -> {freelist_after}，reclaimed={reclaimed}）"
		);

		// 上界 0 / 负数都是 no-op。
		assert_eq!(store.incremental_vacuum(0).unwrap(), 0);
		assert_eq!(store.incremental_vacuum(-1).unwrap(), 0);
	}

	// -----------------------------------------------------------------------
	// Task B：只读连接池
	// -----------------------------------------------------------------------

	/// 池里的查询结果必须与写连接一致（同一份 SQL 实现），且池大小固定。
	#[test]
	fn read_pool_matches_store_query_results() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();
		for i in 0..20_i64 {
			let mut meta = test_item_meta(&format!("item-{i:02}"), &format!("sha-{i:02}"));
			meta.created_ts_ms = i;
			store.insert_meta_and_history("acct", &meta, i).unwrap();
		}

		let reader = StoreReader::with_size(dir.path(), 2);
		assert_eq!(reader.size(), 2);

		let from_store = store.list_history_metas("acct", 100).unwrap();
		let from_pool = reader.list_history_metas("acct", 100).unwrap();
		assert_eq!(from_store.len(), from_pool.len());
		assert_eq!(
			from_store
				.iter()
				.map(|m| m.item_id.as_str())
				.collect::<Vec<_>>(),
			from_pool
				.iter()
				.map(|m| m.item_id.as_str())
				.collect::<Vec<_>>()
		);

		assert_eq!(
			store
				.get_item_meta_by_id("item-07")
				.unwrap()
				.unwrap()
				.item_id,
			reader
				.get_item_meta_by_id("item-07")
				.unwrap()
				.unwrap()
				.item_id
		);
		assert_eq!(
			store.sum_present_bytes().unwrap(),
			reader.sum_present_bytes().unwrap()
		);
		assert_eq!(
			store.activity_buckets(0, i64::MAX, 60_000).unwrap().len(),
			reader.activity_buckets(0, i64::MAX, 60_000).unwrap().len()
		);
	}

	/// 池有界：借满后新的借用必须等待归还，而不是新建连接或失败；
	/// 正常返回与 panic 展开都不得泄漏连接。
	#[test]
	fn read_pool_is_bounded_and_does_not_leak_connections() {
		let dir = tempfile::tempdir().unwrap();
		let _store = Store::open(dir.path()).unwrap();
		let reader = StoreReader::with_size(dir.path(), 2);

		let lease1 = reader.acquire().unwrap();
		let lease2 = reader.acquire().unwrap();
		let (tx, rx) = std::sync::mpsc::channel();
		std::thread::scope(|scope| {
			scope.spawn(|| {
				reader.with_conn(|_| anyhow::Ok(())).unwrap();
				tx.send(()).unwrap();
			});
			assert!(
				rx.recv_timeout(std::time::Duration::from_millis(150))
					.is_err(),
				"池耗尽时借用必须等待，而不是无界新建连接"
			);
			drop(lease1);
			rx.recv_timeout(std::time::Duration::from_secs(2))
				.expect("归还后等待者必须被唤醒");
		});
		drop(lease2);
		// 惰性池：并发借用已经打开了 size 条连接；全部必须回到池里。
		assert_eq!(reader.created(), reader.size());
		assert_eq!(
			reader.idle.lock().unwrap().len(),
			reader.created(),
			"所有连接都必须回到池里"
		);

		// panic 展开路径同样要归还连接。
		let hook = std::panic::take_hook();
		std::panic::set_hook(Box::new(|_| {})); // 静默预期中的 panic
		let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let _ = reader.with_conn(|_| -> anyhow::Result<()> { panic!("injected panic") });
		}));
		std::panic::set_hook(hook);
		assert!(caught.is_err());
		assert_eq!(
			reader.idle.lock().unwrap().len(),
			reader.created(),
			"panic 展开后连接也必须归池"
		);
	}

	/// `query_only=ON` 必须在池连接上生效：任何写语句都必须报错。
	#[test]
	fn read_pool_connections_are_query_only() {
		let dir = tempfile::tempdir().unwrap();
		let _store = Store::open(dir.path()).unwrap();
		let reader = StoreReader::with_size(dir.path(), 1);

		let guard = reader.acquire().unwrap();
		let conn = guard.conn();
		assert_eq!(
			conn.pragma_query_value::<i64, _>(None, "query_only", |r| r.get(0))
				.unwrap(),
			1
		);
		let write = conn.execute(
			r"INSERT INTO items
               (item_id, kind, owner_device_id, created_ts_ms, size_bytes, mime, sha256_hex)
               VALUES ('should-fail','text','d',1,1,'text/plain','sha-should-fail')",
			[],
		);
		assert!(write.is_err(), "池连接上的写必须失败: {write:?}");
		assert_eq!(
			conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0))
				.unwrap(),
			0
		);
	}

	/// 并发一致性：两个读线程在写线程持续写入期间反复查询，每次都必须是
	/// 某个已提交快照（返回合法行、最新时间戳不回退），且写完后能看到全部数据。
	#[test]
	fn read_pool_readers_see_consistent_snapshots_while_writes_proceed() {
		use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
		use std::sync::Arc;

		let dir = tempfile::tempdir().unwrap();
		let store = Arc::new(Mutex::new(Store::open(dir.path()).unwrap()));
		{
			let mut guard = store.lock().unwrap();
			for i in 0..100_i64 {
				let mut meta =
					test_item_meta(&format!("item-seed-{i:03}"), &format!("sha-s-{i:03}"));
				meta.created_ts_ms = i;
				guard.insert_remote_item("acct", &meta, i).unwrap();
			}
		}
		let reader = Arc::new(StoreReader::with_size(dir.path(), 2));

		let writer_store = Arc::clone(&store);
		let writer = std::thread::spawn(move || {
			for i in 0..300_i64 {
				let mut meta = test_item_meta(&format!("item-w-{i:03}"), &format!("sha-w-{i:03}"));
				meta.created_ts_ms = 1_000 + i;
				writer_store
					.lock()
					.unwrap()
					.insert_remote_item("acct", &meta, 1_000 + i)
					.unwrap();
			}
		});

		let stop = Arc::new(AtomicBool::new(false));
		let mut readers = Vec::new();
		for _ in 0..2 {
			let reader = Arc::clone(&reader);
			let stop = Arc::clone(&stop);
			readers.push(std::thread::spawn(move || {
				let mut newest = i64::MIN;
				for _ in 0..200 {
					if stop.load(AtomicOrdering::Relaxed) {
						break;
					}
					let list = reader.list_history_metas("acct", 100).unwrap();
					assert!(!list.is_empty(), "查询不得返回空快照");
					let max_ts = list.iter().map(|m| m.created_ts_ms).max().unwrap();
					assert!(max_ts >= newest, "只读快照不得回退（{newest} -> {max_ts}）");
					newest = max_ts;
				}
			}));
		}

		writer.join().unwrap();
		stop.store(true, AtomicOrdering::Relaxed);
		for handle in readers {
			handle.join().unwrap();
		}

		let all = reader.list_history_metas("acct", 1_000).unwrap();
		assert_eq!(all.len(), 400, "写入完成后读池必须看到全部 400 条");
	}
}
