// cb_core/src/session/mod.rs

mod actor;
pub use actor::SessionActor; // 导出 Actor 供 NetManager 使用

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 会话状态 (文档 2.2.6 + 握手细化)
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    /// 传输层已建立，等待 TLS 握手完成拿到指纹
    TransportReady,
    /// 正在进行应用层握手 (Hello / OPAQUE)
    Handshaking(HandshakeStep),
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
    // OPAQUE 3-Step (M1 Mock)
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
    pub device_id: String,
    pub(crate) cmd_tx: mpsc::Sender<SessionCmd>,
    // 状态缓存，用于快速同步读取，避免 await
    state: Arc<Mutex<SessionState>>,
}

impl SessionHandle {
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(SessionCmd::Shutdown).await;
    }

    pub fn is_online(&self) -> bool {
        let s = self.state.lock().unwrap();
        matches!(*s, SessionState::Online)
    }
}