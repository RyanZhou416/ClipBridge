// cb_core/src/models.rs
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClipKind {
    Text,
    Image,
    FileRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipItem {
    pub kind: ClipKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    // 预留：图片/二进制用 base64；M2/M6 扩展
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_hint: Option<String>,
    /// 内容指纹（当前用 sha256）
    pub content_sha256: String,
}

impl ClipItem {
    pub fn make_text(s: impl Into<String>) -> Self {
        let s = s.into();
        let mut hasher = Sha256::new();
        hasher.update(s.as_bytes());
        let hash = hex::encode(hasher.finalize());
        Self {
            kind: ClipKind::Text,
            text: Some(s),
            blob_b64: None,
            file_hint: None,
            content_sha256: hash,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MsgType {
    Meta,
    Clip,
    Ack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(rename = "type")]
    pub ty: MsgType,
    pub ts_epoch_ms: i64,
    pub src_device_id: String,
    pub nonce: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ClipItem>,
}

impl Envelope {
    pub fn now_ms() -> i64 {
        let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        dur.as_millis() as i64
    }
    pub fn new_nonce() -> String {
        let mut buf = [0u8; 16];
        OsRng.fill_bytes(&mut buf);
        hex::encode(buf)
    }
    pub fn make_text(src_device_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            ty: MsgType::Clip,
            ts_epoch_ms: Self::now_ms(),
            src_device_id: src_device_id.into(),
            nonce: Self::new_nonce(),
            item: Some(ClipItem::make_text(text)),
        }
    }
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialize envelope")
    }
    pub fn from_json_bytes(b: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(b)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_text() {
        let env = Envelope::make_text("My-PC", "hello CB");
        let b = env.to_json_bytes();
        let de = Envelope::from_json_bytes(&b).unwrap();
        assert_eq!(de.ty, MsgType::Clip);
        assert!(de.item.is_some());
        assert_eq!(de.src_device_id, "My-PC");
        assert_eq!(de.item.as_ref().unwrap().kind, ClipKind::Text);
    }
}
