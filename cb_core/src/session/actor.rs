// cb_core/src/session/actor.rs

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::SeekFrom;
use std::path::PathBuf;
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
use crate::model::ItemKind;
use crate::policy::SizeLimits;
use crate::proto::{AuthSessionFlags, CBFrame, CBFrameCodec, CtrlMsg, PROTOCOL_VERSION};
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
	postcard::to_stdvec(value).map_err(|e| anyhow::anyhow!("OPAQUE 序列化失败: {e}"))
}

fn opaque_decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
	postcard::from_bytes(bytes).map_err(|e| anyhow::anyhow!("OPAQUE 反序列化失败: {e}"))
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
/// 只限制 channel 槽位数量是不够的：32 个槽位若每槽都是协议允许的最大帧（10 MiB），
/// 单个 receiver 就能占 320 MiB。这里用字节信号量计量在途字节，上限 12 MiB：
/// 足以容纳单个最大帧（`proto::CBFrameCodec` 的 10 MiB 上限）再加少量小帧，
/// 又不会让慢 writer 把内存撑爆。8 个 receiver 最坏约 96 MiB 在途。
const MAX_INFLIGHT_BYTES_PER_RECEIVER: usize = 12 * 1024 * 1024;

/// writer 背压时，actor 侧允许暂存的字节数上限（帧已经从连接里读出来了，必须自己兜住）。
///
/// 12 MiB 在途 + 1 MiB 暂存 = 每个 receiver 最坏约 13 MiB，8 个约 104 MiB。
/// 超出该预算说明 writer 完全跟不上而对端还在灌数据：在"不阻塞整条连接"的约束下
/// 无法保住这条传输，只能让它失败（`TRANSFER_BACKPRESSURE`），而不是无限增长内存。
const MAX_PENDING_BYTES_PER_RECEIVER: usize = 1024 * 1024;

/// 接收方空闲超时：超过该时长没有收到任何数据块就取消传输并清理 tmp 文件。
///
/// 对端可以打开传输后保持心跳但不发数据；若不清理，tmp 文件与 fd 会一直泄漏。
const RECEIVER_IDLE_TIMEOUT_MS: i64 = 30_000;

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

