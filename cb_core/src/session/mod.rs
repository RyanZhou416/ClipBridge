// cb_core/src/session/mod.rs

mod actor;
pub use actor::SessionActor; // 导出 Actor 供 NetManager 使用

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use crate::api::PeerConnectionState;

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

/// 发送给 Session Actor 的命令
#[derive(Debug)]
pub enum SessionCmd {
    SendMeta(crate::model::ItemMeta), // 发送元数据
    Shutdown,                         // 关闭会话
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

    /// 映射内部状态到外部 API 状态
    pub fn public_state(&self) -> PeerConnectionState {
        let s = self.state.lock().unwrap();
        match *s {
            SessionState::TransportReady => PeerConnectionState::Connecting,
            SessionState::Handshaking(_) => PeerConnectionState::Handshaking,
            SessionState::AccountVerified => PeerConnectionState::AccountVerified,
            SessionState::Online => PeerConnectionState::Online,
            SessionState::Terminated => PeerConnectionState::Offline,
        }
    }

    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(SessionCmd::Shutdown).await;
    }
}

#[cfg(test)]
mod tests;