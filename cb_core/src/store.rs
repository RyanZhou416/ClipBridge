use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{FileMeta, ItemKind, ItemMeta, ItemPreview};

pub struct Store {
	pub(crate) conn: Connection,
	/// 本次 `open` 是否因为 schema 版本/指纹不匹配而整库重建（见 [`SCHEMA_VERSION`]）。
	pub rebuilt: bool,
}

/// 当前 `core.db` 的 schema 版本。
///
/// **v2 起采用“版本不匹配即整库重建”（drop-and-rebuild），不做逐版本迁移。**
/// 项目没有兼容负担：schemas 一旦变化，`items` / `history` / `content_cache` /
/// `trusted_peers` / `peer_rules` 全部删除重建，配对信任与共享策略也按设计一起丢失，
/// 设备需要重新输入密码配对。这是有意的取舍，不是 bug。
///
/// 任何未来的 DDL 变更（新增/删除列、索引、约束）都必须递增本常量；否则已有库
/// 会带着旧结构继续运行，正是 v1 时代 `CREATE TABLE IF NOT EXISTS` 踩过的坑。
///
/// v3（ADR-002 §3.7）：`content_cache` 新增 `durable INTEGER NOT NULL DEFAULT 1`，
/// 0 表示“blob 已 rename 到最终路径，但后台 `sync_data` 尚未完成”。升级到 v3 的
/// 旧库会走既有 drop-and-rebuild：内容/身份表全部重建，CAS 由 `Core::init` 一并清空，
/// 磁盘对象不可恢复（这是已批准的重建策略，不是本列引入的额外风险）。
pub const SCHEMA_VERSION: i64 = 3;

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

/// v3 的完整 DDL。
///
/// 表职责：
/// - `items`：每个剪贴板条目的元数据（内容本体在 CAS，`sha256_hex` 关联 `content_cache`）。
/// - `history`：按账号的历史视图；软删除只置 `is_deleted=1`，不删行。
/// - `content_cache`：SHA-256 内容的体积、`present` 状态（LRU/GC 依据）与
///   `durable` 持久化标记（1 = 已 fsync 或按策略无需 fsync；0 = 后台同步待完成）。
/// - `trusted_peers`：TOFU 设备指纹（配对信任）。
/// - `peer_rules`：每个设备的共享/接收策略。
///
/// 约束说明（v3 生成）：
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
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA foreign_keys=ON;
            PRAGMA busy_timeout=5000;
            ",
		)?;
		Ok(())
	}

	/// 按 [`SCHEMA_VERSION`] 校验并（必要时）重建数据库模式。
	///
	/// 旧实现只执行 `CREATE TABLE IF NOT EXISTS`，所以任何 DDL 变化都不会作用到
	/// 已存在的库（注释里“每次启动都重新创建”的说法是错的）。这里改为：
	///
	/// 1. 读取 `PRAGMA user_version`；
	/// 2. 与 [`SCHEMA_VERSION`] 不同（含新文件的 0）→ drop-and-rebuild；
	/// 3. 版本号相同也再校验一次 schema 指纹：旧版 `migrate_v1..v3` 曾把
	///    `user_version` 写成 1/2/3，其中“v2”并没有 `files_json`，不能只信版本号；
	/// 4. 重建后把 `user_version` 写为 [`SCHEMA_VERSION`]。
	///
	/// 重建是**有意的**：内容表、身份表（含 `trusted_peers` / `peer_rules`）全部销毁，
	/// 配对数据按设计丢失，设备需要重新配对。日志会明确写出 old → new 版本。
	///
	/// # 返回值
	/// `Ok(true)` 表示本次执行了 drop-and-rebuild；`Ok(false)` 表示结构已是最新。
	fn ensure_schema(conn: &Connection) -> anyhow::Result<bool> {
		let old_version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
		let rebuilt = old_version != SCHEMA_VERSION || !Self::schema_ready(conn)?;
		if !rebuilt {
			return Ok(false);
		}

		// DDL 与版本号写在同一个事务里：要么整套新结构 + 新版本号一起生效，
		// 要么保持旧库（下次启动重试），不会留下“表删了、版本没写”的中间态。
		let tx = conn.unchecked_transaction()?;
		tx.execute_batch(DROP_OWNED_TABLES_DDL)?;
		tx.execute_batch(SCHEMA_DDL)?;
		tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
		tx.commit()?;

		eprintln!(
			"[store] core.db schema changed ({old_version} -> {SCHEMA_VERSION}): \
             dropped and recreated all content/identity tables; trusted_peers/peer_rules \
             are lost by design and devices must re-pair with the password"
		);
		Ok(true)
	}

	/// 检查“版本号相同”的库是否真的具备 v3 结构。
	///
	/// 这是兜底而不是迁移：旧 migrate_v2 时代 `user_version=2` 但 `items` 没有
	/// `files_json`，v1 时代也没有 `trusted_peers` / `peer_rules`；v2 库的
	/// `content_cache` 没有 `durable`。只要关键表或关键列缺失就按旧库整库重建。
	fn schema_ready(conn: &Connection) -> anyhow::Result<bool> {
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
		let mut stmt = self.conn.prepare(
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
			.query_row(params![item_id], Self::row_to_item_meta)
			.optional()?;
		Ok(result)
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
		let mut stmt = self.conn.prepare(
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

		let rows = stmt.query_map(params![account_uid, limit as i64], Self::row_to_item_meta)?;

		let mut out = Vec::new();
		for it in rows {
			out.push(it?);
		}
		Ok(out)
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

		let mut stmt = self.conn.prepare(sql)?;

		let rows = if has_cursor {
			stmt.query_map(
				params![account_uid, limit as i64, cursor.unwrap()],
				Self::row_to_item_meta,
			)?
		} else {
			stmt.query_map(params![account_uid, limit as i64], Self::row_to_item_meta)?
		};

		let mut out = Vec::new();
		for it in rows {
			out.push(it?);
		}
		Ok(out)
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
		let s: i64 = self.conn.query_row(
			"SELECT COALESCE(SUM(total_bytes),0) FROM content_cache WHERE present=1",
			[],
			|r| r.get(0),
		)?;
		Ok(s)
	}

	/// 缓存统计：`present=1` 的 (created_ts_ms, total_bytes)，按时间升序。
	///
	/// 只返回“声称仍存在”的行；`present=0` 的内容已被 GC/删除，不算当前占用。
	pub fn list_present_cache_rows(&self) -> anyhow::Result<Vec<(i64, i64)>> {
		let mut stmt = self.conn.prepare(
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
		let bucket_ms = bucket_ms.max(1);
		let mut stmt = self.conn.prepare(
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
}
