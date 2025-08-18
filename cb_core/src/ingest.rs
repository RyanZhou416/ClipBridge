// cb_core/src/ingest.rs
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

/// 最近去重窗口大小（可调）
const RECENT_WINDOW: usize = 256;

static RECENT: Lazy<Mutex<(HashSet<String>, VecDeque<String>)>> = Lazy::new(|| {
    Mutex::new((HashSet::new(), VecDeque::new()))
});

#[derive(Debug, Clone)]
pub struct PreparedMeta {
    /// 建议用于 clip_meta.id 的规范化ID（当前：sha256 前16位，前缀 "t-"）
    pub id: String,
    /// MIME 建议值
    pub preview_mime: &'static str,
    /// 文本 UTF-8 字节长度（可做 size/preview 参考）
    pub preview_size: u32,
    /// 内容指纹（全量 sha256 hex）
    pub sha256_hex: String,
}

/// 根据文本计算指纹与规范化ID；若在“最近窗口”内出现过，返回 None（去重命中）
/// 否则返回 PreparedMeta
pub fn ingest_local_text_prepare(text_utf8: &str) -> Option<PreparedMeta> {
    // 1) 指纹
    let sha = Sha256::digest(text_utf8.as_bytes());
    let sha_hex = hex::encode(sha);
    let id = format!("t-{}", &sha_hex[..16]);

    // 2) 最近窗口去重
    {
        let mut guard = RECENT.lock().unwrap();
        let (set, queue) = &mut *guard;
        if set.contains(&sha_hex) {
            return None;
        }
        set.insert(sha_hex.clone());
        queue.push_back(sha_hex.clone());
        if queue.len() > RECENT_WINDOW {
            if let Some(old) = queue.pop_front() {
                set.remove(&old);
            }
        }
    }

    // 3) 生成 PreparedMeta
    Some(PreparedMeta {
        id,
        preview_mime: "text/plain",
        preview_size: text_utf8.len() as u32,
        sha256_hex: sha_hex,
    })
}
