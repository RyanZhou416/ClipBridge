// cb_core/src/session/actor.rs

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncSeekExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::api::{CoreConfig, CoreEventSink};
use crate::crypto::{
	p2p_get_server_registration, CbClientLogin, CbClientLoginState, CbServerLogin,
	CbServerLoginState, DefaultCipherSuite,
};
// 只引入存在的结构体
use opaque_ke::{ClientLoginFinishParameters, ServerLoginParameters};
use rand::rngs::OsRng;

use super::{HandshakeStep, SessionCmd, SessionHandle, SessionRole, SessionState};
use crate::cas::DurabilityOutcome;
use crate::model::ItemKind;
use crate::policy::SizeLimits;
use crate::proto::{
	encode_transfer_header_frame, postcard_decode, postcard_encode, AuthSessionFlags, CBFrame,
	CBFrameCodec, CtrlMsg, TransferHeader, TransferRange, MAX_TRANSFER_HEADER_BYTES,
	PROTOCOL_VERSION, TRANSFER_HEADER_LEN_BYTES,
};
use crate::store::Store;
use crate::transport::{Connection, RecvStream, SendStream};
use crate::util::{now_ms, sha256_hex};

/// OPAQUE 登录消息的二进制编码（postcard + serde）。
///
/// 历史：早期用 bincode 1.3 的 `serialize`/`deserialize`。bincode 项目已永久停止开发
/// （RUSTSEC-2025-0141 覆盖所有版本，2.0.1 同样触发；3.0.0 是只含一行 compile_error!
/// 的"墓碑版本"），因此整体换成仍活跃维护、同样紧凑的 postcard。
///
/// **线格式与 bincode 1.x 不兼容**：这是有意的产品决定 —— 不保留与老对端的互通，
/// 双方必须运行同一协议版本。将来若需兼容旧版本，必须引入显式的协议版本协商。
fn opaque_encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
	postcard_encode(value)
}

fn opaque_decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
	postcard_decode(bytes)
}

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(6);

/// 单个会话允许同时存在的入站传输数量。
///
/// 每个 receiver 都会占用一个 writer channel、一个 writer Task、一个 tmp 文件 fd
/// 和最多 [`MAX_INFLIGHT_BYTES_PER_RECEIVER`] + [`MAX_PENDING_BYTES_PER_RECEIVER`]
/// 字节的内存，对端可以无限发 `ContentBegin`，因此必须设硬上限（超出的 Begin 直接用
/// `ContentCancel` 拒绝，不分配任何资源）。
const MAX_CONCURRENT_RECEIVERS: usize = 8;

/// 单个入站传输允许同时"在途"（writer channel 中排队 + writer 正在写）的字节数。
///
/// 只限制 channel 槽位数量是不够的：32 个槽位若每槽都是 256 KiB 的分块，单个
/// receiver 就能占 8 MiB。这里用字节信号量计量在途字节，上限 12 MiB：
/// 足以容忍一次完整的 32 槽突发，又不会让慢 writer 把内存撑爆。
/// 8 个 receiver 最坏约 96 MiB 在途。
///
/// 注：B1 起每个传输独占一条数据流，背压由 QUIC 流控 + 本信号量共同承担；
/// 接收任务用 [`ReceiverInbox::push_or_wait`] 等待许可，因此不会丢帧。
const MAX_INFLIGHT_BYTES_PER_RECEIVER: usize = 12 * 1024 * 1024;

/// 上传任务每次读文件 / 每帧写数据流的分块大小（256 KiB）。
///
/// 旧值是 64 KiB；纯 QUIC 基线显示 64 KiB → 1 MiB 只提升 4-12%，但应用层还有
/// 每帧的编码/拷贝与 `FramedWrite` 调度开销，256 KiB 在接收端预算
/// （见 [`MAX_INFLIGHT_BYTES_PER_RECEIVER`] / [`MAX_PENDING_BYTES_PER_RECEIVER`]）
/// 之内能减少一半以上的分块而无需改动协议。
const UPLOAD_CHUNK_BYTES: usize = 256 * 1024;

/// writer 背压时，接收任务允许暂存的字节数上限。
///
/// B1 起数据流是独立流：接收任务在 writer channel / 在途预算满时可以先等
/// （[`ReceiverInbox::push_or_wait`]），因此正常路径不会产生 pending。这个上限
/// 保留下来作为防御：`try_push` 的同步路径（以及测试）仍把积压限制在
/// 4 MiB = 16 × [`UPLOAD_CHUNK_BYTES`]，超过即判定 writer 完全跟不上并让该传输
/// 失败（`TRANSFER_BACKPRESSURE`），而不是无限增长内存。
///
/// 内存边界（每个 receiver，正常对端）：在途 32 槽 × 256 KiB = 8 MiB（先于
/// 12 MiB 预算触顶）+ 暂存 4 MiB + 当前正在处理的一块 256 KiB ≈ 12.25 MiB；
/// 8 个 receiver ≈ 98 MiB。
const MAX_PENDING_BYTES_PER_RECEIVER: usize = 4 * 1024 * 1024;

/// 接收方空闲超时：超过该时长没有收到任何数据块就取消传输并清理 tmp 文件。
///
/// 对端可以打开传输后保持心跳但不发数据；若不清理，tmp 文件与 fd 会一直泄漏。
/// B1 起也覆盖"数据流已 FIN 但 ContentEnd 一直不来"的情况：FIN 会刷新活动时间，
/// 随后 30s 无事件同样回收。
const RECEIVER_IDLE_TIMEOUT_MS: i64 = 30_000;

/// 数据流头部读取超时：数据流建立后对端必须立刻写头部，否则回收该流。
///
/// 没有这个超时，恶意/卡死的对端可以打开一条 bidi 流后只保持连接，让我们永久
/// 挂着一个读取任务与流句柄。
const PAYLOAD_HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// 数据流可能早于 `ContentBegin` 到达（控制流与数据流是两条独立 QUIC 流）。
/// 在等待注册的最长时间；超时后视为未知 req_id 并丢弃该数据流。
const PAYLOAD_REGISTRATION_GRACE_MS: i64 = 2_000;

/// `ContentEnd` 已到、但数据流头还没被路由到 receiver 时允许等待的时间。
///
/// 发送端的顺序是"数据流 FIN 之后才发 ContentEnd"，正常情况下 FIN 先到；若
/// `ContentEnd` 先到，说明数据流仍在路上。超过该宽限仍没有数据流即视为
/// 协议错误（`PAYLOAD_STREAM_MISSING`），绝不静默 finalize 一个空文件。
const CONTENT_END_WITHOUT_PAYLOAD_GRACE_MS: i64 = 2_000;

/// 同时等待 `ContentBegin` 注册的数据流上限（每个都持有一个读取任务/流句柄）。
const MAX_PENDING_PAYLOAD_STREAMS: usize = 16;

/// 同时读取头部的数据流上限：头部读取在独立任务里做，避免慢对端阻塞 actor。
const MAX_PAYLOAD_HEADER_READERS: usize = 16;

/// 已取消的 req_id 记忆时长：取消与数据流到达可能交叉，记住一小段时间可以把
/// 迟到的数据流直接判为"已取消"而不是误当成未知传输。
const CANCELLED_PAYLOAD_TTL_MS: i64 = 5_000;

/// 已取消 req_id 的记忆条数上限（防御对端刷任意 req_id 的 `ContentCancel`）。
const MAX_CANCELLED_PAYLOADS: usize = 128;

/// `ContentEnd` 之后 flush / close / 校验 / commit / materialize 的总超时。
///
/// 2 GiB 的最终化本来就可能耗时；但磁盘卡死时不能让它永远占着 writer Task 与 tmp 文件，
/// 超时后该传输以 COMMIT_TIMEOUT 失败并 abort writer Task。
const CONTENT_END_TIMEOUT: Duration = Duration::from_secs(30);

/// `Committing` 占位符的兜底回收时间（最终化任务自身已有 30s 超时，这里只防任务异常消失）。
const FINALIZE_PLACEHOLDER_GRACE_MS: i64 = 35_000;

/// 单个会话允许同时存在的出站传输数量（与入站 `MAX_CONCURRENT_RECEIVERS` 对称）。
///
/// 每个 `ContentGet` 都会 spawn 一个读文件任务；若不对认证对端设上限，
/// 一个对端就能开出无限上传、耗尽 fd / 内存 / CPU。
const MAX_CONCURRENT_SENDERS: usize = 8;

/// 单个传输最多拆成多少条并行数据流（ADR-002 §4.2 的硬上限 N ≤ 6）。
///
/// 与 [`MAX_CONCURRENT_SENDERS`] 是**两个维度**：前者限制"同时有多少个传输"，
/// 这里限制"一个传输最多开多少条数据流"。接收端在 `ContentBegin` 里用同一个
/// 上限校验区间计划，两端不可能对"最多几条流"产生分歧。
const MAX_PAYLOAD_STREAMS_PER_TRANSFER: usize = 6;

/// 每个会话同时活跃的**出站**数据流上限（独立于 [`MAX_CONCURRENT_SENDERS`]）。
///
/// 8 个并发 sender × 每传输最多 6 条 = 48 条，若不设会话级闸门，多个 WAN
/// 传输同时上探会瞬间打开几十条 QUIC 流（每条都有流控窗口与接收任务）。
/// 24 = 4 个传输同时用满 6 条，足够覆盖正常并发，又给慢速/异常 sender 留出
/// 明确的背压点：拿不到许可的区间任务会等待，而不是无限开流。
const MAX_PAYLOAD_STREAMS_PER_SESSION: usize = 24;

/// 冷启动 RTT 分档阈值（ADR-002 §3.3 / §4.8）：低于该值视为 LAN。
///
/// LAN 上单流已接近回环上限（实测 195 MiB/s，4 流同连接 190），因此不需要
/// 6 条等长流：初始计划只额外切一个**小探针区间**（见
/// [`LAN_PROBE_SLICE_MAX_BYTES`]），够执行一次探针，又不会让回退后的多流
/// 长期占着半个文件。
const LAN_RTT_THRESHOLD: Duration = Duration::from_millis(20);

/// LAN 探针区间的上限 / 下限。
///
/// LAN 上控制器最多探针一次，探针流只承载文件尾部的一小段（默认 16 MiB 上限）。
/// 这样即使探针被接受、或回退时探针流还在飞，物理并发也只在很短的窗口内 >1，
/// 不会像"等长 2/3 段"那样让回退后的传输长期停留在多流。
const LAN_PROBE_SLICE_MAX_BYTES: u64 = 4 * 1024 * 1024;
const LAN_PROBE_SLICE_MIN_BYTES: u64 = 2 * 1024 * 1024;

/// 允许上探多流的最小剩余字节数（ADR-002 §4.2 / §4.7）。
///
/// 剩余 ≤ 该值（含边界）时控制器强制 N=1：小传输不值得为探针付出一个评估
/// 窗口的固定成本，而 16 MiB 在 8 MiB 流窗口下也足以填满 2 个 BDP。
const PROBE_MIN_REMAINING_BYTES: u64 = 16 * 1024 * 1024;

/// 探针被接受所需的最小窗口 goodput 增益（8%，ADR-002 §4.2）。
const STREAM_PROBE_GAIN_THRESHOLD: f64 = 0.08;

/// 触发流数回退的丢包率阈值（1%）。`lost_packets` / `sent_packets` 增量。
const STREAM_LOSS_BACKOFF_RATE: f64 = 0.01;

/// 触发流数回退的 srtt 倍数阈值（相对传输开始时的平滑 RTT）。
const STREAM_SRTT_BACKOFF_RATIO: f64 = 2.5;

/// 控制器评估周期下限 / 上限：`T = clamp(4 × srtt, 300ms, 2s)`。
const STREAM_PROBE_MIN_INTERVAL: Duration = Duration::from_millis(300);
const STREAM_PROBE_MAX_INTERVAL: Duration = Duration::from_secs(2);

/// 一个区间流失败后的最大重试次数（ADR-002 §3.8）。
///
/// 失败后先降低目标并发并冷却 2T，再以更低的 N 重试同一区间；超过次数则让
/// 整个传输失败（用非致命的 `ContentCancel` 通知对端）。
const MAX_RANGE_STREAM_RETRIES: u8 = 3;

/// 数据块发送超时的步进粒度：旧版 64 KiB 分块时代固定使用 500ms。
const CHUNK_SEND_TIMEOUT_STEP_MS: u64 = 500;

/// 每个 [`CHUNK_SEND_TIMEOUT_STEP_MS`] 预算覆盖的分块字节数（即旧分块大小 64 KiB）。
const CHUNK_SEND_TIMEOUT_STEP_BYTES: usize = 64 * 1024;

/// 数据块发送超时的硬上限（4s）。
///
/// 必须严格低于 [`HEARTBEAT_TIMEOUT`]（当前 6s）：chunk 发送发生在 actor 主循环里，
/// 一次卡住的发送会让心跳 tick 最多推迟这么久。4s 留出 2s 余量（至少 1s），
/// 保证不会把本端/对端的心跳判定拖到超时，从而把整条会话误判为死亡。
const CHUNK_SEND_TIMEOUT_CAP: Duration = Duration::from_secs(4);

/// 数据块发送超时：按分块大小缩放，保底 500ms、封顶 [`CHUNK_SEND_TIMEOUT_CAP`]。
///
/// 公式：`max(500ms, 500ms × ceil(chunk_len / 64 KiB))`，即"每 64 KiB（不足按
/// 64 KiB 计）给 500ms 预算，至少给 500ms"。含义：
/// - 64 KiB → 500ms，与旧版行为完全一致；
/// - 256 KiB（[`UPLOAD_CHUNK_BYTES`]）→ 2s，吞吐下限 = 256 KiB / 2s = 128 KiB/s，
///   与旧版 64 KiB / 500ms 的 128 KiB/s 下限相同，分块放大 4 倍不会再误杀慢速链路
///   （移动网络、弱 Wi-Fi、小窗口 WAN）；
/// - 超大分块（如协议允许的 10 MiB 单帧）→ 封顶 4s，仍低于心跳超时。
fn chunk_send_timeout(chunk_len: usize) -> Duration {
	let steps = chunk_len.div_ceil(CHUNK_SEND_TIMEOUT_STEP_BYTES).max(1);
	let millis = (steps as u64).saturating_mul(CHUNK_SEND_TIMEOUT_STEP_MS);
	Duration::from_millis(millis).min(CHUNK_SEND_TIMEOUT_CAP)
}

/// 认证前（尚未进入 Online）允许处理的消息白名单。
///
/// 只放行握手、OPAQUE、心跳与会话控制消息；业务消息（内容读写、元数据、删除等）
/// 必须等 OPAQUE 完成且设备指纹校验通过后才能处理。
///
/// 注意：`AuthOk` / `AuthFail` 是 OPAQUE 的最后一步（客户端收到 `AuthOk` 才转为
/// Online），`Pong` 是对白名单中 `Ping` 的应答，三者同样属于协议控制消息。
fn ctrl_is_pre_auth_allowed(msg: &CtrlMsg) -> bool {
	matches!(
		msg,
		CtrlMsg::Hello { .. }
			| CtrlMsg::HelloAck { .. }
			| CtrlMsg::OpaqueStart { .. }
			| CtrlMsg::OpaqueResponse { .. }
			| CtrlMsg::OpaqueFinish { .. }
			| CtrlMsg::AuthOk { .. }
			| CtrlMsg::AuthFail { .. }
			| CtrlMsg::Ping { .. }
			| CtrlMsg::Pong { .. }
			| CtrlMsg::Error { .. }
			| CtrlMsg::Close { .. }
	)
}

/// 消息类型名，仅用于拒绝日志与错误信息。
///
/// 这里是穷举 match：以后新增 `CtrlMsg` 变体必须在此命名，从而提醒实现者
/// 判断它是否应加入认证前白名单。
fn ctrl_msg_kind(msg: &CtrlMsg) -> &'static str {
	match msg {
		CtrlMsg::Hello { .. } => "Hello",
		CtrlMsg::HelloAck { .. } => "HelloAck",
		CtrlMsg::AuthFail { .. } => "AuthFail",
		CtrlMsg::OpaqueStart { .. } => "OpaqueStart",
		CtrlMsg::OpaqueResponse { .. } => "OpaqueResponse",
		CtrlMsg::OpaqueFinish { .. } => "OpaqueFinish",
		CtrlMsg::AuthOk { .. } => "AuthOk",
		CtrlMsg::Ping { .. } => "Ping",
		CtrlMsg::Pong { .. } => "Pong",
		CtrlMsg::ItemMeta { .. } => "ItemMeta",
		CtrlMsg::ItemDelete { .. } => "ItemDelete",
		CtrlMsg::Error { .. } => "Error",
		CtrlMsg::Close { .. } => "Close",
		CtrlMsg::ContentGet { .. } => "ContentGet",
		CtrlMsg::ContentBegin { .. } => "ContentBegin",
		CtrlMsg::ContentEnd { .. } => "ContentEnd",
		CtrlMsg::ContentCancel { .. } => "ContentCancel",
		CtrlMsg::ContentDenied { .. } => "ContentDenied",
	}
}

/// 按 MIME 归类入站内容（`file_id` 非空时由调用方直接判为 FileList）。
fn kind_from_mime(mime: &str) -> ItemKind {
	if mime.starts_with("text/") {
		ItemKind::Text
	} else if mime.starts_with("image/") {
		ItemKind::Image
	} else {
		ItemKind::FileList
	}
}

/// 取出该类内容的硬上限（字节）。
///
/// 硬上限来自 `AppConfig.size_limits`（policy.rs）；配置为负值时按 0 处理，
/// 即拒绝一切传输 —— 保守优先于放行。
fn hard_cap_bytes(kind: &ItemKind, limits: &SizeLimits) -> u64 {
	let hard = match kind {
		ItemKind::Text => limits.hard_text_bytes,
		ItemKind::Image => limits.hard_image_bytes,
		ItemKind::FileList => limits.hard_file_total_bytes,
	};
	u64::try_from(hard).unwrap_or(0)
}

/// 并发 receiver 上限判定：第 9 个（已存在 8 个时）必须被拒绝。
fn receiver_slot_available(current_receivers: usize) -> bool {
	current_receivers < MAX_CONCURRENT_RECEIVERS
}

/// 并发 sender 上限判定：第 9 个出站传输必须被拒绝（与入站对称）。
fn sender_slot_available(current_senders: usize) -> bool {
	current_senders < MAX_CONCURRENT_SENDERS
}

/// 出站并发闸门：满员时返回要回给对端的非致命拒绝消息，未满时返回 `None`。
///
/// **不会修改 `senders`**：调用方只有在拿到 `None` 且完成后续检查后才插入新条目，
/// 拒绝路径绝不能凭空留下一个 `SenderState`。用 `ContentDenied` 而不是 `Error`：
/// 对端把任何 `Error` 都当作致命错误并断开会话，一次并发拒绝不应引发断连。
fn sender_admission(senders: &HashMap<String, SenderState>, transfer_id: &str) -> Option<CtrlMsg> {
	if sender_slot_available(senders.len()) {
		return None;
	}
	Some(CtrlMsg::ContentDenied {
		req_id: transfer_id.to_string(),
		code: "TOO_MANY_TRANSFERS".into(),
		message: Some(format!(
			"rejecting ContentGet: {MAX_CONCURRENT_SENDERS} concurrent outbound transfers already active"
		)),
	})
}

/// 接收空闲超时判定。
fn receiver_idle_expired(now: i64, last_activity_ms: i64) -> bool {
	now - last_activity_ms > RECEIVER_IDLE_TIMEOUT_MS
}

// ---------------------------------------------------------------------------
// B2 分片计划、流数控制器与接收端区间账本
//
// 全部是纯函数 / 纯数据结构，不需要 socket，因此可以在 actor.rs 的 test 模块
// 里直接覆盖：区间并集不变式、控制器决策（探针/回退/退避/ceiling/小文件），
// 以及接收端乱序完成、重复区间、ContentEnd 时缺段等账本行为。
// ---------------------------------------------------------------------------

/// 把 `[offset, offset+length)` 均分成 `count` 个连续、互不重叠的区间。
///
/// 不变式（单元测试断言）：
/// - `r[0].offset == offset`，`r[i].end() == r[i+1].offset`，最后一条
///   `end() == offset + length`（若 `length > 0`）——无空洞、无重叠、并集等于
///   请求区间；
/// - `count` 被夹到 `[1, MAX_PAYLOAD_STREAMS_PER_TRANSFER]`，且不超过字节数
///   （`length < count` 时每条折叠成 1 字节，保证 `length > 0`）；
/// - 区间长度差 ≤ 1（余数按顺序分给前 `length % count` 条），避免最后一段
///   特别大导致尾部单流化。
///
/// `length == 0` 返回空计划（空文件没有任何数据流）。
fn plan_ranges(offset: u64, length: u64, count: usize) -> Vec<TransferRange> {
	if length == 0 {
		return Vec::new();
	}
	let requested = (count as u64).clamp(1, MAX_PAYLOAD_STREAMS_PER_TRANSFER as u64);
	let count = requested.min(length) as usize;
	let base = length / count as u64;
	let remainder = length % count as u64;

	let mut ranges = Vec::with_capacity(count);
	let mut cursor = offset;
	for index in 0..count {
		let extra = u64::from((index as u64) < remainder);
		let range_len = base + extra;
		ranges.push(TransferRange {
			offset: cursor,
			length: range_len,
		});
		cursor = cursor.saturating_add(range_len);
	}
	ranges
}

/// 传输开始时的区间计划（冷启动值，ADR-002 §3.3 / §4.8）。
///
/// - 剩余 ≤ [`PROBE_MIN_REMAINING_BYTES`]：**单流**，控制器不会探针；
/// - 低 RTT（< 20 ms，LAN）：**2 段**——主区间 + 文件尾部的小探针区间。
///   单流已接近回环上限，多流通常无收益；小探针区间让"探一次、回退"的代价
///   只有十几 MiB，不会像等长 2/3 段那样在回退后仍长期多流；
/// - 否则（WAN）：按 [`MAX_PAYLOAD_STREAMS_PER_TRANSFER`] 个等长区间切好，
///   控制器从 N=1 起步逐步"多领"，每加一条流都有独立窗口。
///
/// 固定区间（而不是传输中动态切分）保证任意时刻的区间并集始终等于请求范围，
/// 不会出现空洞或重叠，也不需要额外的控制面 "缩短区间" 消息。
fn initial_range_plan(offset: u64, total_bytes: u64, srtt: Duration) -> Vec<TransferRange> {
	let remaining = total_bytes.saturating_sub(offset);
	if remaining <= PROBE_MIN_REMAINING_BYTES {
		return plan_ranges(offset, remaining, 1);
	}
	if srtt < LAN_RTT_THRESHOLD {
		let probe = (remaining / 8)
			.clamp(LAN_PROBE_SLICE_MIN_BYTES, LAN_PROBE_SLICE_MAX_BYTES)
			.min(remaining - 1)
			.max(1);
		let main = remaining - probe;
		return vec![
			TransferRange {
				offset,
				length: main,
			},
			TransferRange {
				offset: offset.saturating_add(main),
				length: probe,
			},
		];
	}
	plan_ranges(offset, remaining, MAX_PAYLOAD_STREAMS_PER_TRANSFER)
}

/// 校验对端在 `ContentBegin` 里声明的完整区间计划。
///
/// `total_bytes` 是整文件大小；计划必须从某个 `base_offset` 连续覆盖到
/// `total_bytes`（公开 API 的起点是 0；断点续传时 `base_offset > 0`，接收端
/// 仍要求一直覆盖到文件末尾，避免尾部缺段被静默接受）。
///
/// 返回 `Err((error_code, detail))`，调用方直接映射为 TRANSFER_FAILED。
fn validate_range_plan(
	ranges: &[TransferRange],
	total_bytes: u64,
) -> std::result::Result<(), (&'static str, String)> {
	if ranges.len() > MAX_PAYLOAD_STREAMS_PER_TRANSFER {
		return Err((
			"TRANSFER_RANGE_TOO_MANY",
			format!(
				"{} ranges declared, max {}",
				ranges.len(),
				MAX_PAYLOAD_STREAMS_PER_TRANSFER
			),
		));
	}
	let Some(first) = ranges.first() else {
		if total_bytes == 0 {
			return Ok(());
		}
		return Err((
			"TRANSFER_RANGE_EMPTY",
			format!("no ranges declared for {total_bytes}-byte transfer"),
		));
	};
	let base = first.offset;
	if base >= total_bytes {
		return Err((
			"TRANSFER_RANGE_UNEXPECTED",
			format!("range plan starts at {base}, total_bytes={total_bytes}"),
		));
	}
	let mut cursor = base;
	for (index, range) in ranges.iter().enumerate() {
		if range.length == 0 {
			return Err((
				"TRANSFER_RANGE_GAP",
				format!("range[{index}] at offset {} has zero length", range.offset),
			));
		}
		if range.offset < cursor {
			return Err((
				"TRANSFER_RANGE_OVERLAP",
				format!(
					"range[{index}] starts at {} but previous range ends at {cursor}",
					range.offset
				),
			));
		}
		if range.offset > cursor {
			return Err((
				"TRANSFER_RANGE_GAP",
				format!(
					"range[{index}] starts at {} but a gap exists (expected {cursor})",
					range.offset
				),
			));
		}
		cursor = cursor.saturating_add(range.length);
		if cursor > total_bytes {
			return Err((
				"TRANSFER_RANGE_OVERRUN",
				format!("range[{index}] ends at {cursor}, beyond total_bytes={total_bytes}"),
			));
		}
	}
	if cursor != total_bytes {
		return Err((
			"TRANSFER_RANGE_GAP",
			format!("range plan ends at {cursor}, expected total_bytes={total_bytes}"),
		));
	}
	Ok(())
}

/// 控制器输入（ADR-002 §4.2）。单位：`gain`/`loss_rate`/`srtt_ratio` 为比值，
/// `remaining_bytes` 为尚未发送的字节数。
#[derive(Debug, Clone, Copy, PartialEq)]
struct StreamControllerInput {
	/// 当前目标并发流数（1..=6）。
	current_n: u8,
	/// `Some(gain)` = 上一次决策刚多开一条流，本窗口要判定该探针的收益；
	/// `None` = 当前没有待判定的探针（稳定期）。
	gain: Option<f64>,
	/// 本窗口丢包率（`lost_packets` 增量 / `sent_packets` 增量）。
	loss_rate: f64,
	/// 本窗口 srtt / 传输起始基线 srtt。
	srtt_ratio: f64,
	/// 尚未发送的字节数。
	remaining_bytes: u64,
	/// 本传输是否已判定"加流没有收益"（ceiling）。true 后不再上探。
	ceiling_reached: bool,
}

/// 决策原因（仅用于日志与单测断言；wire format 不携带）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamDecisionReason {
	/// 剩余 ≤ 16 MiB：强制单流，不再探针。
	SmallTransfer,
	/// 丢包 > 1% 或 srtt > 2.5× 基线：回退一条流并冷却。
	CongestionBackoff,
	/// 稳定期多开一条流（开始探针）。
	ProbeStarted,
	/// 上个窗口收益 > 8%：接受并继续上探。
	ProbeAccepted,
	/// 上个窗口收益 ≤ 8%：回退一条流并标记 ceiling。
	ProbeRejected,
	/// ceiling 已标记：保持当前 N。
	Ceiling,
	/// 没有可执行动作：保持当前 N。
	Hold,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct StreamControllerDecision {
	next_n: u8,
	ceiling_reached: bool,
	reason: StreamDecisionReason,
}

/// **纯函数**：根据一个评估窗口的信号决定下一周期目标流数（ADR-002 §4.2）。
///
/// 规则（顺序即优先级）：
/// 1. 剩余 ≤ 16 MiB → N=1（不探针）；
/// 2. 丢包 > 1% 或 srtt > 2.5× 基线 → N = max(1, N-1)（退避，不置 ceiling）；
/// 3. 有待判定探针且 gain > 8% → N+1（继续探；到上限则保持）；
/// 4. 有待判定探针且 gain ≤ 8% → N-1（至少 1）并置 ceiling；
/// 5. 无待判定探针且未 ceiling 且 N<6 → N+1（开始探针）；
/// 6. 其余 → 保持。
///
/// `ceiling_reached` 是**粘性**的：一次探针失败后本传输不再上探，因此 LAN
/// 回环上最多付出一次 300ms 的探针窗口，之后稳定在 N=1，不会来回振荡。
fn decide_stream_count(input: StreamControllerInput) -> StreamControllerDecision {
	let max_n = MAX_PAYLOAD_STREAMS_PER_TRANSFER as u8;
	let current = input.current_n.clamp(1, max_n);
	let hold = |next_n: u8, ceiling: bool, reason: StreamDecisionReason| StreamControllerDecision {
		next_n,
		ceiling_reached: ceiling,
		reason,
	};

	if input.remaining_bytes <= PROBE_MIN_REMAINING_BYTES {
		return hold(
			1,
			input.ceiling_reached,
			StreamDecisionReason::SmallTransfer,
		);
	}
	if input.loss_rate > STREAM_LOSS_BACKOFF_RATE || input.srtt_ratio > STREAM_SRTT_BACKOFF_RATIO {
		// 如果这次退避发生在探针窗口（gain=Some），说明"加流"直接引发了丢包 /
		// RTT 膨胀：这比 goodput 更可靠，直接标记 ceiling，避免在劣化后反复探针
		// （滞回）。非探针窗口的偶发丢包只退避 + 冷却，冷却后仍可再探。
		return hold(
			current.saturating_sub(1).max(1),
			input.ceiling_reached || input.gain.is_some(),
			StreamDecisionReason::CongestionBackoff,
		);
	}

	match input.gain {
		Some(gain) if gain > STREAM_PROBE_GAIN_THRESHOLD => {
			if !input.ceiling_reached && current < max_n {
				hold(current + 1, false, StreamDecisionReason::ProbeAccepted)
			} else {
				hold(
					current,
					input.ceiling_reached,
					if input.ceiling_reached {
						StreamDecisionReason::Ceiling
					} else {
						StreamDecisionReason::Hold
					},
				)
			}
		}
		// 探针收益不足：回退并永久标记 ceiling（本传输不再上探）。
		Some(_) => hold(
			current.saturating_sub(1).max(1),
			true,
			StreamDecisionReason::ProbeRejected,
		),
		None if !input.ceiling_reached && current < max_n => {
			hold(current + 1, false, StreamDecisionReason::ProbeStarted)
		}
		None => hold(
			current,
			input.ceiling_reached,
			if input.ceiling_reached {
				StreamDecisionReason::Ceiling
			} else {
				StreamDecisionReason::Hold
			},
		),
	}
}

