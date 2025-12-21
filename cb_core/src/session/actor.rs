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

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(6);

pub struct SessionActor {
    role: SessionRole,
    writer: FramedWrite<SendStream, CBFrameCodec>,
    reader: FramedRead<RecvStream, CBFrameCodec>,
    config: CoreConfig,
    sink: Arc<dyn CoreEventSink>,
    state_ref: Arc<Mutex<SessionState>>,
    peer_id_ref: Arc<Mutex<Option<String>>>,
    state: SessionState,
    remote_device_id: Option<String>,
    remote_fingerprint: String,
    last_active_at: i64,
    cmd_rx: mpsc::Receiver<SessionCmd>,
}

impl SessionActor {
    pub fn spawn(
        role: SessionRole,
        conn: Connection,
        config: CoreConfig,
        sink: Arc<dyn CoreEventSink>,
        expected_peer_id: Option<String>,
    ) -> SessionHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let state_ref = Arc::new(Mutex::new(SessionState::TransportReady));
        let peer_id_ref = Arc::new(Mutex::new(None));

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

        let initial_did = match expected_peer_id {
            Some(id) => id,
            None => "pending_server".to_string(),
        };

        let handle = SessionHandle {
            initial_id: initial_did.clone(),
            peer_id: peer_id_ref.clone(),
            state: state_ref.clone(),
            cmd_tx,
        };

