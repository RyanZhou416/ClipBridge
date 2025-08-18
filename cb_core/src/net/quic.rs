// cb_core/src/net/quic.rs
//! QUIC/HTTP3 懒取（Lazy Fetch）通道（v1 占位实现）
//!
//! 设计目标：
//! - 为 v1 提供稳定的拉取接口（签名不变，内部可从“占位”无缝升级到 quinn/rustls 等真正实现）；
//! - 支持传输进度回调；
//!
//! v1 当前实现：
//! - 不做真实网络连接；直接生成一份**占位正文**并通过回调上报 0%→100% 进度；
//! - 后续接 quinn 时，保持 `fetch_lazy` 的签名不变即可；
//!
//! 公开 API：
//! - [`QuicClient::new()`]：构造客户端；
//! - [`QuicClient::fetch_lazy`]：按 `ItemMeta + prefer_mime` 拉取正文，返回字节数组。

use crate::api::CbResult;
use crate::proto::ItemMeta;

/// QUIC 客户端句柄（v1 为无状态占位）。
#[derive(Default)]
pub struct QuicClient;

impl QuicClient {
    /// 构造一个客户端句柄。
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// 懒取正文：根据 `meta.uri`、`prefer_mime` 从源设备拉取数据。
    ///
    /// - `on_progress`: 传输进度回调（`done`, `total`），可用于 UI 显示与统计；
    /// - 返回：整个正文的字节数组；
    ///
    /// v1：生成占位内容（非真实网络），仍然调用一次 0% 和一次 100% 进度，方便壳层联调。
    pub fn fetch_lazy<F>(&self, meta: &ItemMeta, prefer_mime: &str, mut on_progress: F) -> CbResult<Vec<u8>>
    where
        F: FnMut(u64, u64),
    {
        // total 用 meta.size_bytes 提示 UI（真实实现应以 Content-Length 或流量统计为准）
        let total = meta.size_bytes;
        on_progress(0, total);

        // v1：占位内容（把先前的 placeholder 逻辑迁到这里）
        let bytes = placeholder_content(meta, prefer_mime);

        on_progress(total, total);
        Ok(bytes)
    }

    // 未来接口（预留）：广播元数据、握手、连接池、带宽/并发控制等
    // pub fn broadcast_meta(&self, meta: &ItemMeta) { ... }
}

/// 生成占位正文（用于 v1 无网络实现）。
fn placeholder_content(meta: &ItemMeta, mime: &str) -> Vec<u8> {
    if mime.starts_with("text/") {
        let s = format!(
            "ClipBridge LazyFetch Placeholder\n\
             item_id: {}\nfrom: {}\nsha256: {}\nbytes(meta): {}\n\npreview: {}\n",
            meta.item_id,
            meta.owner_device_id,
            meta.sha256_hex,
            meta.size_bytes,
            meta.preview_json
        );
        s.into_bytes()
    } else if mime.starts_with("image/") {
        // 简单二进制占位（非有效图片）
        b"CB_PLACEHOLDER_IMAGE".to_vec()
    } else {
        b"CB_PLACEHOLDER_DATA".to_vec()
    }
}
