use uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::model::{FileMeta, ItemContent, ItemKind, ItemMeta, ItemPreview};
use crate::util::{sha256_hex, truncate_chars};
use crate::policy::{SizeLimits, MetaStrategy, PolicyOutcome, decide};

const FILELIST_MIME: &str = "application/x-clipbridge-filelist+json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardFileEntry {
    pub rel_name: String,
	#[serde(default)]
	pub abs_path: Option<String>,
    pub size_bytes: i64,
    pub sha256: Option<String>,
}

pub enum ClipboardSnapshot {
    Text { text_utf8: String, ts_ms: i64 },
    Image { bytes: Vec<u8>, mime: String, ts_ms: i64 },
    FileList { files: Vec<ClipboardFileEntry>, ts_ms: i64 },
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
    pub content_bytes: Vec<u8>, // 真正要写入 CAS 的字节（FileList 写的是 manifest JSON）
}

fn snapshot_content_bytes(snap: &ClipboardSnapshot) -> Vec<u8> {
    match snap {
        ClipboardSnapshot::Text { text_utf8, .. } => text_utf8.as_bytes().to_vec(),
        ClipboardSnapshot::Image { bytes, .. } => bytes.clone(),
        ClipboardSnapshot::FileList { files, .. } => {
            // 稳定序列化：把 file list 当成“manifest”，CAS 去重的是 manifest
            serde_json::to_vec(files).unwrap_or_else(|_| b"[]".to_vec())
        }
    }
}

pub fn build_item_meta(deps: &LocalIngestDeps<'_>, snap: &ClipboardSnapshot) -> ItemMeta {
    let item_id = Uuid::new_v4().to_string();

    match snap {
        ClipboardSnapshot::Text { text_utf8, ts_ms } => {
            let bytes = snapshot_content_bytes(snap);
            let sha = sha256_hex(&bytes);
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
                expires_ts_ms: Some(*ts_ms + 7 * 24 * 3600 * 1000),
            }
        }
        ClipboardSnapshot::Image { bytes, mime, ts_ms } => {
            let bytes2 = snapshot_content_bytes(snap);
            let sha = sha256_hex(&bytes2);

            ItemMeta {
                ty: "ItemMeta".to_string(),
                item_id,
                kind: ItemKind::Image,
                created_ts_ms: *ts_ms,
                source_device_id: deps.device_id.to_string(),
                source_device_name: Some(deps.device_name.to_string()),
                size_bytes: bytes.len() as i64,
                preview: ItemPreview::default(),
                content: ItemContent { mime: mime.clone(), sha256: sha, total_bytes: bytes.len() as i64 },
                files: vec![],
                expires_ts_ms: Some(*ts_ms + 7 * 24 * 3600 * 1000),
            }
        }
        ClipboardSnapshot::FileList { files, ts_ms } => {
            let manifest = snapshot_content_bytes(snap);
            let sha = sha256_hex(&manifest);

            let total_files_bytes: i64 = files.iter().map(|f| f.size_bytes).sum();

			let metas: Vec<FileMeta> = files.iter().map(|f| FileMeta {
				file_id: Uuid::new_v4().to_string(),
				rel_name: f.rel_name.clone(),
				size_bytes: f.size_bytes,
				sha256: f.sha256.clone(),
				local_path: f.abs_path.clone(),
			}).collect();

            ItemMeta {
                ty: "ItemMeta".to_string(),
                item_id,
                kind: ItemKind::FileList,
                created_ts_ms: *ts_ms,
                source_device_id: deps.device_id.to_string(),
                source_device_name: Some(deps.device_name.to_string()),
                size_bytes: total_files_bytes, // 注意：FileList 的 size_bytes 是“文件总大小”
                preview: ItemPreview { file_count: Some(metas.len() as u32), ..Default::default() },
                content: ItemContent { mime: FILELIST_MIME.to_string(), sha256: sha, total_bytes: manifest.len() as i64 },
                files: metas,
                expires_ts_ms: Some(*ts_ms + 7 * 24 * 3600 * 1000),
            }
        }
    }
}

pub fn make_ingest_plan(
	deps: &LocalIngestDeps<'_>,
	snap: &ClipboardSnapshot,
	limits: &SizeLimits,
	force: bool,
) -> anyhow::Result<IngestPlan> {
    let meta = build_item_meta(deps, snap);

    let outcome = decide(meta.kind.clone(), meta.size_bytes, force, limits);
    let (strategy, needs_user_confirm) = match outcome {
        PolicyOutcome::RejectedHardCap { code } => anyhow::bail!(code),
        PolicyOutcome::Allowed { strategy, needs_user_confirm } => (strategy, needs_user_confirm),
    };

    let content_bytes = snapshot_content_bytes(snap);

    Ok(IngestPlan { meta, strategy, needs_user_confirm, content_bytes })
}
