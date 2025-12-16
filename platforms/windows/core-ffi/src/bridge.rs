use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;

use cb_core::clipboard::ClipboardSnapshot;
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
    })
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FfiSnapshot {
    Text { text_utf8: String, ts_ms: i64 },
    Image { mime: String, bytes_b64: String, ts_ms: i64 },
}

pub fn parse_snapshot(json: &str) -> anyhow::Result<ClipboardSnapshot> {
    let s: FfiSnapshot = serde_json::from_str(json).context("invalid snapshot_json")?;
    match s {
        FfiSnapshot::Text { text_utf8, ts_ms } => Ok(ClipboardSnapshot::Text { text_utf8, ts_ms }),
        FfiSnapshot::Image { mime, bytes_b64, ts_ms } => {
            let bytes = B64.decode(bytes_b64.as_bytes()).context("invalid bytes_b64")?;
            Ok(ClipboardSnapshot::Image { bytes, mime, ts_ms })
        }
    }
}