/// 控制器评估周期：`clamp(4 × srtt, 300ms, 2s)`（ADR-002 §4.2）。
fn stream_probe_interval(srtt: Duration) -> Duration {
	srtt.saturating_mul(4)
		.max(STREAM_PROBE_MIN_INTERVAL)
		.min(STREAM_PROBE_MAX_INTERVAL)
}

/// 单个计划区间的运行状态（接收端账本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeSlotState {
	/// `ContentBegin` 已登记，尚未收到该区间的数据流头。
	Awaiting,
	/// 数据流已接入，接收任务在跑。
	Attached,
	/// 数据流已 FIN 且字节数正确。
	Finished,
	/// 数据流失败（可重试）：保留槽位，等发送端以更低 N 重开同一区间。
	Failed,
}

/// 计划区间 + 运行状态。`offset`/`length` 来自发送端 `ContentBegin.ranges`；
/// 数据流头必须与其中一条**完全一致**才能接入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeSlot {
	offset: u64,
	length: u64,
	state: RangeSlotState,
}

impl RangeSlot {
	fn end(&self) -> u64 {
		self.offset.saturating_add(self.length)
	}
}

/// 一次入站传输的区间账本（actor 状态里的纯数据部分，便于 socket-free 测试）。
///
/// 不变式由 [`validate_range_plan`] 在登记时保证：slots 按 offset 升序、首尾
/// 相接、并集恰好是 `[base_offset, total_bytes)`。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReceiverRanges {
	slots: Vec<RangeSlot>,
	base_offset: u64,
	total_bytes: u64,
}

impl ReceiverRanges {
	fn from_plan(plan: &[TransferRange], total_bytes: u64) -> Self {
		let base_offset = plan.first().map_or(0, |range| range.offset);
		Self {
			slots: plan
				.iter()
				.map(|range| RangeSlot {
					offset: range.offset,
					length: range.length,
					state: RangeSlotState::Awaiting,
				})
				.collect(),
			base_offset,
			total_bytes,
		}
	}

	fn planned_bytes(&self) -> u64 {
		self.slots.iter().map(|slot| slot.length).sum()
	}

	/// 本次传输是否覆盖整个文件（起点 0 且区间并集等于 total_bytes）。
	///
	/// 只有完整传输才能在 finalize 时重算整文件 hash；断点续传（非 0 起点）
	/// 只覆盖文件的一部分，无法用 `ContentBegin.sha256` 校验。
	fn is_whole_file(&self) -> bool {
		self.base_offset == 0 && self.planned_bytes() == self.total_bytes
	}

	/// 空计划（空文件）视为"所有区间已完成"。
	fn all_finished(&self) -> bool {
		self.slots
			.iter()
			.all(|slot| slot.state == RangeSlotState::Finished)
	}

	fn find_exact(&self, offset: u64, length: u64) -> Option<usize> {
		self.slots
			.iter()
			.position(|slot| slot.offset == offset && slot.length == length)
	}

	fn slot_at_offset(&self, offset: u64) -> Option<usize> {
		self.slots.iter().position(|slot| slot.offset == offset)
	}

	fn overlaps_any(&self, offset: u64, length: u64) -> bool {
		let candidate = TransferRange { offset, length };
		self.slots.iter().any(|slot| {
			slot.length > 0
				&& candidate.length > 0
				&& slot.offset < candidate.end()
				&& candidate.offset < slot.end()
		})
	}

	fn set_state(&mut self, index: usize, state: RangeSlotState) {
		if let Some(slot) = self.slots.get_mut(index) {
			slot.state = state;
		}
	}
}

/// 数据流头相对区间计划的路由结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeRoute {
	/// 计划内、尚未接入：正常运行。
	Attach(usize),
	/// 计划内但之前失败：允许重试（同一 offset/length，从区间起点重写）。
	Retry(usize),
	/// 该区间已接入/已完成：拒绝重复流。
	Duplicate,
	/// 与某个计划区间重叠但不是完整匹配：重叠/错位。
	Overlap,
	/// offset 匹配但 length 不符（例如"区间比声明的长"）。
	LengthMismatch,
	/// 既不在计划内也不与任何区间重叠：未声明区间。
	Unexpected,
}

fn route_range_header(ranges: &ReceiverRanges, offset: u64, length: u64) -> RangeRoute {
	if let Some(index) = ranges.find_exact(offset, length) {
		return match ranges.slots[index].state {
			RangeSlotState::Awaiting => RangeRoute::Attach(index),
			RangeSlotState::Failed => RangeRoute::Retry(index),
			RangeSlotState::Attached | RangeSlotState::Finished => RangeRoute::Duplicate,
		};
	}
	if ranges.slot_at_offset(offset).is_some() {
		return RangeRoute::LengthMismatch;
	}
	if ranges.overlaps_any(offset, length) {
		return RangeRoute::Overlap;
	}
	RangeRoute::Unexpected
}

/// `ContentEnd` 的决策：只有**全部计划区间都已完成**才允许 finalize。
///
/// 计划区间还没全 FIN → `WaitForPayload`，由 sweep 在
/// [`CONTENT_END_WITHOUT_PAYLOAD_GRACE_MS`] 之后判 `TRANSFER_RANGE_MISSING`；
/// 绝不提前 commit 一个缺段的临时文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentEndRoute {
	FinalizeNow,
	WaitForPayload,
	Ignore,
}

fn content_end_route(all_ranges_finished: bool, end_already_received: bool) -> ContentEndRoute {
	if end_already_received {
		return ContentEndRoute::Ignore;
	}
	if all_ranges_finished {
		ContentEndRoute::FinalizeNow
	} else {
		ContentEndRoute::WaitForPayload
	}
}

/// 收到的数据流头对应的接收端实体状态（仅用于决定"暂存 / 接入 / 拒绝"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadRoute {
	/// 尚未收到 `ContentBegin`：暂存，等待注册（有 `PAYLOAD_REGISTRATION_GRACE_MS` 上限）。
	AwaitRegistration,
	/// 已有 `Receiving` 的 receiver：交给区间路由判定。
	ReceiverReady,
	/// 同一 req_id 已进入最终化：拒绝。
	Duplicate,
	/// 该 req_id 刚被取消：立即拒绝，绝不重新接入。
	Cancelled,
}

fn route_payload_stream(
	receiver_exists: bool,
	commit_in_progress: bool,
	cancelled: bool,
) -> PayloadRoute {
	if cancelled {
		return PayloadRoute::Cancelled;
	}
	if commit_in_progress {
		return PayloadRoute::Duplicate;
	}
	if receiver_exists {
		PayloadRoute::ReceiverReady
	} else {
		PayloadRoute::AwaitRegistration
	}
}

/// 入站数据流的实时统计（actor 与接收任务共享）。
///
/// actor 不再亲自读数据，因此空闲超时/进度/最终日志所需的"已收字节数、最后活跃
/// 时间"由接收任务通过原子变量回写。
struct PayloadStats {
	received_bytes: AtomicU64,
	last_activity_ms: AtomicI64,
}

impl PayloadStats {
	fn new(now: i64) -> Arc<Self> {
		Arc::new(Self {
			received_bytes: AtomicU64::new(0),
			last_activity_ms: AtomicI64::new(now),
		})
	}

	fn received_bytes(&self) -> u64 {
		self.received_bytes.load(Ordering::Relaxed)
	}

	fn last_activity_ms(&self) -> i64 {
		self.last_activity_ms.load(Ordering::Relaxed)
	}

	fn record_chunk(&self, bytes: u64, now: i64) {
		self.received_bytes.fetch_add(bytes, Ordering::Relaxed);
		self.last_activity_ms.store(now, Ordering::Relaxed);
	}

	/// 一条区间流失败/重试时，把它已经计入总数但会被重写覆盖的字节扣回。
	///
	/// 只影响进度事件与空闲超时的"已收字节"，不影响文件内容：重试的数据流
	/// 会从同一 offset 重新写一遍。
	fn discard_bytes(&self, bytes: u64) {
		let _ = self
			.received_bytes
			.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
				Some(current.saturating_sub(bytes))
			});
	}

	fn touch(&self, now: i64) {
		self.last_activity_ms.store(now, Ordering::Relaxed);
	}
}

/// 已读到头部、但还没来得及路由（`ContentBegin` 尚未到达）的数据流。
///
/// B2 起同一传输可能有多条数据流先于控制面的 `ContentBegin` 到达，因此
/// `pending_payloads` 的值是 `Vec<PendingPayload>`，而不是单条。
struct PendingPayload {
	header: TransferHeader,
	recv: RecvStream,
	send: SendStream,
	opened_at_ms: i64,
}

/// 头部读取任务 → actor：一条已完成头部的数据流。
struct PayloadOpened {
	header: TransferHeader,
	recv: RecvStream,
	send: SendStream,
}

/// 接收任务 → actor 的完成/失败事件（B2：按区间上报）。
enum PayloadEvent {
	/// 一个区间干净读到 FIN，且本区间收到的字节数 == 计划长度。
	RangeFinished {
		req_id: String,
		offset: u64,
		received: u64,
	},
	/// 一个区间失败。`retryable=true` 时只标记该区间失败（保留 receiver，
	/// 等发送端降低 N 后重开同一区间）；`false` 时整个传输失败。
	RangeFailed {
		req_id: String,
		offset: u64,
		code: String,
		detail: String,
		retryable: bool,
	},
}

/// 把 item 自身 `content.sha256` 对应的 content_cache 行标记为 present。
///
/// FileList 传输里 `ContentBegin.sha256` 只是**单个文件**的 hash，而 `content_cache`
/// 以 item 的 `content.sha256` 为键；直接用它 UPDATE 会命中 0 行（旧代码还把结果丢弃）。
/// 因此优先取 item 的 `sha256_hex`，查不到时才回退到本次传输的 hash。
///
/// `durable=false` 只在 `large-only` 且大文件（≥ 8 MiB）时传入：blob 已 rename
/// 到最终路径并允许上报完成，但 `sync_data` 还没做；后台 worker / 30s sweep 负责
/// 把它翻回 1。
///
/// 返回 `(实际标记的 sha, 受影响行数)`，调用方负责在 0 行时记录警告而不是硬失败。
fn mark_item_content_cached(
	store: &mut Store,
	item_id: &str,
	fallback_sha: &str,
	now: i64,
	durable: bool,
) -> anyhow::Result<(String, usize)> {
	let item_sha = store
		.get_item_sha256(item_id)?
		.filter(|sha| !sha.is_empty())
		.unwrap_or_else(|| fallback_sha.to_string());
	let rows = store.mark_cache_present_with_durability(&item_sha, now, durable)?;
	Ok((item_sha, rows))
}

/// 数据块越界判定：绝不允许对端写出其声明的 `total_bytes`，也不允许超过硬上限。
///
/// 返回 `Some(code)` 表示本块必须让整个传输失败（code 用于 TRANSFER_FAILED 事件）。
fn chunk_overrun(
	received: u64,
	chunk: u64,
	declared_total: u64,
	hard_cap: u64,
) -> Option<&'static str> {
	let next = received.saturating_add(chunk);
	if next > declared_total {
		Some("TRANSFER_OVERRUN")
	} else if next > hard_cap {
		Some("ITEM_TOO_LARGE")
	} else {
		None
	}
}

/// 定义接收状态
#[allow(dead_code)]
enum ReceiverState {
	Receiving {
		transfer_id: String,
		item_id: String,
		file_id: Option<String>, // 如果是 FileList 中的子文件
		expected_sha256: String,
		mime: String,
		/// writer Task 的入口。B1 起数据由独立接收任务读出来后推进这里，
		/// 最终化时 actor 再用它的 clone 构造 [`ReceiverInbox`] 发 `Finish`。
		tx: mpsc::Sender<ReceiverTaskMsg>,
		/// writer 每写完一块就归还的字节许可（见 [`MAX_INFLIGHT_BYTES_PER_RECEIVER`]）。
		budget: Arc<Semaphore>,
		/// 数据流实时统计（接收任务回写，actor 读取）。
		stats: Arc<PayloadStats>,
		/// writer Task 的 abort 句柄：超时/取消/越界时直接终止并触发 tmp 清理。
		handle: tokio::task::AbortHandle,
		/// 本传输的区间账本（计划来自 `ContentBegin.ranges`，按 offset 精确匹配
		/// 数据流头，跟踪每条区间的 FIN / 失败状态）。
		ranges: ReceiverRanges,
		/// 每个已接入区间的接收任务 abort 句柄（key = 区间 offset）。
		/// 取消/失败时必须 abort 全部区间，不能只停一条。
		range_handles: HashMap<u64, tokio::task::AbortHandle>,
		/// 是否已收到 `ContentEnd`（控制面）。
		end_received: bool,
		/// `ContentEnd` 已到但还有区间未 FIN 时的时间戳；用于
		/// `CONTENT_END_WITHOUT_PAYLOAD_GRACE_MS` 宽限后判 `TRANSFER_RANGE_MISSING`。
		end_wait_started_ms: i64,
		total_bytes: u64,
		/// 本次传输适用的硬上限（来自 size_limits，按 item 类型选定）
		hard_cap_bytes: u64,
		/// 传输开始时间（用于结束时只写一条汇总日志，含耗时）
		started_ts_ms: i64,
	},
	// 已触发 Finish，等待落地结果（避免重复 Finish / 重复 End）
	//
	// 该占位符在最终化完成前一直占用 req_id：否则新到的同 req_id ContentBegin
	// 会与仍在 commit 的旧传输竞争同一个 tmp 文件。
	Committing {
		transfer_id: String,
		item_id: String,
		file_id: Option<String>,
		expected_sha256: String,
		mime: String,
		total_bytes: u64,
		started_ts_ms: i64,
		/// 最终化任务开始时间（兜底回收占位符用）
		finalize_started_ms: i64,
	},

	Done {
		transfer_id: String,
		item_id: String,
		file_id: Option<String>,
		sha256: String,
		mime: String,
		total_bytes: u64,
		local_path: Option<String>,
		done_ts_ms: i64,
	},

	Failed {
		transfer_id: String,
		code: String,
		message: String,
		ts_ms: i64,
	},

	Cancelled {
		transfer_id: String,
		reason: String,
		ts_ms: i64,
	},
}

/// 定义发送任务的消息。
///
/// B1 起发送任务自己持有数据流（读文件 → 写头 → 写字节 → FIN），不再把分块
/// 交回 actor 主循环，因此 `Chunk` 变体被删除；actor 只负责收尾控制消息
/// （`Done` → `ContentEnd`，`Error` → 取消通知）与汇总日志。
enum UploadMsg {
	Done {
		transfer_id: String,
		bytes: u64,
	},
	Error {
		transfer_id: String,
		bytes: u64,
		err: String,
	},
}

/// 发送中的传输：AbortHandle + 汇总统计。
///
/// 统计只用于传输结束时写**一条**汇总日志。逐块日志已移除：
/// 1GB 传输按 256 KiB 分块会写约 4 千条 SQLite 行，且都在 actor 主循环里同步插入。
struct SenderState {
	handle: tokio::task::AbortHandle,
	bytes_sent: u64,
	started_ts_ms: i64,
}

enum ReceiverTaskMsg {
	Chunk {
		/// 本块在临时文件中的**绝对**偏移。B2 的区间任务各自顺序产出，
		/// 但不同区间会交错推进，writer 必须按偏移定位写入（乱序写）。
		offset: u64,
		data: bytes::Bytes,
		/// 在途字节预算许可：writer 写完（或消息被丢弃）才归还，从而把内存占用
		/// 限制在 [`MAX_INFLIGHT_BYTES_PER_RECEIVER`] 以内。
		permit: OwnedSemaphorePermit,
	},
	Finish {
		/// `ContentBegin.sha256`：整文件 hash（不是区间 hash）。
		expected_sha256: String,
		/// 本传输是否覆盖整个文件（base_offset == 0 且区间并集 == total_bytes）。
		/// 只有完整传输才在 commit 前按顺序重读临时文件校验整文件 hash；
		/// 断点续传（非 0 起点）无法重算整文件 hash，跳过校验并记警告。
		verify_whole_file: bool,
		reply_tx: oneshot::Sender<Result<FinalizeOutcome>>, // 返回最终文件路径与持久化状态
	},
}

/// 最终化成功后的产物。
///
/// - `path`：对外可见的最终路径（materialize 后的视图或 blob 本身）；
/// - `blob_path`：CAS 里已 rename 的 blob（后台补同步按它取 `sync_data`）；
/// - `durability`：本次 commit 是否已同步落盘 / 是否 pending / 是否按 never 跳过。
struct FinalizeOutcome {
	path: PathBuf,
	blob_path: PathBuf,
	durability: DurabilityOutcome,
}

/// 单个入站传输的投递队列（writer Task 之前）。
///
/// 背景：B1 之前所有传输的帧复用同一条双向流，actor 只能逐个 `reader.next()`
/// 读帧；一旦在某个传输的 writer 有界 channel 上 `.await`，整条连接都会被队头
/// 阻塞。因此旧路径必须是非阻塞的 try_send + pending 暂存。
///
/// B1 起每个传输独占一条数据流，接收任务可以安全地等待自己的 writer，因此
/// 正常路径走 [`ReceiverInbox::push_or_wait`]；`try_push` / `flush_pending` 的
/// 非阻塞路径保留给最终化与单元测试，并继续守住
/// [`MAX_PENDING_BYTES_PER_RECEIVER`] 的积压上限：
///
/// 1. 优先 `try_send`；成功即完成，永不等待；
/// 2. channel 满或 12 MiB 在途预算耗尽 → 把块放进 `pending` 并标记 `paused`；
/// 3. `push_or_wait` 在 Paused 后立即 `flush_all`，等待 writer 排空后按原顺序补发；
/// 4. `pending` 超过 4 MiB → 判定 writer 完全跟不上，该传输失败（不丢数据、
///    不静默损坏，也不把内存变成无界）。
struct ReceiverInbox {
	tx: mpsc::Sender<ReceiverTaskMsg>,
	budget: Arc<Semaphore>,
	/// 下一个数据块在临时文件里的绝对偏移。区间内块是顺序到达的，因此只需
	/// 单调前进；不同区间交错写入由 writer 按偏移 seek 处理。
	next_offset: u64,
	pending: VecDeque<(u64, bytes::Bytes)>,
	pending_bytes: usize,
	paused: bool,
}

/// [`ReceiverInbox::try_push`] / [`ReceiverInbox::flush_pending`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushOutcome {
	/// 已全部进入 writer channel（pending 也清空了）。
	Queued,
	/// 仍有积压，数据已暂存，等 writer 恢复后继续。
	Paused,
	/// pending 预算耗尽，该传输必须中止。
	Overflow,
	/// writer Task 已退出。
	Closed,
}

impl ReceiverInbox {
	fn new(tx: mpsc::Sender<ReceiverTaskMsg>, budget: Arc<Semaphore>, range_offset: u64) -> Self {
		Self {
			tx,
			budget,
			next_offset: range_offset,
			pending: VecDeque::new(),
			pending_bytes: 0,
			paused: false,
		}
	}

	/// 把 pending 中的块按顺序推进 writer channel，**永不 await**。
	fn flush_pending(&mut self) -> PushOutcome {
		while let Some((offset, front)) = self.pending.front() {
			// 块大小受 UPLOAD_CHUNK_BYTES 约束，一定放得进 u32；异常超大值按不可获取处理。
			let len = u32::try_from(front.len()).unwrap_or(u32::MAX);
			let Ok(permit) = self.budget.clone().try_acquire_many_owned(len) else {
				return PushOutcome::Paused;
			};
			let offset = *offset;
			let (_, data) = self.pending.pop_front().expect("front() 刚刚确认队列非空");
			self.pending_bytes -= data.len();
			match self.tx.try_send(ReceiverTaskMsg::Chunk {
				offset,
				data,
				permit,
			}) {
				Ok(()) => {}
				Err(mpsc::error::TrySendError::Full(msg)) => {
					// 槽位又被占满：原样放回队首，保证顺序；许可随消息一起 drop。
					// 本方法只会发送 Chunk 变体，其他变体不可能出现。
					if let ReceiverTaskMsg::Chunk { offset, data, .. } = msg {
						self.pending_bytes += data.len();
						self.pending.push_front((offset, data));
					}
					return PushOutcome::Paused;
				}
				Err(mpsc::error::TrySendError::Closed(_)) => {
					self.pending.clear();
					self.pending_bytes = 0;
					return PushOutcome::Closed;
				}
			}
		}
		self.paused = false;
		PushOutcome::Queued
	}

	/// 非阻塞投递一个数据块：先 flush 旧 pending，再直接入 channel。
	fn try_push(&mut self, data: bytes::Bytes) -> PushOutcome {
		let offset = self.next_offset;
		let len = data.len() as u64;
		let outcome = self.try_push_at(offset, data);
		// writer 已退出时数据没人消费，游标前进与否都无所谓；其余路径的数据
		// 都已进入 channel 或 pending，必须前进。
		if outcome != PushOutcome::Closed {
			self.next_offset = offset.saturating_add(len);
		}
		outcome
	}

	fn try_push_at(&mut self, offset: u64, data: bytes::Bytes) -> PushOutcome {
		if self.paused || !self.pending.is_empty() {
			match self.flush_pending() {
				PushOutcome::Closed => return PushOutcome::Closed,
				PushOutcome::Paused => return self.push_pending(offset, data),
				// 积压已全部推进 channel，可以继续走下面的直发路径。
				PushOutcome::Queued | PushOutcome::Overflow => {}
			}
		}

		let permit_len = u32::try_from(data.len()).unwrap_or(u32::MAX);
		match self.budget.clone().try_acquire_many_owned(permit_len) {
			Ok(permit) => match self.tx.try_send(ReceiverTaskMsg::Chunk {
				offset,
				data,
				permit,
			}) {
				Ok(()) => PushOutcome::Queued,
				Err(mpsc::error::TrySendError::Full(msg)) => {
					// 本方法只会发送 Chunk 变体，其他变体不可能出现。
					if let ReceiverTaskMsg::Chunk { data, .. } = msg {
						self.push_pending(offset, data)
					} else {
						PushOutcome::Paused
					}
				}
				Err(mpsc::error::TrySendError::Closed(_)) => PushOutcome::Closed,
			},
			// 在途字节预算耗尽（writer 慢）：暂存而不是等待。
			Err(_) => self.push_pending(offset, data),
		}
	}

	fn push_pending(&mut self, offset: u64, data: bytes::Bytes) -> PushOutcome {
		if self.pending_bytes.saturating_add(data.len()) > MAX_PENDING_BYTES_PER_RECEIVER {
			return PushOutcome::Overflow;
		}
		self.pending_bytes += data.len();
		self.pending.push_back((offset, data));
		self.paused = true;
		PushOutcome::Paused
	}

	/// 接收任务专用：投递一个数据块，背压满时**等待** writer 排空。
	///
	/// 与 `try_push` 的唯一区别是 Paused 后会调用 [`Self::flush_all`]（允许 await）。
	/// 这条路径只被单条区间数据流的接收任务使用，因此等待只会阻塞本区间，
	/// 不会影响控制流、心跳或其他传输。
	async fn push_or_wait(&mut self, data: bytes::Bytes) -> PushOutcome {
		match self.try_push(data) {
			PushOutcome::Paused => match self.flush_all().await {
				Ok(()) => PushOutcome::Queued,
				Err(_) => PushOutcome::Closed,
			},
			other => other,
		}
	}

	/// 最终化时在**独立任务**里排空 pending（这里允许 await，不占 actor 主循环）。
	///
	/// 必须先把 pending 按序送进 writer，再发 `Finish`，否则会丢块或校验失败。
	async fn flush_all(&mut self) -> std::result::Result<(), String> {
		while let Some((offset, data)) = self.pending.pop_front() {
			self.pending_bytes -= data.len();
			let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
			let permit = self
				.budget
				.clone()
				.acquire_many_owned(len)
				.await
				.map_err(|_| "writer channel closed".to_string())?;
			self.tx
				.send(ReceiverTaskMsg::Chunk {
					offset,
					data,
					permit,
				})
				.await
				.map_err(|_| "writer channel closed".to_string())?;
		}
		self.paused = false;
		Ok(())
	}
}

/// 最终化任务回传给 actor 的完成事件（在 `tokio::select!` 分支里处理）。
struct ReceiverDone {
	transfer_id: String,
	item_id: String,
	file_id: Option<String>,
	expected_sha256: String,
	received_bytes: u64,
	total_bytes: u64,
	started_ts_ms: i64,
	/// `Err((code, message))` 直接映射到 TRANSFER_FAILED。
	result: std::result::Result<FinalizeOutcome, (String, String)>,
}

/// 在独立任务里驱动 writer 完成 flush / close / 校验 / commit / materialize。
///
/// 注意：这里的每一处 `.await` 都在 actor 主循环之外，因此慢磁盘只影响这条传输。
async fn finalize_receiver(
	mut inbox: ReceiverInbox,
	expected_sha256: String,
	verify_whole_file: bool,
) -> std::result::Result<FinalizeOutcome, (String, String)> {
	inbox
		.flush_all()
		.await
		.map_err(|e| ("WRITE_TASK_DEAD".to_string(), e))?;

	let (reply_tx, reply_rx) = oneshot::channel();
	inbox
		.tx
		.send(ReceiverTaskMsg::Finish {
			expected_sha256,
			verify_whole_file,
			reply_tx,
		})
		.await
		.map_err(|_| {
			(
				"WRITE_TASK_DEAD".to_string(),
				"Writer task crashed".to_string(),
			)
		})?;

	match reply_rx.await {
		Ok(Ok(outcome)) => Ok(outcome),
		Ok(Err(e)) => Err(("COMMIT_FAILED".to_string(), e.to_string())),
		Err(_) => Err((
			"COMMIT_TIMEOUT".to_string(),
			"Writer task dropped reply".to_string(),
		)),
	}
}

/// 从数据流读取 `TransferHeader`（`[u32 LE len][postcard]`）。
///
/// 长度前缀先做上限校验：绝不按对端声明的巨大长度分配内存。
async fn read_payload_header(recv: &mut RecvStream) -> Result<TransferHeader> {
	let mut len_buf = [0u8; TRANSFER_HEADER_LEN_BYTES];
	recv.read_exact(&mut len_buf)
		.await
		.context("payload header length read failed")?;
	let len = u32::from_le_bytes(len_buf) as usize;
	if len == 0 || len > MAX_TRANSFER_HEADER_BYTES {
		anyhow::bail!("invalid payload header length: {len} (max {MAX_TRANSFER_HEADER_BYTES})");
	}
	let mut body = vec![0u8; len];
	recv.read_exact(&mut body)
		.await
		.context("payload header body read failed")?;
	TransferHeader::decode(&body)
}

/// 在数据流上写入头部（随后由调用方写原始文件字节并 `finish`）。
async fn write_payload_header(send: &mut SendStream, header: &TransferHeader) -> Result<()> {
	let frame = encode_transfer_header_frame(header)?;
	send.write_all(&frame)
		.await
		.context("payload header write failed")
}

