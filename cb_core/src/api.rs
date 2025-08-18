// cb_core/src/api.rs
//! ClipBridge Core — Public API
//!
//! 设计目标：
//! - “**共享内核**”暴露少而稳的 API，供 FFI 桥与各平台壳调用；
//! - 强内聚：设备身份、发现、会话、缓存、历史都由 Core 管；
//! - 弱耦合：壳层只负责 UI/系统剪贴板/系统秘钥读写；
//!
//! 使用方式（典型流程）：
//! 1. 宿主（WinUI / iOS）启动 → 构造 [`CbConfig`] → 实现 [`CbCallbacks`] 与 [`SecureStore`] → 调 `CbCore::init`；
//! 2. 监听系统剪贴板变更并上报：将采集到的 `ClipboardSnapshot(JSON)` 交给 `ingest_local_copy`；
//! 3. Core 去重并生成 [`ItemMeta`]，向对端广播（v1 中网络可先用占位），同时回调 `on_new_metadata`；
//! 4. 另一端粘贴时，调用 `ensure_content_cached(item_id, prefer_mime)`，命中本地 CAS 或从源设备拉流（v1 先用占位文件写入），返回 [`LocalContentRef`]；
//! 5. 壳层据 `LocalContentRef.path` 注入系统剪贴板（延迟渲染）。

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use serde::{Deserialize, Serialize};

use crate::net::{mdns::MdnsHandle, quic::QuicClient};
use crate::proto::{DeviceInfo, ItemMeta, LocalContentRef, Platform, PROTOCOL_VERSION};
use crate::storage::{CasPaths, Storage};

/// 统一的结果类型。
pub type CbResult<T> = Result<T, CbError>;

/// 错误种类（用于壳层分类处理/上报）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CbErrorKind {
    /// 配置/参数错误（不可重试）
    InvalidArg,
    /// 初始化失败（环境/权限/文件系统不可用）
    InitFailed,
    /// 存储层错误（DB/CAS）
    Storage,
    /// 网络层错误（发现/握手/传输）
    Network,
    /// 找不到条目或资源
    NotFound,
    /// 已暂停（策略阻止）
    Paused,
    /// 未预期的内部错误
    Internal,
}

/// Core 错误。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CbError {
    pub kind: CbErrorKind,
    pub message: String,
}

impl CbError {
    fn new(kind: CbErrorKind, msg: impl Into<String>) -> Self {
        Self { kind, message: msg.into() }
    }
}

/// Core 入口：一个进程内建议只持有一个实例。
pub struct CbCore {
    /// 运行时配置（只读）。
    cfg: CbConfig,
    /// 本设备信息（包括稳定的 device_id）。
    device: DeviceInfo,
    /// 回调（壳层实现，Core 在线程/任务中调用）。
    callbacks: Arc<dyn CbCallbacks>,
    /// 安全存储（用于保存私钥/身份/信任列表等；v1 只用到 device_id）。
    secure_store: Arc<dyn SecureStore>,
    /// 存储与缓存（SQLite + CAS）。
    storage: Storage,
    /// CAS 路径计算（缓存根目录）。
    cas_paths: CasPaths,
    /// mDNS 发现句柄（v1 可为假实现）。
    mdns: MdnsHandle,
    /// QUIC 客户端（v1 可为占位）。
    quic: QuicClient,
    /// 暂停标记（策略/用户一键暂停时置位）。
    paused: AtomicBool,
}

/// 运行时配置（可从 JSON/FFI 传入）。
#[derive(Clone, Debug)]
pub struct CbConfig {
    /// 设备在 UI 中显示的名称（例如 “My-PC”/“Ryan’s iPhone”）。
    pub device_name: String,
    /// 数据目录（SQLite/索引等放这里）。
    pub data_dir: PathBuf,
    /// 缓存目录（CAS/临时下载等放这里）。
    pub cache_dir: PathBuf,
    /// 缓存限制与清理策略。
    pub cache_limits: CacheLimits,
    /// 网络选项。
    pub net: NetOptions,
    /// 安全选项。
    pub security: SecurityOptions,
}

/// 缓存限制（v1 最小实现：最大体积/最大条数）。
#[derive(Clone, Debug)]
pub struct CacheLimits {
    pub max_bytes: u64,
    pub max_items: u32,
}

/// 网络选项（v1 最小实现：是否开启 mDNS）。
#[derive(Clone, Debug)]
pub struct NetOptions {
    pub enable_mdns: bool,
    /// 预留：本地监听端口/端口段、首选传输等。
    pub prefer_quic: bool,
}

/// 安全选项（v1 最小实现：是否仅信任列表可连、是否强制加密）。
#[derive(Clone, Debug)]
pub struct SecurityOptions {
    pub trusted_only: bool,
    pub require_encryption: bool,
}

