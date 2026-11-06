// cb_core/src/session.rs

mod actor;
pub use actor::SessionActor; // 导出 Actor 供 NetManager 使用

use crate::api::PeerConnectionState;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 会话状态 (文档 2.2.6 + 握手细化)
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
	/// 传输层已建立，等待 TLS 握手完成拿到指纹
	TransportReady,
	/// 正在进行应用层握手 (Hello / OPAQUE)
	Handshaking(HandshakeStep),
	/// 账号已验证 (OPAQUE 完成)，正在检查或写入设备指纹
	AccountVerified,
	/// 握手完成，连接健康，可交换数据
	Online,
	/// 连接已关闭
	Terminated,
}

/// 握手子状态
#[derive(Debug, Clone, PartialEq)]
pub enum HandshakeStep {
	SendingHello,
	WaitingForHello,
	WaitingForHelloAck,
	OpaqueStart,    // Client 发 ke1, Server 等 ke1
	OpaqueResponse, // Server 发 ke2, Client 等 ke2
	OpaqueFinish,   // Client 发 ke3, Server 等 ke3
	WaitingAuthOk,  // Client 等 AuthOk
}

/// 会话角色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
	Client, // 主动发起连接方
	Server, // 接受连接方
}

/// 会话种类（C1，ADR-002 §4.3）。
///
/// - `Control`：每 peer 唯一的控制会话（控制流 + 元数据 + 心跳 + 小传输）。
///   它是 `NetManager::sessions` / `list_peers` / mDNS 语义里的"这个 peer"。
/// - `Data`：C1 新增的数据连接会话。协议握手/认证与控制会话完全相同，但
///   **不**对外发出 `PEER_ONLINE` / `PEER_OFFLINE` 事件（否则外壳会看到同一
///   设备反复上下线），也不参与元数据广播；它只承载被显式分配过来的传输。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
	Control,
	Data,
}

/// 单条连接的实时负载（供 C1 连接池做"最不忙优先"选择）。
///
/// 只统计**该连接上当前活跃的传输数**（`receivers + senders`），不统计字节数：
/// 字节进度在数据流任务里，跨任务汇总 eprintln 级别的成本不值得；传输数已经
/// 足以把并发传输摊开到不同连接上（任务 C1 第 4 条允许"字节在途 / 活跃传输数"
/// 二选一，这里选后者并在文档中固定语义）。
#[derive(Debug, Default)]
pub struct TransferLoad {
	active: AtomicUsize,
}

impl TransferLoad {
	pub fn active(&self) -> usize {
		self.active.load(Ordering::Relaxed)
	}

	/// 由 `SessionActor` 在每次状态推进后写入最新计数。
	pub(crate) fn store(&self, value: usize) {
		self.active.store(value, Ordering::Relaxed);
	}
}

/// 发送给 Session Actor 的命令
#[derive(Debug)]
pub enum SessionCmd {
	/// 发送元数据
	SendMeta(Box<crate::model::ItemMeta>),
	/// 关闭会话
	Shutdown,
	/// 请求向对端拉取文件 (B 端发起)
	RequestTransfer {
		item_id: String,
		file_id: Option<String>,
		reply_tx: tokio::sync::oneshot::Sender<anyhow::Result<String>>, // 返回 transfer_id
	},
	/// 取消本地发起的传输 (B 端取消)
	CancelTransfer { transfer_id: String },
	/// 发送删除：向对端发送 ItemDelete
	SendDelete { item_id: String },
}

/// Session 对外暴露的句柄 (线程安全)
#[derive(Clone, Debug)]
pub struct SessionHandle {
	/// 初始 ID (pending_xxx)
	pub initial_id: String,
	/// 真实的对端设备 ID (握手成功后更新)
	pub peer_id: Arc<Mutex<Option<String>>>,
	/// 会话状态
	pub state: Arc<Mutex<SessionState>>,

	pub cmd_tx: mpsc::Sender<SessionCmd>,

	/// 会话种类（控制 / 数据）。
	pub kind: SessionKind,
	/// 对端地址（握手前即可用；数据连接池用其中的 IP 拼出对端数据端口）。
	///
	/// 注意：这是 QUIC 连接的 `remote_address()`，端口是对端的**临时源端口**
	/// （控制连接时不是对方监听端口）。C1 只用它的 IP 部分，数据端口来自
	/// Hello/HelloAck 的 `data_ports` 通告。
	pub remote_addr: Option<SocketAddr>,
	/// 对端在 Hello/HelloAck 中通告的数据端口（按对方给出的顺序）。
	pub peer_data_ports: Arc<Mutex<Vec<u16>>>,
	/// 本连接当前活跃传输数（见 [`TransferLoad`]）。
	pub load: Arc<TransferLoad>,
}

impl SessionHandle {
	/// 获取当前最好的设备标识
	pub fn device_id(&self) -> String {
		let lock = self.peer_id.lock().unwrap();
		// 如果有握手后的真实 ID，用真实的；否则用初始的
		lock.clone().unwrap_or_else(|| self.initial_id.clone())
	}

	pub fn is_online(&self) -> bool {
		let s = self.state.lock().unwrap();
		matches!(*s, SessionState::Online)
	}

	pub fn is_finished(&self) -> bool {
		let s = self.state.lock().unwrap();
		matches!(*s, SessionState::Terminated)
	}

	/// 对端通告的数据端口快照。
	pub fn peer_data_ports(&self) -> Vec<u16> {
		self.peer_data_ports.lock().unwrap().clone()
	}

	/// 映射内部状态到外部 API 状态
	pub fn public_state(&self) -> PeerConnectionState {
		let s = self.state.lock().unwrap().clone();
		match s {
			SessionState::TransportReady => PeerConnectionState::TransportReady,

			SessionState::Handshaking(_) => PeerConnectionState::AccountVerifying,

			SessionState::AccountVerified => PeerConnectionState::AccountVerified,
			SessionState::Online => PeerConnectionState::Online,

			// actor 结束了，按文档对外是 Offline（是否 Backoff 由 NetManager 决定）
			SessionState::Terminated => PeerConnectionState::Offline,
		}
	}

	pub async fn shutdown(&self) {
		let _ = self.cmd_tx.send(SessionCmd::Shutdown).await;
	}

	/// 仅测试使用：构造一个最小可用句柄（不启动 actor、不持有连接）。
	#[cfg(test)]
	pub(crate) fn for_test(device_id: &str) -> Self {
		let (cmd_tx, cmd_rx) = mpsc::channel(4);
		// 保留接收端存活，否则 `cmd_tx.is_closed()` 会立即为 true，
		// 连接池会把测试句柄当成死连接。测试里泄漏一个 4 槽 channel 无关紧要。
		std::mem::forget(cmd_rx);
		Self {
			initial_id: device_id.to_string(),
			peer_id: Arc::new(Mutex::new(Some(device_id.to_string()))),
			state: Arc::new(Mutex::new(SessionState::Online)),
			cmd_tx,
			kind: SessionKind::Data,
			remote_addr: None,
			peer_data_ports: Arc::new(Mutex::new(Vec::new())),
			load: Arc::new(TransferLoad::default()),
		}
	}
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod transfer_load_tests {
	use super::TransferLoad;

	#[test]
	fn transfer_load_tracks_active_count() {
		let load = TransferLoad::default();
		assert_eq!(load.active(), 0);
		load.store(3);
		assert_eq!(load.active(), 3);
		load.store(0);
		assert_eq!(load.active(), 0);
	}
}
