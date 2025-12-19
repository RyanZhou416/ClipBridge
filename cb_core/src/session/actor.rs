// cb_core/src/session/actor.rs

use std::sync::{Arc, Mutex};
use std::time::Duration;
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::api::{CoreConfig, CoreEventSink};
use crate::proto::{CBFrameCodec, CtrlMsg, PROTOCOL_VERSION, AuthSessionFlags};
use crate::transport::{Connection, SendStream, RecvStream};
use crate::store::Store;
use crate::util::{now_ms, sha256_hex};
use super::{SessionCmd, SessionHandle, SessionRole, SessionState, HandshakeStep};

// 心跳配置
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(6);

pub struct SessionActor {
    role: SessionRole,
    // conn: Connection, // 如果 Actor 运行中不需要操作 Connection 本身，可不持有

    // 读写分离
    writer: FramedWrite<SendStream, CBFrameCodec>,
    reader: FramedRead<RecvStream, CBFrameCodec>,

    config: CoreConfig,
    sink: Arc<dyn CoreEventSink>,

    // 状态管理
    state_ref: Arc<Mutex<SessionState>>, // 暴露给 Handle 的引用
    state: SessionState,                 // Actor 内部状态副本

    remote_device_id: Option<String>,
    remote_fingerprint: String, // TLS 指纹 (SHA256 Hex)
    last_active_at: i64,

    cmd_rx: mpsc::Receiver<SessionCmd>,
}

impl SessionActor {
    pub fn spawn(
        role: SessionRole,
        conn: Connection,
        config: CoreConfig,
        sink: Arc<dyn CoreEventSink>,
    ) -> SessionHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let state_ref = Arc::new(Mutex::new(SessionState::TransportReady));

        // 1. 提取 TLS 指纹 (TOFU 用)
        let fingerprint = match conn.peer_identity() {
            Some(id) => {
                let certs = id.downcast::<Vec<rustls::pki_types::CertificateDer>>().unwrap_or_default();
                if let Some(cert) = certs.first() {
                    sha256_hex(cert.as_ref())
                } else {
                    "unknown".to_string()
                }
            }
            None => "unknown".to_string(),
        };

        let initial_did = match role {
            SessionRole::Client => "pending_client".to_string(),
            SessionRole::Server => "pending_server".to_string(),
        };

        let handle = SessionHandle {
            device_id: initial_did,
            cmd_tx,
            state: state_ref.clone(),
        };

        // 启动 Actor 任务
        let actor_device_id_handle = handle.device_id.clone(); // 用于日志或调试
        tokio::spawn(async move {
            if let Err(e) = Self::run_actor(role, conn, config, sink, state_ref, cmd_rx, fingerprint).await {
                // 这里可以打一些 debug log
                eprintln!("[Session] Actor {} error: {:?}", actor_device_id_handle, e);
            }
        });

