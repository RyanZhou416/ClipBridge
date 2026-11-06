// cb_core/src/proto.rs
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

// 协议版本号
//
// 变更历史：
// - 2：bincode → postcard 之后的首个版本
// - 3：新增 `CtrlMsg::ContentDenied`（策略拒绝改为非致命）。握手会强制比对版本，
//      老对端在 Hello/HelloAck 阶段即被拒绝，而不是卡在解码失败。
// - 4（ADR-002 阶段 B1）：控制面/数据面分离。控制流（本条 bidi 流）只承载
//      `CBFrame::Control`；每个传输额外开一条 bidi 数据流，先写
//      `[u32 LE header_len][postcard(TransferHeader)]`，随后直接写文件字节，
//      最后 FIN。旧的 `CBFrame::Data` 帧类型从线格式中删除（版本号已 +1，
//      两端必须同版本，无需兼容旧对端）。
// - 5（ADR-002 阶段 B2）：一个传输拆成 N 个互不重叠、连续覆盖的子区间，每个
//      子区间一条数据流。`ContentBegin` 增加 `ranges`（发送端的完整区间计划），
//      接收端据此检测缺段/重叠/未声明区间，并在 `ContentEnd` 时确认所有区间
//      都已 FIN。`TransferHeader` 的 `offset` / `length` 语义从"整文件"变为
//      "本子区间"；`sha256` 仍是整文件 hash（与 `ContentBegin.sha256` 一致）。
//      整文件的完整性在接收端 finalize 时按顺序重读临时文件校验（见 ADR-002
//      §4.2 的两种方案与 actor.rs 的注释）。两端必须同版本。
pub const PROTOCOL_VERSION: u32 = 5;

#[derive(Debug)]
pub enum CBFrame {
	/// 控制流上唯一的帧类型。数据传输已迁移到独立的每传输数据流
	/// （见 [`TransferHeader`]），不再走这条流。
	Control(Box<CtrlMsg>),
}

// 帧类型常量
const FRAME_TYPE_CTRL: u8 = 0x01;

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
		account_uid: String,
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

	/// 删除项（本地软删除 + 清理缓存）
	ItemDelete {
		msg_id: Option<String>,
		item_id: String,
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
		/// B2：本传输的完整区间计划。发送端保证这些区间两两不重叠、按 `offset`
		/// 升序连续覆盖 `[ranges[0].offset, total_bytes)`；接收端在登记时校验
		/// 空洞/重叠/上限，并在 `ContentEnd` 时要求所有区间都已 FIN。
		///
		/// 空文件（`total_bytes == 0`）计划为空。该字段是 B2 新增的线格式，
		/// `PROTOCOL_VERSION` 已升到 5。
		ranges: Vec<TransferRange>,
	},
	/// 内容传输结束 (A -> B)
	ContentEnd {
		req_id: String,
	},
	/// 取消传输 (双向)
	ContentCancel {
		req_id: String,
		reason: String,
	},
	/// 内容请求被策略拒绝（**非致命**：只影响本次传输，不断开会话）。
	///
	/// 与 `Error` 的区别：`Error` 在对端被视为致命并终止整个会话。用户切换
	/// `share_to_peer` 这类策略不应导致连接反复重建，所以单独用这个变体表达
	/// "这次拉取不允许"；对端应把它转成该 `req_id` 的 TRANSFER_FAILED。
	ContentDenied {
		req_id: String,
		code: String,
		message: Option<String>,
	},
}

/// 一个传输子区间（ADR-002 阶段 B2）。
///
/// 同一次传输的所有 `TransferRange` 必须：
/// - 按 `offset` 严格升序；
/// - 两两不重叠（`offset >= 前一个 end`）；
/// - 连续无空洞，并一直覆盖到 `ContentBegin.total_bytes`；
/// - 数量不超过 `MAX_PAYLOAD_STREAMS_PER_TRANSFER`（6）。
///
/// 每个区间由一条独立 bidi 数据流承载，流头里的 `offset` / `length` 必须与
/// 计划中的某一条**完全一致**（接收端按 offset 精确匹配；不匹配按重叠/空洞/
/// 未声明区间拒绝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRange {
	pub offset: u64,
	pub length: u64,
}