        let actor_log_id = initial_did.clone();
        let state_ref_clone = state_ref.clone();
        let peer_id_ref_clone = peer_id_ref.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::run_actor(
                role,
                conn,
                config,
                sink,
                state_ref_clone,
                peer_id_ref_clone,
                cmd_rx,
                fingerprint
            ).await {
                eprintln!("[Session] Actor {} error: {:?}", actor_log_id, e);
            }
        });
        handle
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_actor(
        role: SessionRole,
        conn: Connection,
        config: CoreConfig,
        sink: Arc<dyn CoreEventSink>,
        state_ref: Arc<Mutex<SessionState>>,
        peer_id_ref: Arc<Mutex<Option<String>>>,
        cmd_rx: mpsc::Receiver<SessionCmd>,
        fingerprint: String,
    ) -> Result<()> {
        let (send, recv) = match role {
            SessionRole::Client => conn.open_bi().await.context("Client open_bi failed")?,
            SessionRole::Server => conn.accept_bi().await.context("Server accept_bi failed")?,
        };

        let writer = FramedWrite::new(send, CBFrameCodec);
        let reader = FramedRead::new(recv, CBFrameCodec);

        let mut actor = Self {
            role,
            writer,
            reader,
            config,
            sink,
            state_ref,
            peer_id_ref,
            state: SessionState::TransportReady,
            remote_device_id: None,
            remote_fingerprint: fingerprint,
            last_active_at: now_ms(),
            cmd_rx,
        };

        actor.start_handshake().await?;

        let mut heartbeat_ticker = interval(HEARTBEAT_INTERVAL);
        heartbeat_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let run_result: Result<()> = async {
            loop {
                tokio::select! {
                    msg = actor.reader.next() => {
                        match msg {
                            Some(Ok(m)) => {
                                actor.last_active_at = now_ms();
                                if let Err(e) = actor.handle_msg(m).await {
                                    // 如果 handle_msg 内部已经处理了业务错误（如 TOFU 失败），它应该已经发了 Error 帧。
                                    // 这里我们为了保险，如果是 codec 解析错误等底层错误，才发 PROTOCOL_ERROR。
                                    // 但区分起来比较麻烦。

                                    // 简单策略：仅记录日志。我们假设 handle_msg 里的逻辑会负责“体面地拒绝”。
                                    // 如果 handle_msg 只是单纯 crash，这里发一个 Generic Error 也没问题。
                                    println!("[Session] Error handling msg: {:?}", e);
                                    return Err(e);
                                }
                            }
                            Some(Err(e)) => return Err(e.into()),
                            None => break,
                        }
                    }
                    cmd = actor.cmd_rx.recv() => {
                        match cmd {
                            Some(SessionCmd::SendMeta(meta)) => {
                                if actor.state == SessionState::Online {
                                    actor.writer.send(CtrlMsg::ItemMeta {
                                        msg_id: Some(uuid::Uuid::new_v4().to_string()),
                                        item: meta
                                    }).await?;
                                }
                            }
                            Some(SessionCmd::Shutdown) => {
                                let _ = actor.writer.send(CtrlMsg::Close {
                                    msg_id: Some(uuid::Uuid::new_v4().to_string()),
                                    reason: "Shutdown".into()
                                }).await;
                                break;
                            }
                            None => break,
                        }
                    }
                    _ = heartbeat_ticker.tick() => {
                        actor.tick_heartbeat().await?;
                    }
                }
            }
            Ok(())
        }.await;

        actor.update_state(SessionState::Terminated);
        if let Some(did) = &actor.remote_device_id {
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
        run_result
    }

    fn update_state(&mut self, new_state: SessionState) {
        // Only log major transitions
        match (&self.state, &new_state) {
            (SessionState::Handshaking(_), SessionState::AccountVerified) => {
                println!("[Session] Handshake OK, Account Verified. Checking TOFU...");
            }
            (SessionState::AccountVerified, SessionState::Online) => {
                println!("[Session] TOFU OK. Session ONLINE.");
            }
            _ => {}
        }
        self.state = new_state.clone();
        let mut s = self.state_ref.lock().unwrap();
        *s = new_state;
    }

    fn update_remote_id(&mut self, id: String) {
        self.remote_device_id = Some(id.clone());
        let mut lock = self.peer_id_ref.lock().unwrap();
        *lock = Some(id);
    }

    async fn start_handshake(&mut self) -> Result<()> {
        match self.role {
            SessionRole::Client => {
                self.update_state(SessionState::Handshaking(HandshakeStep::SendingHello));
                let msg = CtrlMsg::Hello {
                    msg_id: Some(uuid::Uuid::new_v4().to_string()),
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
            CtrlMsg::Hello { device_id, account_tag, msg_id, .. } => {
                if self.role == SessionRole::Server {
                    if account_tag != self.config.account_tag {
                        let _ = self.writer.send(CtrlMsg::AuthFail {
                            reply_to: msg_id.clone(),
                            error_code: "AUTH_ACCOUNT_TAG_MISMATCH".into(),
                        }).await;
                        let _ = self.writer.send(CtrlMsg::Close {
                            msg_id: None,
                            reason: "Auth failed".into(),
                        }).await;
                        anyhow::bail!("Auth failed: tag mismatch");
                    }

                    self.update_remote_id(device_id);
                    self.writer.send(CtrlMsg::HelloAck {
                        reply_to: msg_id,
                        server_device_id: self.config.device_id.clone(),
                        protocol_version: PROTOCOL_VERSION,
                    }).await?;
                    self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueStart));
                }
            }

            CtrlMsg::HelloAck { server_device_id, reply_to, .. } => {
                if self.role == SessionRole::Client {
                    self.update_remote_id(server_device_id);
                    self.writer.send(CtrlMsg::OpaqueStart {
                        msg_id: Some(uuid::Uuid::new_v4().to_string()),
                        reply_to: reply_to,
                        opaque: "mock_ke1".into()
                    }).await?;
                    self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueResponse));
                }
            }

            CtrlMsg::OpaqueStart { msg_id, .. } => {
                self.writer.send(CtrlMsg::OpaqueResponse {
                    reply_to: msg_id,
                    msg_id: Some(uuid::Uuid::new_v4().to_string()),
                    opaque: "mock_ke2".into()
                }).await?;
                self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueFinish));
            }

            CtrlMsg::OpaqueResponse { msg_id, .. } => {
                self.writer.send(CtrlMsg::OpaqueFinish {
                    reply_to: msg_id,
                    msg_id: Some(uuid::Uuid::new_v4().to_string()),
                    opaque: "mock_ke3".into()
                }).await?;
                self.update_state(SessionState::Handshaking(HandshakeStep::WaitingAuthOk));
            }

            // --- Server End of Handshake ---
            CtrlMsg::OpaqueFinish { msg_id, .. } => {
                // 1. 状态流转：账号已验证
                self.update_state(SessionState::AccountVerified);

                // 2. 执行 TOFU 检查 (Policy Check)
                // [修改重点]：这里不再直接用 '?' 抛出错误，而是捕获错误进行处理
                if let Err(e) = self.perform_tofu_check_async().await {
                    let err_str = e.to_string();

                    // 根据错误内容决定发送给对方的错误码
                    let error_code = if err_str.contains("TLS_PIN_MISMATCH") {
                        "TLS_PIN_MISMATCH" // 明确告诉对方：指纹不对
                    } else {
                        "POLICY_REJECT"    // 其他策略拒绝
                    };

                    // 先发送具体的错误消息给对方
                    // (复用你已有的 send_error_and_close 或手动发送 CtrlMsg::Error)
                    let _ = self.writer.send(CtrlMsg::Error {
                        reply_to: msg_id.clone(), // 回复这条请求
                        error_code: error_code.into(),
                        message: Some(err_str),
                    }).await;

                    // 发完消息后，再返回 Err，触发本地断开
                    return Err(e);
                }

                // 3. 如果没出错，才发送 AuthOk
                self.writer.send(CtrlMsg::AuthOk {
                    reply_to: msg_id,
                    session_flags: AuthSessionFlags { account_verified: true }
                }).await?;

                // 4. 上线
                self.set_online().await;
            }

            // --- Client End of Handshake ---
            CtrlMsg::AuthOk { .. } => {
                // 1. Account Verified (Server said OK)
                self.update_state(SessionState::AccountVerified);

                // 2. Perform TOFU Check
                self.perform_tofu_check_async().await?;

                // 3. Go Online
                self.set_online().await;
            }

            CtrlMsg::AuthFail { error_code, .. } => anyhow::bail!("Remote AuthFail: {}", error_code),

            CtrlMsg::Ping { ts, msg_id } => {
                self.writer.send(CtrlMsg::Pong { reply_to: msg_id, ts }).await?;
            }
            CtrlMsg::Pong { .. } => {},

            CtrlMsg::ItemMeta { item, .. } => {
                if self.state == SessionState::Online {
                    let json = serde_json::json!({
                        "type": "ITEM_META_ADDED",
                        "ts_ms": now_ms(),
                        "payload": { "meta": item }
                    });
                    self.sink.emit(json.to_string());
                }
            }

            CtrlMsg::Error { error_code, message, .. } => anyhow::bail!("Remote error {}: {:?}", error_code, message),
            CtrlMsg::Close { .. } => anyhow::bail!("Remote closed connection"),
        }
        Ok(())
    }

    async fn perform_tofu_check_async(&self) -> Result<()> {
        let data_dir = self.config.data_dir.clone();
        let uid = self.config.account_uid.clone();
        let did = self.remote_device_id.clone().context("missing remote device id")?;
        let rfp = self.remote_fingerprint.clone();

        tokio::task::spawn_blocking(move || {
            let store = Store::open(&data_dir)?;
            match store.get_peer_fingerprint(&uid, &did)? {
                Some(saved_fp) => {
                    if saved_fp != rfp {
                        anyhow::bail!("TLS_PIN_MISMATCH: saved={}, got={}", saved_fp, rfp);
                    }
                }
                None => {
                    let mut store_mut = Store::open(&data_dir)?;
                    store_mut.save_peer_fingerprint(&uid, &did, &rfp, now_ms())?;
                    println!("[Session] TOFU pinned device {} with fp {}", did, rfp);
                }
            }
            Ok(())
        }).await?
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
            let _ = self.writer.send(CtrlMsg::Error {
                reply_to: None,
                error_code: "TIMEOUT".into(),
                message: Some("Heartbeat timeout".into())
            }).await;
            anyhow::bail!("Heartbeat timeout");
        }
        if self.state == SessionState::Online {
            self.writer.send(CtrlMsg::Ping {
                msg_id: Some(uuid::Uuid::new_v4().to_string()),
                ts: now_ms()
            }).await?;
        }
        Ok(())
    }

    async fn send_error_and_close(&mut self, code: &str, msg: &str) -> Result<()> {
        let _ = self.writer.send(CtrlMsg::Error {
            reply_to: None,
            error_code: code.into(),
            message: Some(msg.into()),
        }).await;
        Ok(())
    }
}