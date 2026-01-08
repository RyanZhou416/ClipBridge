use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{FileMeta, ItemKind, ItemMeta};

pub struct Store {
    pub(crate) conn: Connection,
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

    /// 初始化数据库模式（统一的初始化函数，合并了所有迁移）
    ///
    /// 此函数直接创建完整的数据库结构，不进行版本检查。
    /// 开发阶段，每次启动都重新创建表结构（使用 CREATE TABLE IF NOT EXISTS）。
    ///
    /// # 模式
    /// - `items`: 存储项的元数据
    /// - `history`: 追踪项的历史数据
    /// - `content_cache`: 存储内容缓存信息
    /// - `trusted_peers`: 记录已信任的设备指纹
    ///
    /// # 参数
    /// - `conn`: 指向 SQLite 数据库连接的引用
    ///
    /// # 返回值
    /// - `Ok(())`: 如果初始化成功
    /// - `Err(anyhow::Error)`: 如果初始化失败
    fn init_schema(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            r#"
            -- items 表：存储项的元数据
            CREATE TABLE IF NOT EXISTS items (
              item_id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              owner_device_id TEXT NOT NULL,
              created_ts_ms INTEGER NOT NULL,
              size_bytes INTEGER NOT NULL,
              mime TEXT NOT NULL,
              sha256_hex TEXT NOT NULL,
              preview_json TEXT,
              files_json TEXT,
              expires_ts_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_ts_ms);
            CREATE INDEX IF NOT EXISTS idx_items_sha256 ON items(sha256_hex);
            CREATE INDEX IF NOT EXISTS idx_items_owner_created ON items(owner_device_id, created_ts_ms);

            -- history 表：追踪项的历史数据
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
            CREATE UNIQUE INDEX IF NOT EXISTS idx_history_account_item ON history(account_uid, item_id);

            -- content_cache 表：存储内容缓存信息
            CREATE TABLE IF NOT EXISTS content_cache (
              sha256_hex TEXT PRIMARY KEY,
              total_bytes INTEGER NOT NULL,
              present INTEGER NOT NULL,
              last_access_ts_ms INTEGER NOT NULL,
              created_ts_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cache_lru ON content_cache(present, last_access_ts_ms);
            CREATE INDEX IF NOT EXISTS idx_cache_size ON content_cache(present, total_bytes);

            -- trusted_peers 表：记录已信任的设备指纹
            CREATE TABLE IF NOT EXISTS trusted_peers (
                account_uid TEXT NOT NULL,
                device_id TEXT NOT NULL,
                fingerprint_sha256 TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (account_uid, device_id)
            );
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
    ///     preview: Some(Preview { thumbnail_url: "http://example.com/thumbnail.png".to_string() }),
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

    /// 从具有指定账号 UID 的用户历史记录中获取项元数据列表。
    ///
    /// 此函数查询数据库，以检索由给定 `account_uid` 标识的用户历史中存在的项元数据。
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
    /// - `expires_ts_ms`: 项的过期时间戳（如果适用）。
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
    ///   `preview_json`, `files_json`, `expires_ts_ms` 和 `total_bytes`。
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

    /// 获取已保存的设备指纹
    pub fn get_peer_fingerprint(&self, account_uid: &str, device_id: &str) -> anyhow::Result<Option<String>> {
        let res: Option<String> = self.conn.query_row(
            "SELECT fingerprint_sha256 FROM trusted_peers WHERE account_uid=?1 AND device_id=?2",
            params![account_uid, device_id],
            |r| r.get(0),
        ).optional()?;
        Ok(res)
    }

    /// 保存/更新设备指纹 (TOFU pinning)
    pub fn save_peer_fingerprint(&mut self, account_uid: &str, device_id: &str, fingerprint: &str, now_ms: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trusted_peers (account_uid, device_id, fingerprint_sha256, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![account_uid, device_id, fingerprint, now_ms]
        )?;
        Ok(())
    }

    // [新增] M2: 处理远端广播来的元数据
    // 返回 true 表示这是新数据（需要触发事件），false 表示已存在（忽略）
    pub fn insert_remote_item(
        &mut self,
        account_uid: &str,
        meta: &ItemMeta,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let tx = self.conn.transaction()?;

        // 1. content_cache: 远端来的默认 present=0 (Lazy Fetch)
        // 如果本地已存在(可能曾经下载过)，IGNORE 会保留原有状态
        tx.execute(
            r#"INSERT OR IGNORE INTO content_cache
               (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
               VALUES (?1, ?2, 0, ?3, ?4)"#,
            params![meta.content.sha256, meta.content.total_bytes, now_ms, meta.created_ts_ms],
        )?;

        // 2. items: 插入元数据
        let preview_json = serde_json::to_string(&meta.preview)?;
        let files_json = serde_json::to_string(&meta.files)?;

        tx.execute(
            r#"INSERT OR IGNORE INTO items
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

        // 3. history: 插入历史
        // 使用 INSERT OR IGNORE。如果 (account_uid, item_id) 已存在，则不执行插入，changes() 为 0
        let changes = tx.execute(
            r#"INSERT OR IGNORE INTO history(account_uid, item_id, sort_ts_ms, source_device_id)
               VALUES (?1, ?2, ?3, ?4)"#,
            params![account_uid, meta.item_id, meta.created_ts_ms, meta.source_device_id],
        )?;

        tx.commit()?;

        // 如果 history 表发生了写入，说明这是一条新记录
        Ok(changes > 0)
    }

    /// M3: 获取 item 的 owner (用于 NetManager 路由)
    pub fn get_item_owner(&self, item_id: &str) -> anyhow::Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT owner_device_id FROM items WHERE item_id = ?")?;
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
        let mut stmt = self.conn.prepare("SELECT sha256_hex FROM items WHERE item_id = ?")?;
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
        let mut stmt = self.conn.prepare("SELECT mime FROM items WHERE item_id = ?")?;
        let mut rows = stmt.query([item_id])?;
        if let Some(row) = rows.next()? {
            let mime: String = row.get(0)?;
            Ok(Some(mime))
        } else {
            Ok(None)
        }
    }

    /// M3-3: 获取 FileList 中指定 file_id 的详细信息 (sha256, rel_name, size)
    pub fn get_file_meta(&self, item_id: &str, file_id: &str) -> anyhow::Result<Option<crate::model::FileMeta>> {
        let mut stmt = self.conn.prepare("SELECT files_json FROM items WHERE item_id = ?")?;
        let mut rows = stmt.query([item_id])?;

        if let Some(row) = rows.next()? {
            let json_str: Option<String> = row.get(0)?;
            if let Some(s) = json_str {
                let files: Vec<crate::model::FileMeta> = serde_json::from_str(&s).unwrap_or_default();
                // 找到对应的 file_id
                let target = files.into_iter().find(|f| f.file_id == file_id);
                return Ok(target);
            }
        }
        Ok(None)
    }

    /// 清空核心数据库的所有表
    pub fn clear_core_db(&mut self) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM history", [])?;
        tx.execute("DELETE FROM items", [])?;
        tx.execute("DELETE FROM content_cache", [])?;
        tx.execute("DELETE FROM trusted_peers", [])?;
        tx.commit()?;
        Ok(())
    }
}
