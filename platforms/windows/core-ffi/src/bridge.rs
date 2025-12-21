use anyhow::{Context, Error};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::Deserialize;

use cb_core::clipboard::{ClipboardFileEntry, ClipboardSnapshot};
use cb_core::policy::Limits;


#[derive(Deserialize)]
pub struct FfiCfg {
    pub device_id: String,
    pub device_name: String,
    pub account_uid: String,
    pub account_tag: String,
    pub data_dir: String,
    pub cache_dir: String,
    #[serde(default)]
    pub limits: Option<Limits>,
    pub gc_history_max_items: String,
    pub gc_cas_max_bytes: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
    Default,
    LocalOnly,
    Force,
}
impl Default for ShareMode {
    fn default() -> Self { ShareMode::Default }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotKind {
    Text,
    Image,
    FileList,
}

#[derive(Debug, Deserialize)]
struct TextDto {
    #[serde(default)]
    mime: Option<String>,
    utf8: String,
}

#[derive(Debug, Deserialize)]
struct ImageDto {
    mime: String,
    bytes_b64: String,
}

#[derive(Debug, Deserialize)]
struct ClipboardSnapshotDto {
    #[serde(rename = "type")]
    ty: Option<String>,

    ts_ms: i64,

    #[serde(default)]
    source_app: Option<String>,

    kind: SnapshotKind,

    #[serde(default)]
    share_mode: ShareMode,

    #[serde(default)]
    text: Option<TextDto>,
    #[serde(default)]
    image: Option<ImageDto>,
    #[serde(default)]
    files: Vec<ClipboardFileEntry>,
}




pub fn parse_cfg(json: &str) -> anyhow::Result<cb_core::api::CoreConfig> {
    let c: FfiCfg = serde_json::from_str(json).context("invalid cfg_json")?;
    Ok(cb_core::api::CoreConfig {
        device_id: c.device_id,
        device_name: c.device_name,
        account_uid: c.account_uid,
        account_tag: c.account_tag,
        data_dir: c.data_dir,
        cache_dir: c.cache_dir,
        limits: c.limits.unwrap_or_default(),
        gc_history_max_items: c.gc_history_max_items.parse()?,
        gc_cas_max_bytes: c.gc_cas_max_bytes.parse()?,
        global_policy: Default::default(),
    })
}



pub fn parse_snapshot(json: &str) -> anyhow::Result<(ClipboardSnapshot, ShareMode)> {
    let dto: ClipboardSnapshotDto =
        serde_json::from_str(json).context("invalid snapshot_json")?;

    // type 字段：建议严格
    if dto.ty.as_deref() != Some("ClipboardSnapshot") {
        anyhow::bail!("invalid snapshot.type: expected ClipboardSnapshot");
    }

    let ts_ms = dto.ts_ms;

    let snap = match dto.kind {
        SnapshotKind::Text => {
            let t = dto.text.context("kind=text requires .text")?;
            ClipboardSnapshot::Text { text_utf8: t.utf8, ts_ms }
        }
        SnapshotKind::Image => {
            let i = dto.image.context("kind=image requires .image")?;
            let bytes = B64.decode(i.bytes_b64.as_bytes()).context("invalid image.bytes_b64")?;
            ClipboardSnapshot::Image { bytes, mime: i.mime, ts_ms }
        }
        SnapshotKind::FileList => {
            if dto.files.is_empty() { anyhow::bail!("kind=file_list requires non-empty files[]"); }
            ClipboardSnapshot::FileList { files: dto.files, ts_ms }
        }
    };

    Ok((snap, dto.share_mode))
}