impl TransferRange {
	/// 区间结束偏移（不含）。饱和加法：恶意超大 `offset+length` 不会溢出。
	pub fn end(&self) -> u64 {
		self.offset.saturating_add(self.length)
	}

	/// `[self.offset, self.end())` 与另一个区间是否重叠（零长度区间不重叠）。
	pub fn overlaps(&self, other: &TransferRange) -> bool {
		self.length > 0
			&& other.length > 0
			&& self.offset < other.end()
			&& other.offset < self.end()
	}
}

/// 每传输独立数据流的头（ADR-002 阶段 B1，B2 扩展为"每个子区间一条流"）。
///
/// 线格式（由发送端在数据流上写入）：
///
/// ```text
/// [u32 LE header_len][postcard(TransferHeader)][raw payload bytes ...]
/// ```
///
/// 头里携带的字段与控制面的 `ContentBegin` / `ContentGet` 一一对应，因此接收端
/// 即使先收到数据流（控制流与数据流是两条独立的 QUIC 流，交付顺序无法保证）
/// 也能据此完成路由与校验，不会丢信息：
///
/// - `req_id`：等于 `ContentGet.msg_id` / `ContentBegin.req_id`，即 transfer_id；
/// - `offset` / `length`：本子区间在**整个传输**中的起点与长度，必须与
///   `ContentBegin.ranges` 中某一条完全一致（B2）；
/// - `sha256`：与 `ContentBegin.sha256` 相同的**整文件** hash（不是本区间的
///   hash）。区间会乱序完成，因此接收端不逐区间比较整文件 hash，而是在所有区间
///   FIN + `ContentEnd` 之后按偏移顺序重读临时文件、重算整文件 hash 再 commit
///   （见 actor.rs 的 `Finish` 分支与 ADR-002 §4.2 的取舍说明）；
/// - `mime` / `item_id` / `file_id`：与 `ContentBegin` 相同，便于独立核对。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferHeader {
	pub req_id: String,
	pub item_id: String,
	pub file_id: Option<String>,
	/// 本数据流第一个字节在整个传输中的偏移。
	pub offset: u64,
	/// 本数据流头部之后跟随的负载字节数。
	pub length: u64,
	/// 整文件期望 SHA-256（与 `ContentBegin.sha256` 一致）。
	pub sha256: String,
	/// MIME（与 `ContentBegin.mime` 一致）。
	pub mime: String,
}

/// 数据流头部的长度前缀字节数（小端 u32）。
pub const TRANSFER_HEADER_LEN_BYTES: usize = 4;

/// 数据流头部的上限（编码后）。防止恶意对端用超大长度前缀迫使我们分配内存。
pub const MAX_TRANSFER_HEADER_BYTES: usize = 64 * 1024;

/// 通用 postcard 编码（线格式与 OPAQUE 消息共用，见 actor 的 opaque_encode）。
pub fn postcard_encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
	postcard::to_stdvec(value).map_err(|e| anyhow::anyhow!("postcard 序列化失败: {e}"))
}

/// 通用 postcard 解码（见 actor 的 opaque_decode）。
pub fn postcard_decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
	postcard::from_bytes(bytes).map_err(|e| anyhow::anyhow!("postcard 反序列化失败: {e}"))
}

impl TransferHeader {
	/// 编码为 postcard 字节（不含长度前缀）。
	pub fn encode(&self) -> Result<Vec<u8>> {
		postcard_encode(self)
	}

	/// 从 postcard 字节（不含长度前缀）解码。
	pub fn decode(bytes: &[u8]) -> Result<Self> {
		postcard_decode(bytes)
	}
}