/// 每传输数据流的 accept loop（与 actor 并行运行）。
///
/// 它只做一件事：`accept_bi()` → 在独立任务里读头部（带超时与并发上限）→ 把
/// `PayloadOpened` 交给 actor 路由。真正的数据接收由 actor 在路由成功后 spawn
/// 的接收任务完成。
///
/// 头部读取必须放在独立任务里：否则一个打开流后不写头的对端就能把 accept loop
/// 卡住，让同一连接上后续所有数据流都无法接入。
fn spawn_payload_accept_loop(
	conn: Connection,
	opened_tx: mpsc::Sender<PayloadOpened>,
	log_sink: crate::logs::LogSink,
) -> tokio::task::AbortHandle {
	let task = tokio::spawn(async move {
		// 头部读取任务的并发上限。B1 每个传输只有一条数据流，正常并发 ≤ 8；
		// 留 16 个槽位吸收"已取消/已结束"前后的短暂重叠。
		let readers = Arc::new(Semaphore::new(MAX_PAYLOAD_HEADER_READERS));
		loop {
			let Ok((send, mut recv)) = conn.accept_bi().await else {
				// 连接已关闭 / 会话结束：accept loop 自然退出。
				break;
			};

			let Ok(permit) = readers.clone().try_acquire_owned() else {
				log_sink.log_warn(
					"Session",
					&format!(
						"Rejecting payload stream: {MAX_PAYLOAD_HEADER_READERS} header reads already in flight"
					),
					Some(&format!(
						"拒绝数据流：已有 {MAX_PAYLOAD_HEADER_READERS} 个头部读取任务在处理"
					)),
				);
				let mut send = send;
				let mut recv = recv;
				let _ = send.reset(0u32.into());
				let _ = recv.stop(0u32.into());
				continue;
			};

			let tx = opened_tx.clone();
			let log = log_sink.clone();
			tokio::spawn(async move {
				// 许可在头部读取完成后释放；数据流本身由 actor/接收任务继续持有。
				let _permit = permit;
				match tokio::time::timeout(PAYLOAD_HEADER_TIMEOUT, read_payload_header(&mut recv))
					.await
				{
					Ok(Ok(header)) => {
						// actor 已结束时 send 失败，数据流随闭包结束一起关闭。
						let _ = tx.send(PayloadOpened { header, recv, send }).await;
					}
					Ok(Err(e)) => {
						log.log_warn(
							"Session",
							&format!("Rejected payload stream: invalid header: {e}"),
							Some(&format!("拒绝数据流：头部无效: {e}")),
						);
					}
					Err(_) => {
						log.log_warn(
							"Session",
							&format!(
								"Rejected payload stream: header not received within {}ms",
								PAYLOAD_HEADER_TIMEOUT.as_millis()
							),
							Some(&format!(
								"拒绝数据流：{}ms 内未收到头部",
								PAYLOAD_HEADER_TIMEOUT.as_millis()
							)),
						);
					}
				}
			});
		}
	});
	task.abort_handle()
}

/// 发送端一个区间的失败分类。
///
/// 只有**网络**错误才值得降 N 后重试同一区间（接收端会保留 Failed 槽位并
/// 覆盖重写）；本地文件读/seek 错误通常是永久性的，直接让传输失败。
enum RangeSendError {
	Network { bytes: u64, message: String },
	Local { bytes: u64, message: String },
}

impl RangeSendError {
	fn bytes(&self) -> u64 {
		match self {
			Self::Network { bytes, .. } | Self::Local { bytes, .. } => *bytes,
		}
	}

	fn message(&self) -> &str {
		match self {
			Self::Network { message, .. } | Self::Local { message, .. } => message,
		}
	}

	fn retryable(&self) -> bool {
		matches!(self, Self::Network { .. })
	}
}

/// 在一条数据流上发送**一个子区间**（B2）。
///
/// 头部写 `[u32 LE len][postcard(TransferHeader)]`，随后只写
/// `header.length` 个字节（不会读到文件 EOF 之后），最后 FIN。文件在区间起点
/// `seek`，因此多个区间任务可以并发读同一个文件、各自从自己的 offset 开始。
///
/// `bytes_sent` 是整个传输共享的原子计数：每写成功一块就累加，供控制器计算
/// 窗口 goodput；失败时把本次尝试已写的字节扣回，避免重试把 goodput 虚高。
async fn send_payload_range(
	conn: &Connection,
	path: &std::path::Path,
	header: TransferHeader,
	bytes_sent: &AtomicU64,
	budget: Arc<Semaphore>,
) -> std::result::Result<u64, RangeSendError> {
	// 会话级流数闸门：拿不到许可时等待，绝不无限开流。
	let _stream_permit = budget
		.acquire_owned()
		.await
		.map_err(|_| RangeSendError::Network {
			bytes: 0,
			message: "payload stream budget closed".to_string(),
		})?;

	let (mut send, _recv) = conn.open_bi().await.map_err(|e| RangeSendError::Network {
		bytes: 0,
		message: format!("open payload stream failed: {e}"),
	})?;
	write_payload_header(&mut send, &header)
		.await
		.map_err(|e| RangeSendError::Network {
			bytes: 0,
			message: e.to_string(),
		})?;

	let mut file = File::open(path).await.map_err(|e| RangeSendError::Local {
		bytes: 0,
		message: format!("payload file open failed: {e}"),
	})?;
	if header.offset > 0 {
		file.seek(SeekFrom::Start(header.offset))
			.await
			.map_err(|e| RangeSendError::Local {
				bytes: 0,
				message: format!("payload file seek failed: {e}"),
			})?;
	}
	let mut reader = BufReader::new(file);
	let mut buf = vec![0u8; UPLOAD_CHUNK_BYTES];
	let mut sent = 0u64;

	while sent < header.length {
		// 只读到本区间末尾：文件比声明长也不会把下一个区间的字节重复写入。
		let want = (header.length - sent).min(buf.len() as u64) as usize;
		let n = match reader.read(&mut buf[..want]).await {
			Ok(0) => {
				return Err(RangeSendError::Local {
					bytes: sent,
					message: format!(
						"payload file ended before range length: sent={sent}/{}",
						header.length
					),
				});
			}
			Ok(n) => n,
			Err(e) => {
				return Err(RangeSendError::Local {
					bytes: sent,
					message: format!("payload read failed: {e}"),
				});
			}
		};
		let timeout = chunk_send_timeout(n);
		match tokio::time::timeout(timeout, send.write_all(&buf[..n])).await {
			Ok(Ok(())) => {
				sent = sent.saturating_add(n as u64);
				bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
			}
			Ok(Err(e)) => {
				rollback_sent_bytes(bytes_sent, sent);
				return Err(RangeSendError::Network {
					bytes: sent,
					message: format!("payload stream write failed: {e}"),
				});
			}
			Err(_) => {
				rollback_sent_bytes(bytes_sent, sent);
				return Err(RangeSendError::Network {
					bytes: sent,
					message: format!(
						"Send data chunk timeout (network congestion): chunk_bytes={n}, timeout_ms={}",
						timeout.as_millis()
					),
				});
			}
		}
	}

	// FIN：接收端把"读到 FIN"当作"本区间字节已全部到达"。
	if let Err(e) = send.finish() {
		rollback_sent_bytes(bytes_sent, sent);
		return Err(RangeSendError::Network {
			bytes: sent,
			message: format!("payload stream finish failed: {e}"),
		});
	}
	Ok(sent)
}

/// 区间发送失败时扣回本次尝试已写入的字节（供控制器 goodput 使用）。
fn rollback_sent_bytes(bytes_sent: &AtomicU64, sent: u64) {
	let _ = bytes_sent.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
		Some(current.saturating_sub(sent))
	});
}

/// 一个传输的分片发送任务（发送端）。
///
/// 控制器在此任务里以 `T = clamp(4×srtt, 300ms, 2s)` 评估窗口 goodput / 丢包 /
/// srtt，并用纯函数 [`decide_stream_count`] 决定目标并发 N：
///
/// - 从 N=1 起步，每个窗口在"还有未发送区间"时尝试 N+1；
/// - 下个窗口若 goodput 增益 ≤ 8%：回退 N 并标记 `ceiling`（本传输不再上探）；
/// - 丢包 > 1% 或 srtt > 2.5× 基线：回退一条并冷却 2T；
/// - 剩余 ≤ 16 MiB：强制 N=1，不再探针；
/// - 区间流失败：先降 N、冷却 2T，再重试同一区间（最多 3 次）。
///
/// 计划区间是**固定**的（见 [`initial_range_plan`]）：N 只决定"同时消费几个
/// 尚未发送的区间"。因此 `target_n` 调小时，已经在飞的区间不会被中断（不会
/// 留下空洞）；它只是不再领取新区间，等活跃数自然降到目标后继续。
struct PayloadPlanJob {
	conn: Connection,
	path: PathBuf,
	req_id: String,
	item_id: String,
	file_id: Option<String>,
	sha256: String,
	mime: String,
	total_bytes: u64,
	ranges: Vec<TransferRange>,
	budget: Arc<Semaphore>,
	log_sink: crate::logs::LogSink,
}

#[allow(clippy::cast_precision_loss)]
async fn drive_payload_plan(job: PayloadPlanJob) -> std::result::Result<u64, (u64, String)> {
	let PayloadPlanJob {
		conn,
		path,
		req_id,
		item_id,
		file_id,
		sha256,
		mime,
		total_bytes,
		ranges,
		budget,
		log_sink,
	} = job;

	let bytes_sent = Arc::new(AtomicU64::new(0));
	let mut pending: VecDeque<usize> = (0..ranges.len()).collect();
	// 失败区间的重试不能"立刻用同一个 N 重开"：先进 deferred，等 2T 冷却结束
	// 后（由下面的 tick 唤醒）再放回 pending，以降低后的 N 重试。
	let mut deferred_retries: Vec<usize> = Vec::new();
	let mut retries = vec![0u8; ranges.len()];
	let mut join_set: tokio::task::JoinSet<(usize, std::result::Result<u64, RangeSendError>)> =
		tokio::task::JoinSet::new();
	let mut active = 0usize;

	let mut target_n: u8 = 1;
	let mut ceiling = false;
	let mut probing = false;
	let mut probe_baseline_goodput = 0.0f64;
	let mut cooldown_until = tokio::time::Instant::now();

	let initial_stats = conn.stats();
	let srtt_base = initial_stats.path.rtt.max(Duration::from_millis(2));
	let mut last_bytes = 0u64;
	let mut last_lost = initial_stats.path.lost_packets;
	let mut last_sent = initial_stats.path.sent_packets;
	let mut last_sample_at = tokio::time::Instant::now();
	let mut tick = tokio::time::interval_at(
		tokio::time::Instant::now() + stream_probe_interval(srtt_base),
		stream_probe_interval(srtt_base),
	);
	tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

	loop {
		// 冷却结束：把退避中的失败区间放回待发送队列（以降低后的 N 重试）。
		if !deferred_retries.is_empty() && tokio::time::Instant::now() >= cooldown_until {
			pending.extend(deferred_retries.drain(..));
		}

		// 领取新区间：只受目标并发 N 与剩余区间数限制。
		while active < target_n as usize {
			let Some(index) = pending.pop_front() else {
				break;
			};
			let range = ranges[index];
			let header = TransferHeader {
				req_id: req_id.clone(),
				item_id: item_id.clone(),
				file_id: file_id.clone(),
				offset: range.offset,
				length: range.length,
				sha256: sha256.clone(),
				mime: mime.clone(),
			};
			let conn = conn.clone();
			let path = path.clone();
			let counter = bytes_sent.clone();
			let budget = budget.clone();
			join_set.spawn(async move {
				let result = send_payload_range(&conn, &path, header, &counter, budget).await;
				(index, result)
			});
			active += 1;
		}

		if active == 0 && pending.is_empty() && deferred_retries.is_empty() {
			break;
		}

		tokio::select! {
			joined = join_set.join_next(), if active > 0 => {
				let Some(joined) = joined else { break };
				active = active.saturating_sub(1);
				let (index, result) = match joined {
					Ok(value) => value,
					Err(e) => {
						return Err((
							bytes_sent.load(Ordering::Relaxed),
							format!("payload range task panicked: {e}"),
						));
					}
				};
				match result {
					Ok(sent) => {
						let expected = ranges[index].length;
						if sent != expected {
							return Err((
								bytes_sent.load(Ordering::Relaxed),
								format!(
									"range {} sent {sent} bytes, expected {expected}",
									ranges[index].offset
								),
							));
						}
					}
					Err(error) => {
						let total = bytes_sent.load(Ordering::Relaxed);
						if error.retryable() && retries[index] < MAX_RANGE_STREAM_RETRIES {
							retries[index] += 1;
							// 关键：失败后**不能**立刻用同一个 N 重试；降一条流并冷却 2T。
							target_n = target_n.saturating_sub(1).max(1);
							probing = false;
							let cooldown = stream_probe_interval(conn.stats().path.rtt);
							cooldown_until = tokio::time::Instant::now() + cooldown.saturating_mul(2);
							log_sink.log_warn(
								"Session",
								&format!(
									"Range stream failed, backing off before retry: transfer_id={req_id}, offset={}, attempt={}/{}, new_n={target_n}, error={}",
									ranges[index].offset,
									retries[index],
									MAX_RANGE_STREAM_RETRIES,
									error.message()
								),
								Some(&format!(
									"区间数据流失败，先退避再重试: 传输ID={req_id}，偏移={}，第 {}/{} 次，新并发={target_n}，错误={}",
									ranges[index].offset,
									retries[index],
									MAX_RANGE_STREAM_RETRIES,
									error.message()
								)),
							);
							deferred_retries.push(index);
						} else {
							return Err((
								total,
								format!(
									"range {} failed after {} attempt(s) (last attempt sent {} bytes): {}",
									ranges[index].offset,
									retries[index] + 1,
									error.bytes(),
									error.message()
								),
							));
						}
					}
				}
			}
			_ = tick.tick() => {
				let now = tokio::time::Instant::now();
				let stats = conn.stats();
				let elapsed = now.duration_since(last_sample_at).as_secs_f64().max(1e-3);
				let bytes_now = bytes_sent.load(Ordering::Relaxed);
				let window_goodput =
					bytes_now.saturating_sub(last_bytes) as f64 / elapsed;
				let sent_delta = stats.path.sent_packets.saturating_sub(last_sent);
				let lost_delta = stats.path.lost_packets.saturating_sub(last_lost);
				let loss_rate = if sent_delta > 0 {
					lost_delta as f64 / sent_delta as f64
				} else {
					0.0
				};
				let srtt = stats.path.rtt.max(Duration::from_micros(50));
				let srtt_ratio = srtt.as_secs_f64() / srtt_base.as_secs_f64();
				let remaining = total_bytes.saturating_sub(bytes_now);

				last_bytes = bytes_now;
				last_lost = stats.path.lost_packets;
				last_sent = stats.path.sent_packets;
				last_sample_at = now;

				let interval = stream_probe_interval(srtt);
				tick = tokio::time::interval_at(now + interval, interval);
				tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

				// 冷却期内只采样，不做决策（退避后的 N 保持至少 2T）。
				if now < cooldown_until {
					continue;
				}

				let gain = if probing && probe_baseline_goodput > 0.0 {
					Some((window_goodput - probe_baseline_goodput) / probe_baseline_goodput)
				} else {
					None
				};
				let decision = decide_stream_count(StreamControllerInput {
					current_n: target_n,
					gain,
					loss_rate,
					srtt_ratio,
					remaining_bytes: remaining,
					ceiling_reached: ceiling,
				});
				if decision.next_n != target_n || decision.reason != StreamDecisionReason::Hold {
					log_sink.log_info(
						"Session",
						&format!(
							"transfer.adapt transfer_id={req_id} old_n={target_n} new_n={} reason={:?} gain={gain:?} loss_rate={loss_rate:.4} srtt_ratio={srtt_ratio:.2} window_goodput={window_goodput:.0}B/s remaining={remaining}",
							decision.next_n, decision.reason
						),
						Some(&format!(
							"传输流数自适应: 传输ID={req_id}，旧并发={target_n}，新并发={}，原因={:?}，增益={gain:?}，丢包率={loss_rate:.4}，RTT倍数={srtt_ratio:.2}，窗口吞吐={window_goodput:.0}B/s，剩余={remaining}",
							decision.next_n, decision.reason
						)),
					);
				}

				match decision.reason {
					StreamDecisionReason::ProbeStarted | StreamDecisionReason::ProbeAccepted => {
						probing = true;
						probe_baseline_goodput = window_goodput;
					}
					StreamDecisionReason::ProbeRejected => {
						// decision 已把 ceiling 置 true，本传输不再上探。
						probing = false;
					}
					StreamDecisionReason::CongestionBackoff => {
						probing = false;
						cooldown_until = now + interval.saturating_mul(2);
					}
					StreamDecisionReason::SmallTransfer
					| StreamDecisionReason::Ceiling
					| StreamDecisionReason::Hold => {
						probing = false;
					}
				}
				// 目标并发不能超过计划区间数（LAN 只切 2 段时，探针最多到 2）。
				let plan_cap = ranges.len().clamp(1, MAX_PAYLOAD_STREAMS_PER_TRANSFER) as u8;
				target_n = decision.next_n.min(plan_cap);
				ceiling = decision.ceiling_reached;
			}
		}
	}

	Ok(bytes_sent.load(Ordering::Relaxed))
}

/// 接收端单条**区间**数据流的读取任务（B2）。
///
/// 它把数据流字节聚合成 [`UPLOAD_CHUNK_BYTES`] 大小的块，经
/// [`ReceiverInbox::push_or_wait`] 推进 writer channel（块携带绝对偏移，writer
/// 负责乱序定位写），并维护 [`PayloadStats`] 与进度事件。
///
/// 失败语义：
/// - 本区间字节数超过 `range_length` / 总大小 / 硬上限：**致命**，整传输失败；
/// - FIN 时字节数不足 `range_length`：**致命**（`TRANSFER_RANGE_UNDERRUN`），
///   绝不把缺字节的区间当成完成；
/// - 网络 reset/连接中断：**可重试**（`retryable=true`），actor 只把该区间标记
///   为 Failed；发送端降 N 后从同一 offset 重写。
#[allow(clippy::too_many_arguments)]
async fn receive_payload_stream(
	mut recv: RecvStream,
	_send: SendStream,
	req_id: String,
	item_id: String,
	range_offset: u64,
	range_length: u64,
	total_bytes: u64,
	hard_cap_bytes: u64,
	mut inbox: ReceiverInbox,
	stats: Arc<PayloadStats>,
	sink: Arc<dyn CoreEventSink>,
) -> PayloadEvent {
	let mut buf = bytes::BytesMut::with_capacity(UPLOAD_CHUNK_BYTES);
	let mut range_received = 0u64;
	let mut last_progress_emit = 0i64;

	macro_rules! fail {
		($code:expr, $detail:expr, $retryable:expr) => {
			return PayloadEvent::RangeFailed {
				req_id,
				offset: range_offset,
				code: $code.to_string(),
				detail: $detail,
				retryable: $retryable,
			}
		};
	}

	loop {
		// 单次最多读一个分块大小：`Chunk` 边界不代表对端 write 边界，限制上限可以
		// 让聚合缓冲区（≤ 2 个分块）与在途预算保持可预测。
		match recv.read_chunk(UPLOAD_CHUNK_BYTES, true).await {
			Ok(Some(chunk)) => {
				buf.extend_from_slice(&chunk.bytes);
				if buf.len() < UPLOAD_CHUNK_BYTES {
					continue;
				}
				let data = buf.split().freeze();
				let chunk_len = data.len() as u64;

				// 区间越界：本区间声明的 length 是硬边界，多 1 字节都失败。
				if range_received.saturating_add(chunk_len) > range_length {
					fail!(
						"TRANSFER_RANGE_OVERRUN",
						format!(
							"range {range_offset} received={range_received}+{chunk_len}, declared_length={range_length}"
						),
						false
					);
				}
				let absolute_received = range_offset.saturating_add(range_received);
				if let Some(code) =
					chunk_overrun(absolute_received, chunk_len, total_bytes, hard_cap_bytes)
				{
					fail!(
						code,
						format!(
							"absolute_received={absolute_received}+{chunk_len}, declared_total={total_bytes}, hard_cap={hard_cap_bytes}"
						),
						false
					);
				}

				match inbox.push_or_wait(data).await {
					PushOutcome::Queued | PushOutcome::Paused => {}
					PushOutcome::Overflow => fail!(
						"TRANSFER_BACKPRESSURE",
						format!(
							"writer backpressure: pending buffer exceeded {MAX_PENDING_BYTES_PER_RECEIVER} bytes"
						),
						false
					),
					PushOutcome::Closed => fail!(
						"WRITE_TASK_DEAD",
						"writer task exited while receiving payload".to_string(),
						false
					),
				}

				range_received = range_received.saturating_add(chunk_len);
				let now = now_ms();
				stats.record_chunk(chunk_len, now);
				if now - last_progress_emit > 200 {
					// 与旧实现同样的事件结构与字段；received 是整传输累计值。
					let progress_evt = serde_json::json!({
						"type": "TRANSFER_PROGRESS",
						"payload": {
							"transfer_id": req_id,
							"item_id": item_id,
							"state": "downloading",
							"received": stats.received_bytes(),
							"total": total_bytes
						}
					});
					sink.emit(progress_evt.to_string());
					last_progress_emit = now;
				}
			}
			Ok(None) => {
				// FIN：把最后不足一个分块的尾巴也交给 writer。
				if !buf.is_empty() {
					let data = buf.split().freeze();
					let chunk_len = data.len() as u64;
					if range_received.saturating_add(chunk_len) > range_length {
						fail!(
							"TRANSFER_RANGE_OVERRUN",
							format!(
								"range {range_offset} received={range_received}+{chunk_len}, declared_length={range_length}"
							),
							false
						);
					}
					let absolute_received = range_offset.saturating_add(range_received);
					if let Some(code) =
						chunk_overrun(absolute_received, chunk_len, total_bytes, hard_cap_bytes)
					{
						fail!(
							code,
							format!(
								"absolute_received={absolute_received}+{chunk_len}, declared_total={total_bytes}, hard_cap={hard_cap_bytes}"
							),
							false
						);
					}
					match inbox.push_or_wait(data).await {
						PushOutcome::Queued | PushOutcome::Paused => {
							range_received = range_received.saturating_add(chunk_len);
							stats.record_chunk(chunk_len, now_ms());
						}
						PushOutcome::Overflow => fail!(
							"TRANSFER_BACKPRESSURE",
							format!(
								"writer backpressure: pending buffer exceeded {MAX_PENDING_BYTES_PER_RECEIVER} bytes"
							),
							false
						),
						PushOutcome::Closed => fail!(
							"WRITE_TASK_DEAD",
							"writer task exited while receiving payload".to_string(),
							false
						),
					}
				}
				if range_received != range_length {
					stats.discard_bytes(range_received);
					fail!(
						"TRANSFER_RANGE_UNDERRUN",
						format!(
							"range {range_offset} finished early: received={range_received}, declared_length={range_length}"
						),
						false
					);
				}
				stats.touch(now_ms());
				return PayloadEvent::RangeFinished {
					req_id,
					offset: range_offset,
					received: range_received,
				};
			}
			Err(e) => {
				// 网络错误可重试：把本区间已计入进度的字节扣回，发送端会从
				// 区间起点重写（writer 按偏移覆盖）。
				stats.discard_bytes(range_received);
				fail!(
					"TRANSFER_STREAM_ERROR",
					format!("payload stream reset/closed before FIN: {e}"),
					true
				);
			}
		}
	}
}

/// 顺序重读整个临时文件并计算整文件 SHA-256（同步版本）。
///
/// 只在 `spawn_blocking` 的最终化任务里调用。B2 的区间是乱序完成的，接收端
/// 无法在写入路径上按顺序增量哈希；这是 ADR-002 §4.2 两种完整性方案中的第二种：
/// **在所有区间 FIN + ContentEnd 之后，按偏移顺序重读临时文件、重算整文件 hash**，
/// 与 `ContentBegin.sha256` 比对后再 commit。
///
/// 成本（release，本机实测口径）：额外一次顺序读（NVMe ~4.7 GiB/s），哈希本身
/// 用 ring 的 SHA-256（SHA-NI，约 4~6 GiB/s；sha2 纯 Rust 约 2.5 GiB/s）。
/// 256 MiB 传输约 +55ms 读 + 40~60ms hash（读与 hash 在同一循环里重叠，墙钟
/// 主要受较慢者限制）；B1 原本在 writer 写路径上逐块哈希，B2 把同一份 hash CPU
/// 挪到最终化阶段，净 CPU 基本不变，只多一次顺序读。
/// 换来的是：区间乱序/重叠/缺段无法伪造通过、且能校验发送端声明的**整文件**
/// hash（逐区间 hash 只能证明每个区间自洽，无法证明拼接顺序与整文件 hash 一致）。
fn sha256_file_hex(path: &std::path::Path) -> anyhow::Result<String> {
	use ring::digest::{Context, SHA256};
	use std::io::Read;
	let mut file = std::fs::File::open(path)?;
	// 用 ring 的 SHA-256（带 SHA-NI/汇编优化）而不是 sha2 的纯 Rust 实现：
	// finalize 的哈希在传输尾部串行执行，256 MiB 按 sha2 实测约 100 ms，是
	// 端到端吞吐的主要尾延迟；ring 在本机约 2-3× 快，能把这段成本压到与 B1
	// 写路径重叠哈希相当的水平。输出仍是标准 SHA-256，与 ContentBegin 一致。
	let mut ctx = Context::new(&SHA256);
	let mut buf = vec![0u8; 1024 * 1024];
	loop {
		let n = file.read(&mut buf)?;
		if n == 0 {
			break;
		}
		ctx.update(&buf[..n]);
	}
	Ok(hex::encode(ctx.finish().as_ref()))
}

/// 临时文件守护者 (RAII)，任务结束/Panic时自动删除残留文件
struct TempFileGuard {
	path: PathBuf,
	committed: bool,
}

impl Drop for TempFileGuard {
	fn drop(&mut self) {
		if !self.committed {
			let p = self.path.clone();
			// Drop 是同步的，利用 std::fs 删除，或者 spawn 一个异步删除
			// 为了安全起见，这里仅打印日志或尝试删除
			let _ = std::fs::remove_file(p);
		}
	}
}

/// 会话终止守护 (RAII)：保证 `state_ref` 在所有退出路径上都落到 `Terminated`。
///
/// `run_actor` 里 `conn.open_bi().await?` / `start_handshake().await?` 等早期 `?`
/// 返回、以及任务 panic，都不会执行末尾的状态赋值。若状态停在 TransportReady /
/// Handshaking，`NetManager::maintain_sessions` 就永远不会回收该句柄，
/// `handle_discovery_event` 也会一直认为该设备已有会话而拒绝重连 ——
/// 一次失败的首次连接会永久废掉这个对端，直到应用重启。这里用 Drop 兜底。
struct TerminatedGuard {
	state_ref: Arc<Mutex<SessionState>>,
}

impl TerminatedGuard {
	fn new(state_ref: Arc<Mutex<SessionState>>) -> Self {
		Self { state_ref }
	}
}

impl Drop for TerminatedGuard {
	fn drop(&mut self) {
		// Drop 里不能 panic：毒化的锁也要拿回内部值继续写，
		// 否则 panic=unwind 下会直接 abort 进程。
		let mut guard = match self.state_ref.lock() {
			Ok(guard) => guard,
			Err(poisoned) => poisoned.into_inner(),
		};
		*guard = SessionState::Terminated;
	}
}

/// 任务 abort 守护 (RAII)：run_actor 从任何路径退出（含早期 `?` / panic）时，
/// 都必须终止数据流 accept loop，否则它持有的 `Connection` clone 会让连接
/// 一直存活到对端超时，形成僵尸会话。
struct TaskAbortGuard(tokio::task::AbortHandle);

impl Drop for TaskAbortGuard {
	fn drop(&mut self) {
		self.0.abort();
	}
}

pub struct SessionActor {
	role: SessionRole,
	writer: FramedWrite<SendStream, CBFrameCodec>,
	reader: FramedRead<RecvStream, CBFrameCodec>,
	config: Arc<CoreConfig>,
	sink: Arc<dyn CoreEventSink>,
	state_ref: Arc<Mutex<SessionState>>,
	peer_id_ref: Arc<Mutex<Option<String>>>,
	state: SessionState,
	remote_device_id: Option<String>,
	remote_fingerprint: String,
	last_active_at: i64,
	cmd_rx: mpsc::Receiver<SessionCmd>,
	store: Arc<Mutex<Store>>,
	log_sink: crate::logs::LogSink,
	opaque_client_state: Option<CbClientLoginState>,
	opaque_server_state: Option<CbServerLoginState>,
	receivers: HashMap<String, ReceiverState>,
	senders: HashMap<String, SenderState>,
	cas: crate::cas::Cas,
	upload_tx: mpsc::Sender<UploadMsg>,
	/// 最终化任务 → actor 的完成事件通道（ContentEnd 不再在 actor 主循环里等待）。
	receiver_done_tx: mpsc::Sender<ReceiverDone>,
	/// 会话的 QUIC 连接。发送端用它 `open_bi()` 开每传输数据流；
	/// 接收端由独立的 accept loop 用它 `accept_bi()`。
	conn: Connection,
	/// 接收任务 → actor 的数据流完成/失败事件。
	payload_event_tx: mpsc::Sender<PayloadEvent>,
	/// 已读到头部、还在等 `ContentBegin` 注册的数据流（每个 req_id 最多
	/// `MAX_PAYLOAD_STREAMS_PER_TRANSFER` 条，有宽限上限）。
	pending_payloads: HashMap<String, Vec<PendingPayload>>,
	/// 最近取消的 req_id → 取消时间。用于立即拒绝与取消交叉到达的数据流。
	cancelled_payloads: HashMap<String, i64>,
	/// 会话级出站数据流闸门（[`MAX_PAYLOAD_STREAMS_PER_SESSION`]）。
	///
	/// 与 [`MAX_CONCURRENT_SENDERS`] 是独立维度：一个传输最多 6 条流，但一个
	/// 会话同时活跃的流必须另有上限，否则多个 WAN 探针会瞬间放大调度/内存开销。
	payload_stream_budget: Arc<Semaphore>,
}

