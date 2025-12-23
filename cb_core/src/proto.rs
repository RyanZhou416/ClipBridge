// cb_core/src/proto.rs
use serde::{Deserialize, Serialize};
use anyhow::Result;

// 协议版本号
pub const PROTOCOL_VERSION: u32 = 1;

// 鉴权成功后的会话标记
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSessionFlags {
    pub account_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CtrlMsg {
    // 1. 握手：Hello
    Hello {
        msg_id: Option<String>,
        protocol_version: u32,
        device_id: String,
        account_tag: String,
        capabilities: Vec<String>,
        client_nonce: Option<String>,
    },

    // 2. 握手：HelloAck
    HelloAck {
        reply_to: Option<String>,
        server_device_id: String,
        protocol_version: u32,
    },

    // 3. 鉴权失败
    AuthFail {
        reply_to: Option<String>,
        error_code: String,
    },

    // --- OPAQUE 流程 (关键修改：opaque 字段改为 Vec<u8>) ---

    // KE1
    OpaqueStart {
        msg_id: Option<String>,
        reply_to: Option<String>,
        opaque: Vec<u8>, // [修改] 从 String 改为 Vec<u8> 以支持真实加密数据
    },

    // KE2
    OpaqueResponse {
        msg_id: Option<String>,
        reply_to: Option<String>,
        opaque: Vec<u8>, // [修改] Vec<u8>
    },

    // KE3
    OpaqueFinish {
        msg_id: Option<String>,
        reply_to: Option<String>,
        opaque: Vec<u8>, // [修改] Vec<u8>
    },

    // --- 鉴权成功 ---
    AuthOk {
        reply_to: Option<String>,
        session_flags: AuthSessionFlags,
    },

    // --- 业务与控制 ---

    Ping {
        msg_id: Option<String>,
        ts: i64,
    },
    Pong {
        reply_to: Option<String>,
        ts: i64,
    },

    // 元数据广播
    ItemMeta {
        msg_id: Option<String>,
        item: crate::api::ItemMeta,
    },

    // 通用错误
    Error {
        reply_to: Option<String>,
        error_code: String,
        message: Option<String>,
    },

    // 关闭连接
    Close {
        msg_id: Option<String>,
        reason: String,
    },
}

// ... CBFrameCodec 实现保持不变 ...
use tokio_util::codec::{Decoder, Encoder};
use bytes::{Buf, BufMut, BytesMut};
use anyhow::Error;

const MAX_FRAME_SIZE: usize = 10 * 1024 * 1024; // 10MB

pub struct CBFrameCodec;

impl Encoder<CtrlMsg> for CBFrameCodec {
    type Error = Error;
    fn encode(&mut self, item: CtrlMsg, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let json_bytes = serde_json::to_vec(&item)?;
        let len = json_bytes.len();
        if len > MAX_FRAME_SIZE {
            return Err(anyhow::anyhow!("Frame too large"));
        }
        dst.reserve(4 + len);
        dst.put_u32_le(len as u32);
        dst.put_slice(&json_bytes);
        Ok(())
    }
}

impl Decoder for CBFrameCodec {
    type Item = CtrlMsg;
    type Error = Error;
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&src[..4]);
        let len = u32::from_le_bytes(len_bytes) as usize;

        if len > MAX_FRAME_SIZE {
            return Err(anyhow::anyhow!("Frame too large during decode"));
        }

        if src.len() < 4 + len {
            src.reserve(4 + len - src.len());
            return Ok(None);
        }

        src.advance(4);
        let data = src.split_to(len);
        let msg = serde_json::from_slice(&data)?;
        Ok(Some(msg))
    }
}