/// 构造数据流头部的完整线格式：`[u32 LE len][postcard]`。
pub fn encode_transfer_header_frame(header: &TransferHeader) -> Result<Vec<u8>> {
	let body = header.encode()?;
	if body.len() > MAX_TRANSFER_HEADER_BYTES {
		anyhow::bail!(
			"transfer header too large: {} > {}",
			body.len(),
			MAX_TRANSFER_HEADER_BYTES
		);
	}
	let mut frame = Vec::with_capacity(TRANSFER_HEADER_LEN_BYTES + body.len());
	frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
	frame.extend_from_slice(&body);
	Ok(frame)
}

/// 解析完整的数据流头部线格式（`[u32 LE len][postcard]`）。
///
/// `frame` 必须恰好包含一个头部（允许后面还有负载字节，但解析只消费头部）。
pub fn decode_transfer_header_frame(frame: &[u8]) -> Result<TransferHeader> {
	if frame.len() < TRANSFER_HEADER_LEN_BYTES {
		anyhow::bail!("transfer header frame too short: {}", frame.len());
	}
	let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
	if len == 0 || len > MAX_TRANSFER_HEADER_BYTES {
		anyhow::bail!("invalid transfer header length: {len}");
	}
	let end = TRANSFER_HEADER_LEN_BYTES + len;
	if frame.len() < end {
		anyhow::bail!(
			"transfer header frame truncated: need {end} bytes, got {}",
			frame.len()
		);
	}
	TransferHeader::decode(&frame[TRANSFER_HEADER_LEN_BYTES..end])
}

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

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
		let payload = src.split_to(len).split_off(1);

		match type_byte {
			FRAME_TYPE_CTRL => {
				let msg: CtrlMsg = serde_json::from_slice(&payload)?;
				Ok(Some(CBFrame::Control(Box::new(msg))))
			}
			_ => {
				// B1 起数据帧不再出现在控制流上；旧版的 TYPE=2 也被视为协议违规。
				Err(anyhow::anyhow!(
					"Unknown control-stream frame type: {type_byte} (data frames are no longer allowed on the control stream)"
				))
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample_header(file_id: Option<&str>) -> TransferHeader {
		TransferHeader {
			req_id: "transfer-1".to_string(),
			item_id: "item-1".to_string(),
			file_id: file_id.map(ToString::to_string),
			offset: 0,
			length: 64 * 1024,
			sha256: "a".repeat(64),
			mime: "text/plain".to_string(),
		}
	}

	/// `TransferHeader` 必须能通过 postcard 无损往返（含 `Option::Some/None` 两种
	/// `file_id`），且长度前缀与负载长度一致。这是 B1 数据流唯一的应用层编解码，
	/// 用 socket-free 测试覆盖。
	#[test]
	fn transfer_header_postcard_roundtrip() {
		for file_id in [None, Some("file-7")] {
			let header = sample_header(file_id);
			let frame = encode_transfer_header_frame(&header).expect("encode header");

			let declared = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
			assert_eq!(
				declared,
				frame.len() - TRANSFER_HEADER_LEN_BYTES,
				"长度前缀必须等于 postcard 负载长度"
			);

			let decoded = decode_transfer_header_frame(&frame).expect("decode header");
			assert_eq!(decoded, header);
		}
	}

	/// 头部的长度前缀是安全边界：截断/超长/零长度都必须报错，而不是分配或 panic。
	#[test]
	fn transfer_header_frame_rejects_malformed_input() {
		let frame = encode_transfer_header_frame(&sample_header(None)).unwrap();

		// 截断的长度前缀
		assert!(decode_transfer_header_frame(&frame[..2]).is_err());
		// 截断的负载
		assert!(decode_transfer_header_frame(&frame[..frame.len() - 1]).is_err());
		// 长度前缀为 0
		assert!(decode_transfer_header_frame(&0u32.to_le_bytes()).is_err());
		// 长度前缀超过上限
		let mut oversized = Vec::new();
		oversized.extend_from_slice(&((MAX_TRANSFER_HEADER_BYTES + 1) as u32).to_le_bytes());
		oversized.extend_from_slice(&[0u8; 8]);
		assert!(decode_transfer_header_frame(&oversized).is_err());
	}

	/// 控制流仍使用原有 `[u32 LE len][type=1][json]` 分帧；B1 起 TYPE=2（旧
	/// Data 帧）必须被拒绝，避免半新半旧的线格式被静默接受。
	#[test]
	fn control_stream_roundtrips_and_rejects_legacy_data_frames() {
		use bytes::BytesMut;
		use tokio_util::codec::{Decoder, Encoder};

		let msg = CtrlMsg::ContentEnd {
			req_id: "transfer-1".to_string(),
		};
		let mut codec = CBFrameCodec;
		let mut buf = BytesMut::new();
		codec
			.encode(CBFrame::Control(Box::new(msg)), &mut buf)
			.expect("encode control frame");
		assert_eq!(buf[4], FRAME_TYPE_CTRL);

		let decoded = codec.decode(&mut buf).expect("decode control frame");
		assert!(matches!(
			decoded,
			Some(CBFrame::Control(inner)) if matches!(*inner, CtrlMsg::ContentEnd { .. })
		));

		// 旧 Data 帧：Type=0x02。长度 3 = Type + 2 字节 ID 长度。
		let mut legacy = BytesMut::new();
		legacy.extend_from_slice(&3u32.to_le_bytes());
		legacy.extend_from_slice(&[0x02, 0x00, 0x00]);
		assert!(
			codec.decode(&mut legacy).is_err(),
			"legacy Data frames must be rejected on the control stream"
		);
	}

	/// B2 的区间计划必须能随 `ContentBegin` 走控制流 JSON 无损往返；`TransferRange`
	/// 自身的 postcard 编解码（数据流头之外的潜在用途）同样要稳定。
	#[test]
	fn content_begin_range_plan_roundtrips() {
		let ranges = vec![
			TransferRange {
				offset: 0,
				length: 1024,
			},
			TransferRange {
				offset: 1024,
				length: 2048,
			},
		];
		let msg = CtrlMsg::ContentBegin {
			req_id: "transfer-2".to_string(),
			item_id: "item-2".to_string(),
			file_id: None,
			total_bytes: 3072,
			sha256: "b".repeat(64),
			mime: "application/octet-stream".to_string(),
			ranges: ranges.clone(),
		};

		let json = serde_json::to_vec(&msg).expect("serialize ContentBegin");
		let decoded: CtrlMsg = serde_json::from_slice(&json).expect("deserialize ContentBegin");
		match decoded {
			CtrlMsg::ContentBegin {
				ranges: decoded_ranges,
				total_bytes,
				..
			} => {
				assert_eq!(decoded_ranges, ranges);
				assert_eq!(total_bytes, 3072);
			}
			other => panic!("expected ContentBegin, got {other:?}"),
		}

		let bytes = postcard_encode(&ranges).expect("postcard encode ranges");
		let decoded_ranges: Vec<TransferRange> =
			postcard_decode(&bytes).expect("postcard decode ranges");
		assert_eq!(decoded_ranges, ranges);
	}

	/// 区间重叠判定：相邻边界不算重叠、零长度区间不算重叠、包含/部分交叉都算。
	#[test]
	fn transfer_range_overlap_boundaries() {
		let a = TransferRange {
			offset: 10,
			length: 10,
		};
		assert!(!a.overlaps(&TransferRange {
			offset: 20,
			length: 10
		}));
		assert!(!a.overlaps(&TransferRange {
			offset: 0,
			length: 10
		}));
		assert!(!a.overlaps(&TransferRange {
			offset: 12,
			length: 0
		}));
		assert!(a.overlaps(&TransferRange {
			offset: 19,
			length: 2
		}));
		assert!(a.overlaps(&TransferRange {
			offset: 11,
			length: 1
		}));
		assert!(a.overlaps(&TransferRange {
			offset: 0,
			length: 100
		}));
		assert_eq!(a.end(), 20);
		assert_eq!(
			TransferRange {
				offset: u64::MAX,
				length: 1
			}
			.end(),
			u64::MAX,
			"offset+length 必须饱和，不能溢出"
		);
	}
}
