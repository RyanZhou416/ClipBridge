// cb_core/src/proto.rs
use serde::{Deserialize, Serialize};
use anyhow::Result;
use bytes::Bytes;

// 协议版本号
pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Debug)]
pub enum CBFrame {
    Control(CtrlMsg), // JSON 信令 (Type=1)
	Data {
		transfer_id: String,
		data: Bytes,
	},
}

// 帧类型常量
const FRAME_TYPE_CTRL: u8 = 0x01;
const FRAME_TYPE_DATA: u8 = 0x02;


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
		code: String,
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
        code: String,
        message: Option<String>,
    },

    // 关闭连接
    Close {
        msg_id: Option<String>,
        reason: String,
    },

    // --- M3 新增信令 ---
    /// 请求拉取内容 (B -> A)
    ContentGet {
        msg_id: Option<String>,
        item_id: String,
        // 如果是 FileList，这里指定具体要下哪个文件；如果是 Text/Image，这里留空或忽略
        file_id: Option<String>,
        offset: Option<u64>, // 支持断点续传（预留）
    },
    /// 准备发送内容头 (A -> B)
    ContentBegin {
        req_id: String, // 对应 ContentGet.msg_id (即 transfer_id)
        item_id: String,
        file_id: Option<String>,
        total_bytes: u64,
        sha256: String,
        mime: String,
    },
    /// 内容传输结束 (A -> B)
    ContentEnd {
        req_id: String,
        sha256: String, // 用于最终校验
    },
    /// 取消传输 (双向)
    ContentCancel {
        req_id: String,
        reason: String,
    },
}

use tokio_util::codec::{Decoder, Encoder};
use bytes::{Buf, BufMut, BytesMut};

const MAX_FRAME_SIZE: usize = 10 * 1024 * 1024; // 10MB

pub struct CBFrameCodec;

impl Encoder<CBFrame> for CBFrameCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, frame: CBFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match frame {
            CBFrame::Control(msg) => {
                let json_bytes = serde_json::to_vec(&msg)?;
                let len = json_bytes.len() + 1; // +1 for Type byte
                if len > MAX_FRAME_SIZE {
                    return Err(anyhow::anyhow!("Frame too large"));
                }

                dst.reserve(4 + len);
                dst.put_u32_le(len as u32);
                dst.put_u8(FRAME_TYPE_CTRL); // Type = 1
                dst.put_slice(&json_bytes);
            }
			CBFrame::Data { transfer_id, data } => {
				// 格式: [Len(4)][Type(1)][ID_Len(2)][ID_Bytes][Data]
				let id_bytes = transfer_id.as_bytes();
				let id_len = id_bytes.len();
				if id_len > 65535 { return Err(anyhow::anyhow!("Transfer ID too long")); }

				let frame_len = 1 + 2 + id_len + data.len(); // Type + ID_Len + ID + Data
				if frame_len > MAX_FRAME_SIZE { return Err(anyhow::anyhow!("Frame too large")); }

				dst.reserve(4 + frame_len);
				dst.put_u32_le(frame_len as u32);
				dst.put_u8(FRAME_TYPE_DATA);

				// 写入 ID
				dst.put_u16_le(id_len as u16);
				dst.put_slice(id_bytes);
				// 写入数据
				dst.put_slice(&data);
			}
        }
        Ok(())
    }
}

impl Decoder for CBFrameCodec {
    type Item = CBFrame;
    type Error = anyhow::Error;

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

        // 消费 Length (4 bytes)
        src.advance(4);

        // 读取 Type (1 byte)
        let type_byte = src[0];

        // 读取 Payload
		let mut payload = src.split_to(len).split_off(1);

        match type_byte {
            FRAME_TYPE_CTRL => {
                let msg: CtrlMsg = serde_json::from_slice(&payload)?;
                Ok(Some(CBFrame::Control(msg)))
            }
            FRAME_TYPE_DATA => {
				// 解码 ID
				if payload.len() < 2 { return Err(anyhow::anyhow!("Data frame too short")); }
				let id_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
				payload.advance(2);

				if payload.len() < id_len { return Err(anyhow::anyhow!("Data frame truncated ID")); }
				let id_bytes = payload.split_to(id_len);
				let transfer_id = String::from_utf8(id_bytes.to_vec())?;

				// 剩下的就是 data
				let data = payload.freeze();
				Ok(Some(CBFrame::Data { transfer_id, data }))
            }
            _ => {
                // 未知帧类型，如果是兼容性考虑可以选择忽略，这里先报错
                Err(anyhow::anyhow!("Unknown frame type: {}", type_byte))
            }
        }
    }
}