/// 回调接口（壳层实现）。
/// - v1 仅同步方法，避免引入 async trait 依赖；
/// - Core 在内部线程/任务触发这些回调，请确保实现是线程安全/快速返回。
pub trait CbCallbacks: Send + Sync {
    /// 对端设备上线（mDNS 发现 + 可连通）。
    fn on_device_online(&self, _device: &DeviceInfo) {}
    /// 对端设备下线。
    fn on_device_offline(&self, _device_id: &str) {}
    /// 接收到**新元数据**（包括本机复制产生的 meta）。
    fn on_new_metadata(&self, _meta: &ItemMeta) {}
    /// 传输进度（Lazy Fetch）。
    fn on_transfer_progress(&self, _item_id: &str, _done: u64, _total: u64) {}
    /// 错误上报（非致命）。
    fn on_error(&self, _err: &CbError) {}
}

/// 安全存储（壳层提供，用于持久化设备身份/信任表等）。
/// v1 仅要求实现 get/set 任意 bytes；Key 约定用文本。
pub trait SecureStore: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn set(&self, key: &str, value: &[u8]) -> CbResult<()>;
}

/// 历史查询条件（v1：limit/offset + kind 过滤）。
#[derive(Clone, Debug)]
pub struct HistoryQuery {
    pub limit: u32,
    pub offset: u32,
    pub kind: Option<HistoryKind>,
}

/// 历史条目类别。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HistoryKind {
    /// 本机复制（Local → Outbound）
    CopyLocal,
    /// 远端接收（Inbound 元数据）
    RecvRemote,
    /// 本机粘贴（使用了 ensure_content_cached）
    PasteLocal,
}

/// 历史条目（供 UI 列表展示）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub seq_id: i64,
    pub item_id: String,
    pub summary: String, // 从 preview_json 提取/拼接
    pub created_at: i64,
    pub owner_device_id: String,
    pub mimes: Vec<String>,
}

/// 记录详情（元数据 + 是否已缓存正文）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemRecord {
    pub meta: ItemMeta,
    pub present: bool,
}

// ------------------------------ 初始化与生命周期 -----------------------------------

impl CbCore {
    /// 初始化 Core：准备设备身份 / DB / CAS / 发现与网络。
    ///
    /// 约定：一个进程内建议只创建一次实例；多实例会竞争文件锁/端口。
    pub fn init(
        cfg: CbConfig,
        callbacks: Arc<dyn CbCallbacks>,
        secure_store: Arc<dyn SecureStore>,
    ) -> CbResult<Self> {
        // 1) 校验路径存在性（不存在则创建）。
        std::fs::create_dir_all(&cfg.data_dir)
            .map_err(|e| CbError::new(CbErrorKind::InitFailed, format!("data_dir: {e}")))?;
        std::fs::create_dir_all(&cfg.cache_dir)
            .map_err(|e| CbError::new(CbErrorKind::InitFailed, format!("cache_dir: {e}")))?;

        // 2) 设备身份：尝试从 SecureStore 读取，否则生成并写回。
        let device_id_key = "cb.device_id";
        let device_id = if let Some(raw) = secure_store.get(device_id_key) {
            String::from_utf8(raw).unwrap_or_else(|_| gen_uuid_v4())
        } else {
            let id = gen_uuid_v4();
            secure_store
                .set(device_id_key, id.as_bytes())
                .map_err(|e| CbError::new(CbErrorKind::InitFailed, e.message))?;
            id
        };

        let device = DeviceInfo {
            device_id: device_id.clone(),
            device_name: cfg.device_name.clone(),
            platform: Platform::Windows, // 壳层可在 FFI 层重写；这里默认 Windows 便于 MVP
            app_version: None,
            protocol_version: PROTOCOL_VERSION,
        };

        // 3) 打开存储（SQLite + CAS）。
        let storage = Storage::open(cfg.data_dir.clone())
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?;
        let cas_paths = CasPaths::new(cfg.cache_dir.clone());

        // 4) 启动发现（mDNS）与网络（QUIC）占位。
        let mdns = MdnsHandle::start(device.clone(), callbacks.clone());
        let quic = QuicClient::new();

        Ok(Self {
            cfg,
            device,
            callbacks,
            secure_store,
            storage,
            cas_paths,
            mdns,
            quic,
            paused: AtomicBool::new(false),
        })
    }

    /// 返回本设备信息（供壳层诊断/展示）。
    #[must_use]
    pub fn device_info(&self) -> &DeviceInfo {
        &self.device
    }

    /// 暂停/恢复 Core（暂停后，`ingest_* / ensure_*` 返回 `Paused`）。
    pub fn pause(&self, yes: bool) {
        self.paused.store(yes, Ordering::SeqCst);
    }