impl SessionActor {
	/// 兼容入口：用现有 `LogStore` 构造一个**独占** sink（会启动一条 writer 线程）。
	///
	/// 生产路径（`net.rs`）使用 [`SessionActor::spawn_with_sink`]，让所有会话共享
	/// `Core` 的同一个 writer；这里保留给直接持有 `LogStore` 的调用方/测试。
	#[allow(clippy::too_many_arguments)]
	pub fn spawn(
		role: SessionRole,
		conn: Connection,
		config: CoreConfig,
		sink: Arc<dyn CoreEventSink>,
		store: Arc<Mutex<Store>>,
		cas: crate::cas::Cas,
		expected_peer_id: Option<String>,
		log_store: Arc<Mutex<crate::logs::LogStore>>,
	) -> SessionHandle {
		Self::spawn_with_sink(
			role,
			conn,
			config,
			sink,
			store,
			cas,
			expected_peer_id,
			crate::logs::LogSink::new(log_store),
		)
	}

	/// 用共享的 [`crate::logs::LogSink`] 启动会话 actor（生产路径）。
	#[allow(clippy::too_many_arguments)]
	pub fn spawn_with_sink(
		role: SessionRole,
		conn: Connection,
		config: CoreConfig,
		sink: Arc<dyn CoreEventSink>,
		store: Arc<Mutex<Store>>,
		cas: crate::cas::Cas,
		expected_peer_id: Option<String>,
		log_sink: crate::logs::LogSink,
	) -> SessionHandle {
		let (cmd_tx, cmd_rx) = mpsc::channel(32);
		let state_ref = Arc::new(Mutex::new(SessionState::TransportReady));
		let peer_id_ref = Arc::new(Mutex::new(None));

		let initial_did = expected_peer_id.unwrap_or_else(|| "pending_server".to_string());

		let handle = SessionHandle {
			initial_id: initial_did.clone(),
			peer_id: peer_id_ref.clone(),
			state: state_ref.clone(),
			cmd_tx,
		};

		// 缺失/空的对端证书视为握手失败：绝不能把 "unknown" 当作指纹写入
		// trusted_peers，否则 TOFU 会永远“验证通过”。此时不启动 Actor，
		// 会话标记为 Terminated（对外 Offline），连接随 conn 一起关闭。
		let fingerprint = match Self::peer_fingerprint(&conn) {
			Ok(fp) => fp,
			Err(e) => {
				eprintln!("[Session] Rejecting session: {e} (fingerprint=unknown, not pinned)");
				*state_ref.lock().unwrap() = SessionState::Terminated;
				return handle;
			}
		};

		let actor_log_id = initial_did.clone();
		let state_ref_clone = state_ref.clone();
		// 单独留一份给 Drop 守护：state_ref_clone 会被 move 进 run_actor。
		let state_ref_for_guard = state_ref.clone();
		let peer_id_ref_clone = peer_id_ref.clone();
		let config_arc = Arc::new(config);

		tokio::spawn(async move {
			// 关键：守护比 run_actor 更早创建、更晚 drop。无论 run_actor 从哪个
			// `?` 提前返回（open_bi / start_handshake 失败）还是任务 panic，
			// 状态最终都会被写成 Terminated，NetManager 才能回收会话并允许重连。
			let _terminated_guard = TerminatedGuard::new(state_ref_for_guard);
			let (upload_tx, upload_rx) = mpsc::channel(32);
			if let Err(e) = Self::run_actor(
				role,
				conn,
				config_arc,
				sink,
				store,
				cas,
				state_ref_clone,
				peer_id_ref_clone,
				cmd_rx,
				fingerprint,
				upload_tx,
				upload_rx,
				log_sink,
			)
			.await
			{
				eprintln!("[Session] Actor {actor_log_id} error: {e:?}");
			}
		});
		handle
	}

	/// 从 QUIC 连接的对端证书提取 SHA-256 指纹。
	/// 缺失证书 / 空证书链都返回错误，调用方必须按握手失败处理。
	fn peer_fingerprint(conn: &Connection) -> Result<String> {
		let id = conn
			.peer_identity()
			.ok_or_else(|| anyhow::anyhow!("peer did not present a TLS client certificate"))?;
		let certs = id
			.downcast::<Vec<rustls::pki_types::CertificateDer>>()
			.unwrap_or_default();
		let cert = certs
			.first()
			.ok_or_else(|| anyhow::anyhow!("peer certificate chain is empty"))?;
		Ok(sha256_hex(cert.as_ref()))
	}

	#[allow(clippy::too_many_arguments)]
	async fn run_actor(
		role: SessionRole,
		conn: Connection,
		config: Arc<CoreConfig>,
		sink: Arc<dyn CoreEventSink>,
		store: Arc<Mutex<Store>>,
		cas: crate::cas::Cas,
		state_ref: Arc<Mutex<SessionState>>,
		peer_id_ref: Arc<Mutex<Option<String>>>,
		cmd_rx: mpsc::Receiver<SessionCmd>,
		fingerprint: String,
		upload_tx: mpsc::Sender<UploadMsg>,
		mut upload_rx: mpsc::Receiver<UploadMsg>,
		log_sink: crate::logs::LogSink,
	) -> Result<()> {
		let (send, recv) = match role {
			SessionRole::Client => conn.open_bi().await.context("Client open_bi failed")?,
			SessionRole::Server => conn.accept_bi().await.context("Server accept_bi failed")?,
		};

		let writer = FramedWrite::new(send, CBFrameCodec);
		let reader = FramedRead::new(recv, CBFrameCodec);

		// B1：每传输独立数据流。控制流建立完成（上面的 open_bi/accept_bi）之后
		// 才能启动 accept loop —— 否则 Server 端的 accept loop 可能抢走对端的第一条
		// 流（控制流本身）。数据流只在握手完成后才会出现，因此这一顺序是安全的。
		//
		// - `payload_open_rx`：已读到头部的数据流 → actor 路由；
		// - `payload_event_rx`：接收任务的完成/失败事件 → actor 收尾；
		// - `payload_accept_handle`：会话结束时 abort，让 accept loop 释放 Connection 引用。
		let (payload_open_tx, mut payload_open_rx) = mpsc::channel::<PayloadOpened>(16);
		let (payload_event_tx, mut payload_event_rx) = mpsc::channel::<PayloadEvent>(32);
		// Guard：run_actor 从任何路径提前返回（握手失败等）时也终止 accept loop。
		let _payload_accept_guard = TaskAbortGuard(spawn_payload_accept_loop(
			conn.clone(),
			payload_open_tx,
			log_sink.clone(),
		));

		// 记录会话创建
		log_sink.log_info(
			"Session",
			&format!("Session actor spawned: role={role:?}, fingerprint={fingerprint}"),
			Some(&format!(
				"会话执行器已创建: 角色={role:?}，指纹={fingerprint}"
			)),
		);

		// 最终化任务 → actor 的完成事件通道。容量足够覆盖 MAX_CONCURRENT_RECEIVERS
		// 同时完成的情况；即使满了，发送端在独立任务里 await，也不会阻塞主循环。
		let (receiver_done_tx, mut receiver_done_rx) = mpsc::channel::<ReceiverDone>(32);

		let mut actor = Self {
			role,
			writer,
			reader,
			config,
			sink,
			store,
			log_sink,
			state_ref,
			peer_id_ref,
			state: SessionState::TransportReady,
			remote_device_id: None,
			remote_fingerprint: fingerprint,
			last_active_at: now_ms(),
			cmd_rx,
			opaque_client_state: None,
			opaque_server_state: None,
			receivers: HashMap::new(),
			senders: HashMap::new(),
			cas,
			upload_tx,
			receiver_done_tx,
			conn,
			payload_event_tx,
			pending_payloads: HashMap::new(),
			cancelled_payloads: HashMap::new(),
			payload_stream_budget: Arc::new(Semaphore::new(MAX_PAYLOAD_STREAMS_PER_SESSION)),
		};

		actor.start_handshake().await?;

		let mut heartbeat_ticker = interval(HEARTBEAT_INTERVAL);
		heartbeat_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

		let run_result: Result<()> = async {
			loop {
				tokio::select! {
					// 1. 网络消息
					msg = actor.reader.next() => {
						match msg {
							Some(Ok(frame)) => {
								actor.last_active_at = now_ms();
								if let Err(e) = actor.handle_frame(frame).await {
									 eprintln!("[Session] Frame error: {e:?}");
									 // 严重错误断开连接
									 return Err(e);
								}
							}
							Some(Err(e)) => return Err(e),
							None => break, // EOF
						}
					}

					// 2. 本地命令
					cmd = actor.cmd_rx.recv() => {
						match cmd {
							Some(SessionCmd::SendMeta(mut meta)) => {
								if actor.state == SessionState::Online {
									// 出站策略：share_to_peer=false 的对端不广播元数据。
									// net.rs 的 broadcast_meta 只处理全局 DenyAll，这里按会话逐对端拦截。
									if !actor.outbound_share_allowed().await? {
										let device_id = actor
											.remote_device_id
											.clone()
											.unwrap_or_else(|| "unknown".to_string());
										actor.log_sink.log_warn(
											"Session",
											&format!("Metadata broadcast skipped by share_to_peer policy: device_id={device_id}, item_id={}", meta.item_id),
											Some(&format!("共享策略禁止向该设备广播元数据: 设备ID={device_id}，项目ID={}", meta.item_id)),
										);
									} else {
										let msg_id = uuid::Uuid::new_v4().to_string();
										let item_id = meta.item_id.clone();
										let device_id = actor.remote_device_id.clone().unwrap_or_else(|| "unknown".to_string());

										// 不要把发送端的本地路径告诉接收端
										for f in &mut meta.files {
											f.local_path = None;
										}

										actor.log_sink.log_info(
												"Session",
												&format!("Metadata sent to peer: device_id={device_id}, item_id={item_id}, msg_id={msg_id}"),
												Some(&format!("元数据已发送至对等设备: 设备ID={device_id}，项目ID={item_id}，消息ID={msg_id}")),
										);

										actor.send_ctrl(CtrlMsg::ItemMeta {
											msg_id: Some(msg_id),
											item: *meta
										}).await?;
									}
								}
							}
							Some(SessionCmd::Shutdown) => {
								let _ = actor.send_ctrl(CtrlMsg::Close {
									msg_id: Some(uuid::Uuid::new_v4().to_string()),
									reason: "Shutdown".into()
								}).await;
								break;
							}
							Some(SessionCmd::RequestTransfer { item_id, file_id, reply_tx }) => {
								// M3: B 端发起拉取
								let _ = actor.start_pull_request(item_id, file_id, reply_tx).await;
							}
							Some(SessionCmd::CancelTransfer { transfer_id }) => {
								actor.handle_local_cancel(transfer_id).await?;
							}
							Some(SessionCmd::SendDelete { item_id }) => {
								if actor.state == SessionState::Online {
									actor.send_ctrl(CtrlMsg::ItemDelete {
										msg_id: Some(uuid::Uuid::new_v4().to_string()),
										item_id,
									}).await?;
								}
							}
							None => break,
						}
					}

					Some(msg) = upload_rx.recv() => {
						match msg {
							UploadMsg::Done { transfer_id, bytes } => {
								// 顺序契约：发送端先 finish() 数据流，再发 ContentEnd。
								// 接收端把 FIN 当"字节全部到达"、把 ContentEnd 当"可以最终化"。
								actor.send_ctrl(CtrlMsg::ContentEnd { req_id: transfer_id.clone() }).await?;
								// 每个传输只写一条汇总日志（替代原先每 256 KiB 一条的 debug 日志）
								if let Some(state) = actor.senders.remove(&transfer_id) {
									let elapsed_ms = now_ms() - state.started_ts_ms;
									actor.log_sink.log_info(
										"Session",
										&format!(
											"Outbound transfer completed: transfer_id={transfer_id}, bytes={}, elapsed_ms={elapsed_ms}",
											state.bytes_sent.max(bytes)
										),
										Some(&format!(
											"出站传输已完成: 传输ID={transfer_id}，字节数={}，耗时={elapsed_ms}ms",
											state.bytes_sent.max(bytes)
										)),
									);
								}
							}
							UploadMsg::Error { transfer_id, bytes, err } => {
								// 数据流失败只影响本次传输（不再像旧实现那样让 actor 主循环
								// 带着错误返回、直接终止会话）。用非致命的 ContentCancel 通知
								// 对端停止等待，并在本地记录一条失败汇总。
								if let Some(state) = actor.senders.remove(&transfer_id) {
									let elapsed_ms = now_ms() - state.started_ts_ms;
									actor.log_sink.log_warn(
										"Session",
										&format!(
											"Outbound transfer aborted: transfer_id={transfer_id}, bytes={}, elapsed_ms={elapsed_ms}, error={err}",
											state.bytes_sent.max(bytes)
										),
										Some(&format!(
											"出站传输已中止: 传输ID={transfer_id}，字节数={}，耗时={elapsed_ms}ms，错误={err}",
											state.bytes_sent.max(bytes)
										)),
									);
								}
								let _ = actor
									.send_ctrl(CtrlMsg::ContentCancel {
										req_id: transfer_id.clone(),
										reason: format!("sender payload stream failed: {err}"),
									})
									.await;
								eprintln!("[Session] Upload error for {transfer_id}: {err}");
							}
						}
					}

					// 3. 心跳
					_ = heartbeat_ticker.tick() => {
						actor.tick_heartbeat().await?;
						// 每 2s 顺带回收空闲超时的入站传输（避免 tmp 文件 / fd 泄漏），
						// 以及等待注册/等待数据流的过期占位符。
						actor.sweep_idle_receivers().await;
					}

					// 4. 入站最终化完成事件（flush/close/commit/materialize 在独立任务里做）
					Some(done) = receiver_done_rx.recv() => {
						actor.handle_receiver_done(done).await?;
					}

					// 5. 已读到头部的入站数据流：路由到对应 receiver（或暂存等 ContentBegin）
					opened = payload_open_rx.recv() => {
						if let Some(opened) = opened {
							actor.handle_payload_opened(opened).await?;
						}
					}

					// 6. 接收任务的 FIN / 失败事件
					Some(evt) = payload_event_rx.recv() => {
						actor.handle_payload_event(evt).await?;
					}
				}
			}
			Ok(())
		}
		.await;

		// 停止数据流 accept loop（`_payload_accept_guard` 也会在函数返回时兜底 abort），
		// 并显式关闭连接。
		//
		// 为什么要显式 close：accept loop / 头部读取任务 / 数据流接收任务都可能仍
		// 持有 quinn 的 stream/connection 引用；仅依赖"最后一个 handle drop 时隐式
		// 关闭"会让连接一直活到头部读取超时（10s），对端迟迟看不到断开。显式 close
		// 会立即向对端发送 CONNECTION_CLOSE 并让所有数据流复位。
		actor.conn.close(0u32.into(), b"session terminated");

		actor.update_state(SessionState::Terminated);
		if let Some(did) = &actor.remote_device_id {
			let reason = match &run_result {
				Ok(()) => "Connection closed".to_string(),
				Err(e) => format!("Error: {e}"),
			};
			{
				let log_level = if run_result.is_ok() { 2 } else { 4 }; // Info or Error
				if log_level == 2 {
					actor.log_sink.log_info(
						"Session",
						&format!("Session terminated: device_id={did}, reason={reason}"),
						Some(&format!("会话已终止: 设备ID={did}，原因={reason}")),
					);
				} else {
					actor.log_sink.log_error(
						"Session",
						&format!(
							"Session terminated due to error: device_id={did}, error={reason}"
						),
						Some(&format!("会话因错误终止: 设备ID={did}，错误={reason}")),
						Some(&reason),
					);
				}
			}
			let json = serde_json::json!({
				"type": "PEER_OFFLINE",
				"ts_ms": now_ms(),
				"payload": { "device_id": did, "reason": reason }
			});
			actor.sink.emit(json.to_string());
		}
		run_result
	}

	fn update_state(&mut self, new_state: SessionState) {
		let old_state = self.state.clone();
		self.state = new_state.clone();
		{
			let mut s = self.state_ref.lock().unwrap();
			*s = new_state;
		}

		// 记录状态转换
		if let Some(device_id) = &self.remote_device_id {
			self.log_sink.log_info(
				"Session",
				&format!(
					"Session state changed: device_id={device_id}, old_state={old_state:?}, new_state={:?}",
					self.state
				),
				Some(&format!(
					"会话状态已变化: 设备ID={device_id}，旧状态={old_state:?}，新状态={:?}",
					self.state
				)),
			);
		}
	}

	fn update_remote_id(&mut self, id: String) {
		self.remote_device_id = Some(id.clone());
		let mut lock = self.peer_id_ref.lock().unwrap();
		*lock = Some(id);
	}

	async fn send_ctrl(&mut self, msg: CtrlMsg) -> Result<()> {
		self.writer
			.send(CBFrame::Control(Box::new(msg)))
			.await
			.context("Failed to send CtrlMsg")
	}

	async fn start_handshake(&mut self) -> Result<()> {
		self.log_sink.log_info(
			"Session",
			&format!(
				"Handshake started: role={:?}, protocol_version={}",
				self.role, PROTOCOL_VERSION
			),
			Some(&format!(
				"握手已开始: 角色={:?}，协议版本={}",
				self.role, PROTOCOL_VERSION
			)),
		);

		match self.role {
			SessionRole::Client => {
				self.update_state(SessionState::Handshaking(HandshakeStep::SendingHello));
				let msg = CtrlMsg::Hello {
					msg_id: Some(uuid::Uuid::new_v4().to_string()),
					protocol_version: PROTOCOL_VERSION,
					device_id: self.config.device_id.clone(),
					account_uid: self.config.account_uid.clone(),
					capabilities: vec!["text".into(), "image".into(), "file".into()],
					client_nonce: Some(uuid::Uuid::new_v4().to_string()),
				};
				self.log_sink.log_info(
					"Session",
					&format!(
						"Hello message sent: device_id={}, account_uid={}",
						self.config.device_id, self.config.account_uid
					),
					Some(&format!(
						"Hello 消息已发送: 设备ID={}，账号={}",
						self.config.device_id, self.config.account_uid
					)),
				);
				self.send_ctrl(msg).await?;
				self.update_state(SessionState::Handshaking(HandshakeStep::WaitingForHelloAck));
			}
			SessionRole::Server => {
				self.update_state(SessionState::Handshaking(HandshakeStep::WaitingForHello));
			}
		}
		Ok(())
	}

	async fn handle_frame(&mut self, frame: CBFrame) -> Result<()> {
		let CBFrame::Control(msg) = frame;
		self.handle_control_msg(*msg).await
	}

	/// 鉴权闸门：未进入 Online 状态时，拒绝任何业务消息并终止会话。
	///
	/// 只允许在 `handle_control_msg` 顶部调用一次：白名单之外的消息即使将来新增，
	/// 也一定会在唯一入口被拦截，不会散落到各个分支里遗漏。
	async fn require_online(&mut self, what: &str) -> Result<()> {
		if self.state == SessionState::Online {
			return Ok(());
		}
		let _ = self
			.send_ctrl(CtrlMsg::Error {
				reply_to: None,
				code: "NOT_AUTHENTICATED".into(),
				message: Some(format!(
					"{what} rejected: session is not authenticated (state={:?})",
					self.state
				)),
			})
			.await;
		anyhow::bail!(
			"Rejected {what} before authentication: state={:?}",
			self.state
		)
	}

	/// 查询当前对端的共享策略规则（PeerRule）。
	///
	/// DB 访问统一放进 spawn_blocking，避免阻塞异步线程；对端设备 ID 未知时返回 None。
	async fn load_peer_rule(&self) -> Result<Option<crate::store::PeerRule>> {
		let Some(device_id) = self.remote_device_id.clone() else {
			return Ok(None);
		};
		let account_uid = self.config.account_uid.clone();
		let store = self.store.clone();
		tokio::task::spawn_blocking(move || {
			let guard = store.lock().unwrap();
			guard.get_peer_rule(&account_uid, &device_id)
		})
		.await?
	}

	/// 出站共享策略：share_to_peer=false 或对端身份未知时拒绝；
	/// 规则不存在时沿用历史默认（允许），与 get_peers_info 的展示默认值一致。
	async fn outbound_share_allowed(&self) -> Result<bool> {
		if self.remote_device_id.is_none() {
			return Ok(false);
		}
		match self.load_peer_rule().await? {
			Some(rule) => Ok(rule.share_to_peer),
			None => Ok(true),
		}
	}

	/// 入站接收策略：accept_from_peer=false 或对端身份未知时拒绝；
	/// 规则不存在时沿用历史默认（允许）。
	async fn inbound_accept_allowed(&self) -> Result<bool> {
		if self.remote_device_id.is_none() {
			return Ok(false);
		}
		match self.load_peer_rule().await? {
			Some(rule) => Ok(rule.accept_from_peer),
			None => Ok(true),
		}
	}

	/// 归类入站传输的类型，用于选择 size_limits 中的硬上限。
	///
	/// - `file_id` 非空：FileList 的子文件传输，按 file 上限处理；
	/// - 否则优先用本地 item 的 mime（本地元数据比对端声明更可信），
	///   查不到才回退到 `ContentBegin.mime`。
	///
	/// DB 查询走 `spawn_blocking`，不在 Tokio worker 上持有 `store` 锁。
	async fn inbound_item_kind(
		&self,
		item_id: &str,
		file_id: Option<&str>,
		declared_mime: &str,
	) -> ItemKind {
		if file_id.is_some() {
			return ItemKind::FileList;
		}
		let store = self.store.clone();
		let item_id = item_id.to_string();
		let local_mime = tokio::task::spawn_blocking(move || {
			let store = store.lock().unwrap();
			store.get_item_mime(&item_id).ok().flatten()
		})
		.await
		.unwrap_or(None);
		kind_from_mime(local_mime.as_deref().unwrap_or(declared_mime))
	}

	async fn handle_control_msg(&mut self, msg: CtrlMsg) -> Result<()> {
		// 唯一鉴权入口：未认证前只放行白名单消息（握手/协议控制），
		// 其余业务消息统一在这里拒绝并终止会话。
		if !ctrl_is_pre_auth_allowed(&msg) {
			self.require_online(ctrl_msg_kind(&msg)).await?;
		}
		match msg {
			CtrlMsg::Hello {
				device_id,
				account_uid,
				msg_id,
				protocol_version,
				..
			} => {
				if self.role == SessionRole::Server {
					// 协议版本闸门：bincode→postcard 迁移后线格式不兼容，
					// 版本不一致必须立刻回错并断开，不能继续握手。
					if protocol_version != PROTOCOL_VERSION {
						let _ = self
							.send_ctrl(CtrlMsg::Error {
								reply_to: msg_id,
								code: "PROTOCOL_VERSION_MISMATCH".into(),
								message: Some(format!(
									"local protocol_version={PROTOCOL_VERSION}, remote={protocol_version}"
								)),
							})
							.await;
						anyhow::bail!(
							"Protocol version mismatch: local={PROTOCOL_VERSION}, remote={protocol_version}"
						);
					}
					// Hello 只允许在握手初始状态接收一次：已认证会话上再次收到 Hello
					// 属于协议违规，绝不能借此重置状态机或覆盖 remote_device_id。
					if self.state != SessionState::Handshaking(HandshakeStep::WaitingForHello) {
						let _ = self
							.send_ctrl(CtrlMsg::Error {
								reply_to: msg_id,
								code: "UNEXPECTED_HELLO".into(),
								message: Some(format!(
									"Hello not allowed in state {:?}",
									self.state
								)),
							})
							.await;
						anyhow::bail!("Unexpected Hello in state {:?}", self.state);
					}
					self.log_sink.log_info(
                            "Session",
                            &format!("Hello message received: remote_device_id={device_id}, remote_account_uid={account_uid}"),
                            Some(&format!("Hello 消息已接收: 远程设备ID={device_id}，远程账号={account_uid}")),
					);
					if account_uid != self.config.account_uid {
						self.log_sink.log_error(
                                "Session",
                                &format!("Account verification failed (uid mismatch): remote_device_id={device_id}, remote_uid={account_uid}, local_uid={}",
                                        self.config.account_uid),
                                Some(&format!("账号验证失败（账号不匹配）: 远程设备ID={device_id}，远程账号={account_uid}，本地账号={}",
                                        self.config.account_uid)),
                                Some("AUTH_ACCOUNT_UID_MISMATCH"),
						);
						let _ = self
							.send_ctrl(CtrlMsg::AuthFail {
								reply_to: msg_id.clone(),
								code: "AUTH_ACCOUNT_UID_MISMATCH".into(),
							})
							.await;
						let _ = self
							.send_ctrl(CtrlMsg::Close {
								msg_id: None,
								reason: "Auth failed".into(),
							})
							.await;
						anyhow::bail!("Auth failed: uid mismatch");
					}
					self.update_remote_id(device_id.clone());
					self.log_sink.log_info(
                            "Session",
                            &format!("Account verification completed: remote_device_id={device_id}, verified=true"),
                            Some(&format!("账号验证已完成: 远程设备ID={device_id}，已验证=true")),
					);
					self.send_ctrl(CtrlMsg::HelloAck {
						reply_to: msg_id,
						server_device_id: self.config.device_id.clone(),
						protocol_version: PROTOCOL_VERSION,
					})
					.await?;
					self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueStart));
				}
			}
			CtrlMsg::HelloAck {
				server_device_id,
				protocol_version,
				..
			} => {
				if self.role == SessionRole::Client {
					// 客户端侧同样校验线格式版本，版本不一致直接终止。
					if protocol_version != PROTOCOL_VERSION {
						let _ = self
							.send_ctrl(CtrlMsg::Error {
								reply_to: None,
								code: "PROTOCOL_VERSION_MISMATCH".into(),
								message: Some(format!(
									"local protocol_version={PROTOCOL_VERSION}, remote={protocol_version}"
								)),
							})
							.await;
						anyhow::bail!(
							"Protocol version mismatch: local={PROTOCOL_VERSION}, remote={protocol_version}"
						);
					}
					self.log_sink.log_info(
                            "Session",
                            &format!("Hello message received: remote_device_id={server_device_id}, remote_account_uid=N/A"),
                            Some(&format!("Hello 消息已接收: 远程设备ID={server_device_id}，远程账号=N/A")),
					);
					self.update_remote_id(server_device_id.clone());
					self.start_opaque_login().await?;
				}
			}
			CtrlMsg::OpaqueStart { opaque: bytes, .. } => {
				if self.role == SessionRole::Server {
					self.handle_opaque_start(&bytes).await?;
				}
			}
			CtrlMsg::OpaqueResponse { opaque: bytes, .. } => {
				if self.role == SessionRole::Client {
					self.handle_opaque_response(&bytes).await?;
				}
			}
			CtrlMsg::OpaqueFinish {
				msg_id,
				opaque: bytes,
				..
			} => {
				if self.role == SessionRole::Server {
					self.handle_opaque_finish(&bytes).await?;
					if let Err(e) = self.perform_tofu_check_async().await {
						let _ = self
							.send_ctrl(CtrlMsg::Error {
								reply_to: msg_id.clone(),
								code: "POLICY_REJECT".into(),
								message: Some(e.to_string()),
							})
							.await;
						return Err(e);
					}
					self.send_ctrl(CtrlMsg::AuthOk {
						reply_to: msg_id,
						session_flags: AuthSessionFlags {
							account_verified: true,
						},
					})
					.await?;
					self.transition_to_online();
				}
			}
			CtrlMsg::AuthOk { .. } => {
				if self.role == SessionRole::Client {
					self.update_state(SessionState::AccountVerified);
					self.perform_tofu_check_async().await?;
					self.transition_to_online();
				}
			}
			CtrlMsg::AuthFail { code, .. } => anyhow::bail!("Remote AuthFail: {code}"),
			CtrlMsg::Ping { ts, msg_id } => {
				self.send_ctrl(CtrlMsg::Pong {
					reply_to: msg_id,
					ts,
				})
				.await?;
			}
			CtrlMsg::Pong { .. } => {}
			CtrlMsg::ItemMeta { item, msg_id, .. } => {
				if let Some(did) = &self.remote_device_id {
					self.log_sink.log_info(
						"Session",
						&format!(
							"Metadata received from peer: device_id={did}, item_id={}, msg_id={msg_id:?}",
							item.item_id
						),
						Some(&format!(
							"从对等设备接收元数据: 设备ID={did}，项目ID={}，消息ID={msg_id:?}",
							item.item_id
						)),
					);
				}
				// 认证闸门已在 handle_control_msg 顶部保证此时必然 Online，无需重复判断。
				let store = self.store.clone();
				let account_uid = self.config.account_uid.clone();
				let item_clone = item.clone();
				let device_id = self.remote_device_id.clone();
				// 入站策略检查与落库放在同一个 spawn_blocking 中：
				// accept_from_peer=false（或对端身份未知）时整条元数据丢弃。
				let (accepted, is_new) =
					tokio::task::spawn_blocking(move || -> Result<(bool, bool)> {
						let mut guard = store.lock().unwrap();
						let Some(did) = device_id else {
							return Ok((false, false));
						};
						if matches!(
							guard.get_peer_rule(&account_uid, &did)?,
							Some(rule) if !rule.accept_from_peer
						) {
							return Ok((false, false));
						}
						let is_new =
							guard.insert_remote_item(&account_uid, &item_clone, now_ms())?;
						Ok((true, is_new))
					})
					.await??;
				if !accepted {
					// 这里不回 CtrlMsg::Error：对端收到 Error 会直接终止会话，
					// 而共享策略开关不应误杀正常连接，丢弃并记录日志即可。
					self.log_sink.log_warn(
						"Session",
						&format!(
							"Metadata rejected by accept_from_peer policy: item_id={}",
							item.item_id
						),
						Some(&format!(
							"接收策略禁止接收该设备的元数据: 项目ID={}",
							item.item_id
						)),
					);
					return Ok(());
				}
				if is_new {
					let json = serde_json::json!({
						"type": "ITEM_META_ADDED",
						"ts_ms": now_ms(),
						"payload": { "meta": item }
					});
					self.sink.emit(json.to_string());
					// 禁用核心的文字自动预取，而是在外壳实现。
					/*
					if item.kind == crate::model::ItemKind::Text {
						if item.size_bytes <= self.config.app_config.size_limits.text_auto_prefetch_bytes {
							println!("[Session] Auto-prefetching text item: {}", item.item_id);
							let (tx, _) = tokio::sync::oneshot::channel();
							let _ = self.start_pull_request(item.item_id.clone(), None, tx).await;
						}
					}
					*/
				}
			}
			CtrlMsg::Error { code, message, .. } => {
				anyhow::bail!("Remote error {code}: {message:?}")
			}
			CtrlMsg::Close { .. } => anyhow::bail!("Remote closed connection"),

