//cb_core/src/api.rs
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, Arc};

pub const PROTOCOL_VERSION: u32 = 1;

// ===== 错误（极简版） =====
#[derive(Debug, Clone)]
pub struct CbError {
    pub code: i32,
    pub message: String,
}
impl CbError {
    pub fn invalid(msg: impl Into<String>) -> Self { Self { code: -2, message: msg.into() } }
    pub fn internal(msg: impl Into<String>) -> Self { Self { code: -1, message: msg.into() } }
}
pub type CbResult<T> = Result<T, CbError>;

// ===== DTO（裁剪到本阶段需要的最小字段） =====
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardSnapshot {
    pub item_id: Option<String>,     // 壳可传，空则 core 生成
    pub mimes: Vec<String>,          // 如 ["text/plain"]
    pub size_bytes: u64,             // 文本字节数
    pub sha256_hex: String,          // 正文指纹
    pub preview_json: Option<String>,// 文本前缀等
    pub created_at: i64,             // unix ts 秒
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemMeta {
    pub protocol_version: u32,
    pub item_id: String,
    pub owner_device_id: String,     // 先占位，后续接设备/配对
    pub mimes: Vec<String>,
    pub size_bytes: u64,
    pub sha256_hex: String,
    pub preview_json: Option<String>,
    pub uri: String,                 // “cb://local/item/{id}” 暂占
    pub created_at: i64,
}

// ===== Core 配置 & Core 实例骨架 =====
#[derive(Clone, Debug)]
pub struct CbConfig {
    pub device_name: String,
    pub data_dir: std::path::PathBuf,
    pub cache_dir: std::path::PathBuf,
}

pub struct CbCore {
    cfg: CbConfig,
    // 去重窗口
    recent: Mutex<(HashSet<String>, VecDeque<String>)>,
}

static CORE: Lazy<Mutex<Option<Arc<CbCore>>>> = Lazy::new(|| Mutex::new(None));

impl CbCore {
    pub fn init(cfg: CbConfig) -> CbResult<()> {
        let mut g = CORE.lock().unwrap();
        if g.is_some() { return Ok(()); }
        let core = CbCore {
            cfg,
            recent: Mutex::new((HashSet::new(), VecDeque::new())),
        };
        *g = Some(Arc::new(core));
        Ok(())
    }
    pub fn shutdown() {
        let mut g = CORE.lock().unwrap();
        *g = None;
    }
    pub fn with<F, T>(f: F) -> CbResult<T>
    where F: FnOnce(&CbCore) -> CbResult<T> {
        let g = CORE.lock().unwrap();
        let s = g.as_ref().ok_or_else(|| CbError::internal("core not initialized"))?;
        f(s)
    }

    /// ingest_local_copy：去重+生成 item_id，返回标准化的 ItemMeta（这一步先不落库）
    pub fn ingest_local_copy(&self, mut snap: ClipboardSnapshot) -> CbResult<ItemMeta> {
        if snap.mimes.is_empty() { return Err(CbError::invalid("mimes empty")); }
        // 1) 最近窗口去重（基于 sha256）
        const WIN: usize = 256;
        {
            let mut guard = self.recent.lock().unwrap();
            let (set, queue) = &mut *guard;
            if set.contains(&snap.sha256_hex) {
                return Err(CbError{ code: 1, message: "duplicate".into() }); // 1 表示去重命中
            }
            set.insert(snap.sha256_hex.clone());
            queue.push_back(snap.sha256_hex.clone());
            if queue.len() > WIN {
                if let Some(old) = queue.pop_front() { set.remove(&old); }
            }
        }
        // 2) 标准化 item_id：优先使用 snapshot.item_id，否则用 sha256 前16位
        let item_id = snap.item_id.take().unwrap_or_else(|| {
            format!("t-{}", &snap.sha256_hex[..16])
        });
        // 3) 组装 ItemMeta（owner 先占位）
        let owner = "device-local";
        let meta = ItemMeta {
            protocol_version: PROTOCOL_VERSION,
            item_id: item_id.clone(),
            owner_device_id: owner.to_string(),
            mimes: snap.mimes,
            size_bytes: snap.size_bytes,
            sha256_hex: snap.sha256_hex,
            preview_json: snap.preview_json,
            uri: format!("cb://{}/item/{}", owner, item_id),
            created_at: snap.created_at,
        };
        Ok(meta)
    }
}

// 工具：计算文本的 sha256（壳也可自己算；这里备用）
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}