/// 把 item 自身 `content.sha256` 对应的 content_cache 行标记为 present。
///
/// FileList 传输里 `ContentBegin.sha256` 只是**单个文件**的 hash，而 `content_cache`
/// 以 item 的 `content.sha256` 为键；直接用它 UPDATE 会命中 0 行（旧代码还把结果丢弃）。
/// 因此优先取 item 的 `sha256_hex`，查不到时才回退到本次传输的 hash。
///
/// 返回 `(实际标记的 sha, 受影响行数)`，调用方负责在 0 行时记录警告而不是硬失败。
fn mark_item_content_cached(
	store: &mut Store,
	item_id: &str,
	fallback_sha: &str,
	now: i64,
) -> anyhow::Result<(String, usize)> {
	let item_sha = store
		.get_item_sha256(item_id)?
		.filter(|sha| !sha.is_empty())
		.unwrap_or_else(|| fallback_sha.to_string());
	let rows = store.mark_cache_present(&item_sha, now)?;
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
		/// 非阻塞投递队列：actor 主循环绝不 `.await` writer 的有界 channel。
		inbox: ReceiverInbox,
		/// writer Task 的 abort 句柄：超时/取消/越界时直接终止并触发 tmp 清理。
		handle: tokio::task::AbortHandle,
		received_bytes: u64,
		total_bytes: u64,
		/// 本次传输适用的硬上限（来自 size_limits，按 item 类型选定）
		hard_cap_bytes: u64,
		/// 传输开始时间（用于结束时只写一条汇总日志，含耗时）
		started_ts_ms: i64,
		/// 最后一次收到数据块的时间（用于空闲超时回收）
		last_activity_ms: i64,
		last_progress_emit: i64, // 用于节流 progress 事件
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

/// 定义发送任务的消息
#[allow(dead_code)]
enum UploadMsg {
	Chunk {
		transfer_id: String,
		data: bytes::Bytes,
	},
	Done {
		transfer_id: String,
		sha256: String,
	},
	Error {
		transfer_id: String,
		err: String,
	},
}

/// 发送中的传输：AbortHandle + 汇总统计。
///
/// 统计只用于传输结束时写**一条**汇总日志。逐块日志已移除：
/// 1GB 传输按 64KB 分块会写约 1.6 万条 SQLite 行，且都在 actor 主循环里同步插入。
struct SenderState {
	handle: tokio::task::AbortHandle,
	bytes_sent: u64,
	started_ts_ms: i64,
}

enum ReceiverTaskMsg {
	Chunk {
		data: bytes::Bytes,
		/// 在途字节预算许可：writer 写完（或消息被丢弃）才归还，从而把内存占用
		/// 限制在 [`MAX_INFLIGHT_BYTES_PER_RECEIVER`] 以内。
		permit: OwnedSemaphorePermit,
	},
	Finish {
		expected_sha256: String,                    // 这里指本次传输片段的 Hash
		reply_tx: oneshot::Sender<Result<PathBuf>>, // 返回最终文件路径
	},
}

/// 单个入站传输的非阻塞投递队列（actor 主循环专用）。
///
/// 背景：所有传输的帧都复用同一条双向流，actor 只能逐个 `reader.next()` 读帧。
/// 一旦在某个传输的 writer 有界 channel 上 `.await`，整条连接（其他传输、心跳、
/// 本地命令）都会被队头阻塞。因此下泄路径改成：
///
/// 1. 优先 `try_send`；成功即完成，永不等待；
/// 2. channel 满或 12 MiB 在途预算耗尽 → 把块放进 `pending` 并标记 `paused`，
///    该传输自己的帧继续被读取（帧已经在内存里，无法"不读"），但不会阻塞别的帧；
/// 3. writer 每写完一块就归还字节许可，后续到达的数据帧（或心跳 tick）重试
///    [`ReceiverInbox::flush_pending`]，writer 恢复后 pending 会按原顺序补发；
/// 4. `pending` 也超过 1 MiB → 判定 writer 完全跟不上，该传输失败（不丢数据、
///    不静默损坏，也不把内存变成无界）。
struct ReceiverInbox {
	tx: mpsc::Sender<ReceiverTaskMsg>,
	budget: Arc<Semaphore>,
	pending: VecDeque<bytes::Bytes>,
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
	fn new(tx: mpsc::Sender<ReceiverTaskMsg>, budget: Arc<Semaphore>) -> Self {
		Self {
			tx,
			budget,
			pending: VecDeque::new(),
			pending_bytes: 0,
			paused: false,
		}
	}

	/// 把 pending 中的块按顺序推进 writer channel，**永不 await**。
	fn flush_pending(&mut self) -> PushOutcome {
		while let Some(front) = self.pending.front() {
			// 单帧最大 10 MiB，一定放得进 u32；异常超大值按不可获取处理。
			let len = u32::try_from(front.len()).unwrap_or(u32::MAX);
			let Ok(permit) = self.budget.clone().try_acquire_many_owned(len) else {
				return PushOutcome::Paused;
			};
			let data = self.pending.pop_front().expect("front() 刚刚确认队列非空");
			self.pending_bytes -= data.len();
			match self.tx.try_send(ReceiverTaskMsg::Chunk { data, permit }) {
				Ok(()) => {}
				Err(mpsc::error::TrySendError::Full(msg)) => {
					// 槽位又被占满：原样放回队首，保证顺序；许可随消息一起 drop。
					// 本方法只会发送 Chunk 变体，其他变体不可能出现。
					if let ReceiverTaskMsg::Chunk { data, .. } = msg {
						self.pending_bytes += data.len();
						self.pending.push_front(data);
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
		if self.paused || !self.pending.is_empty() {
			match self.flush_pending() {
				PushOutcome::Closed => return PushOutcome::Closed,
				PushOutcome::Paused => return self.push_pending(data),
				// 积压已全部推进 channel，可以继续走下面的直发路径。
				PushOutcome::Queued | PushOutcome::Overflow => {}
			}
		}

		let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
		match self.budget.clone().try_acquire_many_owned(len) {
			Ok(permit) => match self.tx.try_send(ReceiverTaskMsg::Chunk { data, permit }) {
				Ok(()) => PushOutcome::Queued,
				Err(mpsc::error::TrySendError::Full(msg)) => {
					// 本方法只会发送 Chunk 变体，其他变体不可能出现。
					if let ReceiverTaskMsg::Chunk { data, .. } = msg {
						self.push_pending(data)
					} else {
						PushOutcome::Paused
					}
				}
				Err(mpsc::error::TrySendError::Closed(_)) => PushOutcome::Closed,
			},
			// 在途字节预算耗尽（writer 慢）：暂存而不是等待。
			Err(_) => self.push_pending(data),
		}
	}

	fn push_pending(&mut self, data: bytes::Bytes) -> PushOutcome {
		if self.pending_bytes.saturating_add(data.len()) > MAX_PENDING_BYTES_PER_RECEIVER {
			return PushOutcome::Overflow;
		}
		self.pending_bytes += data.len();
		self.pending.push_back(data);
		self.paused = true;
		PushOutcome::Paused
	}

	/// 最终化时在**独立任务**里排空 pending（这里允许 await，不占 actor 主循环）。
	///
	/// 必须先把 pending 按序送进 writer，再发 `Finish`，否则会丢块或校验失败。
	async fn flush_all(&mut self) -> std::result::Result<(), String> {
		while let Some(data) = self.pending.pop_front() {
			self.pending_bytes -= data.len();
			let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
			let permit = self
				.budget
				.clone()
				.acquire_many_owned(len)
				.await
				.map_err(|_| "writer channel closed".to_string())?;
			self.tx
				.send(ReceiverTaskMsg::Chunk { data, permit })
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
	result: std::result::Result<PathBuf, (String, String)>,
}

/// 在独立任务里驱动 writer 完成 flush / close / 校验 / commit / materialize。
///
/// 注意：这里的每一处 `.await` 都在 actor 主循环之外，因此慢磁盘只影响这条传输。
async fn finalize_receiver(
	mut inbox: ReceiverInbox,
	expected_sha256: String,
) -> std::result::Result<PathBuf, (String, String)> {
	inbox
		.flush_all()
		.await
		.map_err(|e| ("WRITE_TASK_DEAD".to_string(), e))?;

	let (reply_tx, reply_rx) = oneshot::channel();
	inbox
		.tx
		.send(ReceiverTaskMsg::Finish {
			expected_sha256,
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
		Ok(Ok(path)) => Ok(path),
		Ok(Err(e)) => Err(("COMMIT_FAILED".to_string(), e.to_string())),
		Err(_) => Err((
			"COMMIT_TIMEOUT".to_string(),
			"Writer task dropped reply".to_string(),
		)),
	}
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
							UploadMsg::Chunk { transfer_id, data } => {
								actor.send_data_chunk(transfer_id, data).await?;
							}
							UploadMsg::Done { transfer_id, sha256: _ } => {
								actor.send_ctrl(CtrlMsg::ContentEnd { req_id: transfer_id.clone() }).await?;
								// 每个传输只写一条汇总日志（替代原先每 64KB 一条的 debug 日志）
								if let Some(state) = actor.senders.remove(&transfer_id) {
									let elapsed_ms = now_ms() - state.started_ts_ms;
									actor.log_sink.log_info(
										"Session",
										&format!(
											"Outbound transfer completed: transfer_id={transfer_id}, bytes={}, elapsed_ms={elapsed_ms}",
											state.bytes_sent
										),
										Some(&format!(
											"出站传输已完成: 传输ID={transfer_id}，字节数={}，耗时={elapsed_ms}ms",
											state.bytes_sent
										)),
									);
								}
							}
							UploadMsg::Error { transfer_id, err } => {
								// 每个传输只写一条（失败）日志
								if let Some(state) = actor.senders.remove(&transfer_id) {
									let elapsed_ms = now_ms() - state.started_ts_ms;
									actor.log_sink.log_warn(
										"Session",
										&format!(
											"Outbound transfer aborted: transfer_id={transfer_id}, bytes={}, elapsed_ms={elapsed_ms}, error={err}",
											state.bytes_sent
										),
										Some(&format!(
											"出站传输已中止: 传输ID={transfer_id}，字节数={}，耗时={elapsed_ms}ms，错误={err}",
											state.bytes_sent
										)),
									);
								}
								eprintln!("[Session] Upload error for {transfer_id}: {err}");
							}
						}
					}

					// 3. 心跳
					_ = heartbeat_ticker.tick() => {
						actor.tick_heartbeat().await?;
						// 每 2s 顺带回收空闲超时的入站传输（避免 tmp 文件 / fd 泄漏）
						actor.sweep_idle_receivers().await;
						// 并重试被背压暂停的 receiver：writer 排空后继续投递积压块
						actor.retry_paused_receivers();
					}

					// 4. 入站最终化完成事件（flush/close/commit/materialize 在独立任务里做）
					Some(done) = receiver_done_rx.recv() => {
						actor.handle_receiver_done(done).await?;
					}
				}
			}
			Ok(())
		}
		.await;

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

	async fn send_data_chunk(&mut self, transfer_id: String, data: bytes::Bytes) -> Result<()> {
		// [修改] 增加超时控制，防止网络拥塞阻塞心跳
		// 如果 500ms 发不出去，认为网络拥塞严重，报错断开传输，保住连接
		let data_size = data.len() as u64;
		let send_future = self.writer.send(CBFrame::Data {
			transfer_id: transfer_id.clone(),
			data,
		});
		match tokio::time::timeout(Duration::from_millis(500), send_future).await {
			Ok(Ok(())) => {
				// 逐块不写日志：这里只累加字节数，传输结束时写一条汇总，
				// 否则 1GB 传输会产生约 1.6 万条 SQLite 日志行。
				if let Some(state) = self.senders.get_mut(&transfer_id) {
					state.bytes_sent = state.bytes_sent.saturating_add(data_size);
				}
				Ok(())
			}
			Ok(Err(e)) => Err(e), // 协议栈错误
			Err(_) => {
				// 超时：主动移除 Sender 任务并通知对方 Cancel
				if let Some(state) = self.senders.remove(&transfer_id) {
					state.handle.abort();
				}
				self.log_sink.log_warn(
						"Session",
						&format!("Data chunk send timeout (network congestion): transfer_id={transfer_id}, cancelling transfer"),
						Some(&format!("数据块发送超时（网络拥塞）: 传输ID={transfer_id}，正在取消传输")),
				);
				// 尝试发一个 Cancel 包（尽力而为）
				let _ = self
					.send_ctrl(CtrlMsg::ContentCancel {
						req_id: transfer_id,
						reason: "Network congested/timeout".into(),
					})
					.await;
				anyhow::bail!("Send data chunk timeout (network congestion)");
			}
		}
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
		match frame {
			CBFrame::Control(msg) => self.handle_control_msg(*msg).await,
			CBFrame::Data { transfer_id, data } => self.handle_data_chunk(transfer_id, data).await,
		}
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
					self.handle_opaque_finish(&bytes)?;
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
			} => {
				self.handle_content_begin(req_id, item_id, file_id, total_bytes, sha256, mime)
					.await?;
			}
			CtrlMsg::ContentEnd { req_id } => {
				self.handle_content_end(req_id);
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
	async fn handle_content_begin(
		&mut self,
		req_id: String,
		item_id: String,
		file_id: Option<String>,
		total_bytes: u64,
		file_sha256: String, // 注意：这是整个文件的 Hash，Resume 时不需要校验它
		mime: String,
	) -> Result<()> {
		// 入站策略：accept_from_peer=false（或对端身份未知）时拒绝接收内容。
		// 用 ContentCancel 而非 Error 通知对端：Error 会被对端视为致命错误并断开会话。
		if !self.inbound_accept_allowed().await? {
			self.log_sink.log_warn(
					"Session",
					&format!("Transfer rejected by accept_from_peer policy: transfer_id={req_id}, item_id={item_id}"),
					Some(&format!("接收策略禁止接收内容: 传输ID={req_id}，项目ID={item_id}")),
			);
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
			// 幂等处理：如果是重复的 Begin，忽略即可
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
			self.send_ctrl(CtrlMsg::ContentCancel {
				req_id,
				reason: format!("hard size limit exceeded: {total_bytes} > {hard_cap}"),
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
		let (tx, mut rx) = mpsc::channel::<ReceiverTaskMsg>(32);
		let budget = Arc::new(Semaphore::new(MAX_INFLIGHT_BYTES_PER_RECEIVER));
		let inbox = ReceiverInbox::new(tx, budget);

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

			// 打开文件
			let mut opts = OpenOptions::new();
			opts.write(true).create(true);
			if is_resume {
				opts.append(true); // 续传追加
			} else {
				opts.truncate(true); // 新传覆盖
			}

			let file_res = opts.open(&guard.path).await;
			if let Err(e) = file_res {
				eprintln!("[Session] Writer open failed: {e}");
				return;
			}
			let mut writer = tokio::io::BufWriter::new(file_res.unwrap());
			let mut hasher = Sha256::new(); // 计算本次传输片段的 Hash

			while let Some(msg) = rx.recv().await {
				match msg {
					ReceiverTaskMsg::Chunk { data, permit } => {
						if let Err(e) = writer.write_all(&data).await {
							eprintln!("[Session] Write failed: {e}");
							return; // 触发 Drop 删除
						}
						hasher.update(&data);
						// 写完后归还字节许可：actor 看到的"在途字节"因此随磁盘进度下降。
						drop(permit);
					}
					ReceiverTaskMsg::Finish {
						expected_sha256,
						reply_tx,
					} => {
						// 1. Flush & Sync
						if writer.flush().await.is_err() {
							return;
						}
						let mut f = writer.into_inner();
						if f.shutdown().await.is_err() {
							return;
						}
						drop(f); // 关闭文件句柄

						// 2. 校验 Hash (本次传输片段)
						let calculated = hex::encode(hasher.finalize());
						if calculated != expected_sha256 {
							let _ = reply_tx.send(Err(anyhow::anyhow!("Checksum mismatch")));
							return; // 触发 Drop 删除
						}

						// 3. Commit 逻辑 (移入 spawn_blocking)
						guard.committed = true; // 禁止 Drop 删除，转交控制权
						let guard_path = guard.path.clone(); // Clone path before move

						let commit_res = tokio::task::spawn_blocking(move || {
							// A. 转正 Blob (CAS) - 注意：Resume 场景下这里应该合并 Hash，
							// 但简化起见，我们假设 CAS 接口能处理或仅作为临时存储。
							// 实际 M3 完整版应计算全量 Hash 存入 blobs。
							// 这里我们直接用 expected_sha256 (片段Hash) 做 blob 名可能不对，
							// 但为了让流程跑通，先产生一个 Blob。
							// *修正*：M3 协议中 ContentBegin 里的 _file_sha256 才是最终 Blob ID。
							// 但 Writer Task 没有那个值。
							// 暂定：用片段 Hash 存，或者如果不做全量校验，直接 commit。

							// 这里复用原本的逻辑
							let blob_path =
								cas_clone.commit_tmp_file(&guard_path, &expected_sha256)?;

							// B. 决定落地路径 (Materialize)
							if let Some(real_fid) = fid {
								// FileList 模式
								let store = store_clone.lock().unwrap();
								if let Some(fmeta) = store.get_file_meta(&iid, &real_fid)? {
									cas_clone.materialize_file(
										&expected_sha256,
										&tid,
										&fmeta.rel_name,
									)
								} else {
									Ok(blob_path)
								}
							} else {
								// Text/Image 模式
								let ext = match mime_clone.as_str() {
									"image/png" => Some("png"),
									"image/jpeg" | "image/jpg" => Some("jpg"),
									_ => None,
								};
								if let Some(extension) = ext {
									cas_clone.materialize_blob(&expected_sha256, extension)
								} else {
									Ok(blob_path)
								}
							}
						})
						.await;

						// 发送结果回主 Actor
						let res = match commit_res {
							Ok(Ok(path)) => Ok(path),
							Ok(Err(e)) => Err(e),
							Err(e) => Err(e.into()),
						};
						let _ = reply_tx.send(res);
						return; // 任务结束
					}
				}
			}
		});
		// 保留 abort 句柄：ContentEnd 超时 / 取消 / 越界时终止 writer，TempFileGuard 会删 tmp。
		let writer_handle = writer_task.abort_handle();

		// 注意：如果是 Resume，这里的 total_bytes 可能是剩余量或总量，
		// 取决于协议约定；无论如何它都是本端允许写入的上限。
		let started_ts_ms = now_ms();
		let receiver_state = ReceiverState::Receiving {
			transfer_id: req_id.clone(),
			item_id,
			file_id,
			expected_sha256: file_sha256,
			mime,
			inbox,
			handle: writer_handle,
			received_bytes: 0,
			total_bytes,
			hard_cap_bytes: hard_cap,
			started_ts_ms,
			last_activity_ms: started_ts_ms,
			last_progress_emit: 0,
		};
		self.receivers.insert(req_id, receiver_state);
		Ok(())
	}

	async fn handle_data_chunk(&mut self, transfer_id: String, data: bytes::Bytes) -> Result<()> {
		let chunk_size = data.len() as u64;

		// 越界闸门：先做只读检查；一旦命中，整条传输失败（writer Task 退出并删除 tmp），
		// 绝不允许对端写出其声明的 total_bytes，也不允许越过 size_limits 的硬上限。
		let overrun: Option<(&'static str, String)> = match self.receivers.get(&transfer_id) {
			Some(ReceiverState::Receiving {
				received_bytes,
				total_bytes,
				hard_cap_bytes,
				..
			}) => chunk_overrun(*received_bytes, chunk_size, *total_bytes, *hard_cap_bytes).map(
				|code| {
					(
						code,
						format!(
							"received={received_bytes}+{chunk_size}, declared_total={total_bytes}, hard_cap={hard_cap_bytes}"
						),
					)
				},
			),
			_ => None,
		};
		if let Some((code, detail)) = overrun {
			self.log_sink.log_error(
					"Session",
					&format!(
						"Transfer aborted for exceeding declared size: transfer_id={transfer_id}, code={code}, {detail}"
					),
					Some(&format!(
						"传输写出声明大小之外的数据，已终止: 传输ID={transfer_id}，错误码={code}，{detail}"
					)),
					Some(&detail),
			);
			self.abort_receiver(&transfer_id, code, &detail, "size limit exceeded")
				.await;
			return Ok(());
		}

		// 非阻塞下泄：绝不在这里 `.await` 有界的 writer channel。
		// writer 变慢时该块进入 per-transfer pending，actor 继续读其他帧；
		// pending 也放不下时才让这一条传输失败（见 ReceiverInbox 的说明）。
		let mut overflow_detail: Option<String> = None;
		let mut writer_closed = false;
		if let Some(ReceiverState::Receiving {
			inbox,
			item_id,
			received_bytes,
			total_bytes,
			last_activity_ms,
			last_progress_emit,
			..
		}) = self.receivers.get_mut(&transfer_id)
		{
			*received_bytes = received_bytes.saturating_add(chunk_size);
			let now = now_ms();
			*last_activity_ms = now;

			match inbox.try_push(data) {
				PushOutcome::Queued | PushOutcome::Paused => {
					// 注意：这里绝不逐块写日志 —— 每 64KB 一条 SQLite 插入会在
					// 1GB 传输中产生约 1.6 万行，且占用 actor 主循环的 std Mutex。
					// 每个传输只在成功/失败/超时结束时写一条汇总。
					if now - *last_progress_emit > 200 {
						let progress_evt = serde_json::json!({
							"type": "TRANSFER_PROGRESS",
							"payload": {
								"transfer_id": transfer_id,
								"item_id": item_id.clone(),
								"state": "downloading",
								"received": *received_bytes,
								"total": *total_bytes
							}
						});
						self.sink.emit(progress_evt.to_string());
						*last_progress_emit = now;
					}
				}
				PushOutcome::Overflow => {
					overflow_detail = Some(format!(
						"writer backpressure: pending buffer exceeded {MAX_PENDING_BYTES_PER_RECEIVER} bytes"
					));
				}
				PushOutcome::Closed => writer_closed = true,
			}
		}

		if writer_closed {
			// writer Task 已退出（磁盘错误等）：清理 receiver，与旧行为一致。
			self.receivers.remove(&transfer_id);
			return Ok(());
		}
		if let Some(detail) = overflow_detail {
			self.log_sink.log_error(
				"Session",
				&format!(
					"Transfer aborted by receiver backpressure: transfer_id={transfer_id}, detail={detail}"
				),
				Some(&format!(
					"接收方背压超限，已终止该传输: 传输ID={transfer_id}，详情={detail}"
				)),
				Some(&detail),
			);
			self.abort_receiver(
				&transfer_id,
				"TRANSFER_BACKPRESSURE",
				&detail,
				"receiver backpressure overflow",
			)
			.await;
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

	/// 立即终止一条接收中的传输：abort writer Task（RAII 删除 tmp 文件），
	/// 上报 TRANSFER_FAILED，并尽力通知对端停止发送（ContentCancel 是非致命的）。
	async fn abort_receiver(&mut self, req_id: &str, code: &str, detail: &str, peer_reason: &str) {
		let Some(ReceiverState::Receiving { handle, .. }) = self.take_receiving(req_id) else {
			return;
		};
		// 直接 abort 而不是往有界 channel 里 await Cancel：这里同样不能阻塞 actor 主循环。
		// writer Task 的 TempFileGuard 在 drop 时删除 tmp 文件。
		handle.abort();
		self.emit_transfer_failed(req_id, code, detail);
		let _ = self
			.send_ctrl(CtrlMsg::ContentCancel {
				req_id: req_id.to_string(),
				reason: peer_reason.to_string(),
			})
			.await;
	}

	/// writer 排空后重试被背压暂停的 receiver（由数据帧或心跳 tick 驱动）。
	fn retry_paused_receivers(&mut self) {
		let paused: Vec<String> = self
			.receivers
			.iter()
			.filter_map(|(id, st)| match st {
				ReceiverState::Receiving { inbox, .. } if inbox.paused => Some(id.clone()),
				_ => None,
			})
			.collect();
		for id in paused {
			let outcome = match self.receivers.get_mut(&id) {
				Some(ReceiverState::Receiving { inbox, .. }) => inbox.flush_pending(),
				_ => continue,
			};
			if outcome == PushOutcome::Closed {
				self.receivers.remove(&id);
			}
		}
	}

	/// 回收空闲超时的 receiver：对端开传输后保持心跳却不发数据时，
	/// 不能让它永久占着 tmp 文件与 fd。
	async fn sweep_idle_receivers(&mut self) {
		let now = now_ms();
		let idle: Vec<String> = self
			.receivers
			.iter()
			.filter_map(|(id, st)| match st {
				ReceiverState::Receiving {
					last_activity_ms, ..
				} if receiver_idle_expired(now, *last_activity_ms) => Some(id.clone()),
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

	/// 处理对端的 `ContentEnd`。
	///
	/// 关键：actor 主循环只负责把"最终化"交给独立任务并登记 `Committing` 占位符，
	/// 绝不在 select 循环里等待 flush/close/commit/materialize（2 GiB 文件或慢磁盘
	/// 会阻塞整条连接的全部帧）。最终化任务完成后通过 `receiver_done_tx` 回报，
	/// 由 select 的完成事件分支处理。
	///
	/// 顺序保证：`Committing` 占位符在最终化完成前一直占用 req_id ——
	/// 期间同 req_id 的 `ContentBegin` 被当作重复 Begin 忽略，`ContentEnd` 也被忽略，
	/// 不会有第二个 writer 与旧 commit 竞争同一个 tmp 文件。
	fn handle_content_end(&mut self, req_id: String) {
		// 只处理 Receiving；正在 commit 的重复 End 直接忽略。
		let Some(ReceiverState::Receiving {
			inbox,
			handle,
			item_id,
			file_id,
			transfer_id,
			expected_sha256,
			received_bytes,
			total_bytes,
			started_ts_ms,
			mime,
			..
		}) = self.take_receiving(&req_id)
		else {
			return;
		};

		// 占位：commit 期间保持 req_id 被占用（也计入并发名额，因为 tmp/fd/Task 还在）。
		self.receivers.insert(
			req_id,
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
		// 独立任务：flush pending → Finish → 等 writer 完成 flush/close/commit/materialize。
		// 30s 超时保证卡死的磁盘不会永久占着 writer Task 与 tmp 文件。
		tokio::spawn(async move {
			let result = if let Ok(result) = tokio::time::timeout(
				CONTENT_END_TIMEOUT,
				finalize_receiver(inbox, expected_sha256.clone()),
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
			Ok(final_path) => {
				// 每个传输只写一条汇总（字节数 + 耗时），不再逐块记录
				let elapsed_ms = now_ms() - done.started_ts_ms;
				self.log_sink.log_info(
					"Session",
					&format!("Inbound transfer completed: transfer_id={}, bytes={}/{}, elapsed_ms={elapsed_ms}, sha256={}", done.transfer_id, done.received_bytes, done.total_bytes, done.expected_sha256),
					Some(&format!("入站传输成功完成: 传输ID={}，字节数={}/{}，耗时={elapsed_ms}ms，SHA256={}", done.transfer_id, done.received_bytes, done.total_bytes, done.expected_sha256)),
				);

				let store = self.store.clone();
				// 注意：ContentBegin.sha256 是本次传输片段的 hash。FileList 传输时它只是
				// 其中一个文件的 sha，不能用来标记 content_cache（见 mark_item_content_cached）。
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
					// 按 item 自身 content.sha256 标记缓存并保留命中行数
					let (marked_sha, marked_rows) =
						mark_item_content_cached(&mut guard, &iid, &transfer_sha, now_ms())?;

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
			item_id,
			transfer_id,
			..
		}) = self.take_receiving(req_id)
		{
			// abort writer Task（TempFileGuard 删除 tmp 文件），不 await 有界 channel。
			handle.abort();

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
			let file = File::open(&path).await?;
			let meta = file.metadata().await?;
			let total_bytes = meta.len();

			// [新增] 处理断点续传 offset
			let start_offset = offset.unwrap_or(0);

			// 发送 Header
			self.send_ctrl(CtrlMsg::ContentBegin {
				req_id: transfer_id.clone(),
				item_id,
				file_id,
				total_bytes,
				sha256, // 注意：这是整个文件的 Hash
				mime: mime_val,
			})
			.await?;

			// [新增] 启动独立任务读取文件
			let tx = self.upload_tx.clone();
			let tid = transfer_id.clone();

			let handle = tokio::spawn(async move {
				let mut file = match File::open(&path).await {
					Ok(f) => f,
					Err(e) => {
						let _ = tx
							.send(UploadMsg::Error {
								transfer_id: tid,
								err: e.to_string(),
							})
							.await;
						return;
					}
				};

				// 断点续传 Seek
				if start_offset > 0 {
					if let Err(_e) = file.seek(SeekFrom::Start(start_offset)).await {
						return;
					}
				}

				let mut reader = BufReader::new(file);
				let mut buf = vec![0u8; 64 * 1024]; // 64KB buffer
				let mut hasher = Sha256::new();

				loop {
					match reader.read(&mut buf).await {
						Ok(n) if n > 0 => {
							hasher.update(&buf[..n]);
							// 使用 Bytes::copy_from_slice 会发生一次内存拷贝，
							// 但对于 64KB chunk 来说开销可控。
							// 若极致优化可用 BytesMut，但此处保持简单即可。
							let data = bytes::Bytes::copy_from_slice(&buf[..n]);

							if tx
								.send(UploadMsg::Chunk {
									transfer_id: tid.clone(),
									data,
								})
								.await
								.is_err()
							{
								break; // Actor 挂了或取消了
							}
						}
						_ => break,
					}
				}

				// 这里计算的是“本次传输部分”的 Hash
				// 在 Resume 场景下，这与 ContentBegin 里的完整 Hash 不同
				// 接收端需要根据协议逻辑决定如何校验（目前 V3 简化版接收端已适配校验片段 Hash）
				let final_sha = hex::encode(hasher.finalize());
				let _ = tx
					.send(UploadMsg::Done {
						transfer_id: tid,
						sha256: final_sha,
					})
					.await;
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

	async fn start_opaque_login(&mut self) -> Result<()> {
		let mut rng = OsRng;
		let password = self.config.account_password.as_bytes();
		let start_result = CbClientLogin::start(&mut rng, password)
			.map_err(|e| anyhow::anyhow!("OPAQUE start failed: {e:?}"))?;
		self.opaque_client_state = Some(start_result.state);
		let payload = opaque_encode(&start_result.message)?;
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
		let password = self.config.account_password.as_bytes();
		let mut rng = OsRng;
		let server_response: opaque_ke::CredentialResponse<DefaultCipherSuite> =
			opaque_decode(response_bytes)
				.map_err(|_| anyhow::anyhow!("Invalid OpaqueResponse bytes"))?;
		let finish_result = client_state
			.finish(
				&mut rng,
				password,
				server_response,
				ClientLoginFinishParameters::default(),
			)
			.map_err(|e| anyhow::anyhow!("OPAQUE finish failed: {e:?}"))?;
		let payload = opaque_encode(&finish_result.message)?;
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
		let mut rng = OsRng;
		let identifier = b"clipbridge-user";
		let (server_setup, server_rec) =
			p2p_get_server_registration(&self.config.account_password)?;
		let client_message =
			opaque_decode(start_bytes).map_err(|_| anyhow::anyhow!("Invalid OpaqueStart bytes"))?;
		let start_result = CbServerLogin::start(
			&mut rng,
			&server_setup,
			Some(server_rec),
			client_message,
			identifier,
			ServerLoginParameters::default(),
		)
		.map_err(|e| anyhow::anyhow!("OPAQUE server start failed: {e:?}"))?;
		self.opaque_server_state = Some(start_result.state);
		let payload = opaque_encode(&start_result.message)?;
		self.send_ctrl(CtrlMsg::OpaqueResponse {
			msg_id: Some(uuid::Uuid::new_v4().to_string()),
			reply_to: None,
			opaque: payload,
		})
		.await?;
		self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueFinish));
		Ok(())
	}

	fn handle_opaque_finish(&mut self, finish_bytes: &[u8]) -> Result<()> {
		let server_state = self
			.opaque_server_state
			.take()
			.ok_or_else(|| anyhow::anyhow!("Protocol error: Missing server state"))?;
		let client_message = opaque_decode(finish_bytes)
			.map_err(|_| anyhow::anyhow!("Invalid OpaqueFinish bytes"))?;
		let _session_key = server_state
			.finish(client_message, ServerLoginParameters::default())
			.map_err(|e| anyhow::anyhow!("Authentication failed: {e:?}"))?;
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
}

#[cfg(test)]
mod auth_gate_tests {
	use super::{ctrl_is_pre_auth_allowed, ctrl_msg_kind};
	use crate::proto::{AuthSessionFlags, CtrlMsg, PROTOCOL_VERSION};

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
		chunk_overrun, hard_cap_bytes, kind_from_mime, mark_item_content_cached,
		receiver_idle_expired, receiver_slot_available, sender_admission, sender_slot_available,
		PushOutcome, ReceiverInbox, ReceiverTaskMsg, SenderState, TerminatedGuard,
		MAX_CONCURRENT_RECEIVERS, MAX_CONCURRENT_SENDERS, MAX_INFLIGHT_BYTES_PER_RECEIVER,
		RECEIVER_IDLE_TIMEOUT_MS,
	};
	use crate::model::{FileMeta, ItemContent, ItemKind, ItemMeta};
	use crate::policy::SizeLimits;
	use crate::proto::CtrlMsg;
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
			mark_item_content_cached(&mut store, &meta.item_id, &file_sha, now_ms()).unwrap();

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
			mark_item_content_cached(&mut store, "item-missing", &missing_sha, now_ms()).unwrap();
		assert_eq!(marked_sha, missing_sha);
		assert_eq!(rows, 0);
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
		let mut inbox = ReceiverInbox::new(tx, budget);

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
}