			// === M3: 传输逻辑 ===
			CtrlMsg::ContentGet {
				msg_id,
				item_id,
				file_id,
				offset,
			} => {
				let transfer_id = msg_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
				self.handle_content_get(transfer_id, item_id, file_id, offset)
					.await?;
			}
			CtrlMsg::ContentBegin {
				req_id,
				item_id,
				file_id,
				total_bytes,
				sha256,
				mime,
				ranges,
			} => {
				self.handle_content_begin(
					req_id,
					item_id,
					file_id,
					total_bytes,
					sha256,
					mime,
					ranges,
				)
				.await?;
			}
			CtrlMsg::ContentEnd { req_id } => {
				self.handle_content_end(&req_id);
			}
			CtrlMsg::ContentCancel { req_id, reason } => {
				self.log_sink.log_info(
					"Session",
					&format!("Transfer cancelled: transfer_id={req_id}, reason={reason}"),
					Some(&format!("传输已取消: 传输ID={req_id}，原因={reason}")),
				);
				// 1. 如果我是发送者：在 senders map 里找
				if let Some(state) = self.senders.remove(&req_id) {
					state.handle.abort(); // 终止发送任务
				}

				// 2. 如果我是接收者：调用 handle_content_cancel
				self.handle_content_cancel(&req_id, &reason);
			}
			CtrlMsg::ContentDenied {
				req_id,
				code,
				message,
			} => {
				// 非致命：对端拒绝提供内容只影响本次传输。绝不能像 CtrlMsg::Error 那样 bail，
				// 否则用户切换一次共享开关会导致会话反复断开/重连。
				let detail =
					message.unwrap_or_else(|| format!("peer denied content request ({code})"));
				self.log_sink.log_warn(
					"Session",
					&format!(
							"Content request denied by peer: transfer_id={req_id}, code={code}, message={detail}"
						),
					Some(&format!(
						"对端拒绝内容请求: 传输ID={req_id}，错误码={code}，信息={detail}"
					)),
				);
				// 清理可能残留的同 req_id 发送任务（正常路径下此时还没有 Sender）
				if let Some(state) = self.senders.remove(&req_id) {
					state.handle.abort();
				}
				// 让发起方看到本次传输失败，而不是会话级错误
				self.emit_transfer_failed(&req_id, &code, &detail);
			}

