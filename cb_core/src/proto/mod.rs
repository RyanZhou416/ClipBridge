// cb_core/src/proto/mod.rs

use serde::{Deserialize, Serialize};

/// 当前协议版本
pub const PROTOCOL_VERSION: u32 = 1;

/// 会话标志位 (在 AuthOk 中返回)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthSessionFlags {
    /// 是否已完成账号所有权验证 (M1 Mock 阶段总是 true)
    pub account_verified: bool,
}

/// 控制平面消息定义
/// 使用 tag="t", content="c" 模式让 JSON 更紧凑
///
/// 修正 (Step 1): 添加 msg_id, reply_to 字段以符合文档 3.3.12.4
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
pub enum CtrlMsg {
    /// 1. 握手第一步: 客户端打招呼
    Hello {
        msg_id: Option<String>, // [New]
        protocol_version: u32,
        device_id: String,
        account_tag: String,
        capabilities: Vec<String>,
        client_nonce: Option<String>,
    },

    /// 2. 握手第二步: 服务端确认
    HelloAck {
        reply_to: Option<String>, // [New]
        server_device_id: String,
        protocol_version: u32,
    },

    // --- M1 新增: OPAQUE 握手相关 ---

    /// 3. OPAQUE Step 1: Client -> Server
    OpaqueStart {
        msg_id: Option<String>,   // [New]
        reply_to: Option<String>, // [New] (关联 HelloAck?)
        opaque: String,
    },

    /// 4. OPAQUE Step 2: Server -> Client
    OpaqueResponse {
        reply_to: Option<String>, // [New]
        msg_id: Option<String>,   // [New]
        opaque: String,
    },

    /// 5. OPAQUE Step 3: Client -> Server
    OpaqueFinish {
        reply_to: Option<String>, // [New]
        msg_id: Option<String>,   // [New]
        opaque: String,
    },

    // ----------------------------------------------------

    /// 6. 握手成功
    AuthOk {
        reply_to: Option<String>, // [New]
        session_flags: AuthSessionFlags,
    },

    /// 握手或鉴权失败
    AuthFail {
        reply_to: Option<String>, // [New]
        error_code: String,
    },

    /// 心跳 Ping
    Ping {
        msg_id: Option<String>, // [New]
        ts: i64,
    },

    /// 心跳 Pong
    Pong {
        reply_to: Option<String>, // [New]
        ts: i64,
    },

    /// 广播元数据
    ItemMeta {
        msg_id: Option<String>, // [New]
        item: crate::model::ItemMeta,
    },

    /// 通用错误
    Error {
        reply_to: Option<String>, // [New]
        error_code: String,
        message: Option<String>,
    },

    /// 关闭连接
    Close {
        msg_id: Option<String>, // [New]
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