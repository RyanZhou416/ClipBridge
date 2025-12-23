use std::sync::{Arc, Mutex};
use std::time::Duration;
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::api::{CoreConfig, CoreEventSink};
use crate::crypto::{
    CbClientLogin, CbServerLogin,
    CbClientLoginState, CbServerLoginState,
    p2p_get_server_registration, DefaultCipherSuite
};
// 只引入存在的结构体
use opaque_ke::{
    ClientLoginFinishParameters,
    ServerLoginStartParameters
};
use rand::rngs::OsRng;

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
    config: Arc<CoreConfig>,
    sink: Arc<dyn CoreEventSink>,
    state_ref: Arc<Mutex<SessionState>>,
    peer_id_ref: Arc<Mutex<Option<String>>>,
    state: SessionState,
    remote_device_id: Option<String>,
    remote_fingerprint: String,
    last_active_at: i64,
    cmd_rx: mpsc::Receiver<SessionCmd>,

    opaque_client_state: Option<CbClientLoginState>,
    opaque_server_state: Option<CbServerLoginState>,
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
        let config_arc = Arc::new(config);

        tokio::spawn(async move {
            if let Err(e) = Self::run_actor(
                role,
                conn,
                config_arc,
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
        config: Arc<CoreConfig>,
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
            opaque_client_state: None,
            opaque_server_state: None,
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
                                    actor.send_ctrl(CtrlMsg::ItemMeta {
                                        msg_id: Some(uuid::Uuid::new_v4().to_string()),
                                        item: meta
                                    }).await?;
                                }
                            }
                            Some(SessionCmd::Shutdown) => {
                                let _ = actor.send_ctrl(CtrlMsg::Close {
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
        self.state = new_state.clone();
        let mut s = self.state_ref.lock().unwrap();
        *s = new_state;
    }

    fn update_remote_id(&mut self, id: String) {
        self.remote_device_id = Some(id.clone());
        let mut lock = self.peer_id_ref.lock().unwrap();
        *lock = Some(id);
    }

    async fn send_ctrl(&mut self, msg: CtrlMsg) -> Result<()> {
        self.writer.send(msg).await.context("Failed to send CtrlMsg")
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
                self.send_ctrl(msg).await?;
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
                        let _ = self.send_ctrl(CtrlMsg::AuthFail {
                            reply_to: msg_id.clone(),
                            error_code: "AUTH_ACCOUNT_TAG_MISMATCH".into(),
                        }).await;
                        let _ = self.send_ctrl(CtrlMsg::Close {
                            msg_id: None,
                            reason: "Auth failed".into(),
                        }).await;
                        anyhow::bail!("Auth failed: tag mismatch");
                    }

                    self.update_remote_id(device_id);
                    self.send_ctrl(CtrlMsg::HelloAck {
                        reply_to: msg_id,
                        server_device_id: self.config.device_id.clone(),
                        protocol_version: PROTOCOL_VERSION,
                    }).await?;
                    self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueStart));
                }
            }

            CtrlMsg::HelloAck { server_device_id, .. } => {
                if self.role == SessionRole::Client {
                    self.update_remote_id(server_device_id);
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

            CtrlMsg::OpaqueFinish { msg_id, opaque: bytes, .. } => {
                if self.role == SessionRole::Server {
                    self.handle_opaque_finish(&bytes).await?;

                    if let Err(e) = self.perform_tofu_check_async().await {
                        let _ = self.send_ctrl(CtrlMsg::Error {
                            reply_to: msg_id.clone(),
                            error_code: "POLICY_REJECT".into(),
                            message: Some(e.to_string()),
                        }).await;
                        return Err(e);
                    }

                    self.send_ctrl(CtrlMsg::AuthOk {
                        reply_to: msg_id,
                        session_flags: AuthSessionFlags { account_verified: true }
                    }).await?;

                    self.transition_to_online().await?;
                }
            }

            CtrlMsg::AuthOk { .. } => {
                if self.role == SessionRole::Client {
                    self.update_state(SessionState::AccountVerified);
                    self.perform_tofu_check_async().await?;
                    self.transition_to_online().await?;
                }
            }

            CtrlMsg::AuthFail { error_code, .. } => anyhow::bail!("Remote AuthFail: {}", error_code),

            CtrlMsg::Ping { ts, msg_id } => {
                self.send_ctrl(CtrlMsg::Pong { reply_to: msg_id, ts }).await?;
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

    // --- OPAQUE Core Logic (v3.0.0 Fixed) ---

    async fn start_opaque_login(&mut self) -> anyhow::Result<()> {
        let mut rng = OsRng;
        // TODO: Consider using a dedicated 'pairing_secret' instead of 'account_uid' for stronger security in production.
        let password = self.config.account_uid.as_bytes();

        let start_result = CbClientLogin::start(&mut rng, password)
            .map_err(|e| anyhow::anyhow!("OPAQUE start failed: {:?}", e))?;

        self.opaque_client_state = Some(start_result.state);

        let payload = bincode::serialize(&start_result.message)?;
        self.send_ctrl(CtrlMsg::OpaqueStart {
            msg_id: Some(uuid::Uuid::new_v4().to_string()),
            reply_to: None,
            opaque: payload
        }).await?;

        self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueResponse));
        Ok(())
    }

    async fn handle_opaque_response(&mut self, response_bytes: &[u8]) -> anyhow::Result<()> {
        let client_state = self.opaque_client_state.take()
            .ok_or_else(|| anyhow::anyhow!("Protocol error: Missing client state"))?;

        let password = self.config.account_uid.as_bytes();

        // [修复] 显式类型标注 (修复 E0277)
        let server_response: opaque_ke::CredentialResponse<DefaultCipherSuite> =
            bincode::deserialize(response_bytes)
                .map_err(|_| anyhow::anyhow!("Invalid OpaqueResponse bytes"))?;

        // [修复] v3.0.0 finish 不需要 rng (修复 E0061: 3 arguments)
        let finish_result = client_state.finish(
            password,
            server_response,
            ClientLoginFinishParameters::default(),
        ).map_err(|e| anyhow::anyhow!("OPAQUE finish failed: {:?}", e))?;

        let payload = bincode::serialize(&finish_result.message)?;
        self.send_ctrl(CtrlMsg::OpaqueFinish {
            msg_id: Some(uuid::Uuid::new_v4().to_string()),
            reply_to: None,
            opaque: payload
        }).await?;

        self.update_state(SessionState::Handshaking(HandshakeStep::WaitingAuthOk));
        Ok(())
    }

    async fn handle_opaque_start(&mut self, start_bytes: &[u8]) -> anyhow::Result<()> {
        let mut rng = OsRng;
        let identifier = b"clipbridge-user";

        // [修复] 参数类型 (修复 E0609)
        let (server_setup, server_rec) = p2p_get_server_registration(&self.config.account_uid)?;

        let client_message = bincode::deserialize(start_bytes)
            .map_err(|_| anyhow::anyhow!("Invalid OpaqueStart bytes"))?;

        // [修复] v3.0.0 start 6 arguments (修复 E0061)
        // (rng, setup, user_record, message, server_id, params)
        // 移除了 credential_request
        let start_result = CbServerLogin::start(
            &mut rng,
            &server_setup,
            Some(server_rec), // [修复] 传递 owned value (修复 E0308)
            client_message,
            identifier,
            ServerLoginStartParameters::default(),
        ).map_err(|e| anyhow::anyhow!("OPAQUE server start failed: {:?}", e))?;

        self.opaque_server_state = Some(start_result.state);

        let payload = bincode::serialize(&start_result.message)?;
        self.send_ctrl(CtrlMsg::OpaqueResponse {
            msg_id: Some(uuid::Uuid::new_v4().to_string()),
            reply_to: None,
            opaque: payload
        }).await?;

        self.update_state(SessionState::Handshaking(HandshakeStep::OpaqueFinish));
        Ok(())
    }

    async fn handle_opaque_finish(&mut self, finish_bytes: &[u8]) -> anyhow::Result<()> {
        let server_state = self.opaque_server_state.take()
            .ok_or_else(|| anyhow::anyhow!("Protocol error: Missing server state"))?;

        let client_message = bincode::deserialize(finish_bytes)
            .map_err(|_| anyhow::anyhow!("Invalid OpaqueFinish bytes"))?;

        let _session_key = server_state.finish(client_message)
            .map_err(|e| anyhow::anyhow!("Authentication failed: {:?}", e))?;

        self.update_state(SessionState::AccountVerified);
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

    async fn transition_to_online(&mut self) -> Result<()> {
        self.update_state(SessionState::Online);
        if let Some(did) = &self.remote_device_id {
            let json = serde_json::json!({
                "type": "PEER_ONLINE",
                "ts_ms": now_ms(),
                "payload": { "device_id": did }
            });
            self.sink.emit(json.to_string());
        }
        Ok(())
    }

    async fn tick_heartbeat(&mut self) -> Result<()> {
        if now_ms() - self.last_active_at > HEARTBEAT_TIMEOUT.as_millis() as i64 {
            let _ = self.send_ctrl(CtrlMsg::Error {
                reply_to: None,
                error_code: "TIMEOUT".into(),
                message: Some("Heartbeat timeout".into())
            }).await;
            anyhow::bail!("Heartbeat timeout");
        }
        if self.state == SessionState::Online {
            self.send_ctrl(CtrlMsg::Ping {
                msg_id: Some(uuid::Uuid::new_v4().to_string()),
                ts: now_ms()
            }).await?;
        }
        Ok(())
    }
}