			CtrlMsg::ItemDelete { item_id, .. } => {
				// 认证闸门已在 handle_control_msg 顶部保证此时必然 Online，无需重复判断。
				let account_uid = self.config.account_uid.clone();
				let store = self.store.clone();
				let cas = self.cas.clone();
				let sink = self.sink.clone();
				let item_id_emit = item_id.clone();
				let deleted = tokio::task::spawn_blocking(move || {
					let mut guard = store.lock().unwrap();
					let ok = guard
						.soft_delete_item(&account_uid, &item_id)
						.ok()
						.unwrap_or(false);
					let sha = guard.get_item_sha256(&item_id).ok().flatten();
					(ok, sha)
				})
				.await?;
				let (ok, sha_opt) = deleted;
				if ok {
					if let Some(sha) = sha_opt {
						// `remove_blob` 会遍历整个 downloads/ 树，`mark_cache_missing`
						// 是 SQLite 写：两者都放进 spawn_blocking，避免占住 worker。
						let store = self.store.clone();
						tokio::task::spawn_blocking(move || {
							let _ = cas.remove_blob(&sha);
							let now = now_ms();
							let mut guard = store.lock().unwrap();
							let _ = guard.mark_cache_missing(&sha, now);
						})
						.await?;
					}
					let evt = serde_json::json!({
						"type": "ITEM_DELETED",
						"item_id": item_id_emit,
					});
					sink.emit(evt.to_string());
				}
			}
		}
		Ok(())
	}

	// --- M3 Receiver Logic ---
	#[allow(clippy::too_many_arguments)]
	async fn handle_content_begin(
		&mut self,
		req_id: String,
		item_id: String,
		file_id: Option<String>,
		total_bytes: u64,
		file_sha256: String, // 注意：这是整个文件的 Hash（B2 的区间 hash 不在控制面）
		mime: String,
		ranges: Vec<TransferRange>,
	) -> Result<()> {
		// 入站策略：accept_from_peer=false（或对端身份未知）时拒绝接收内容。
		// 用 ContentCancel 而非 Error 通知对端：Error 会被对端视为致命错误并断开会话。
		if !self.inbound_accept_allowed().await? {
			self.log_sink.log_warn(
					"Session",
					&format!("Transfer rejected by accept_from_peer policy: transfer_id={req_id}, item_id={item_id}"),
					Some(&format!("接收策略禁止接收内容: 传输ID={req_id}，项目ID={item_id}")),
			);
			self.drop_pending_payload(&req_id);
			self.send_ctrl(CtrlMsg::ContentCancel {
				req_id,
				reason: "accept_from_peer disabled".into(),
			})
			.await?;
			return Ok(());
		}

		self.log_sink.log_info(
				"Session",
				&format!("Transfer request received: transfer_id={req_id}, item_id={item_id}, file_id={file_id:?}"),
				Some(&format!("传输请求已接收: 传输ID={req_id}，项目ID={item_id}，文件ID={file_id:?}")),
		);

		if self.receivers.contains_key(&req_id) {
			// 幂等处理：如果是重复的 Begin，忽略即可。
			// 但若还残留一条等待注册的同 req_id 数据流，必须丢弃，避免越界占用槽位。
			self.drop_pending_payload(&req_id);
			return Ok(());
		}

		// 1) 并发上限：已存在 MAX_CONCURRENT_RECEIVERS 个接收时直接拒绝，
		//    不分配 channel / Task / tmp 文件，避免对端刷 ContentBegin 撑爆资源。
		if !receiver_slot_available(self.receivers.len()) {
			self.log_sink.log_warn(
					"Session",
					&format!(
						"Too many concurrent transfers, rejected ContentBegin: transfer_id={req_id}, item_id={item_id}, active={}",
						self.receivers.len()
					),
					Some(&format!(
						"并发传输过多，已拒绝该传输请求: 传输ID={req_id}，项目ID={item_id}，当前并发={}",
						self.receivers.len()
					)),
			);
			self.emit_transfer_failed(
				&req_id,
				"TOO_MANY_TRANSFERS",
				&format!(
					"rejecting ContentBegin: {MAX_CONCURRENT_RECEIVERS} concurrent transfers already active"
				),
			);
			self.drop_pending_payload(&req_id);
			self.send_ctrl(CtrlMsg::ContentCancel {
				req_id,
				reason: "too many concurrent transfers".into(),
			})
			.await?;
			return Ok(());
		}

		// 2) 硬上限：对端声明的 total_bytes 超过 size_limits 中对应类型的硬上限时，
		//    在落盘前就拒绝（按 item 实际类型选 text/image/file；FileList 单文件用 file 上限）。
		let item_kind = self
			.inbound_item_kind(&item_id, file_id.as_deref(), &mime)
			.await;
		let hard_cap = hard_cap_bytes(&item_kind, &self.config.app_config.size_limits);
		if total_bytes > hard_cap {
			self.log_sink.log_warn(
					"Session",
					&format!(
						"Transfer rejected by hard size limit: transfer_id={req_id}, item_id={item_id}, kind={item_kind:?}, declared_bytes={total_bytes}, hard_cap={hard_cap}"
					),
					Some(&format!(
						"传输因超出硬上限被拒绝: 传输ID={req_id}，项目ID={item_id}，类型={item_kind:?}，声明字节={total_bytes}，硬上限={hard_cap}"
					)),
			);
			self.emit_transfer_failed(
				&req_id,
				"ITEM_TOO_LARGE",
				&format!(
					"declared total_bytes={total_bytes} exceeds hard cap {hard_cap} for {item_kind:?}"
				),
			);
			self.drop_pending_payload(&req_id);
			self.send_ctrl(CtrlMsg::ContentCancel {
				req_id,
				reason: format!("hard size limit exceeded: {total_bytes} > {hard_cap}"),
			})
			.await?;
			return Ok(());
		}

		// 3) 区间计划（B2）：必须无空洞、无重叠、连续覆盖到文件末尾，且不超过
		//    流数上限。任何不合法计划都在落盘前拒绝，并给出可诊断的错误码。
		if let Err((code, detail)) = validate_range_plan(&ranges, total_bytes) {
			self.log_sink.log_error(
				"Session",
				&format!(
					"Transfer rejected: invalid range plan: transfer_id={req_id}, code={code}, detail={detail}"
				),
				Some(&format!(
					"传输被拒绝：区间计划非法: 传输ID={req_id}，错误码={code}，详情={detail}"
				)),
				Some(code),
			);
			self.emit_transfer_failed(&req_id, code, &detail);
			self.drop_pending_payload(&req_id);
			self.send_ctrl(CtrlMsg::ContentCancel {
				req_id,
				reason: format!("invalid range plan: {detail}"),
			})
			.await?;
			return Ok(());
		}

		// 注：磁盘剩余空间检查需要平台无关的 statvfs 封装（当前工作区没有 fs2/sysinfo 依赖），
		// 因此这一子项按任务说明跳过；写入失败仍会由 writer Task 以 COMMIT_FAILED 上报。

		let tmp_path = match self.cas.get_tmp_path(&req_id) {
			Ok(p) => p,
			Err(e) => {
				// 对端传入非法 transfer_id：拒绝本次传输，但保留会话
				self.emit_transfer_failed(&req_id, "INVALID_TRANSFER_ID", &e.to_string());
				self.drop_pending_payload(&req_id);
				return Ok(());
			}
		};
		if let Some(p) = tmp_path.parent() {
			tokio::fs::create_dir_all(p).await?;
		}

		// 启动独立的 Writer Task
		//
		// channel 槽位上限仍是 32（限制单条传输的排队消息数），但真正的内存边界是
		// writer 每写完一块就归还的字节许可（MAX_INFLIGHT_BYTES_PER_RECEIVER）。
		// B1 起 tx/budget 同时保存进 ReceiverState：接收任务与最终化任务都用它们的
		// clone 投递数据，不再由 actor 主循环亲自 read + try_push。
		let (tx, mut rx) = mpsc::channel::<ReceiverTaskMsg>(32);
		let budget = Arc::new(Semaphore::new(MAX_INFLIGHT_BYTES_PER_RECEIVER));

		// Clone 需要的数据传入 Task
		let cas_clone = self.cas.clone();
		let store_clone = self.store.clone();
		let tmp_path_clone = tmp_path.clone();
		let tid = req_id.clone();
		let iid = item_id.clone();
		let fid = file_id.clone();
		let mime_clone = mime.clone();
		// 如果 CAS 中存在该文件，说明是断点续传，需要以 Append 模式打开。
		// `Path::exists` 是同步文件 IO，放进 spawn_blocking，避免运行时线程等磁盘。
		let resume_path = tmp_path.clone();
		let is_resume = tokio::task::spawn_blocking(move || resume_path.exists()).await?;

		let writer_task = tokio::spawn(async move {
			// RAII Guard: 任务退出时如果没 commit，自动删除 tmp 文件
			let mut guard = TempFileGuard {
				path: tmp_path_clone.clone(),
				committed: false,
			};

			// 打开文件。B2 使用**定位写**（每块 seek 到自己的绝对 offset），
			// 因此不能用 append 模式（Windows 上 append 会忽略 seek）。续传时
			// 保留已有内容并把长度截到声明值，避免残留尾部字节污染整文件 hash。
			let mut opts = OpenOptions::new();
			opts.write(true).create(true).truncate(!is_resume);

			let file_res = opts.open(&guard.path).await;
			if let Err(e) = file_res {
				eprintln!("[Session] Writer open failed: {e}");
				return;
			}
			// B2：区间是乱序完成的，writer 不能再用 BufWriter 的顺序追加语义；
			// 直接对 File 定位写：每块携带绝对 offset，偏移不连续就 seek。块大小
			// 256 KiB，跨区间 seek 的开销远小于一次网络/磁盘写。
			let mut writer = file_res.unwrap();
			if is_resume {
				let resume_path = guard.path.clone();
				let expected_len = total_bytes;
				let resize = tokio::task::spawn_blocking(move || {
					let file = std::fs::OpenOptions::new().write(true).open(&resume_path)?;
					file.set_len(expected_len)
				})
				.await;
				match resize {
					Ok(Ok(())) => {}
					Ok(Err(e)) => {
						eprintln!("[Session] Writer resize failed: {e}");
						return; // 触发 Drop 删除
					}
					Err(e) => {
						eprintln!("[Session] Writer resize task failed: {e}");
						return;
					}
				}
			}
			// 实际写入的字节数：fsync 策略按“真实大小”而不是对端声明值选择，
			// 这样续传/截断的传输也不会被误判成大文件。
			let mut written_bytes: u64 = 0;
			let mut write_cursor: Option<u64> = None;

			while let Some(msg) = rx.recv().await {
				match msg {
					ReceiverTaskMsg::Chunk {
						offset,
						data,
						permit,
					} => {
						if write_cursor != Some(offset) {
							if let Err(e) = writer.seek(SeekFrom::Start(offset)).await {
								eprintln!("[Session] Write seek failed: {e}");
								return; // 触发 Drop 删除
							}
						}
						if let Err(e) = writer.write_all(&data).await {
							eprintln!("[Session] Write failed: {e}");
							return; // 触发 Drop 删除
						}
						write_cursor = Some(offset.saturating_add(data.len() as u64));
						written_bytes = written_bytes.saturating_add(data.len() as u64);
						// 写完后归还字节许可：actor 看到的"在途字节"因此随磁盘进度下降。
						drop(permit);
					}
					ReceiverTaskMsg::Finish {
						expected_sha256,
						verify_whole_file,
						reply_tx,
					} => {
						// 1. Flush & 关闭文件句柄（校验/commit 用独立句柄重开）
						if writer.flush().await.is_err() {
							return;
						}
						if writer.shutdown().await.is_err() {
							return;
						}
						drop(writer);

						// 2. 按 ADR-002 §3.7 选择持久化动作：
						// - always / large-only 小文件：rename 前 sync_all（本次 commit 内完成）；
						// - large-only 大文件：只 rename，后台 worker 补 sync_data；
						// - never：完全不做 fsync。
						// 同步 fsync 的代价按实测 740 MiB/s 计，大文件尾延迟 0.3–2.6 s，
						// 因此默认策略把 ≥ 8 MiB 的传输改为后台补同步。
						let decision = cas_clone.fsync_policy().decision(written_bytes);

						// 3. 完整性校验 + Commit（移入 spawn_blocking，不占 runtime worker）
						guard.committed = true; // 禁止 Drop 删除，转交控制权
						let guard_path = guard.path.clone(); // Clone path before move

						let commit_res = tokio::task::spawn_blocking(move || {
							// A. 完整性（ADR-002 §4.2 方案二）：所有区间 FIN 后按偏移
							//    顺序重读临时文件、重算整文件 hash，与 ContentBegin.sha256
							//    比对。代价是一次顺序读（NVMe ~4.7 GiB/s）+ 与 B1 相同的
							//    hash CPU；换来对整文件拼接顺序的校验（逐区间 hash 无法
							//    证明整文件 hash 一致）。
							if verify_whole_file {
								let calculated = sha256_file_hex(&guard_path)?;
								if calculated != expected_sha256 {
									anyhow::bail!(
										"Checksum mismatch: expected={expected_sha256}, calculated={calculated}"
									);
								}
							}

							// B. 转正 Blob (CAS)：blob id 来自 ContentBegin 的整文件
							//    hash（与 B1 相同）。
							let commit = cas_clone.commit_tmp_file_with_policy(
								&guard_path,
								&expected_sha256,
								decision,
							)?;

							// C. 决定落地路径 (Materialize)
							let final_path = if let Some(real_fid) = fid {
								// FileList 模式
								let store = store_clone.lock().unwrap();
								if let Some(fmeta) = store.get_file_meta(&iid, &real_fid)? {
									cas_clone.materialize_file(
										&expected_sha256,
										&tid,
										&fmeta.rel_name,
									)?
								} else {
									commit.blob_path.clone()
								}
							} else {
								// Text/Image 模式
								let ext = match mime_clone.as_str() {
									"image/png" => Some("png"),
									"image/jpeg" | "image/jpg" => Some("jpg"),
									_ => None,
								};
								if let Some(extension) = ext {
									cas_clone.materialize_blob(&expected_sha256, extension)?
								} else {
									commit.blob_path.clone()
								}
							};

							Ok::<FinalizeOutcome, anyhow::Error>(FinalizeOutcome {
								path: final_path,
								blob_path: commit.blob_path,
								durability: commit.durability,
							})
						})
						.await;

						// 发送结果回主 Actor
						let res = match commit_res {
							Ok(Ok(outcome)) => Ok(outcome),
							Ok(Err(e)) => Err(e),
							Err(e) => Err(anyhow::Error::new(e)),
						};
						let _ = reply_tx.send(res);
						return; // 任务结束
					}
				}
			}
		});
		// 保留 abort 句柄：ContentEnd 超时 / 取消 / 越界时终止 writer，TempFileGuard 会删 tmp。
		let writer_handle = writer_task.abort_handle();

		// 注意：如果是 Resume，这里的 total_bytes 是整文件大小，区间计划覆盖
		// [base_offset, total_bytes)；整文件 hash 只在 base_offset == 0 时校验。
		let started_ts_ms = now_ms();
		let stats = PayloadStats::new(started_ts_ms);
		let receiver_state = ReceiverState::Receiving {
			transfer_id: req_id.clone(),
			item_id,
			file_id,
			expected_sha256: file_sha256,
			mime,
			tx,
			budget,
			stats,
			handle: writer_handle,
			ranges: ReceiverRanges::from_plan(&ranges, total_bytes),
			range_handles: HashMap::new(),
			end_received: false,
			end_wait_started_ms: 0,
			total_bytes,
			hard_cap_bytes: hard_cap,
			started_ts_ms,
		};
		self.receivers.insert(req_id.clone(), receiver_state);

		// 数据流可能先于 ContentBegin 到达（控制流/数据流独立交付）。B2 同一传输
		// 可能已暂存多条区间流，注册完成后逐条接入；header 与计划不匹配的会在
		// attach_payload 里按 gap/overlap/unexpected 失败整个传输。
		if let Some(pending_streams) = self.pending_payloads.remove(&req_id) {
			for pending in pending_streams {
				let opened = PayloadOpened {
					header: pending.header,
					recv: pending.recv,
					send: pending.send,
				};
				self.attach_payload(opened).await?;
				if !matches!(
					self.receivers.get(&req_id),
					Some(ReceiverState::Receiving { .. })
				) {
					// attach 失败已终止该传输（剩余暂存流随 drop 关闭）。
					break;
				}
			}
		}
		Ok(())
	}

	/// 丢弃所有仍在等待 `ContentBegin` 的数据流（receiver 被拒绝/已存在时）。
	fn drop_pending_payload(&mut self, req_id: &str) {
		if self.pending_payloads.remove(req_id).is_some() {
			self.log_sink.log_warn(
				"Session",
				&format!(
					"Dropping payload streams that will never be registered: transfer_id={req_id}"
				),
				Some(&format!(
					"丢弃无法注册的数据流（传输已被拒绝或已存在）: 传输ID={req_id}"
				)),
			);
		}
	}

	/// 校验数据流头的**元数据**与已登记的 `ContentBegin` 一致。
	///
	/// 区间 offset/length 不在这里比较，而由 [`route_range_header`] 对照
	/// `ContentBegin.ranges` 精确匹配（B2）。这里只守住 item/file/整文件 sha
	/// 三个字段，避免对端用同 req_id 的头部把不同内容拼在一起。
	fn validate_payload_header(
		header: &TransferHeader,
		item_id: &str,
		file_id: Option<&str>,
		expected_sha256: &str,
	) -> std::result::Result<(), String> {
		if header.item_id != item_id {
			return Err(format!(
				"item_id mismatch: header={}, begin={item_id}",
				header.item_id
			));
		}
		if header.file_id.as_deref() != file_id {
			return Err(format!(
				"file_id mismatch: header={:?}, begin={file_id:?}",
				header.file_id
			));
		}
		if header.sha256 != expected_sha256 {
			return Err(format!(
				"sha256 mismatch: header={}, begin={expected_sha256}",
				header.sha256
			));
		}
		Ok(())
	}

	/// 把一条**区间**数据流接入已登记的 receiver：启动该区间的接收任务并登记
	/// abort 句柄。同一 req_id 的多个区间可以并发、乱序接入。
	///
	/// - 元数据不一致 → 整传输失败（`TRANSFER_HEADER_MISMATCH`）；
	/// - 区间与计划不匹配 → 重叠/错位/未声明/长度不符，整传输失败；
	/// - 重复区间（已接入或已完成）→ 只丢弃这条流，不动传输。
	async fn attach_payload(&mut self, opened: PayloadOpened) -> Result<()> {
		let req_id = opened.header.req_id.clone();

		// 第一段：只读借用完成校验与路由，避免在借用期间调用 &mut self 方法。
		let routing = {
			let Some(ReceiverState::Receiving {
				item_id,
				file_id,
				expected_sha256,
				ranges,
				..
			}) = self.receivers.get(&req_id)
			else {
				// 调用方已做过路由判定；这里再防御一次（状态可能在 await 间隙变化）。
				return Ok(());
			};
			if let Err(detail) = Self::validate_payload_header(
				&opened.header,
				item_id,
				file_id.as_deref(),
				expected_sha256,
			) {
				Err((
					"TRANSFER_HEADER_MISMATCH",
					format!("payload header rejected: {detail}"),
				))
			} else {
				match route_range_header(ranges, opened.header.offset, opened.header.length) {
					RangeRoute::Attach(index) | RangeRoute::Retry(index) => Ok(index),
					RangeRoute::Duplicate => Err((
						"TRANSFER_RANGE_DUPLICATE",
						format!(
							"range {}+{} is already attached/finished",
							opened.header.offset, opened.header.length
						),
					)),
					RangeRoute::Overlap => Err((
						"TRANSFER_RANGE_OVERLAP",
						format!(
							"range {}+{} overlaps a planned range",
							opened.header.offset, opened.header.length
						),
					)),
					RangeRoute::LengthMismatch => Err((
						"TRANSFER_RANGE_LENGTH_MISMATCH",
						format!(
							"range at offset {} has length {}, which does not match the plan",
							opened.header.offset, opened.header.length
						),
					)),
					RangeRoute::Unexpected => Err((
						"TRANSFER_RANGE_UNEXPECTED",
						format!(
							"range {}+{} is not part of the declared plan",
							opened.header.offset, opened.header.length
						),
					)),
				}
			}
		};

		let index = match routing {
			Ok(index) => index,
			Err((code, detail)) => {
				if code == "TRANSFER_RANGE_DUPLICATE" {
					// 重复流只影响它自己：丢掉句柄（drop 会复位该流），传输继续。
					self.log_sink.log_warn(
						"Session",
						&format!(
							"Rejecting duplicate range stream: transfer_id={req_id}, {detail}"
						),
						Some(&format!("拒绝重复的区间数据流: 传输ID={req_id}，{detail}")),
					);
					return Ok(());
				}
				self.log_sink.log_error(
					"Session",
					&format!(
						"Rejecting payload range: transfer_id={req_id}, code={code}, detail={detail}"
					),
					Some(&format!(
						"拒绝区间数据流: 传输ID={req_id}，错误码={code}，详情={detail}"
					)),
					Some(code),
				);
				drop(opened);
				self.abort_receiver(&req_id, code, &detail, "invalid payload range")
					.await;
				return Ok(());
			}
		};

		// 第二段：登记 Attached 并启动该区间的接收任务。
		let sink = self.sink.clone();
		let event_tx = self.payload_event_tx.clone();
		let Some(ReceiverState::Receiving {
			tx,
			budget,
			stats,
			item_id,
			total_bytes,
			hard_cap_bytes,
			ranges,
			range_handles,
			..
		}) = self.receivers.get_mut(&req_id)
		else {
			return Ok(());
		};
		let range = ranges.slots[index];
		ranges.set_state(index, RangeSlotState::Attached);
		let receiver_task_inbox = ReceiverInbox::new(tx.clone(), budget.clone(), range.offset);
		let stats_clone = stats.clone();
		let item_id_clone = item_id.clone();
		let total = *total_bytes;
		let hard_cap = *hard_cap_bytes;
		let event_req_id = req_id.clone();

		let handle = tokio::spawn(async move {
			let event = receive_payload_stream(
				opened.recv,
				opened.send,
				event_req_id,
				item_id_clone,
				range.offset,
				range.length,
				total,
				hard_cap,
				receiver_task_inbox,
				stats_clone,
				sink,
			)
			.await;
			// actor 已结束（通道关闭）时忽略；否则由 actor 收尾。
			let _ = event_tx.send(event).await;
		})
		.abort_handle();
		range_handles.insert(range.offset, handle);
		Ok(())
	}

	/// 处理一条已读完头部的入站数据流（actor select 分支）。
	async fn handle_payload_opened(&mut self, opened: PayloadOpened) -> Result<()> {
		let req_id = opened.header.req_id.clone();
		let receiver_exists = matches!(
			self.receivers.get(&req_id),
			Some(ReceiverState::Receiving { .. })
		);
		let commit_in_progress = matches!(
			self.receivers.get(&req_id),
			Some(ReceiverState::Committing { .. })
		);
		let route = route_payload_stream(
			receiver_exists,
			commit_in_progress,
			self.cancelled_payloads.contains_key(&req_id),
		);

		match route {
			PayloadRoute::ReceiverReady => self.attach_payload(opened).await,
			PayloadRoute::AwaitRegistration => {
				// B2：同一 req_id 的多条区间流都可能先于 ContentBegin 到达，
				// 但每个 req_id 最多暂存计划上限条，且同 offset 不得重复。
				let total_pending: usize = self.pending_payloads.values().map(Vec::len).sum();
				if total_pending >= MAX_PENDING_PAYLOAD_STREAMS {
					self.log_sink.log_warn(
						"Session",
						&format!(
							"Rejecting payload stream: {MAX_PENDING_PAYLOAD_STREAMS} streams already waiting for ContentBegin (transfer_id={req_id})"
						),
						Some(&format!(
							"拒绝数据流：已有 {MAX_PENDING_PAYLOAD_STREAMS} 条流在等待 ContentBegin（传输ID={req_id}）"
						)),
					);
					return Ok(());
				}
				let pending = self.pending_payloads.entry(req_id.clone()).or_default();
				if pending.len() >= MAX_PAYLOAD_STREAMS_PER_TRANSFER {
					self.log_sink.log_warn(
						"Session",
						&format!(
							"Rejecting payload stream: {MAX_PAYLOAD_STREAMS_PER_TRANSFER} pending ranges already waiting (transfer_id={req_id})"
						),
						Some(&format!(
							"拒绝数据流：同一传输已有 {MAX_PAYLOAD_STREAMS_PER_TRANSFER} 条待注册区间流（传输ID={req_id}）"
						)),
					);
					return Ok(());
				}
				if pending
					.iter()
					.any(|item| item.header.offset == opened.header.offset)
				{
					self.log_sink.log_warn(
						"Session",
						&format!(
							"Rejecting duplicate pending payload stream: transfer_id={req_id}, offset={}",
							opened.header.offset
						),
						Some(&format!(
							"拒绝重复的待注册数据流: 传输ID={req_id}，偏移={}",
							opened.header.offset
						)),
					);
					return Ok(());
				}
				pending.push(PendingPayload {
					header: opened.header,
					recv: opened.recv,
					send: opened.send,
					opened_at_ms: now_ms(),
				});
				self.log_sink.log_debug(
					"Session",
					&format!(
						"Payload stream arrived before ContentBegin; holding for registration: transfer_id={req_id}"
					),
					Some(&format!(
						"数据流先于 ContentBegin 到达，暂存等待注册: 传输ID={req_id}"
					)),
				);
				Ok(())
			}
			PayloadRoute::Duplicate => {
				self.log_sink.log_warn(
					"Session",
					&format!("Rejecting payload stream while finalizing: transfer_id={req_id}"),
					Some(&format!("拒绝最终化中的重复数据流: 传输ID={req_id}")),
				);
				Ok(())
			}
			PayloadRoute::Cancelled => {
				self.log_sink.log_warn(
					"Session",
					&format!(
						"Rejecting payload stream for cancelled transfer: transfer_id={req_id}"
					),
					Some(&format!("拒绝已取消传输的数据流: 传输ID={req_id}")),
				);
				Ok(())
			}
		}
	}

	/// 处理区间接收任务的完成/失败事件（actor select 分支）。
	async fn handle_payload_event(&mut self, event: PayloadEvent) -> Result<()> {
		match event {
			PayloadEvent::RangeFinished {
				req_id,
				offset,
				received,
			} => {
				let outcome = {
					let Some(ReceiverState::Receiving {
						ranges,
						range_handles,
						end_received,
						..
					}) = self.receivers.get_mut(&req_id)
					else {
						return Ok(());
					};
					match ranges.slot_at_offset(offset) {
						Some(index) if ranges.slots[index].state == RangeSlotState::Attached => {
							if received != ranges.slots[index].length {
								Some(Err((
									"TRANSFER_RANGE_UNDERRUN",
									format!(
										"range {offset} reported {received} bytes, planned {}",
										ranges.slots[index].length
									),
								)))
							} else {
								ranges.set_state(index, RangeSlotState::Finished);
								range_handles.remove(&offset);
								Some(Ok(*end_received && ranges.all_finished()))
							}
						}
						// 重复/迟到事件：忽略（取消后 FIN 可能晚到）。
						_ => None,
					}
				};
				match outcome {
					Some(Ok(true)) => self.begin_finalize(&req_id),
					Some(Err((code, detail))) => {
						self.abort_receiver(&req_id, code, &detail, "range byte count mismatch")
							.await;
					}
					// 区间已完成但 ContentEnd 未到（或还有别的区间未完成/重复事件）：
					// 保持 Receiving，由 ContentEnd / idle sweep 兜底。
					Some(Ok(false)) | None => {}
				}
			}
			PayloadEvent::RangeFailed {
				req_id,
				offset,
				code,
				detail,
				retryable,
			} => {
				if retryable {
					// 只标记该区间失败：发送端会降 N + 冷却 2T 后从同一 offset
					// 重开一条流；这里保留 receiver 的 tmp 文件与其它区间。
					if let Some(ReceiverState::Receiving {
						ranges,
						range_handles,
						..
					}) = self.receivers.get_mut(&req_id)
					{
						if let Some(index) = ranges.slot_at_offset(offset) {
							ranges.set_state(index, RangeSlotState::Failed);
						}
						range_handles.remove(&offset);
					}
					self.log_sink.log_warn(
						"Session",
						&format!(
							"Range stream failed, waiting for retry from sender: transfer_id={req_id}, offset={offset}, code={code}, detail={detail}"
						),
						Some(&format!(
							"区间数据流失败，等待发送端退避后重试: 传输ID={req_id}，偏移={offset}，错误码={code}，详情={detail}"
						)),
					);
				} else {
					self.log_sink.log_error(
						"Session",
						&format!(
							"Transfer aborted by payload range: transfer_id={req_id}, offset={offset}, code={code}, detail={detail}"
						),
						Some(&format!(
							"区间数据流失败，已终止传输: 传输ID={req_id}，偏移={offset}，错误码={code}，详情={detail}"
						)),
						Some(&detail),
					);
					self.abort_receiver(&req_id, &code, &detail, "payload range failed")
						.await;
				}
			}
		}
		Ok(())
	}

	/// 只取出 `Receiving` 状态；`Committing` 占位符必须留在 map 里，
	/// 否则同 req_id 的新 Begin 会在旧 commit 尚未结束时开写同一个 tmp 文件。
	fn take_receiving(&mut self, req_id: &str) -> Option<ReceiverState> {
		if matches!(
			self.receivers.get(req_id),
			Some(ReceiverState::Receiving { .. })
		) {
			self.receivers.remove(req_id)
		} else {
			None
		}
	}

	/// 立即终止一条接收中的传输：abort writer Task（RAII 删除 tmp 文件）与**全部**
	/// 区间接收任务，上报 TRANSFER_FAILED，并尽力通知对端停止发送（ContentCancel
	/// 是非致命的）。
	async fn abort_receiver(&mut self, req_id: &str, code: &str, detail: &str, peer_reason: &str) {
		let Some(ReceiverState::Receiving {
			handle,
			range_handles,
			..
		}) = self.take_receiving(req_id)
		else {
			return;
		};
		// 直接 abort 而不是往有界 channel 里 await Cancel：这里同样不能阻塞 actor 主循环。
		// writer Task 的 TempFileGuard 在 drop 时删除 tmp 文件。
		handle.abort();
		// 每个区间的接收任务都持有 RecvStream；必须全部 abort，否则未取消的区间
		// 会继续读数据、继续占 QUIC 流（B1 只有一条流，B2 最多 6 条）。
		// abort/drop 后对端会收到 STOP_SENDING/RESET，write_all 立刻失败。
		for range_handle in range_handles.values() {
			range_handle.abort();
		}
		// 若还有区间流在等待注册（Awaiting），一并丢弃，避免它继续占用流。
		self.pending_payloads.remove(req_id);
		self.record_cancelled_payload(req_id);
		self.emit_transfer_failed(req_id, code, detail);
		let _ = self
			.send_ctrl(CtrlMsg::ContentCancel {
				req_id: req_id.to_string(),
				reason: peer_reason.to_string(),
			})
			.await;
	}

	/// 记录一个已取消/已失败的 req_id，用于立即拒绝迟到的数据流。
	fn record_cancelled_payload(&mut self, req_id: &str) {
		if self.cancelled_payloads.len() >= MAX_CANCELLED_PAYLOADS {
			// 上限保护：清掉最旧的一半，绝不因对端刷 Cancel 而无界增长。
			let mut entries: Vec<(String, i64)> = self
				.cancelled_payloads
				.iter()
				.map(|(k, v)| (k.clone(), *v))
				.collect();
			entries.sort_by_key(|(_, ts)| *ts);
			let remove_count = entries.len() / 2;
			for (k, _) in entries.into_iter().take(remove_count) {
				self.cancelled_payloads.remove(&k);
			}
		}
		self.cancelled_payloads.insert(req_id.to_string(), now_ms());
	}

	/// 回收空闲超时的 receiver：对端开传输后保持心跳却不发数据时，
	/// 不能让它永久占着 tmp 文件与 fd。
	///
	/// B1 起额外回收三类占位符：
	/// - 等待 `ContentBegin` 超过宽限的数据流（未知 req_id）；
	/// - `ContentEnd` 先到、但数据流迟迟不到（协议错误，不 finalize 空文件）；
	/// - 过期的取消 tombstones。
	async fn sweep_idle_receivers(&mut self) {
		let now = now_ms();
		let idle: Vec<String> = self
			.receivers
			.iter()
			.filter_map(|(id, st)| match st {
				ReceiverState::Receiving { stats, .. }
					if receiver_idle_expired(now, stats.last_activity_ms()) =>
				{
					Some(id.clone())
				}
				_ => None,
			})
			.collect();
		for id in idle {
			self.log_sink.log_warn(
				"Session",
				&format!("Receiver idle timeout, cancelling transfer: transfer_id={id}"),
				Some(&format!(
					"接收方空闲超时，正在取消传输: 传输ID={id}，超时={RECEIVER_IDLE_TIMEOUT_MS}ms"
				)),
			);
			self.abort_receiver(
				&id,
				"TRANSFER_IDLE_TIMEOUT",
				"no data chunk received before idle timeout",
				"receiver idle timeout",
			)
			.await;
		}

		// `ContentEnd` 已到、但计划里还有区间没 FIN：超过宽限即判缺段（协议错误或
		// 发送端放弃），绝不挂起等一个永远不会来的 FIN，也绝不 commit 缺段文件。
		let missing_ranges: Vec<(String, String)> = self
			.receivers
			.iter()
			.filter_map(|(id, st)| match st {
				ReceiverState::Receiving {
					ranges,
					end_received: true,
					end_wait_started_ms,
					..
				} if !ranges.all_finished()
					&& now - *end_wait_started_ms > CONTENT_END_WITHOUT_PAYLOAD_GRACE_MS =>
				{
					let missing = ranges
						.slots
						.iter()
						.filter(|slot| slot.state != RangeSlotState::Finished)
						.map(|slot| format!("{}+{}", slot.offset, slot.length))
						.collect::<Vec<_>>()
						.join(",");
					Some((id.clone(), missing))
				}
				_ => None,
			})
			.collect();
		for (id, missing) in missing_ranges {
			self.log_sink.log_error(
				"Session",
				&format!(
					"ContentEnd without all ranges finished (protocol error), aborting transfer: transfer_id={id}, missing_ranges=[{missing}]"
				),
				Some(&format!(
					"收到 ContentEnd 但仍有区间未完成（协议错误），已终止传输: 传输ID={id}，缺失区间=[{missing}]"
				)),
				Some("TRANSFER_RANGE_MISSING"),
			);
			self.abort_receiver(
				&id,
				"TRANSFER_RANGE_MISSING",
				&format!("ContentEnd received but ranges still missing: [{missing}]"),
				"missing payload range",
			)
			.await;
		}

		// 未注册数据流的宽限回收：未知 req_id 或已取消传输。B2 每个 req_id
		// 可能暂存多条区间流，按条回收。
		let mut stale_pending: Vec<String> = Vec::new();
		for (id, pending_streams) in &mut self.pending_payloads {
			let before = pending_streams.len();
			pending_streams
				.retain(|pending| now - pending.opened_at_ms <= PAYLOAD_REGISTRATION_GRACE_MS);
			if pending_streams.len() != before {
				stale_pending.push(id.clone());
			}
		}
		self.pending_payloads
			.retain(|_, pending_streams| !pending_streams.is_empty());
		for id in stale_pending {
			let cancelled = self.cancelled_payloads.contains_key(&id);
			self.log_sink.log_warn(
				"Session",
				&format!(
					"Dropping payload streams without ContentBegin registration: transfer_id={id}, cancelled={cancelled}"
				),
				Some(&format!(
					"丢弃没有 ContentBegin 注册的数据流: 传输ID={id}，已取消={cancelled}"
				)),
			);
		}

		// 取消 tombstones 的 TTL 回收。
		self.cancelled_payloads
			.retain(|_, ts| now - *ts <= CANCELLED_PAYLOAD_TTL_MS);

		// 兜底：最终化任务若异常消失，Committing 占位符不能永久占着 req_id
		// 和并发名额（任务自身已有 CONTENT_END_TIMEOUT，正常路径不会走到这里）。
		let stale: Vec<String> = self
			.receivers
			.iter()
			.filter_map(|(id, st)| match st {
				ReceiverState::Committing {
					finalize_started_ms,
					..
				} if now - *finalize_started_ms > FINALIZE_PLACEHOLDER_GRACE_MS => Some(id.clone()),
				_ => None,
			})
			.collect();
		for id in stale {
			self.log_sink.log_error(
				"Session",
				&format!(
					"Finalizing transfer placeholder expired without result: transfer_id={id}"
				),
				Some(&format!("最终化占位符超时未收到结果，已回收: 传输ID={id}")),
				Some("FINALIZE_PLACEHOLDER_EXPIRED"),
			);
			self.receivers.remove(&id);
		}
	}

	/// 处理对端的 `ContentEnd`（语义：**finalize now**）。
	///
	/// 顺序契约：发送端先 `finish()` 数据流（FIN = 字节全部到达），再在控制流上发
	/// `ContentEnd`。两条流是独立 QUIC 流，交付顺序无法保证，因此这里必须同时
	/// 处理两种先后：
	///
	/// - 数据流已 FIN → 立即进入最终化；
	/// - 数据流已接入但未 FIN → 标记 `end_received`，等 `PayloadEvent::Finished`；
	/// - 数据流头还没到 → 标记 `end_received` 并记下时间，由
	///   [`Self::sweep_idle_receivers`] 在宽限后判 `PAYLOAD_STREAM_MISSING`。
	///   绝不阻塞等待，也绝不 finalize 一个空的 tmp 文件。
	///
	/// 关键：actor 主循环只负责把"最终化"交给独立任务并登记 `Committing` 占位符，
	/// 绝不在 select 循环里等待 flush/close/commit/materialize（2 GiB 文件或慢磁盘
	/// 会阻塞整条连接的全部帧）。最终化任务完成后通过 `receiver_done_tx` 回报，
	/// 由 select 的完成事件分支处理。
	fn handle_content_end(&mut self, req_id: &str) {
		let decision = {
			let Some(ReceiverState::Receiving {
				ranges,
				end_received,
				end_wait_started_ms,
				..
			}) = self.receivers.get_mut(req_id)
			else {
				// 重复 End（Committing）或未知 req_id：忽略，与旧行为一致。
				return;
			};
			let route = content_end_route(ranges.all_finished(), *end_received);
			if route == ContentEndRoute::WaitForPayload {
				*end_received = true;
				// 从"第一次知道有区间没完成"开始计宽限；sweep 超时后判
				// TRANSFER_RANGE_MISSING。
				*end_wait_started_ms = now_ms();
			}
			route
		};

		if decision == ContentEndRoute::FinalizeNow {
			self.begin_finalize(req_id);
		}
	}

	/// 所有计划区间都 FIN 且 `ContentEnd` 已到达：把最终化交给独立任务。
	///
	/// `Committing` 占位符在最终化完成前一直占用 req_id —— 期间同 req_id 的
	/// `ContentBegin` 被当作重复 Begin 忽略，`ContentEnd` 也被忽略，不会有第二个
	/// writer 与旧 commit 竞争同一个 tmp 文件。
	fn begin_finalize(&mut self, req_id: &str) {
		let Some(ReceiverState::Receiving {
			tx,
			budget,
			stats,
			handle,
			item_id,
			file_id,
			transfer_id,
			expected_sha256,
			total_bytes,
			started_ts_ms,
			mime,
			ranges,
			..
		}) = self.take_receiving(req_id)
		else {
			return;
		};

		// 数据流已全部 FIN，接收任务已退出；用同一组 tx/budget 构造最终化 inbox。
		let inbox = ReceiverInbox::new(tx, budget, 0);
		let received_bytes = stats.received_bytes();
		// 只有完整传输（起点 0、区间并集 == 文件大小）才可能在 finalize 时校验
		// 整文件 hash；断点续传部分数据无法用 ContentBegin.sha256 校验。
		let verify_whole_file = ranges.is_whole_file();
		if !verify_whole_file {
			self.log_sink.log_warn(
				"Session",
				&format!(
					"Transfer does not cover the whole file (base_offset={}), skipping whole-file checksum: transfer_id={transfer_id}, received={received_bytes}/{total_bytes}",
					ranges.base_offset
				),
				Some(&format!(
					"传输未覆盖整文件（起点={}），跳过整文件校验: 传输ID={transfer_id}，已收={received_bytes}/{total_bytes}",
					ranges.base_offset
				)),
			);
		}

		// 占位：commit 期间保持 req_id 被占用（也计入并发名额，因为 tmp/fd/Task 还在）。
		self.receivers.insert(
			req_id.to_string(),
			ReceiverState::Committing {
				transfer_id: transfer_id.clone(),
				item_id: item_id.clone(),
				file_id: file_id.clone(),
				expected_sha256: expected_sha256.clone(),
				mime,
				total_bytes,
				started_ts_ms,
				finalize_started_ms: now_ms(),
			},
		);

		let done_tx = self.receiver_done_tx.clone();
		// 独立任务：flush pending → Finish → 等 writer 完成 flush/close/校验/commit。
		// 30s 超时保证卡死的磁盘不会永久占着 writer Task 与 tmp 文件。
		tokio::spawn(async move {
			let result = if let Ok(result) = tokio::time::timeout(
				CONTENT_END_TIMEOUT,
				finalize_receiver(inbox, expected_sha256.clone(), verify_whole_file),
			)
			.await
			{
				result
			} else {
				// 尽力终止 writer；若它卡在 spawn_blocking 的磁盘调用里，abort 只能
				// 释放外层任务，阻塞线程本身无法被取消（见最终报告的残余风险）。
				handle.abort();
				Err((
					"COMMIT_TIMEOUT".to_string(),
					format!("finalize exceeded {}ms", CONTENT_END_TIMEOUT.as_millis()),
				))
			};
			let _ = done_tx
				.send(ReceiverDone {
					transfer_id,
					item_id,
					file_id,
					expected_sha256,
					received_bytes,
					total_bytes,
					started_ts_ms,
					result,
				})
				.await;
		});
	}

	/// 处理最终化任务回报的结果（在 select 分支里调用，不阻塞读帧）。
	async fn handle_receiver_done(&mut self, done: ReceiverDone) -> Result<()> {
		// 无论成功失败都先清掉 Committing 占位符，释放 req_id。
		self.receivers.remove(&done.transfer_id);

		match done.result {
			Ok(outcome) => {
				// 持久化顺序契约（ADR-002 §3.7）：
				// - Synced（always / large-only 小文件）：sync_all 已在 rename 前成功，
				//   此刻才允许上报 CONTENT_CACHED；
				// - Pending（large-only ≥ 8 MiB）：rename 已成功，立即上报完成并把
				//   durable=0 写进库，后台 worker 负责 sync_data；
				// - NotSynced（never）：不做 fsync（旧行为），durable 保持默认 1。
				let durability_pending = outcome.durability == DurabilityOutcome::Pending;
				let final_path = outcome.path;
				let blob_path = outcome.blob_path;

				// 每个传输只写一条汇总（字节数 + 耗时），不再逐块记录
				let elapsed_ms = now_ms() - done.started_ts_ms;
				self.log_sink.log_info(
					"Session",
					&format!("Inbound transfer completed: transfer_id={}, bytes={}/{}, elapsed_ms={elapsed_ms}, sha256={}, durability={:?}", done.transfer_id, done.received_bytes, done.total_bytes, done.expected_sha256, outcome.durability),
					Some(&format!("入站传输成功完成: 传输ID={}，字节数={}/{}，耗时={elapsed_ms}ms，SHA256={}，持久化状态={:?}", done.transfer_id, done.received_bytes, done.total_bytes, done.expected_sha256, outcome.durability)),
				);

				let store = self.store.clone();
				// 注意：B2 的 ContentBegin.sha256 是**整文件** hash（区间哈希不在控制面）；
				// FileList 传输时它只是其中一个文件的 sha，不能直接用来标记
				// content_cache（见 mark_item_content_cached）。
				let transfer_sha = done.expected_sha256.clone();
				let iid = done.item_id.clone();
				let fid = done.file_id.clone();
				let final_path_str = final_path.to_string_lossy().to_string();
				let item_id = done.item_id.clone();
				let file_id = done.file_id.clone();
				let expected_sha256 = done.expected_sha256.clone();

				// 【新增】查询 DB 获取完整元数据
				let meta_info = tokio::task::spawn_blocking(move || {
					let mut guard = store.lock().unwrap();
					// 按 item 自身 content.sha256 标记缓存并保留命中行数；
					// 只有需要后台补同步时才写 durable=0（其余模式为 1）。
					let (marked_sha, marked_rows) = mark_item_content_cached(
						&mut guard,
						&iid,
						&transfer_sha,
						now_ms(),
						!durability_pending,
					)?;

					// 补充查询
					let mime = guard.get_item_mime(&iid)?.unwrap_or_default();
					// 简单判断类型
					let kind = if fid.is_some() {
						"file"
					} else if mime.starts_with("text/") {
						"text"
					} else if mime.starts_with("image/") {
						"image"
					} else {
						"file" // 或 "binary"，但要和文档一致
					};

					Ok::<(String, String, String, usize), anyhow::Error>((
						mime,
						kind.to_string(),
						marked_sha,
						marked_rows,
					))
				})
				.await??;

				let (mime, kind, marked_sha, marked_rows) = meta_info;
				if marked_rows == 0 {
					// 0 行说明 content_cache 里没有该 item 的记录（例如元数据已被删除）；
					// 传输本身仍然成功，只记警告，不硬失败。
					self.log_sink.log_warn(
						"Session",
						&format!(
							"mark_cache_present matched 0 rows: item_id={item_id}, item_sha256={marked_sha}, transfer_sha256={expected_sha256}"
						),
						Some(&format!(
							"缓存标记未命中任何行: 项目ID={item_id}，项目SHA256={marked_sha}，传输SHA256={expected_sha256}"
						)),
					);
				}

				// 大文件（large-only ≥ 8 MiB）：durable=0 已经和 present=1 一起落库，
				// 现在把“同步最终 blob”的请求投递给后台 worker。队列有界；满了就
				// 只记警告，行仍然是 durable=0，30 s sweep 会兜底。
				if durability_pending && marked_rows > 0 {
					let queued = self
						.cas
						.enqueue_background_sync(expected_sha256.clone(), marked_sha.clone());
					// 无论入队成功与否都要确保 worker 存在（sweep 兜底路径）。
					crate::api::ensure_durability_worker(
						&self.cas,
						self.store.clone(),
						&self.log_sink,
					);
					if !queued {
						self.log_sink.log_warn(
							"Session",
							&format!(
								"durability queue full/closed; transfer stays durable=0 and the 30s sweep will sync it: transfer_id={}, blob={}, cache_sha256={}",
								done.transfer_id,
								blob_path.display(),
								marked_sha
							),
							Some(&format!(
								"后台持久化队列已满/已关闭，本次传输保持 durable=0，由 30s 周期 sweep 补同步: 传输ID={}，blob={}，缓存SHA256={}",
								done.transfer_id,
								blob_path.display(),
								marked_sha
							)),
						);
					}
				}

				// 【修改】构造符合文档的 local_ref
				let local_ref = serde_json::json!({
					"local_path": final_path_str,
					"item_id": item_id,
					"mime": mime,
					"kind": kind,
					"sha256": expected_sha256,
					"total_bytes": done.total_bytes
				});

				// 发送事件
				let evt = serde_json::json!({
					"type": "CONTENT_CACHED",
					"payload": {
						"transfer_id": done.transfer_id,
						"item_id": item_id,
						"file_id": file_id,
						"local_ref": local_ref
					}
				});
				self.sink.emit(evt.to_string());
			}
			Err((code, message)) => {
				self.emit_transfer_failed(&done.transfer_id, &code, &message);
			}
		}
		Ok(())
	}

	fn handle_content_cancel(&mut self, req_id: &str, _reason: &str) {
		// 只取消 Receiving；正在 finalize 的（Committing）保持占位并交给超时逻辑处理。
		if let Some(ReceiverState::Receiving {
			handle,
			range_handles,
			item_id,
			transfer_id,
			..
		}) = self.take_receiving(req_id)
		{
			// abort writer Task（TempFileGuard 删除 tmp 文件）与**全部**区间接收任务
			// （drop RecvStream → 对端收到 STOP_SENDING/RESET），不 await 有界 channel。
			handle.abort();
			for range_handle in range_handles.values() {
				range_handle.abort();
			}
			self.pending_payloads.remove(req_id);
			// 取消与数据流到达可能交叉：记一小段时间，迟到的数据流会被立即拒绝。
			self.record_cancelled_payload(req_id);

			let evt = serde_json::json!({
				"type": "TRANSFER_CANCELLED",
				"payload": {
					"transfer_id": transfer_id,
					"item_id": item_id,
					"state": "cancelled"
				}
			});
			self.sink.emit(evt.to_string());
		}
	}

	fn emit_transfer_failed(&self, tid: &str, code: &str, msg: &str) {
		// 记录传输失败日志
		self.log_sink.log_error(
			"Session",
			&format!("Transfer failed: transfer_id={tid}, error={msg}"),
			Some(&format!("传输失败: 传输ID={tid}，错误={msg}")),
			Some(msg),
		);

		// 根据文档规则硬编码属性
		let (retryable, affects_session) = match code {
			"CONN_TIMEOUT" => (true, true),
			_ => (false, false),
		};

		let payload = crate::model::CoreErrorPayload {
			code: code.to_string(),
			message: msg.to_string(),
			scope: "Transfer".to_string(),
			retryable,
			affects_session,
			detail: Some(serde_json::json!({ "transfer_id": tid })),
		};

		// 统一使用 TRANSFER_FAILED 或 CORE_ERROR
		let evt = serde_json::json!({
			"type": "TRANSFER_FAILED",
			"ts_ms": now_ms(),
			"payload": payload
		});
		self.sink.emit(evt.to_string());
	}

	// --- M3 Sender Logic ---
	async fn handle_content_get(
		&mut self,
		transfer_id: String,
		item_id: String,
		file_id: Option<String>,
		offset: Option<u64>,
	) -> Result<()> {
		// 出站策略：share_to_peer=false（或对端身份未知）时拒绝提供任何内容。
		// 这是内容外泄的最后一道闸门 —— 即使对端绕过 UI 直接发 ContentGet 也拿不到字节。
		if !self.outbound_share_allowed().await? {
			// 用 ContentDenied（非致命）而不是 Error：对端把任何 Error 都当作致命错误并断开
			// 会话，用户切换一次共享开关就会引发断连/退避/重连的连环抖动。
			let _ = self
				.send_ctrl(CtrlMsg::ContentDenied {
					req_id: transfer_id,
					code: "PEER_POLICY_DENIED".into(),
					message: Some("share_to_peer is disabled for this peer".into()),
				})
				.await;
			return Ok(());
		}

		// 出站并发上限（与入站 MAX_CONCURRENT_RECEIVERS 对称）：每个 ContentGet 都会
		// spawn 一个读文件任务并占用 fd，认证对端不能无限开上传。
		// 用 ContentDenied 而不是 Error：并发拒绝是非致命的，不应断开会话。
		if let Some(denial) = sender_admission(&self.senders, &transfer_id) {
			self.log_sink.log_warn(
				"Session",
				&format!(
					"Too many concurrent outbound transfers, rejected ContentGet: transfer_id={transfer_id}, item_id={item_id}, active={}",
					self.senders.len()
				),
				Some(&format!(
					"并发出站传输过多，已拒绝该内容请求: 传输ID={transfer_id}，项目ID={item_id}，当前并发={}",
					self.senders.len()
				)),
			);
			// 注意：此时绝不能先把条目插进 senders —— sender_admission 只做判定。
			let _ = self.send_ctrl(denial).await;
			return Ok(());
		}

		// [修改] 1. 查找文件路径 (补全了 CAS 和 Local Path 的双重查找)
		//
		// DB 查询与 `Path::exists` 都是同步阻塞操作，整体放进 spawn_blocking：
		// 大目录 / 慢磁盘 / SQLite busy 都不能占住 Tokio worker。错误传播保持原样
		// （`?` 把 DB / CAS 错误原样返回给 handle_control_msg）。
		let store = self.store.clone();
		let cas = self.cas.clone();
		let item_id_bg = item_id.clone();
		let file_id_bg = file_id.clone();
		let (file_path_res, mime_val) =
			tokio::task::spawn_blocking(move || -> Result<(Option<(PathBuf, String)>, String)> {
				let store = store.lock().unwrap();

				// A. 获取目标的 SHA256 和 (可选的) 本地路径
				//    如果是 FileList 子文件，查 files_json；如果是 Text/Image，查 item 主表
				let (target_sha, local_path_opt) = if let Some(fid) = &file_id_bg {
					// Case 1: FileList 中的子文件
					if let Some(fmeta) = store.get_file_meta(&item_id_bg, fid)? {
						(fmeta.sha256.unwrap_or_default(), fmeta.local_path)
					} else {
						(String::new(), None)
					}
				} else {
					// Case 2: Text/Image (Item 本身就是内容)
					let sha = store.get_item_sha256(&item_id_bg)?.unwrap_or_default();
					// Text/Image 通常没有 local_path，除非是极少数情况，这里暂定 None
					(sha, None)
				};

				// B. 获取 MIME (用于通知接收端)
				let mime = store
					.get_item_mime(&item_id_bg)?
					.unwrap_or("application/octet-stream".to_string());

				// C. 决定最终读取路径
				let mut final_path = None;

				// 优先级 1: 本地原始文件 (Local Path)
				// 场景: 用户刚复制了一个 2GB 视频，还没进 CAS，或者为了支持 tail/resuming
				if let Some(lp_str) = local_path_opt {
					let p = PathBuf::from(lp_str);
					if p.exists() {
						final_path = Some((p, target_sha.clone()));
					}
				}

				// 优先级 2: CAS Blob
				// 场景: 图片、文本、或者已经被归档的文件，以及 **测试用例** (测试通常只写 CAS)
				if final_path.is_none() && !target_sha.is_empty() {
					let blob_path = cas.blob_path(&target_sha)?;
					if blob_path.exists() {
						final_path = Some((blob_path, target_sha));
					}
				}

				Ok((final_path, mime))
			})
			.await??;

		if let Some((path, sha256)) = file_path_res {
			// 只取文件长度；真正读取在独立发送任务里完成（那里会重新打开文件）。
			let file = File::open(&path).await?;
			let meta = file.metadata().await?;
			let total_bytes = meta.len();
			drop(file);

			// 处理断点续传 offset（公开 API 目前固定 0；B2 的区间计划从该偏移
			// 连续覆盖到文件末尾）。
			let start_offset = offset.unwrap_or(0).min(total_bytes);
			// 冷启动区间计划：剩余 ≤16MiB 单流；LAN（srtt<20ms）只切 2 段以摊薄
			// 建流开销，WAN 切 6 段让控制器逐步加流（ADR-002 §3.3 / §4.2）。
			let ranges = initial_range_plan(start_offset, total_bytes, self.conn.stats().path.rtt);
			let range_count = ranges.len();

			// 控制面：ContentBegin 先于数据流发送，携带完整区间计划。接收端据此
			// 校验无空洞/无重叠/不超硬上限，并登记每条区间的槽位。
			self.send_ctrl(CtrlMsg::ContentBegin {
				req_id: transfer_id.clone(),
				item_id: item_id.clone(),
				file_id: file_id.clone(),
				total_bytes,
				sha256: sha256.clone(), // 注意：这是整个文件的 Hash
				mime: mime_val.clone(),
				ranges: ranges.clone(),
			})
			.await?;

			self.log_sink.log_info(
				"Session",
				&format!(
					"Outbound transfer started: transfer_id={transfer_id}, total_bytes={total_bytes}, start_offset={start_offset}, ranges={range_count}"
				),
				Some(&format!(
					"出站传输已开始: 传输ID={transfer_id}，字节数={total_bytes}，起点={start_offset}，区间数={range_count}"
				)),
			);

			let job = PayloadPlanJob {
				conn: self.conn.clone(),
				path,
				req_id: transfer_id.clone(),
				item_id,
				file_id,
				sha256,
				mime: mime_val,
				total_bytes,
				ranges,
				budget: self.payload_stream_budget.clone(),
				log_sink: self.log_sink.clone(),
			};
			let tx = self.upload_tx.clone();
			let tid = transfer_id.clone();

			let handle = tokio::spawn(async move {
				match drive_payload_plan(job).await {
					Ok(bytes) => {
						let _ = tx
							.send(UploadMsg::Done {
								transfer_id: tid,
								bytes,
							})
							.await;
					}
					Err((bytes, err)) => {
						let _ = tx
							.send(UploadMsg::Error {
								transfer_id: tid,
								bytes,
								err,
							})
							.await;
					}
				}
			});

			self.senders.insert(
				transfer_id,
				SenderState {
					handle: handle.abort_handle(),
					bytes_sent: 0,
					started_ts_ms: now_ms(),
				},
			);
		} else {
			// 找不到文件，发送错误
			self.send_ctrl(CtrlMsg::Error {
				reply_to: Some(transfer_id),
				code: "ITEM_NOT_FOUND".into(),
				message: Some("File content not found in CAS or local path".into()),
			})
			.await?;
		}
		Ok(())
	}

	async fn start_pull_request(
		&mut self,
		item_id: String,
		file_id: Option<String>,
		reply_tx: oneshot::Sender<Result<String>>,
	) -> Result<()> {
		let transfer_id = uuid::Uuid::new_v4().to_string();
		self.log_sink.log_info(
				"Session",
				&format!("Transfer request initiated: transfer_id={transfer_id}, item_id={item_id}, file_id={file_id:?}"),
				Some(&format!("传输请求已发起: 传输ID={transfer_id}，项目ID={item_id}，文件ID={file_id:?}")),
		);
		self.send_ctrl(CtrlMsg::ContentGet {
			msg_id: Some(transfer_id.clone()),
			item_id,
			file_id,
			offset: Some(0),
		})
		.await?;
		let _ = reply_tx.send(Ok(transfer_id));
		Ok(())
	}

	async fn handle_local_cancel(&mut self, transfer_id: String) -> Result<()> {
		// 1. 尝试作为 Sender 取消
		if let Some(state) = self.senders.remove(&transfer_id) {
			state.handle.abort(); // 杀掉读文件任务
			self.send_ctrl(CtrlMsg::ContentCancel {
				req_id: transfer_id.clone(),
				reason: "User cancelled".into(),
			})
			.await?;
			self.log_sink.log_info(
				"Session",
				&format!("Transfer cancelled: transfer_id={transfer_id}, reason=User cancelled"),
				Some(&format!("传输已取消: 传输ID={transfer_id}，原因=用户取消")),
			);
			return Ok(());
		}

		// 2. 尝试作为 Receiver 取消（finalize 中的 Committing 不在这里处理，
		//    由最终化任务的 30s 超时兜底，避免对端收到已结束传输的 Cancel）。
		if matches!(
			self.receivers.get(&transfer_id),
			Some(ReceiverState::Receiving { .. })
		) {
			self.send_ctrl(CtrlMsg::ContentCancel {
				req_id: transfer_id.clone(),
				reason: "User cancelled".into(),
			})
			.await?;
			self.log_sink.log_info(
				"Session",
				&format!("Transfer cancelled: transfer_id={transfer_id}, reason=User cancelled"),
				Some(&format!("传输已取消: 传输ID={transfer_id}，原因=用户取消")),
			);
			// 调用上面的处理函数清理资源
			self.handle_content_cancel(&transfer_id, "User cancelled");
		}
		Ok(())
	}

	// --- OPAQUE Core Logic (v3.0.0 Fixed) ---
	// (Existing Opaque methods omitted for brevity as they are unchanged from previous context)
	// Please ensure start_opaque_login, handle_opaque_response, handle_opaque_start,
	// handle_opaque_finish, perform_tofu_check_async, transition_to_online, tick_heartbeat
	// are kept exactly as they were in the input file.

	// 【并发审计修复】OPAQUE 的 Argon2id(19MiB, t=2) 绝不能跑在 tokio worker 上：
	// 实测单次客户端登录的 KSF 约 40~60ms；服务端首次登录（`p2p_get_server_registration`
	// 现算注册记录，之后命中 crypto.rs 的缓存）约 80~120ms。这些纯 CPU 调用过去直接
	// 落在 actor 的 select 主循环里，会卡住复用同一 worker 的其他会话任务 —— 重连风暴时
	// 互相拖慢，甚至把心跳拖到超时。现在四个 OPAQUE 阶段的 CPU 部分全部丢给
	// `spawn_blocking`，async 侧只保留状态机推进与发送，线格式/状态机完全不变。
	//
	// 注意：真正的 Argon2 在 `ClientLogin::finish`（`get_password_derived_key`）与
	// `handle_opaque_start` 的注册流程里；`ClientLogin::start`/`ServerLogin::start`
	// 本身较轻，但为了让四步共用同一条"CPU 在阻塞池"的边界也一并包进去。
	async fn start_opaque_login(&mut self) -> Result<()> {
		// 只跨线程传 owned 数据：密码 String 进，ClientLogin 状态 + 已序列化 KE1 出。
		let password = self.config.account_password.clone();
		let (state, payload) = tokio::task::spawn_blocking(move || {
			let mut rng = OsRng;
			let start_result = CbClientLogin::start(&mut rng, password.as_bytes())
				.map_err(|e| anyhow::anyhow!("OPAQUE start failed: {e:?}"))?;
			let payload = opaque_encode(&start_result.message)?;
			Ok::<_, anyhow::Error>((start_result.state, payload))
		})
		.await
		.context("OPAQUE client start blocking task failed")??;
		self.opaque_client_state = Some(state);
		self.send_ctrl(CtrlMsg::OpaqueStart {
			msg_id: Some(uuid::Uuid::new_v4().to_string()),
			reply_to: None,
			opaque: payload,
		})
		.await?;
		self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueResponse));
		Ok(())
	}

	async fn handle_opaque_response(&mut self, response_bytes: &[u8]) -> Result<()> {
		let client_state = self
			.opaque_client_state
			.take()
			.ok_or_else(|| anyhow::anyhow!("Protocol error: Missing client state"))?;
		// KSF 就在这里的 `finish` 里：状态/密码/服务端响应移进阻塞池，
		// 只把序列化好的 KE3 字节带回来（session_key 在闭包内用完即 drop）。
		let password = self.config.account_password.clone();
		let server_response: opaque_ke::CredentialResponse<DefaultCipherSuite> =
			opaque_decode(response_bytes)
				.map_err(|_| anyhow::anyhow!("Invalid OpaqueResponse bytes"))?;
		let payload = tokio::task::spawn_blocking(move || {
			let mut rng = OsRng;
			let finish_result = client_state
				.finish(
					&mut rng,
					password.as_bytes(),
					server_response,
					ClientLoginFinishParameters::default(),
				)
				.map_err(|e| anyhow::anyhow!("OPAQUE finish failed: {e:?}"))?;
			opaque_encode(&finish_result.message)
		})
		.await
		.context("OPAQUE client finish blocking task failed")??;
		self.send_ctrl(CtrlMsg::OpaqueFinish {
			msg_id: Some(uuid::Uuid::new_v4().to_string()),
			reply_to: None,
			opaque: payload,
		})
		.await?;
		self.update_state(SessionState::Handshaking(HandshakeStep::WaitingAuthOk));
		Ok(())
	}

	async fn handle_opaque_start(&mut self, start_bytes: &[u8]) -> Result<()> {
		// 首次登录的 80~120ms 主要花在 `p2p_get_server_registration`（内部跑 Argon2id）；
		// 它和 `ServerLogin::start` 一起放进同一个阻塞任务，闭包内完成反序列化与 KE2 编码。
		let password = self.config.account_password.clone();
		let start_bytes = start_bytes.to_vec();
		let (state, payload) = tokio::task::spawn_blocking(move || {
			let identifier = b"clipbridge-user";
			let (server_setup, server_rec) = p2p_get_server_registration(&password)?;
			let client_message: opaque_ke::CredentialRequest<DefaultCipherSuite> =
				opaque_decode(&start_bytes)
					.map_err(|_| anyhow::anyhow!("Invalid OpaqueStart bytes"))?;
			let mut rng = OsRng;
			let start_result = CbServerLogin::start(
				&mut rng,
				&server_setup,
				Some(server_rec),
				client_message,
				identifier,
				ServerLoginParameters::default(),
			)
			.map_err(|e| anyhow::anyhow!("OPAQUE server start failed: {e:?}"))?;
			let payload = opaque_encode(&start_result.message)?;
			Ok::<_, anyhow::Error>((start_result.state, payload))
		})
		.await
		.context("OPAQUE server start blocking task failed")??;
		self.opaque_server_state = Some(state);
		self.send_ctrl(CtrlMsg::OpaqueResponse {
			msg_id: Some(uuid::Uuid::new_v4().to_string()),
			reply_to: None,
			opaque: payload,
		})
		.await?;
		self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueFinish));
		Ok(())
	}

	async fn handle_opaque_finish(&mut self, finish_bytes: &[u8]) -> Result<()> {
		let server_state = self
			.opaque_server_state
			.take()
			.ok_or_else(|| anyhow::anyhow!("Protocol error: Missing server state"))?;
		// 服务端 finish 不跑 KSF，但仍包含 HMAC/群运算；同样移出 worker。
		// 会话密钥在闭包内生成并 drop，不跨线程。
		let finish_bytes = finish_bytes.to_vec();
		tokio::task::spawn_blocking(move || -> Result<()> {
			let client_message: opaque_ke::CredentialFinalization<DefaultCipherSuite> =
				opaque_decode(&finish_bytes)
					.map_err(|_| anyhow::anyhow!("Invalid OpaqueFinish bytes"))?;
			let _session_key = server_state
				.finish(client_message, ServerLoginParameters::default())
				.map_err(|e| anyhow::anyhow!("Authentication failed: {e:?}"))?;
			Ok(())
		})
		.await
		.context("OPAQUE server finish blocking task failed")??;
		self.update_state(SessionState::AccountVerified);
		Ok(())
	}

	async fn perform_tofu_check_async(&self) -> Result<()> {
		let data_dir = self.config.data_dir.clone();
		let uid = self.config.account_uid.clone();
		let did = self
			.remote_device_id
			.clone()
			.context("missing remote device id")?;
		let rfp = self.remote_fingerprint.clone();
		tokio::task::spawn_blocking(move || {
			let store = Store::open(&data_dir)?;
			if let Some(saved_fp) = store.get_peer_fingerprint(&uid, &did)? {
				if saved_fp != rfp {
					anyhow::bail!("TLS_PIN_MISMATCH: saved={saved_fp}, got={rfp}");
				}
			} else {
				let mut store_mut = Store::open(&data_dir)?;
				store_mut.save_peer_fingerprint(&uid, &did, &rfp, now_ms())?;
				println!("[Session] TOFU pinned device {did} with fp {rfp}");
			}
			Ok(())
		})
		.await?
	}

	fn transition_to_online(&mut self) {
		self.update_state(SessionState::Online);
		if let Some(did) = &self.remote_device_id {
			self.log_sink.log_info(
				"Session",
				&format!("Handshake completed, session online: remote_device_id={did}"),
				Some(&format!("握手已完成，会话在线: 远程设备ID={did}")),
			);
			// 日志只入队（非阻塞），随后触发回调，不存在持锁重入问题
			let json = serde_json::json!({
				"type": "PEER_ONLINE",
				"ts_ms": now_ms(),
				"payload": { "device_id": did }
			});
			self.sink.emit(json.to_string());
		}
	}

	async fn tick_heartbeat(&mut self) -> Result<()> {
		if now_ms() - self.last_active_at > HEARTBEAT_TIMEOUT.as_millis() as i64 {
			if let Some(did) = &self.remote_device_id {
				self.log_sink.log_warn(
					"Session",
					&format!(
						"Heartbeat timeout, session may be dead: device_id={did}, last_active={}",
						self.last_active_at
					),
					Some(&format!(
						"心跳超时，会话可能已死亡: 设备ID={did}，最后活跃={}",
						self.last_active_at
					)),
				);
			}
			let _ = self
				.send_ctrl(CtrlMsg::Error {
					reply_to: None,
					code: "TIMEOUT".into(),
					message: Some("Heartbeat timeout".into()),
				})
				.await;
			anyhow::bail!("Heartbeat timeout");
		}
		if self.state == SessionState::Online {
			// 心跳发送（Debug 级别，避免日志过多）
			if let Some(did) = &self.remote_device_id {
				self.log_sink.log_debug(
					"Session",
					&format!("Heartbeat sent: device_id={did}"),
					Some(&format!("心跳已发送: 设备ID={did}")),
				);
			}
			self.send_ctrl(CtrlMsg::Ping {
				msg_id: Some(uuid::Uuid::new_v4().to_string()),
				ts: now_ms(),
			})
			.await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod opaque_wire_tests {
	use super::{opaque_decode, opaque_encode};
	use crate::crypto::{
		p2p_get_server_registration, CbClientLogin, CbServerLogin, DefaultCipherSuite,
	};
	use opaque_ke::{ClientLoginFinishParameters, ServerLoginParameters};
	use rand::rngs::OsRng;

	/// 走序列化往返跑完整 OPAQUE 握手（ke1/ke2/ke3 都编码成字节再解回来），
	/// 证明新的 postcard 编码能正确承载 opaque-ke 的消息类型 —— 而不只是内存里传递。
	/// 这是 bincode 1.3 → 2.0.1 → postcard 两次迁移后，唯一直接覆盖该线路径的测试。
	#[test]
	fn opaque_handshake_through_serialization() {
		let password = "user_secret_key_123456";
		let (server_setup, server_rec) = p2p_get_server_registration(password).unwrap();

		let mut client_rng = OsRng;
		let client_start = CbClientLogin::start(&mut client_rng, password.as_bytes()).unwrap();

		// ke1: client → server
		let ke1_bytes = opaque_encode(&client_start.message).unwrap();
		let ke1: opaque_ke::CredentialRequest<DefaultCipherSuite> =
			opaque_decode(&ke1_bytes).unwrap();

		let mut server_rng = OsRng;
		let server_start = CbServerLogin::start(
			&mut server_rng,
			&server_setup,
			Some(server_rec),
			ke1,
			b"clipbridge-user",
			ServerLoginParameters::default(),
		)
		.expect("Server failed to start session");

		// ke2: server → client
		let ke2_bytes = opaque_encode(&server_start.message).unwrap();
		let ke2: opaque_ke::CredentialResponse<DefaultCipherSuite> =
			opaque_decode(&ke2_bytes).unwrap();

		let client_finish = client_start
			.state
			.finish(
				&mut client_rng,
				password.as_bytes(),
				ke2,
				ClientLoginFinishParameters::default(),
			)
			.expect("Client failed to finish session");

		// ke3: client → server
		let ke3_bytes = opaque_encode(&client_finish.message).unwrap();
		let ke3: opaque_ke::CredentialFinalization<DefaultCipherSuite> =
			opaque_decode(&ke3_bytes).unwrap();

		let server_finish = server_start
			.state
			.finish(ke3, ServerLoginParameters::default())
			.expect("Server failed to verify client");

		assert_eq!(client_finish.session_key, server_finish.session_key);

		// 同一消息重复编码必须字节稳定（否则对端校验/幂等会出问题）
		assert_eq!(
			ke1_bytes,
			opaque_encode(
				&opaque_decode::<opaque_ke::CredentialRequest<DefaultCipherSuite>>(&ke1_bytes)
					.unwrap()
			)
			.unwrap()
		);
	}

	/// 覆盖 Argon2 迁移后的**线程边界**（不需要 socket）：四个 OPAQUE 阶段都按 actor 的
	/// 做法把 CPU 部分放进 `spawn_blocking`，让 `ClientLogin`/`ServerLogin` 状态跨线程
	/// 移入、序列化消息移出。加密数学本身由 crypto.rs 的测试覆盖，这里只验证
	/// "状态是 Send、换线程后握手仍能走完"—— 正是本次改动唯一可能出错的地方。
	#[tokio::test]
	async fn opaque_login_phases_survive_spawn_blocking() {
		use opaque_ke::{CredentialFinalization, CredentialRequest, CredentialResponse};

		let password = "user_secret_key_123456";
		let caller_thread = std::thread::current().id();

		// 阶段 1：客户端 start（状态跨线程回到 async 侧 + KE1 字节）。
		let password_owned = password.to_string();
		let (client_state, ke1_bytes) = tokio::task::spawn_blocking(move || {
			let mut rng = OsRng;
			let start = CbClientLogin::start(&mut rng, password_owned.as_bytes()).unwrap();
			(start.state, opaque_encode(&start.message).unwrap())
		})
		.await
		.expect("client start blocking task must not panic");

		let ke1: CredentialRequest<DefaultCipherSuite> = opaque_decode(&ke1_bytes).unwrap();

		// 阶段 2：服务端 start（首次登录的注册 Argon2 + KE2 全部留在阻塞池里）。
		let password_owned = password.to_string();
		let (server_state, ke2_bytes) = tokio::task::spawn_blocking(move || {
			let (setup, rec) = p2p_get_server_registration(&password_owned).unwrap();
			let mut rng = OsRng;
			let start = CbServerLogin::start(
				&mut rng,
				&setup,
				Some(rec),
				ke1,
				b"clipbridge-user",
				ServerLoginParameters::default(),
			)
			.unwrap();
			(start.state, opaque_encode(&start.message).unwrap())
		})
		.await
		.expect("server start blocking task must not panic");

		let ke2: CredentialResponse<DefaultCipherSuite> = opaque_decode(&ke2_bytes).unwrap();

		// 阶段 3：客户端 finish（真正的 Argon2id KSF）——状态移入，只有 KE3 出来。
		let password_owned = password.to_string();
		let ke3_bytes = tokio::task::spawn_blocking(move || {
			// 阻塞池线程 ≠ 调用方 async 线程：这是 KSF 已离开 tokio worker 的直接证据。
			assert_ne!(std::thread::current().id(), caller_thread);
			let mut rng = OsRng;
			let finish = client_state
				.finish(
					&mut rng,
					password_owned.as_bytes(),
					ke2,
					ClientLoginFinishParameters::default(),
				)
				.unwrap();
			opaque_encode(&finish.message).unwrap()
		})
		.await
		.expect("client finish blocking task must not panic");

		// 阶段 4：服务端 finish —— 状态移入，会话密钥在闭包内 drop。
		let ke3: CredentialFinalization<DefaultCipherSuite> = opaque_decode(&ke3_bytes).unwrap();
		tokio::task::spawn_blocking(move || {
			server_state
				.finish(ke3, ServerLoginParameters::default())
				.expect("server must verify the client across the blocking boundary");
		})
		.await
		.expect("server finish blocking task must not panic");
	}
}

#[cfg(test)]
mod auth_gate_tests {
	use super::{ctrl_is_pre_auth_allowed, ctrl_msg_kind};
	use crate::proto::{AuthSessionFlags, CtrlMsg, TransferRange, PROTOCOL_VERSION};

	/// 未完成 OPAQUE 之前，任何业务消息都必须被认证闸门拒绝。
	#[test]
	fn business_messages_are_not_allowed_before_auth() {
		let business = [
			CtrlMsg::ItemDelete {
				msg_id: None,
				item_id: "item-1".into(),
			},
			CtrlMsg::ContentGet {
				msg_id: Some("t-1".into()),
				item_id: "item-1".into(),
				file_id: None,
				offset: None,
			},
			CtrlMsg::ContentBegin {
				req_id: "t-1".into(),
				item_id: "item-1".into(),
				file_id: None,
				total_bytes: 1,
				sha256: "00".into(),
				mime: "text/plain".into(),
				ranges: vec![TransferRange {
					offset: 0,
					length: 1,
				}],
			},
			CtrlMsg::ContentEnd {
				req_id: "t-1".into(),
			},
			CtrlMsg::ContentCancel {
				req_id: "t-1".into(),
				reason: "test".into(),
			},
			// 策略拒绝属于业务面：未认证时同样必须被认证闸门拦下
			CtrlMsg::ContentDenied {
				req_id: "t-1".into(),
				code: "PEER_POLICY_DENIED".into(),
				message: None,
			},
		];
		for msg in business {
			assert!(
				!ctrl_is_pre_auth_allowed(&msg),
				"{} 必须在认证前被拒绝",
				ctrl_msg_kind(&msg)
			);
		}
	}

	/// 握手/协议控制消息在认证前必须放行，否则 OPAQUE 流程无法完成。
	#[test]
	fn handshake_messages_are_allowed_before_auth() {
		let handshake = [
			CtrlMsg::Hello {
				msg_id: None,
				protocol_version: PROTOCOL_VERSION,
				device_id: "dev".into(),
				account_uid: "uid".into(),
				capabilities: Vec::new(),
				client_nonce: None,
			},
			CtrlMsg::HelloAck {
				reply_to: None,
				server_device_id: "srv".into(),
				protocol_version: PROTOCOL_VERSION,
			},
			CtrlMsg::OpaqueStart {
				msg_id: None,
				reply_to: None,
				opaque: Vec::new(),
			},
			CtrlMsg::OpaqueResponse {
				msg_id: None,
				reply_to: None,
				opaque: Vec::new(),
			},
			CtrlMsg::OpaqueFinish {
				msg_id: None,
				reply_to: None,
				opaque: Vec::new(),
			},
			CtrlMsg::AuthOk {
				reply_to: None,
				session_flags: AuthSessionFlags {
					account_verified: true,
				},
			},
			CtrlMsg::AuthFail {
				reply_to: None,
				code: "test".into(),
			},
			CtrlMsg::Ping {
				msg_id: None,
				ts: 0,
			},
			CtrlMsg::Pong {
				reply_to: None,
				ts: 0,
			},
			CtrlMsg::Error {
				reply_to: None,
				code: "test".into(),
				message: None,
			},
			CtrlMsg::Close {
				msg_id: None,
				reason: "test".into(),
			},
		];
		for msg in handshake {
			assert!(
				ctrl_is_pre_auth_allowed(&msg),
				"{} 必须允许在认证前处理",
				ctrl_msg_kind(&msg)
			);
		}
	}
}

#[cfg(test)]
mod transfer_guard_tests {
	//! 入站限额 / 会话终止守护的单元测试。
	//!
	//! 这些用例都不需要真实 socket：限额判定与终止守护都是纯函数 / RAII，
	//! 可以独立于 QUIC 连接断言；需要真实连接的行为仍由 `session::tests` 里的
	//! `#[ignore]` 网络用例覆盖。
	use super::{
		chunk_overrun, chunk_send_timeout, content_end_route, decide_stream_count, hard_cap_bytes,
		initial_range_plan, kind_from_mime, mark_item_content_cached, plan_ranges,
		receiver_idle_expired, receiver_slot_available, route_payload_stream, route_range_header,
		sender_admission, sender_slot_available, stream_probe_interval, validate_range_plan,
		ContentEndRoute, PayloadRoute, PushOutcome, RangeRoute, RangeSlotState, ReceiverInbox,
		ReceiverRanges, ReceiverTaskMsg, SenderState, SessionActor, StreamControllerInput,
		StreamDecisionReason, TerminatedGuard, CHUNK_SEND_TIMEOUT_CAP, HEARTBEAT_TIMEOUT,
		LAN_PROBE_SLICE_MAX_BYTES, MAX_CONCURRENT_RECEIVERS, MAX_CONCURRENT_SENDERS,
		MAX_INFLIGHT_BYTES_PER_RECEIVER, MAX_PAYLOAD_STREAMS_PER_TRANSFER,
		PROBE_MIN_REMAINING_BYTES, RECEIVER_IDLE_TIMEOUT_MS, UPLOAD_CHUNK_BYTES,
	};
	use crate::model::{FileMeta, ItemContent, ItemKind, ItemMeta};
	use crate::policy::SizeLimits;
	use crate::proto::{CtrlMsg, TransferHeader, TransferRange};
	use crate::session::{HandshakeStep, SessionHandle, SessionState};
	use crate::store::Store;
	use crate::util::now_ms;
	use std::collections::HashMap;
	use std::sync::{Arc, Mutex};
	use std::time::Duration;
	use tokio::sync::{mpsc, Semaphore};

	/// 接收方可写字节的上限 = min(声明值, 硬上限)：多出 1 字节就必须整条传输失败。
	#[test]
	fn receiver_rejects_chunk_beyond_declared_size() {
		let declared = 100 * 1024u64;
		let hard = 10 * 1024 * 1024u64;

		// 刚好等于声明值是允许的
		assert_eq!(chunk_overrun(0, 64 * 1024, declared, hard), None);
		assert_eq!(chunk_overrun(64 * 1024, 36 * 1024, declared, hard), None);

		// 越界 1 字节即失败
		assert_eq!(
			chunk_overrun(64 * 1024, 36 * 1024 + 1, declared, hard),
			Some("TRANSFER_OVERRUN")
		);
		// 已经收满后继续发也要失败
		assert_eq!(
			chunk_overrun(declared, 1, declared, hard),
			Some("TRANSFER_OVERRUN")
		);
	}

	/// 即使对端在 Begin 里声明了超大 total_bytes（或配置在运行期变化），
	/// 逐块累计也绝不能越过 size_limits 的硬上限。
	#[test]
	fn receiver_rejects_chunk_beyond_hard_cap() {
		let declared = 1024 * 1024u64;
		let hard = 200 * 1024u64;
		assert_eq!(
			chunk_overrun(0, 300 * 1024, declared, hard),
			Some("ITEM_TOO_LARGE")
		);
	}

	/// 第 9 个并发接收必须被拒绝（每个 receiver 会占 channel/task/fd）。
	#[test]
	fn concurrent_receiver_cap_rejects_ninth() {
		assert_eq!(MAX_CONCURRENT_RECEIVERS, 8);
		for i in 0..MAX_CONCURRENT_RECEIVERS {
			assert!(
				receiver_slot_available(i),
				"已存在 {i} 个 receiver 时应允许新传输"
			);
		}
		assert!(
			!receiver_slot_available(MAX_CONCURRENT_RECEIVERS),
			"第 9 个并发传输必须被拒绝"
		);
		assert!(!receiver_slot_available(MAX_CONCURRENT_RECEIVERS + 1));
	}

	/// 空闲超时：无数据超过 30s 才回收（边界上不误杀）。
	#[test]
	fn idle_receiver_expires_only_after_timeout() {
		let now = 1_000_000i64;
		assert!(!receiver_idle_expired(now, now));
		assert!(!receiver_idle_expired(now, now - RECEIVER_IDLE_TIMEOUT_MS));
		assert!(receiver_idle_expired(
			now,
			now - RECEIVER_IDLE_TIMEOUT_MS - 1
		));
	}

	/// 数据块发送超时按分块大小缩放，而不是固定 500ms。
	///
	/// 旧版 64 KiB 分块 + 500ms ⇒ 128 KiB/s 吞吐下限；分块放大到 256 KiB 后必须给 2s
	/// 才能保住同一下限，否则移动网络 / 弱 Wi-Fi / 小窗口 WAN 会被误判为拥塞而中止。
	#[test]
	fn chunk_send_timeout_scales_with_chunk_size_and_stays_below_heartbeat() {
		// 64 KiB：与旧版一致，500ms（"≥500ms" 的保底语义）。
		assert!(chunk_send_timeout(64 * 1024) >= Duration::from_millis(500));
		assert_eq!(chunk_send_timeout(64 * 1024), Duration::from_millis(500));
		// 空分块 / 不足一个步进也保底 500ms。
		assert_eq!(chunk_send_timeout(0), Duration::from_millis(500));
		assert_eq!(chunk_send_timeout(1), Duration::from_millis(500));

		// 128 KiB → 1s；256 KiB（UPLOAD_CHUNK_BYTES）→ 2s，即 128 KiB/s，与旧版相同。
		assert_eq!(chunk_send_timeout(128 * 1024), Duration::from_millis(1000));
		assert_eq!(
			chunk_send_timeout(UPLOAD_CHUNK_BYTES),
			Duration::from_secs(2)
		);

		// 超大分块（含协议允许的 10 MiB 单帧、以及极端 usize 值）封顶。
		assert_eq!(chunk_send_timeout(10 * 1024 * 1024), CHUNK_SEND_TIMEOUT_CAP);
		assert_eq!(chunk_send_timeout(usize::MAX), CHUNK_SEND_TIMEOUT_CAP);

		// 封顶必须严格低于心跳超时（当前 6s），且至少留 1s 余量：
		// chunk 发送卡住时不能把心跳拖到超时，否则会误判整条会话死亡。
		assert!(CHUNK_SEND_TIMEOUT_CAP < HEARTBEAT_TIMEOUT);
		assert!(CHUNK_SEND_TIMEOUT_CAP + Duration::from_secs(1) <= HEARTBEAT_TIMEOUT);
	}

	/// 硬上限取自 policy.rs 的 size_limits，按 item 类型选择。
	#[test]
	fn hard_cap_uses_size_limits_by_kind() {
		let limits = SizeLimits::default();
		assert_eq!(
			hard_cap_bytes(&ItemKind::Text, &limits),
			limits.hard_text_bytes as u64
		);
		assert_eq!(
			hard_cap_bytes(&ItemKind::Image, &limits),
			limits.hard_image_bytes as u64
		);
		assert_eq!(
			hard_cap_bytes(&ItemKind::FileList, &limits),
			limits.hard_file_total_bytes as u64
		);

		// 负值配置按 0 处理（拒绝一切），绝不因为配置错误而放行
		let negative = SizeLimits {
			hard_text_bytes: -1,
			..SizeLimits::default()
		};
		assert_eq!(hard_cap_bytes(&ItemKind::Text, &negative), 0);
	}

	#[test]
	fn mime_classification_matches_item_kind() {
		assert_eq!(kind_from_mime("text/plain"), ItemKind::Text);
		assert_eq!(kind_from_mime("image/png"), ItemKind::Image);
		// 未知 MIME 按文件处理（file 上限最大，但不能因此绕过 text/image 的检查）
		assert_eq!(
			kind_from_mime("application/octet-stream"),
			ItemKind::FileList
		);
	}

	/// 模拟 `run_actor` 在 `open_bi` / `start_handshake` 处以 `?` 提前失败：
	/// 守护必须把状态写成 Terminated，否则该会话句柄永远不会被回收。
	async fn simulate_start_failure(state_ref: Arc<Mutex<SessionState>>) -> anyhow::Result<()> {
		let _guard = TerminatedGuard::new(state_ref);
		// 模拟真实路径上的第一个 await 点（conn.open_bi() / start_handshake()）
		tokio::task::yield_now().await;
		Err(anyhow::anyhow!("simulated open_bi/start_handshake failure"))
	}

	#[tokio::test]
	async fn terminated_guard_sets_terminated_on_early_return() {
		let state_ref = Arc::new(Mutex::new(SessionState::TransportReady));
		let result = simulate_start_failure(state_ref.clone()).await;
		assert!(result.is_err());
		assert_eq!(*state_ref.lock().unwrap(), SessionState::Terminated);
	}

	#[tokio::test]
	async fn terminated_guard_sets_terminated_on_normal_exit() {
		let state_ref = Arc::new(Mutex::new(SessionState::Handshaking(
			HandshakeStep::WaitingForHello,
		)));
		{
			let _guard = TerminatedGuard::new(state_ref.clone());
		}
		assert_eq!(*state_ref.lock().unwrap(), SessionState::Terminated);
	}

	/// 任务 panic 同样必须落 Terminated（Drop 在 unwind 时执行）。
	#[tokio::test]
	async fn terminated_guard_sets_terminated_on_panic() {
		let state_ref = Arc::new(Mutex::new(SessionState::Handshaking(
			HandshakeStep::SendingHello,
		)));
		let guard_state = state_ref.clone();
		let join = tokio::spawn(async move {
			let _guard = TerminatedGuard::new(guard_state);
			panic!("simulated actor panic");
		});
		assert!(join.await.is_err(), "任务应当以 panic 结束");
		assert_eq!(*state_ref.lock().unwrap(), SessionState::Terminated);
	}

	/// net.rs 的防御网：actor 任务结束后 `cmd_tx.is_closed()` 必须为 true，
	/// 即使状态没来得及写成 Terminated 也能被回收。
	#[test]
	fn closed_cmd_tx_identifies_dead_session() {
		let (cmd_tx, cmd_rx) = mpsc::channel(1);
		let handle = SessionHandle {
			initial_id: "dev-1".to_string(),
			peer_id: Arc::new(Mutex::new(Some("dev-1".to_string()))),
			state: Arc::new(Mutex::new(SessionState::TransportReady)),
			cmd_tx,
		};

		assert!(!handle.is_finished());
		assert!(!handle.cmd_tx.is_closed());

		// 模拟 actor 任务结束（持有 receiver 的任务被 drop）
		drop(cmd_rx);
		assert!(handle.cmd_tx.is_closed());
	}

	/// FileList 传输时必须按 item 自身的 content.sha256 标记缓存；
	/// 用单文件的传输 hash 去 UPDATE 会命中 0 行（历史 bug）。
	#[test]
	fn content_end_marks_item_sha_not_transfer_sha() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();

		let item_sha = "a".repeat(64);
		let file_sha = "b".repeat(64);
		let meta = ItemMeta {
			ty: "ItemMeta".to_string(),
			item_id: "item-filelist".to_string(),
			kind: ItemKind::FileList,
			created_ts_ms: now_ms(),
			source_device_id: "peer-1".to_string(),
			source_device_name: None,
			size_bytes: 16,
			preview: crate::model::ItemPreview::default(),
			content: ItemContent {
				mime: "application/x-clipbridge-files".to_string(),
				sha256: item_sha.clone(),
				total_bytes: 16,
			},
			files: vec![FileMeta {
				file_id: "file-1".to_string(),
				rel_name: "a.bin".to_string(),
				size_bytes: 16,
				sha256: Some(file_sha.clone()),
				local_path: None,
			}],
			expires_ts_ms: None,
		};
		store
			.insert_remote_item("test_uid", &meta, now_ms())
			.unwrap();
		assert_eq!(store.cache_row_count_for_sha(&item_sha).unwrap(), 1);

		// ContentEnd 侧传入的是「本次传输片段 hash」（FileList 里 = file_sha）
		let (marked_sha, marked_rows) =
			mark_item_content_cached(&mut store, &meta.item_id, &file_sha, now_ms(), true).unwrap();

		assert_eq!(marked_sha, item_sha);
		assert_eq!(marked_rows, 1, "必须命中 item content sha 对应的行");
		assert!(store.get_cache_present(&item_sha).unwrap());
		assert_eq!(
			store.cache_row_count_for_sha(&file_sha).unwrap(),
			0,
			"文件 hash 不应被当作缓存键写入"
		);
	}

	/// 无 item 行时回退到传输 hash，且 0 行不会让调用方报错（只记警告）。
	#[test]
	fn mark_item_content_cached_falls_back_and_reports_zero_rows() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();
		let missing_sha = "c".repeat(64);
		let (marked_sha, rows) =
			mark_item_content_cached(&mut store, "item-missing", &missing_sha, now_ms(), true)
				.unwrap();
		assert_eq!(marked_sha, missing_sha);
		assert_eq!(rows, 0);
	}

	/// 接收完成路径的 durable 写入：大文件 pending 时必须与 present=1 一起写 0，
	/// 后台同步（或 sweep）成功后由 `mark_cache_durable` 翻回 1。
	#[test]
	fn receiver_completion_can_mark_durability_pending() {
		let dir = tempfile::tempdir().unwrap();
		let mut store = Store::open(dir.path()).unwrap();
		let sha = "9".repeat(64);
		store
			.conn
			.execute(
				r"INSERT INTO content_cache
                   (sha256_hex, total_bytes, present, last_access_ts_ms, created_ts_ms)
                   VALUES (?1, 10, 0, 1, 1)",
				rusqlite::params![sha],
			)
			.unwrap();

		// 大文件路径：present=1 + durable=0 同一条 UPDATE。
		let (marked, rows) =
			mark_item_content_cached(&mut store, "no-item", &sha, 2, false).expect("mark pending");
		assert_eq!(marked, sha);
		assert_eq!(rows, 1);
		assert!(store.get_cache_present(&sha).unwrap());
		assert!(!store.get_cache_durable(&sha).unwrap());
		assert_eq!(
			store.select_durability_pending(10).unwrap(),
			vec![sha.clone()]
		);

		// 后台 sync_data 成功后的写回。
		assert_eq!(store.mark_cache_durable(&sha).unwrap(), 1);
		assert!(store.get_cache_durable(&sha).unwrap());
		assert_eq!(
			store.select_durability_pending(10).unwrap(),
			Vec::<String>::new()
		);
	}

	/// writer channel 满（慢磁盘 / 慢 writer）时，actor 的数据帧分支必须立刻返回，
	/// 且同一轮 select 里的控制帧（Ping/ItemMeta）仍能被处理。
	///
	/// 旧实现是 `tx.send(chunk).await`：writer 一慢，actor 主循环就停在这里，
	/// 心跳、命令和其他传输全部被队头阻塞。现在 `try_push` 是同步的：
	/// channel 满 → 转入 per-transfer pending，并返回 `Paused`。
	#[tokio::test]
	async fn full_writer_channel_does_not_stall_actor() {
		const CHUNK: usize = 64 * 1024;

		// channel 只有 1 个槽位，字节预算充足：第二个块撞的是"槽位满"。
		let (tx, mut rx) = mpsc::channel::<ReceiverTaskMsg>(1);
		let budget = Arc::new(Semaphore::new(MAX_INFLIGHT_BYTES_PER_RECEIVER));
		let mut inbox = ReceiverInbox::new(tx, budget, 4096);

		assert_eq!(
			inbox.try_push(bytes::Bytes::from(vec![0u8; CHUNK])),
			PushOutcome::Queued
		);
		// 槽位已满：块转入 pending，paused=true；try_push 本身是同步的，不会等 writer。
		assert_eq!(
			inbox.try_push(bytes::Bytes::from(vec![0u8; CHUNK])),
			PushOutcome::Paused
		);
		assert!(inbox.paused);

		// 模拟 actor 循环的一轮：数据帧下发（同步）+ 控制帧处理。
		// 50ms 超时证明下发路径没有阻塞在 writer 上。
		let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<&'static str>(1);
		ctrl_tx.send("Ping").await.unwrap();
		let start = std::time::Instant::now();
		let (outcome, ping) = tokio::time::timeout(Duration::from_millis(50), async {
			let outcome = inbox.try_push(bytes::Bytes::from(vec![0u8; CHUNK]));
			let ping = ctrl_rx.recv().await.expect("control message");
			(outcome, ping)
		})
		.await
		.expect("actor loop must not stall on a full writer channel");
		assert_eq!(outcome, PushOutcome::Paused);
		assert_eq!(ping, "Ping");
		assert!(
			start.elapsed() < Duration::from_millis(50),
			"数据帧下发不能等待 writer"
		);

		// writer 排空一个块后：pending 只能按 channel 容量逐块补发（仍是同步调用，不阻塞）。
		drop(rx.recv().await.expect("writer chunk")); // 排空 chunk1
		assert_eq!(inbox.flush_pending(), PushOutcome::Paused);
		drop(rx.recv().await.expect("writer chunk")); // 排空 chunk2
		assert_eq!(inbox.flush_pending(), PushOutcome::Queued);
		assert!(!inbox.paused);
		// 再排空 channel 后，新块直接入队。
		drop(rx.recv().await.expect("writer chunk")); // 排空 chunk3
		assert_eq!(
			inbox.try_push(bytes::Bytes::from(vec![0u8; CHUNK])),
			PushOutcome::Queued
		);
	}

	/// 出站并发闸门：`senders` 已满时，新的 ContentGet 走非致命 ContentDenied 拒绝，
	/// 且拒绝路径不会把条目插进 `senders`（第 9 个上传不 spawn、不占 fd）。
	#[tokio::test]
	async fn outbound_cap_refuses_content_get_without_growing_senders() {
		assert_eq!(MAX_CONCURRENT_SENDERS, MAX_CONCURRENT_RECEIVERS);

		let mut senders: HashMap<String, SenderState> = HashMap::new();
		for i in 0..MAX_CONCURRENT_SENDERS {
			// 结束的占位任务只用于提供同类型的 AbortHandle。
			let task = tokio::spawn(async {});
			senders.insert(
				format!("transfer-{i}"),
				SenderState {
					handle: task.abort_handle(),
					bytes_sent: 0,
					started_ts_ms: 0,
				},
			);
		}
		assert!(!sender_slot_available(senders.len()));

		let before = senders.len();
		let denial = sender_admission(&senders, "transfer-new").expect("满员时必须拒绝");
		match denial {
			CtrlMsg::ContentDenied { req_id, code, .. } => {
				assert_eq!(req_id, "transfer-new");
				assert_eq!(code, "TOO_MANY_TRANSFERS");
			}
			other => panic!("必须使用非致命的 ContentDenied，实际: {other:?}"),
		}
		// 拒绝路径不插入条目（senders 不会增长）。
		assert_eq!(senders.len(), before);

		// 空出一个名额后必须恢复接纳。
		senders.remove("transfer-0");
		assert!(sender_admission(&senders, "transfer-new").is_none());
	}

	// -----------------------------------------------------------------------
	// B2 区间计划 / 流数控制器 / 接收端区间账本（socket-free）
	// -----------------------------------------------------------------------

	const MB: u64 = 1024 * 1024;

	fn controller_input(
		current_n: u8,
		gain: Option<f64>,
		loss_rate: f64,
		srtt_ratio: f64,
		remaining_bytes: u64,
		ceiling_reached: bool,
	) -> StreamControllerInput {
		StreamControllerInput {
			current_n,
			gain,
			loss_rate,
			srtt_ratio,
			remaining_bytes,
			ceiling_reached,
		}
	}

	/// 计划必须无空洞、无重叠、并集恰好等于请求区间，且各段长度差 ≤ 1。
	#[allow(clippy::assert_is_empty)]
	fn assert_ranges_cover(plan: &[TransferRange], offset: u64, length: u64) {
		if length == 0 {
			assert!(plan.is_empty(), "空文件不应有数据流区间: {plan:?}");
			return;
		}
		assert!(!plan.is_empty());
		assert_eq!(plan[0].offset, offset);
		let mut cursor = offset;
		for range in plan {
			assert_eq!(range.offset, cursor, "区间不连续: {plan:?}");
			assert!(range.length > 0, "区间长度必须 > 0: {plan:?}");
			cursor = cursor.saturating_add(range.length);
		}
		assert_eq!(
			cursor,
			offset.saturating_add(length),
			"区间并集必须等于请求区间"
		);
	}

	#[test]
	fn range_plan_invariants_cover_requested_range_disjointly() {
		for (offset, length, count) in [
			(0u64, 64 * MB, 6usize),
			(0, 256 * MB, 6),
			(1024, 1000, 3),
			(u64::from(u32::MAX), 1024 * 1024 + 7, 4),
		] {
			let plan = plan_ranges(offset, length, count);
			assert!(
				plan.len() <= MAX_PAYLOAD_STREAMS_PER_TRANSFER,
				"区间数不能超过上限: {}",
				plan.len()
			);
			assert!(
				plan.len() <= count.max(1),
				"count={count} 时不应生成更多区间"
			);
			assert_ranges_cover(&plan, offset, length);
			let smallest = plan.iter().map(|range| range.length).min().unwrap();
			let largest = plan.iter().map(|range| range.length).max().unwrap();
			assert!(
				largest - smallest <= 1,
				"区间长度必须均匀（差 ≤ 1）: {plan:?}"
			);
		}

		// count 超过上限、超过字节数、或为 0 时都要安全折叠。
		let clamped_by_cap = plan_ranges(0, 600, 100);
		assert_eq!(clamped_by_cap.len(), MAX_PAYLOAD_STREAMS_PER_TRANSFER);
		assert_ranges_cover(&clamped_by_cap, 0, 600);
		let folded = plan_ranges(0, 3, 6);
		assert_eq!(folded.len(), 3);
		assert_ranges_cover(&folded, 0, 3);
		assert_eq!(plan_ranges(0, 0, 6), Vec::<TransferRange>::new());
		assert_eq!(plan_ranges(0, 10, 0).len(), 1);
	}

	/// 小传输（≤ 16 MiB）必须保持单流；超过阈值后 LAN 只切 2 段、WAN 切 6 段。
	#[test]
	fn tiny_transfers_stay_single_stream_and_do_not_probe() {
		for length in [1u64, 8 * MB, PROBE_MIN_REMAINING_BYTES] {
			for srtt in [Duration::from_millis(1), Duration::from_millis(100)] {
				let plan = initial_range_plan(0, length, srtt);
				assert_eq!(plan.len(), 1, "length={length} 必须单流");
				assert_ranges_cover(&plan, 0, length);
			}
		}

		// LAN：2 段 = 主区间 + 小探针区间（尾部），保证回退代价有限。
		let lan = initial_range_plan(0, PROBE_MIN_REMAINING_BYTES + 1, Duration::from_millis(1));
		assert_eq!(lan.len(), 2);
		assert_ranges_cover(&lan, 0, PROBE_MIN_REMAINING_BYTES + 1);
		assert!(lan[0].length > 0 && lan[1].length > 0);
		let big_lan = initial_range_plan(0, 256 * MB, Duration::from_millis(1));
		assert_eq!(big_lan.len(), 2);
		assert!(
			big_lan[1].length <= LAN_PROBE_SLICE_MAX_BYTES,
			"LAN 探针区间必须有上限: {big_lan:?}"
		);
		assert_ranges_cover(&big_lan, 0, 256 * MB);

		// WAN：切满 6 段。
		let wan = initial_range_plan(0, PROBE_MIN_REMAINING_BYTES + 1, Duration::from_millis(100));
		assert_eq!(wan.len(), MAX_PAYLOAD_STREAMS_PER_TRANSFER);
		assert_ranges_cover(&wan, 0, PROBE_MIN_REMAINING_BYTES + 1);

		// 断点续传：计划从非 0 起点连续覆盖到文件末尾。
		let resumed = initial_range_plan(1024, 64 * MB, Duration::from_millis(100));
		assert_eq!(resumed[0].offset, 1024);
		assert_ranges_cover(&resumed, 1024, 64 * MB - 1024);
	}

	#[test]
	fn range_plan_validation_rejects_gaps_overlaps_and_oversize() {
		let total = 1000u64;
		assert!(validate_range_plan(
			&[
				TransferRange {
					offset: 0,
					length: 400,
				},
				TransferRange {
					offset: 400,
					length: 600,
				},
			],
			total
		)
		.is_ok());

		let gap = validate_range_plan(
			&[
				TransferRange {
					offset: 0,
					length: 400,
				},
				TransferRange {
					offset: 500,
					length: 500,
				},
			],
			total,
		)
		.unwrap_err();
		assert_eq!(gap.0, "TRANSFER_RANGE_GAP");

		let overlap = validate_range_plan(
			&[
				TransferRange {
					offset: 0,
					length: 600,
				},
				TransferRange {
					offset: 500,
					length: 500,
				},
			],
			total,
		)
		.unwrap_err();
		assert_eq!(overlap.0, "TRANSFER_RANGE_OVERLAP");

		let short = validate_range_plan(
			&[TransferRange {
				offset: 0,
				length: 500,
			}],
			total,
		)
		.unwrap_err();
		assert_eq!(short.0, "TRANSFER_RANGE_GAP");

		let long = validate_range_plan(
			&[TransferRange {
				offset: 0,
				length: 1200,
			}],
			total,
		)
		.unwrap_err();
		assert_eq!(long.0, "TRANSFER_RANGE_OVERRUN");

		let too_many: Vec<TransferRange> = (0..=MAX_PAYLOAD_STREAMS_PER_TRANSFER as u64)
			.map(|index| TransferRange {
				offset: index,
				length: 1,
			})
			.collect();
		assert_eq!(
			validate_range_plan(&too_many, 7).unwrap_err().0,
			"TRANSFER_RANGE_TOO_MANY"
		);

		assert!(validate_range_plan(&[], 0).is_ok());
		assert_eq!(
			validate_range_plan(&[], total).unwrap_err().0,
			"TRANSFER_RANGE_EMPTY"
		);
	}

	/// 乱序完成：任何区间未 FIN 都不能 finalize；全部 FIN 后仍需 ContentEnd。
	#[test]
	fn receiver_ranges_complete_out_of_order_and_need_content_end() {
		let plan = vec![
			TransferRange {
				offset: 0,
				length: 100,
			},
			TransferRange {
				offset: 100,
				length: 200,
			},
			TransferRange {
				offset: 300,
				length: 300,
			},
		];
		let mut ranges = ReceiverRanges::from_plan(&plan, 600);
		assert_eq!(ranges.slots.len(), 3);
		assert!(!ranges.all_finished());

		for index in [2usize, 0] {
			ranges.set_state(index, RangeSlotState::Finished);
			assert!(!ranges.all_finished(), "index={index} 之后仍应有区间未完成");
			assert_eq!(
				content_end_route(ranges.all_finished(), false),
				ContentEndRoute::WaitForPayload
			);
		}

		ranges.set_state(1, RangeSlotState::Finished);
		assert!(ranges.all_finished());
		assert_eq!(
			content_end_route(ranges.all_finished(), false),
			ContentEndRoute::FinalizeNow
		);
		// 重复 ContentEnd 不得再次 commit。
		assert_eq!(content_end_route(true, true), ContentEndRoute::Ignore);
		assert_eq!(content_end_route(false, true), ContentEndRoute::Ignore);

		// 空计划（空文件）视为完成。
		assert!(ReceiverRanges::from_plan(&[], 0).all_finished());
	}

	/// 接收端区间路由：重复/重试/长度不符/重叠/未声明都必须可区分。
	#[test]
	fn receiver_range_routing_detects_duplicate_overlap_and_unexpected() {
		let plan = vec![
			TransferRange {
				offset: 0,
				length: 100,
			},
			TransferRange {
				offset: 100,
				length: 100,
			},
		];
		let mut ranges = ReceiverRanges::from_plan(&plan, 200);
		assert_eq!(route_range_header(&ranges, 100, 100), RangeRoute::Attach(1));

		// 已完成的区间再收到一条流：重复，只丢流、不影响传输。
		ranges.set_state(1, RangeSlotState::Finished);
		assert_eq!(route_range_header(&ranges, 100, 100), RangeRoute::Duplicate);

		// 失败区间允许重试（同 offset/length）。
		ranges.set_state(0, RangeSlotState::Failed);
		assert_eq!(route_range_header(&ranges, 0, 100), RangeRoute::Retry(0));

		// 同 offset 不同 length：长度不符。
		assert_eq!(
			route_range_header(&ranges, 0, 99),
			RangeRoute::LengthMismatch
		);
		// 与计划区间部分重叠。
		assert_eq!(route_range_header(&ranges, 50, 100), RangeRoute::Overlap);
		// 完全在计划之外。
		assert_eq!(route_range_header(&ranges, 200, 10), RangeRoute::Unexpected);
	}

	/// 控制器：稳定期探针 +1；有收益继续 +1；无收益回退并置 ceiling。
	#[test]
	fn controller_probes_up_on_gain_and_reverts_with_ceiling() {
		let probe = decide_stream_count(controller_input(1, None, 0.0, 1.0, 64 * MB, false));
		assert_eq!(probe.next_n, 2);
		assert_eq!(probe.reason, StreamDecisionReason::ProbeStarted);
		assert!(!probe.ceiling_reached);

		let accepted =
			decide_stream_count(controller_input(2, Some(0.20), 0.0, 1.0, 64 * MB, false));
		assert_eq!(accepted.next_n, 3);
		assert_eq!(accepted.reason, StreamDecisionReason::ProbeAccepted);
		assert!(!accepted.ceiling_reached);

		let rejected =
			decide_stream_count(controller_input(3, Some(0.05), 0.0, 1.0, 64 * MB, false));
		assert_eq!(rejected.next_n, 2, "收益 ≤ 8% 必须回退一条");
		assert_eq!(rejected.reason, StreamDecisionReason::ProbeRejected);
		assert!(rejected.ceiling_reached, "探针失败后本传输不再上探");

		// ceiling 粘性：后续稳定窗口保持 N，不再探针。
		let held = decide_stream_count(controller_input(2, None, 0.0, 1.0, 64 * MB, true));
		assert_eq!(held.next_n, 2);
		assert_eq!(held.reason, StreamDecisionReason::Ceiling);
		assert!(held.ceiling_reached);
	}

	/// 控制器：丢包 > 1% 或 srtt > 2.5× 基线时退避（不低于 1）。
	#[test]
	fn controller_backs_off_on_loss_and_srtt() {
		// 非探针窗口的偶发丢包：只退避，不置 ceiling（冷却后仍可再探）。
		let loss = decide_stream_count(controller_input(4, None, 0.02, 1.0, 64 * MB, false));
		assert_eq!(loss.next_n, 3);
		assert_eq!(loss.reason, StreamDecisionReason::CongestionBackoff);
		assert!(
			!loss.ceiling_reached,
			"非探针窗口的退避不是 ceiling，可在冷却后再次探针"
		);

		// 探针窗口出现丢包：说明加流本身导致劣化，回退并标记 ceiling（滞回）。
		let probe_loss =
			decide_stream_count(controller_input(2, Some(0.10), 0.02, 1.0, 64 * MB, false));
		assert_eq!(probe_loss.next_n, 1);
		assert_eq!(probe_loss.reason, StreamDecisionReason::CongestionBackoff);
		assert!(probe_loss.ceiling_reached);

		let srtt = decide_stream_count(controller_input(4, None, 0.0, 3.0, 64 * MB, false));
		assert_eq!(srtt.next_n, 3);
		assert_eq!(srtt.reason, StreamDecisionReason::CongestionBackoff);

		let floor = decide_stream_count(controller_input(1, None, 0.5, 5.0, 64 * MB, false));
		assert_eq!(floor.next_n, 1, "退避不能低于 1 条流");
	}

	/// 小传输（剩余 ≤ 16 MiB）强制 N=1；上限 6 也不越界。
	#[test]
	fn controller_small_transfer_stays_single_and_respects_cap() {
		let small = decide_stream_count(controller_input(
			4,
			Some(0.5),
			0.0,
			1.0,
			PROBE_MIN_REMAINING_BYTES,
			false,
		));
		assert_eq!(small.next_n, 1);
		assert_eq!(small.reason, StreamDecisionReason::SmallTransfer);

		let at_cap = decide_stream_count(controller_input(6, None, 0.0, 1.0, 64 * MB, false));
		assert_eq!(at_cap.next_n, 6);
		// 已到上限且探针有收益：保持 6，不能误判为"收益不足"而回退。
		let cap_gain =
			decide_stream_count(controller_input(6, Some(0.5), 0.0, 1.0, 64 * MB, false));
		assert_eq!(cap_gain.next_n, 6);
		assert!(!cap_gain.ceiling_reached);
	}

	/// 评估周期 T = clamp(4×srtt, 300ms, 2s)。
	#[test]
	fn probe_interval_clamps_to_adr_bounds() {
		assert_eq!(
			stream_probe_interval(Duration::from_millis(1)),
			Duration::from_millis(300)
		);
		assert_eq!(
			stream_probe_interval(Duration::from_millis(100)),
			Duration::from_millis(400)
		);
		assert_eq!(
			stream_probe_interval(Duration::from_secs(10)),
			Duration::from_secs(2)
		);
	}

	// -----------------------------------------------------------------------
	// 数据流路由（B2：按 req_id / 区间两级判定）
	// -----------------------------------------------------------------------

	/// 未知 req_id：先暂存等待 ContentBegin（有宽限上限），绝不直接接入。
	#[test]
	fn payload_route_unknown_waits_for_registration() {
		assert_eq!(
			route_payload_stream(false, false, false),
			PayloadRoute::AwaitRegistration
		);
	}

	/// 已有 Receiving receiver：交给区间路由判定（不是直接接入）。
	#[test]
	fn payload_route_accepts_registered_transfer() {
		assert_eq!(
			route_payload_stream(true, false, false),
			PayloadRoute::ReceiverReady
		);
	}

	/// 同一 req_id 进入最终化/已完成后必须拒绝。
	#[test]
	fn payload_route_rejects_duplicate_and_finalizing() {
		assert_eq!(
			route_payload_stream(true, true, false),
			PayloadRoute::Duplicate
		);
		assert_eq!(
			route_payload_stream(false, true, false),
			PayloadRoute::Duplicate
		);
	}

	/// 已取消的 req_id：即使状态看起来可接入也必须拒绝（取消与数据流可能交叉）。
	#[test]
	fn payload_route_rejects_cancelled_transfer() {
		assert_eq!(
			route_payload_stream(true, false, true),
			PayloadRoute::Cancelled
		);
		assert_eq!(
			route_payload_stream(false, false, true),
			PayloadRoute::Cancelled
		);
	}

	/// 头部与 ContentBegin 的 item/file/整文件 sha 必须逐字段一致。
	#[test]
	fn payload_header_validation_matches_content_begin() {
		let sha = "a".repeat(64);
		let header = TransferHeader {
			req_id: "t-1".into(),
			item_id: "item-1".into(),
			file_id: Some("file-1".into()),
			offset: 0,
			length: 1024,
			sha256: sha.clone(),
			mime: "application/octet-stream".into(),
		};
		assert!(
			SessionActor::validate_payload_header(&header, "item-1", Some("file-1"), &sha).is_ok()
		);
		assert!(
			SessionActor::validate_payload_header(&header, "item-2", Some("file-1"), &sha).is_err()
		);
		assert!(SessionActor::validate_payload_header(&header, "item-1", None, &sha).is_err());
		assert!(SessionActor::validate_payload_header(
			&header,
			"item-1",
			Some("file-1"),
			&"b".repeat(64)
		)
		.is_err());
	}
}