        handle
    }

    /// 内部静态方法来初始化并运行 Actor
    async fn run_actor(
        role: SessionRole,
        conn: Connection,
        config: CoreConfig,
        sink: Arc<dyn CoreEventSink>,
        state_ref: Arc<Mutex<SessionState>>,
        cmd_rx: mpsc::Receiver<SessionCmd>,
        fingerprint: String,
    ) -> Result<()> {
        // 1. 建立双向 Control Stream
        let (send, recv) = match role {
            SessionRole::Client => conn.open_bi().await.context("Client open_bi failed")?,
            SessionRole::Server => conn.accept_bi().await.context("Server accept_bi failed")?,
        };

        let writer = FramedWrite::new(send, CBFrameCodec);
        let reader = FramedRead::new(recv, CBFrameCodec);

        let mut actor = Self {
            role,
            // conn,
            writer,
            reader,
            config,
            sink,
            state_ref,
            state: SessionState::TransportReady,
            remote_device_id: None,
            remote_fingerprint: fingerprint,
            last_active_at: now_ms(),
            cmd_rx,
        };

        // 2. 开始握手
        // 如果握手失败，直接返回，不需要触发后续的 PEER_OFFLINE (因为还没 Online 过)
        actor.start_handshake().await?;

        // 3. 主循环 (包裹在一个 block 中以捕获结果)
        let mut heartbeat_ticker = interval(HEARTBEAT_INTERVAL);
        heartbeat_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let run_result: Result<()> = async {
            loop {
                tokio::select! {
                    // A. 接收网络消息
                    msg = actor.reader.next() => {
                        match msg {
                            Some(Ok(m)) => {
                                actor.last_active_at = now_ms();
                                if let Err(e) = actor.handle_msg(m).await {
                                    actor.send_error_and_close("PROTOCOL_ERROR", &e.to_string()).await?;
                                    return Err(e);
                                }
                            }
                            Some(Err(e)) => return Err(e.into()), // Codec 错误
                            None => break, // 连接关闭
                        }
                    }

                    // B. 接收本地命令
                    cmd = actor.cmd_rx.recv() => {
                        match cmd {
                            Some(SessionCmd::SendMeta(meta)) => {
                                if actor.state == SessionState::Online {
                                    actor.writer.send(CtrlMsg::ItemMeta { item: meta }).await?;
                                }
                            }
                            Some(SessionCmd::Shutdown) => {
                                let _ = actor.writer.send(CtrlMsg::Close { reason: "Shutdown".into() }).await;
                                break;
                            }
                            None => break,
                        }
                    }

                    // C. 心跳检查
                    _ = heartbeat_ticker.tick() => {
                        actor.tick_heartbeat().await?;
                    }
                }
            }
            Ok(())
        }.await;

        // 4. 【关键修正】无论 run_result 是 Ok 还是 Err，这里都会执行
        // 确保状态更新为 Terminated
        actor.update_state(SessionState::Terminated);

        // 发送 PEER_OFFLINE 通知
        if let Some(did) = &actor.remote_device_id {
            // 区分一下是正常关闭还是报错
            let reason = match &run_result {
                Ok(_) => "Connection closed".to_string(),
                Err(e) => format!("Error: {}", e),
            };

            let json = serde_json::json!({
                "type": "PEER_OFFLINE",
                "ts_ms": now_ms(),
                "payload": { "device_id": did, "reason": reason }
            });
            actor.sink.emit(json.to_string());
        }

        // 最后再返回结果
        run_result
    }

    // --- 状态辅助 ---
    fn update_state(&mut self, new_state: SessionState) {
        // [新增] 打印状态变更日志，方便调试
        let role_str = if self.role == SessionRole::Client { "Cli" } else { "Srv" };
        println!("[Session::{}] State -> {:?}", role_str, new_state);

        self.state = new_state.clone();
        let mut s = self.state_ref.lock().unwrap();
        *s = new_state;
    }

    // --- 握手流程 ---
    async fn start_handshake(&mut self) -> Result<()> {
        match self.role {
            SessionRole::Client => {
                // Client 发 HELLO
                self.update_state(SessionState::Handshaking(HandshakeStep::SendingHello));
                let msg = CtrlMsg::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    device_id: self.config.device_id.clone(),
                    account_tag: self.config.account_tag.clone(),
                    capabilities: vec!["text".into(), "image".into(), "file".into()],
                    client_nonce: Some(uuid::Uuid::new_v4().to_string()),
                };
                self.writer.send(msg).await?;
                self.update_state(SessionState::Handshaking(HandshakeStep::WaitingForHelloAck));
            }
            SessionRole::Server => {
                self.update_state(SessionState::Handshaking(HandshakeStep::WaitingForHello));
            }
        }
        Ok(())
    }

    async fn handle_msg(&mut self, msg: CtrlMsg) -> Result<()> {
        match msg {
            // --- 握手消息 ---
            CtrlMsg::Hello { device_id, account_tag, .. } => {
                if self.role == SessionRole::Server {
                    // 1. 验证 Tag
                    if account_tag != self.config.account_tag {
                        self.send_error_and_close("AUTH_ACCOUNT_TAG_MISMATCH", "Tag mismatch").await?;
                        anyhow::bail!("Auth failed: tag mismatch");
                    }
                    self.remote_device_id = Some(device_id);

                    // 2. 发 ACK
                    self.writer.send(CtrlMsg::HelloAck {
                        server_device_id: self.config.device_id.clone(),
                        protocol_version: PROTOCOL_VERSION,
                    }).await?;

                    // 3. 转状态 -> OpaqueStart
                    self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueStart));
                }
            }

            CtrlMsg::HelloAck { server_device_id, .. } => {
                if self.role == SessionRole::Client {
                    self.remote_device_id = Some(server_device_id);
                    // Mock OPAQUE 1: Client -> Server (Start)
                    self.writer.send(CtrlMsg::OpaqueStart { opaque: "mock_ke1".into() }).await?;
                    self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueResponse));
                }
            }

            CtrlMsg::OpaqueStart { .. } => {
                // Server 收到 Start -> 发 Response
                self.writer.send(CtrlMsg::OpaqueResponse { opaque: "mock_ke2".into() }).await?;
                self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueFinish));
            }

            CtrlMsg::OpaqueResponse { .. } => {
                // Client 收到 Response -> 发 Finish
                self.writer.send(CtrlMsg::OpaqueFinish { opaque: "mock_ke3".into() }).await?;
                self.update_state(SessionState::Handshaking(HandshakeStep::WaitingAuthOk));
            }

            CtrlMsg::OpaqueFinish { .. } => {
                // Server 收到 Finish -> 验证 -> TOFU -> AuthOk
                self.perform_tofu_check()?;
                self.writer.send(CtrlMsg::AuthOk {
                    session_flags: AuthSessionFlags { account_verified: true }
                }).await?;
                self.set_online().await;
            }

            CtrlMsg::AuthOk { .. } => {
                // Client 收到 OK -> TOFU -> Online
                self.perform_tofu_check()?;
                self.set_online().await;
            }

            CtrlMsg::AuthFail { error_code } => {
                anyhow::bail!("Remote AuthFail: {}", error_code);
            }

            // --- 心跳 ---
            CtrlMsg::Ping { ts } => {
                self.writer.send(CtrlMsg::Pong { ts }).await?;
            }
            CtrlMsg::Pong { .. } => {
                // last_active_at 已在 handle_msg 入口更新
            }

            // --- 业务 ---
            CtrlMsg::ItemMeta { item } => {
                if self.state == SessionState::Online {
                    let json = serde_json::json!({
                        "type": "ITEM_META_ADDED",
                        "ts_ms": now_ms(),
                        "payload": { "meta": item }
                    });
                    self.sink.emit(json.to_string());
                }
            }

            CtrlMsg::Error { error_code, message } => {
                anyhow::bail!("Remote error {}: {:?}", error_code, message);
            }
            CtrlMsg::Close { .. } => {
                anyhow::bail!("Remote closed connection");
            }
            _ => {}
        }
        Ok(())
    }

    // --- 核心辅助 ---

    fn perform_tofu_check(&self) -> Result<()> {
        // 在 tokio 线程中操作 DB 并非最佳实践，但对 M1 MVP 是可接受的。
        // 如有性能问题，可改为 tokio::task::spawn_blocking
        let store = Store::open(&self.config.data_dir)?;
        let uid = &self.config.account_uid;
        let did = self.remote_device_id.as_ref().context("missing remote device id")?;

        match store.get_peer_fingerprint(uid, did)? {
            Some(saved_fp) => {
                if saved_fp != self.remote_fingerprint {
                    anyhow::bail!("TLS_PIN_MISMATCH: saved={}, got={}", saved_fp, self.remote_fingerprint);
                }
            }
            None => {
                // TOFU: 首次信任
                // 注意：Store 需要 mut，这里简单重新 open 一个写连接（Store::open 成本在 SQLite 中主要是文件句柄，MVP 可接受）
                // 生产环境建议通过 NetManager 传入 Arc<Mutex<Store>>
                let mut store_mut = Store::open(&self.config.data_dir)?;
                store_mut.save_peer_fingerprint(uid, did, &self.remote_fingerprint, now_ms())?;
                println!("[Session] TOFU pinned device {} with fp {}", did, self.remote_fingerprint);
            }
        }
        Ok(())
    }

    async fn set_online(&mut self) {
        self.update_state(SessionState::Online);
        if let Some(did) = &self.remote_device_id {
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
            self.send_error_and_close("TIMEOUT", "Heartbeat timeout").await?;
            anyhow::bail!("Heartbeat timeout");
        }
        if self.state == SessionState::Online {
            self.writer.send(CtrlMsg::Ping { ts: now_ms() }).await?;
        }
        Ok(())
    }

    async fn send_error_and_close(&mut self, code: &str, msg: &str) -> Result<()> {
        let _ = self.writer.send(CtrlMsg::Error {
            error_code: code.into(),
            message: Some(msg.into()),
        }).await;
        Ok(())
    }


}