    /// 关闭 Core（v1 无后台线程资源需要特别回收，留空即可）。
    pub fn shutdown(self) {
        // 占位：需要时在 MdnsHandle/QuicClient 内部实现 Drop 进行清理。
    }
}

// ------------------------------ 业务主流程 ------------------------------------------

impl CbCore {
    /// 本机复制：接收壳层采集的剪贴板快照，生成 `ItemMeta`，入库并广播。
    ///
    /// - Core 会负责生成稳定的 `item_id`（若快照未提供）；
    /// - 执行去重；
    /// - 写入 DB（历史 & items 表）；
    /// - 回调 `on_new_metadata`；
    /// - （网络）广播给已发现的对端（v1 可忽略/占位日志）。
    pub fn ingest_local_copy(
        &self,
        mut snapshot: crate::proto::ClipboardSnapshot,
    ) -> CbResult<String> {
        if self.paused.load(Ordering::SeqCst) {
            return Err(CbError::new(CbErrorKind::Paused, "core paused"));
        }

        // 生成/确认 item_id
        let item_id = snapshot.item_id.take().unwrap_or_else(gen_uuid_v4);

        // 组装 ItemMeta
        let meta = ItemMeta {
            protocol_version: PROTOCOL_VERSION,
            item_id: item_id.clone(),
            owner_device_id: self.device.device_id.clone(),
            owner_device_name: Some(self.device.device_name.clone()),
            mimes: snapshot.mimes.clone(),
            size_bytes: snapshot.size_bytes,
            sha256_hex: snapshot.sha256_hex.clone(),
            expires_at: None,
            preview_json: snapshot.preview_json.take(),
            uri: format!("cb://{}/item/{}", self.device.device_id, item_id),
            created_at: snapshot.created_at,
        };

        // 去重与入库
        if let Some(existing_id) = self
            .storage
            .dedup_recent(&meta)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?
        {
            // 命中去重窗口：复用旧的 item_id，避免列表里出现重复条目
            return Ok(existing_id);
        }

        self.storage
            .upsert_item(&meta, /*present=*/ true) // 本机复制 → 本地已“拥有”正文，可视作 present
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?;

        self.storage
            .append_history(&meta, HistoryKind::CopyLocal)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?;

        // 回调给壳层（本机也收到一次，用于 UI 列表立即显示）。
        self.callbacks.on_new_metadata(&meta);

        // TODO(v1+): 通过网络广播给对端（mDNS 列表 → QUIC 控制信道），这里先留占位。
        // self.quic.broadcast_meta(&meta);

        Ok(item_id)
    }

    /// 远端元数据进入本机（通过网络模块上交）。写入 DB、回调、历史标记 `RecvRemote`。
    pub fn ingest_remote_metadata(&self, meta: &ItemMeta) -> CbResult<()> {
        if self.paused.load(Ordering::SeqCst) {
            return Err(CbError::new(CbErrorKind::Paused, "core paused"));
        }

        self.storage
            .upsert_item(meta, /*present=*/ false)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?;

        self.storage
            .append_history(meta, HistoryKind::RecvRemote)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?;

        self.callbacks.on_new_metadata(meta);
        Ok(())
    }

