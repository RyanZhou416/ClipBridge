use uuid::Uuid;

use crate::model::{ItemContent, ItemKind, ItemMeta, ItemPreview};
use crate::util::{sha256_hex, truncate_chars};
use crate::policy::{Limits, MetaStrategy, PolicyOutcome, decide};

pub enum ClipboardSnapshot {
    Text { text_utf8: String, ts_ms: i64 },
    Image { bytes: Vec<u8>, mime: String, ts_ms: i64 },
}

pub struct LocalIngestDeps<'a> {
    pub device_id: &'a str,
    pub device_name: &'a str,
    pub account_uid: &'a str,
}

#[derive(Clone, Debug)]
pub struct IngestPlan {
    pub meta: ItemMeta,
    pub strategy: MetaStrategy,
    pub needs_user_confirm: bool,
    pub content_bytes: Vec<u8>, // 真正要写入 CAS 的字节
}


pub fn build_item_meta(deps: &LocalIngestDeps<'_>, snap: &ClipboardSnapshot) -> ItemMeta {
    let item_id = Uuid::new_v4().to_string();

    match snap {
        ClipboardSnapshot::Text { text_utf8, ts_ms } => {
            let bytes = text_utf8.as_bytes();
            let sha = sha256_hex(bytes);
            let preview = truncate_chars(text_utf8, 300);

            ItemMeta {
                ty: "ItemMeta".to_string(),
                item_id,
                kind: ItemKind::Text,
                created_ts_ms: *ts_ms,
                source_device_id: deps.device_id.to_string(),
                source_device_name: Some(deps.device_name.to_string()),
                size_bytes: bytes.len() as i64,
                preview: ItemPreview { text: Some(preview), ..Default::default() },
                content: ItemContent { mime: "text/plain".to_string(), sha256: sha, total_bytes: bytes.len() as i64 },
                files: vec![],
                // v1 可默认 created+7d :contentReference[oaicite:13]{index=13}
                expires_ts_ms: Some(*ts_ms + 7 * 24 * 3600 * 1000),
            }
        }
        ClipboardSnapshot::Image { bytes, mime, ts_ms } => {
            let sha = sha256_hex(bytes);

            ItemMeta {
                ty: "ItemMeta".to_string(),
                item_id,
                kind: ItemKind::Image,
                created_ts_ms: *ts_ms,
                source_device_id: deps.device_id.to_string(),
                source_device_name: Some(deps.device_name.to_string()),
                size_bytes: bytes.len() as i64,
                preview: ItemPreview::default(), // 图片预览 hint 后面再加
                content: ItemContent { mime: mime.clone(), sha256: sha, total_bytes: bytes.len() as i64 },
                files: vec![],
                expires_ts_ms: Some(*ts_ms + 7 * 24 * 3600 * 1000),
            }
        }
    }
}

pub fn make_ingest_plan(
    deps: &LocalIngestDeps<'_>,
    snap: &ClipboardSnapshot,
    limits: &Limits,
    force: bool,
) -> anyhow::Result<IngestPlan> {
    // 1) 先生成 ItemMeta（纯计算）
    let meta = build_item_meta(deps, snap);

    // 2) 根据大小/类型走 policy，决定策略/是否需要弹窗
    let outcome = decide(meta.kind.clone(), meta.size_bytes, force, limits);

    let (strategy, needs_user_confirm) = match outcome {
        PolicyOutcome::RejectedHardCap { code } => {
            // 超过硬上限：直接报错，后面不用 Apply
            anyhow::bail!(code);
        }
        PolicyOutcome::Allowed { strategy, needs_user_confirm } => {
            (strategy, needs_user_confirm)
        }
    };

    // 3) 收集要写入 CAS 的 bytes
    let content_bytes = match snap {
        ClipboardSnapshot::Text { text_utf8, .. } => text_utf8.clone().into_bytes(),
        ClipboardSnapshot::Image { bytes, .. } => bytes.clone(),
    };

    Ok(IngestPlan {
        meta,
        strategy,
        needs_user_confirm,
        content_bytes,
    })
}