    /// 确保某条目正文已在本地缓存（CAS 命中则直接返回；否则进行“懒取”下载）。
    ///
    /// - `prefer_mime`：在条目提供多种 MIME 表示时，优先选择的那一种；`None` 则按 `meta.mimes[0]`。
    /// - v1 的“下载”实现为**占位**：写入一份示例内容至 CAS 文件并返回，方便壳层先打通“延迟渲染”。
    pub fn ensure_content_cached(
        &self,
        item_id: &str,
        prefer_mime: Option<&str>,
    ) -> CbResult<LocalContentRef> {
        if self.paused.load(Ordering::SeqCst) {
            return Err(CbError::new(CbErrorKind::Paused, "core paused"));
        }

        // 查询元数据
        let rec = self
            .storage
            .get_item(item_id)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?
            .ok_or_else(|| CbError::new(CbErrorKind::NotFound, "item not found"))?;

        let chosen_mime = prefer_mime
            .map(str::to_string)
            .or_else(|| rec.meta.mimes.get(0).cloned())
            .ok_or_else(|| CbError::new(CbErrorKind::InvalidArg, "no mime available"))?;

        // CAS 命中？
        let path = self.cas_paths.path_for_sha256(&rec.meta.sha256_hex);
        if path.exists() {
            // 标记 present & 记历史（粘贴）
            self.storage
                .mark_present(&rec.meta.item_id, &rec.meta.sha256_hex)
                .ok(); // 异常不影响主流程
            self.storage
                .append_history(&rec.meta, HistoryKind::PasteLocal)
                .ok();
            return Ok(LocalContentRef {
                sha256_hex: rec.meta.sha256_hex,
                path,
                mime: chosen_mime,
                size_bytes: rec.meta.size_bytes,
            });
        }

        // 未命中：通过 QUIC 懒取（v1 内部仍为占位实现，签名稳定）
        let item_id_for_cb = rec.meta.item_id.clone();
        let content = self
            .quic
            .fetch_lazy(&rec.meta, &chosen_mime, |done, total| {
                // 将传输进度透传给壳层
                self.callbacks
                    .on_transfer_progress(&item_id_for_cb, done, total);
            })
            .map_err(|e| CbError::new(CbErrorKind::Network, e.message))?;
        let final_path = self
            .storage
            .write_blob(&self.cas_paths, &rec.meta.sha256_hex, &content)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))?;

        // 标记 present & 记历史
        self.storage
            .mark_present(&rec.meta.item_id, &rec.meta.sha256_hex)
            .ok();
        self.storage
            .append_history(&rec.meta, HistoryKind::PasteLocal)
            .ok();


        let out = LocalContentRef {
            sha256_hex: rec.meta.sha256_hex,
            path: final_path,
            mime: chosen_mime,
            size_bytes: content.len() as u64,
        };
        Ok(out)
    }

    /// 查询历史（分页）。
    pub fn list_history(&self, q: HistoryQuery) -> CbResult<Vec<HistoryEntry>> {
        self.storage
            .list_history(q.limit, q.offset, q.kind.as_ref())
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))
    }

    /// 获取条目详情（元数据 + present）。
    pub fn get_item(&self, item_id: &str) -> CbResult<Option<ItemRecord>> {
        self.storage
            .get_item(item_id)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))
    }

    /// 清理缓存（依据 [`CacheLimits`] 简单策略）。v1：按 LRU 近似清理。
    pub fn prune_cache(&self) -> CbResult<()> {
        self.storage
            .prune_cache(&self.cas_paths, &self.cfg.cache_limits)
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))
    }

    /// 清理历史（v1：保留最近 N 条/或按时间线裁剪；这里给出占位实现）。
    pub fn prune_history(&self) -> CbResult<()> {
        self.storage
            .prune_history()
            .map_err(|e| CbError::new(CbErrorKind::Storage, e.to_string()))
    }
}

// ------------------------------ 工具 -----------------------------------------------
fn gen_uuid_v4() -> String {
    // 依赖 uuid crate（Cargo.toml: uuid = { version = "1", features = ["v4"] }）
    uuid::Uuid::new_v4().to_string()
}
// 注意：占位内容的实现已迁移到 `net/quic.rs::placeholder_content` 内部。

// ------------------------------ 单元测试 -------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{ClipboardSnapshot, PROTOCOL_VERSION};
    use serde_json::json;
    use std::sync::Mutex;
    use std::path::PathBuf;

    struct MemStore(Mutex<std::collections::HashMap<String, Vec<u8>>>);
    impl SecureStore for MemStore {
        fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set(&self, key: &str, value: &[u8]) -> CbResult<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_vec());
            Ok(())
        }
    }

    struct NopCb;
    impl CbCallbacks for NopCb {}

    #[test]
    fn init_and_ingest_roundtrip() {
        let cfg = CbConfig {
            device_name: "My-PC".into(),
            data_dir: temp_dir("data"),
            cache_dir: temp_dir("cache"),
            cache_limits: CacheLimits { max_bytes: 32 * 1024 * 1024, max_items: 1000 },
            net: NetOptions { enable_mdns: true, prefer_quic: true },
            security: SecurityOptions { trusted_only: false, require_encryption: true },
        };
        let core = CbCore::init(
            cfg,
            Arc::new(NopCb),
            Arc::new(MemStore(Mutex::new(Default::default()))),
        )
        .unwrap();

        let snap = ClipboardSnapshot {
            item_id: None,
            mimes: vec!["text/plain; charset=utf-8".into()],
            size_bytes: 12,
            sha256_hex: "abc123".into(),
            preview_json: json!({"head":"hello"}),
            created_at: 1_700_000_000_000,
        };
        let id = core.ingest_local_copy(snap).unwrap();

        // ensure
        let loc = core.ensure_content_cached(&id, Some("text/plain")).unwrap();
        assert!(loc.path.exists());
        assert_eq!(loc.mime, "text/plain");

        // query
        let rec = core.get_item(&id).unwrap().unwrap();
        assert_eq!(rec.meta.item_id, id);

        // history
        let h = core
            .list_history(HistoryQuery { limit: 10, offset: 0, kind: None })
            .unwrap();
        assert!(!h.is_empty());

        // versions
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cb_core_test_{tag}_{}", rand_u32()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn rand_u32() -> u32 {
        // 依赖 rand crate（Cargo.toml: rand = "0.8"）
        rand::random()
    }
}
