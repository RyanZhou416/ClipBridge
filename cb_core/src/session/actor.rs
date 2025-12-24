// cb_core/src/session/actor.rs

use std::sync::{Arc, Mutex};
use std::time::Duration;
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::codec::{FramedRead, FramedWrite};
use std::path::PathBuf;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use sha2::{Digest, Sha256};

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

use crate::proto::{CBFrameCodec, CtrlMsg, PROTOCOL_VERSION, AuthSessionFlags, CBFrame};
use crate::transport::{Connection, SendStream, RecvStream};
use crate::store::Store;
use crate::util::{now_ms, sha256_hex};
use super::{SessionCmd, SessionHandle, SessionRole, SessionState, HandshakeStep};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(6);

/// 定义接收状态
enum ReceiverState {
    Idle,
    Receiving {
        transfer_id: String,
        item_id: String,
        file_id: Option<String>, // 如果是 FileList 中的子文件
        mime: String,
        tmp_path: PathBuf,
        writer: tokio::io::BufWriter<File>, // 异步写入
        hasher: Sha256,
        received_bytes: u64,
        total_bytes: u64,
        last_progress_emit: i64, // 用于节流 progress 事件
    },
}

/// 定义发送状态
enum SenderState {
    Idle,
    Sending {
        transfer_id: String,
        reader: BufReader<File>, // 异步读取
        chunk_buf: Vec<u8>,      // 复用 Buffer
        remaining_bytes: u64,
        sha_hasher: sha2::Sha256, // 边读边算 Hash
    },
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
    opaque_client_state: Option<CbClientLoginState>,
    opaque_server_state: Option<CbServerLoginState>,
    receiver: ReceiverState, // 接收状态机
    sender: SenderState,     // 发送状态机
    cas: crate::cas::Cas,
}

impl SessionActor {
    pub fn spawn(
        role: SessionRole,
        conn: Connection,
        config: CoreConfig,
        sink: Arc<dyn CoreEventSink>,
        store: Arc<Mutex<Store>>,
        cas: crate::cas::Cas,
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

        let initial_did = expected_peer_id.unwrap_or_else(|| "pending_server".to_string());

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
                store,
                cas,
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
        store: Arc<Mutex<Store>>,
        cas: crate::cas::Cas,
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
            store,
            state_ref,
            peer_id_ref,
            state: SessionState::TransportReady,
            remote_device_id: None,
            remote_fingerprint: fingerprint,
            last_active_at: now_ms(),
            cmd_rx,
            opaque_client_state: None,
            opaque_server_state: None,
            receiver: ReceiverState::Idle,
            sender: SenderState::Idle,
            cas,
        };

        actor.start_handshake().await?;

        let mut heartbeat_ticker = interval(HEARTBEAT_INTERVAL);
        heartbeat_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let run_result: Result<()> = async {
            loop {
                // 构造一个 "读取下一个 chunk" 的 Future
                // 只有当状态是 Sending 时，这个 Future 才会 resolve，否则 pending
                // 注意：这里需要借用 actor.sender，tokio::select! 会处理好引用
                let send_future = async {
                    match &mut actor.sender {
                        SenderState::Sending { reader, chunk_buf, .. } => {
                            // 读一块
                            let res = reader.read(chunk_buf).await;
                            Some(res) // 返回读取结果
                        }
                        SenderState::Idle => std::future::pending().await,
                    }
                };

                tokio::select! {
                    // 1. 网络消息
                    msg = actor.reader.next() => {
                        match msg {
                            Some(Ok(frame)) => {
                                actor.last_active_at = now_ms();
                                if let Err(e) = actor.handle_frame(frame).await {
                                     eprintln!("[Session] Frame error: {:?}", e);
                                     // 严重错误断开连接
                                     return Err(e);
                                }
                            }
                            Some(Err(e)) => return Err(e.into()),
                            None => break, // EOF
                        }
                    }

                    // 2. 本地命令
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
                            Some(SessionCmd::RequestTransfer { item_id, file_id, reply_tx }) => {
                                // M3: B 端发起拉取
                                let _ = actor.start_pull_request(item_id, file_id, reply_tx).await;
                            }
                            Some(SessionCmd::CancelTransfer { transfer_id }) => {
                                // M3: 双向取消
                                let _ = actor.handle_local_cancel(transfer_id).await;
                            }
                            None => break,
                        }
                    }

                    // 3. 心跳
                    _ = heartbeat_ticker.tick() => {
                        actor.tick_heartbeat().await?;
                    }

                    // 4. 发送文件流 (A 端逻辑)
                    Some(read_res) = send_future => {
                        actor.handle_send_chunk_result(read_res).await?;
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
        self.writer.send(CBFrame::Control(msg)).await.context("Failed to send CtrlMsg")
    }

    async fn send_data_chunk(&mut self, data: bytes::Bytes) -> Result<()> {
        self.writer.send(CBFrame::Data(data)).await.context("Failed to send Data Chunk")
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

    async fn handle_frame(&mut self, frame: CBFrame) -> Result<()> {
        match frame {
            CBFrame::Control(msg) => self.handle_control_msg(msg).await,
            CBFrame::Data(data) => self.handle_data_chunk(data).await,
        }
    }

    async fn handle_control_msg(&mut self, msg: CtrlMsg) -> Result<()> {
        match msg {
            // ... 原有的 Hello/Auth/Opaque/Ping 逻辑 ...
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
                if self.role == SessionRole::Server { self.handle_opaque_start(&bytes).await?; }
            }
            CtrlMsg::OpaqueResponse { opaque: bytes, .. } => {
                if self.role == SessionRole::Client { self.handle_opaque_response(&bytes).await?; }
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
                    let store = self.store.clone();
                    let account_uid = self.config.account_uid.clone();
                    let item_clone = item.clone();
                    let is_new = tokio::task::spawn_blocking(move || {
                        let mut guard = store.lock().unwrap();
                        guard.insert_remote_item(&account_uid, &item_clone, now_ms())
                    }).await??;
                    if is_new {
                        let json = serde_json::json!({
                            "type": "ITEM_META_ADDED",
                            "ts_ms": now_ms(),
                            "payload": { "meta": item }
                        });
                        self.sink.emit(json.to_string());
                    }
                }
            }
            CtrlMsg::Error { error_code, message, .. } => anyhow::bail!("Remote error {}: {:?}", error_code, message),
            CtrlMsg::Close { .. } => anyhow::bail!("Remote closed connection"),

            // === M3: 传输逻辑 ===
            CtrlMsg::ContentGet { msg_id, item_id, file_id, .. } => {
                let transfer_id = msg_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                self.handle_content_get(transfer_id, item_id, file_id).await?;
            }
            CtrlMsg::ContentBegin { req_id, item_id, file_id, total_bytes, sha256, mime} => {
                self.handle_content_begin(req_id, item_id, file_id, total_bytes, sha256, mime).await?;
            }
            CtrlMsg::ContentEnd { req_id, sha256 } => {
                self.handle_content_end(req_id, sha256).await?;
            }
            CtrlMsg::ContentCancel { req_id, reason } => {
                // 如果我是发送者
                if let SenderState::Sending { transfer_id, .. } = &self.sender {
                    if transfer_id == &req_id {
                        println!("[Session] Peer cancelled sending {}", req_id);
                        self.sender = SenderState::Idle;
                    }
                }
                // 如果我是接收者
                self.handle_content_cancel(req_id, reason).await?;
            }
        }
        Ok(())
    }

    // --- M3 Receiver Logic ---

    async fn handle_content_begin(
        &mut self,
        transfer_id: String,
        item_id: String,
        file_id: Option<String>,
        total_bytes: u64,
        _expected_sha256: String,
        mime: String
    ) -> Result<()> {
        println!("[Session] Starting transfer {}: {} bytes", transfer_id, total_bytes);

        if let ReceiverState::Receiving { transfer_id: ref curr, .. } = self.receiver {
            if curr != &transfer_id {
                self.send_ctrl(CtrlMsg::Error {
                    reply_to: Some(transfer_id),
                    error_code: "BUSY".into(),
                    message: Some("Already receiving another file".into())
                }).await?;
                return Ok(());
            }
        }

        let tmp_path = self.cas.get_tmp_path(&transfer_id);
        if let Some(p) = tmp_path.parent() {
            tokio::fs::create_dir_all(p).await?;
        }

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .await
            .context("Failed to open tmp file")?;

        let writer = tokio::io::BufWriter::new(file);

        self.receiver = ReceiverState::Receiving {
            transfer_id,
            item_id,
            file_id,
            mime,
            tmp_path,
            writer,
            hasher: Sha256::new(),
            received_bytes: 0,
            total_bytes,
            last_progress_emit: 0,
        };

        Ok(())
    }

    async fn handle_data_chunk(&mut self, data: bytes::Bytes) -> Result<()> {
        match &mut self.receiver {
            ReceiverState::Idle => Ok(()),
            ReceiverState::Receiving {
                transfer_id, writer, hasher, received_bytes, total_bytes, last_progress_emit, ..
            } => {
                writer.write_all(&data).await.context("Write failed")?;
                hasher.update(&data);
                *received_bytes += data.len() as u64;

                let now = now_ms();
                if now - *last_progress_emit > 200 {
                    let progress_evt = serde_json::json!({
                        "type": "TRANSFER_PROGRESS",
                        "payload": {
                            "transfer_id": transfer_id,
                            "received": *received_bytes,
                            "total": *total_bytes
                        }
                    });
                    self.sink.emit(progress_evt.to_string());
                    *last_progress_emit = now;
                }
                Ok(())
            }
        }
    }

    async fn handle_content_end(&mut self, req_id: String, expected_sha256: String) -> Result<()> {
        let prev_state = std::mem::replace(&mut self.receiver, ReceiverState::Idle);

        if let ReceiverState::Receiving {
            transfer_id, item_id, file_id, mime, tmp_path, mut writer, hasher, ..
        } = prev_state {
            if transfer_id != req_id {
                anyhow::bail!("Protocol mismatch: End frame id {} != current {}", req_id, transfer_id);
            }

            writer.flush().await?;
            let mut file = writer.into_inner();
            file.shutdown().await?;
            drop(file);

            let calculated_hash = hex::encode(hasher.finalize());
            if calculated_hash != expected_sha256 {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                self.emit_transfer_failed(&transfer_id, "CHECKSUM_MISMATCH", "Hash mismatch");
                return Ok(());
            }

            // Commit to Blob (CAS)
            let cas_clone = self.cas.clone();
            let store_clone = self.store.clone(); // Need store to query filename
            let tmp_path_clone = tmp_path.clone();
            let sha_clone = expected_sha256.clone();
            let mime_clone = mime.clone();
            let item_id_clone = item_id.clone();
            let file_id_clone = file_id.clone(); // Option<String>
            let transfer_id_clone = transfer_id.clone();

            let final_path_res = tokio::task::spawn_blocking(move || {
                // A. 转正 Blob
                let blob_path = cas_clone.commit_tmp_file(&tmp_path_clone, &sha_clone)?;

                // B. 决定落地路径
                if let Some(fid) = file_id_clone {
                    // === M3-3: FileList 模式 ===
                    // 查库获取原始文件名
                    let store = store_clone.lock().unwrap();
                    if let Some(fmeta) = store.get_file_meta(&item_id_clone, &fid)? {
                        // 落地到 downloads/<transfer_id>/<rel_name>
                        cas_clone.materialize_file(&sha_clone, &transfer_id_clone, &fmeta.rel_name)
                    } else {
                        // 异常：元数据对不上，只好返回 blob
                        Ok(blob_path)
                    }
                } else {
                    // === M3-1/2: Text/Image 模式 ===
                    // (复用 Step 5 的逻辑: 根据 mime 生成后缀)
                    // ... [Code from Step 5] ...
                    let ext = match mime_clone.as_str() {
                        "image/png" => Some("png"),
                        "image/jpeg" | "image/jpg" => Some("jpg"),
                        _ => None
                    };
                    if let Some(extension) = ext {
                        cas_clone.materialize_blob(&sha_clone, extension)
                    } else {
                        Ok(blob_path)
                    }
                }
            }).await?;

            match final_path_res {
                Ok(blob_path) => {
                    let store = self.store.clone();
                    let sha_for_db = expected_sha256.clone();
                    tokio::task::spawn_blocking(move || {
                        let mut guard = store.lock().unwrap();
                        guard.mark_cache_present(&sha_for_db, crate::util::now_ms())
                    }).await??;

                    let local_ref = serde_json::json!({
                        "local_path": blob_path.to_string_lossy(),
                    });

                    let evt = serde_json::json!({
                        "type": "CONTENT_CACHED",
                        "payload": {
                            "transfer_id": transfer_id,
                            "item_id": item_id,
                            "file_id": file_id,
                            "local_ref": local_ref
                        }
                    });
                    self.sink.emit(evt.to_string());
                }
                Err(e) => {
                    self.emit_transfer_failed(&transfer_id, "COMMIT_FAILED", &e.to_string());
                }
            }
        }
        Ok(())
    }

    async fn handle_content_cancel(&mut self, req_id: String, _reason: String) -> Result<()> {
        let prev_state = std::mem::replace(&mut self.receiver, ReceiverState::Idle);
        if let ReceiverState::Receiving { tmp_path, mut writer, transfer_id, .. } = prev_state {
            if transfer_id == req_id {
                let _ = writer.shutdown().await;
                drop(writer);
                let _ = tokio::fs::remove_file(tmp_path).await;
                let evt = serde_json::json!({
                    "type": "TRANSFER_CANCELLED",
                    "payload": { "transfer_id": transfer_id }
                });
                self.sink.emit(evt.to_string());
            } else {
                // Restore state if ID mismatch (should rarely happen)
                // NOTE: Since we moved out prev_state, restoring requires reconstruction or just log/drop.
                // Here we simply drop because overlapping transfers are protocol errors.
            }
        }
        Ok(())
    }

    fn emit_transfer_failed(&self, tid: &str, code: &str, msg: &str) {
        let evt = serde_json::json!({
            "type": "TRANSFER_FAILED",
            "payload": {
                "transfer_id": tid,
                "error": { "code": code, "message": msg }
            }
        });
        self.sink.emit(evt.to_string());
    }

    // --- M3 Sender Logic ---

    async fn handle_content_get(&mut self, transfer_id: String, item_id: String, file_id: Option<String>) -> Result<()> {
        if matches!(self.sender, SenderState::Sending { .. }) {
            self.send_ctrl(CtrlMsg::Error {
                reply_to: Some(transfer_id),
                error_code: "BUSY".into(),
                message: Some("Sender is busy".into())
            }).await?;
            return Ok(());
        }

        // 同时查询路径和 MIME
        let (file_path_res, mime_val) = {
            let store = self.store.lock().unwrap();

            if let Some(fid) = &file_id {
                // === M3-3 FileList Logic ===
                if let Some(file_meta) = store.get_file_meta(&item_id, fid).unwrap_or(None) {
                    if let Some(sha) = file_meta.sha256 {
                        let path = self.cas.blob_path(&sha);
                        // FileList 子文件默认用通用流 MIME，接收端靠文件名识别
                        let path_res = if path.exists() { Some((path, sha)) } else { None };
                        (path_res, "application/octet-stream".to_string())
                    } else {
                        (None, "".to_string())
                    }
                } else {
                    (None, "".to_string())
                }
            } else {
                // ... (Text/Image Logic from Step 5) ...
                let sha = store.get_item_sha256(&item_id).unwrap_or(None);
                let mime = store.get_item_mime(&item_id).unwrap_or(None).unwrap_or("application/octet-stream".to_string());
                let path_res = if let Some(s) = sha {
                    let path = self.cas.blob_path(&s);
                    if path.exists() { Some((path, s)) } else { None }
                } else { None };
                (path_res, mime)
            }
        };

        if let Some((path, sha256)) = file_path_res {
            let file = File::open(&path).await?;
            let meta = file.metadata().await?;
            let total_bytes = meta.len();

            self.send_ctrl(CtrlMsg::ContentBegin {
                req_id: transfer_id.clone(),
                item_id,
                file_id,
                total_bytes,
                sha256,
                mime: mime_val,
            }).await?;

            self.sender = SenderState::Sending {
                transfer_id,
                reader: BufReader::new(file),
                chunk_buf: vec![0u8; 64 * 1024], // 64KB
                remaining_bytes: total_bytes,
                sha_hasher: sha2::Sha256::new(),
            };
        } else {
            self.send_ctrl(CtrlMsg::Error {
                reply_to: Some(transfer_id),
                error_code: "ITEM_NOT_FOUND".into(),
                message: None,
            }).await?;
        }
        Ok(())
    }

    async fn handle_send_chunk_result(&mut self, read_res: std::io::Result<usize>) -> Result<()> {
        let (is_eof, chunk_data, tid) = match &mut self.sender {
            SenderState::Sending { reader: _, chunk_buf, remaining_bytes, sha_hasher, transfer_id } => {
                match read_res {
                    Ok(n) if n > 0 => {
                        *remaining_bytes = remaining_bytes.saturating_sub(n as u64);
                        sha_hasher.update(&chunk_buf[..n]);
                        (false, Some(bytes::Bytes::copy_from_slice(&chunk_buf[..n])), transfer_id.clone())
                    }
                    Ok(_) => (true, None, transfer_id.clone()),
                    Err(e) => {
                        eprintln!("File read error: {}", e);
                        self.sender = SenderState::Idle;
                        return Ok(());
                    }
                }
            }
            _ => return Ok(()),
        };

        if let Some(data) = chunk_data {
            self.send_data_chunk(data).await?;
        }

        if is_eof {
            let final_hash = if let SenderState::Sending { sha_hasher, .. } = &self.sender {
                hex::encode(sha_hasher.clone().finalize())
            } else { String::new() };

            self.send_ctrl(CtrlMsg::ContentEnd { req_id: tid, sha256: final_hash }).await?;
            self.sender = SenderState::Idle;
            println!("[Session] Sending finished.");
        }
        Ok(())
    }

    async fn start_pull_request(
        &mut self,
        item_id: String,
        file_id: Option<String>,
        reply_tx: tokio::sync::oneshot::Sender<anyhow::Result<String>>
    ) -> Result<()> {
        let transfer_id = uuid::Uuid::new_v4().to_string();
        self.send_ctrl(CtrlMsg::ContentGet {
            msg_id: Some(transfer_id.clone()),
            item_id,
            file_id,
            offset: Some(0),
        }).await?;
        let _ = reply_tx.send(Ok(transfer_id));
        Ok(())
    }

    async fn handle_local_cancel(&mut self, transfer_id: String) -> Result<()> {
        // Sender cancel
        if let SenderState::Sending { transfer_id: curr, .. } = &self.sender {
            if curr == &transfer_id {
                self.send_ctrl(CtrlMsg::ContentCancel {
                    req_id: transfer_id.clone(),
                    reason: "User cancelled".into()
                }).await?;
                self.sender = SenderState::Idle;
                return Ok(());
            }
        }
        // Receiver cancel
        if let ReceiverState::Receiving { transfer_id: curr, .. } = &self.receiver {
            if curr == &transfer_id {
                self.send_ctrl(CtrlMsg::ContentCancel {
                    req_id: transfer_id.clone(),
                    reason: "User cancelled".into()
                }).await?;
                self.handle_content_cancel(transfer_id, "User cancelled".into()).await?;
            }
        }
        Ok(())
    }

    // --- OPAQUE Core Logic (v3.0.0 Fixed) ---
    // (Existing Opaque methods omitted for brevity as they are unchanged from previous context)
    // Please ensure start_opaque_login, handle_opaque_response, handle_opaque_start,
    // handle_opaque_finish, perform_tofu_check_async, transition_to_online, tick_heartbeat
    // are kept exactly as they were in the input file.

    async fn start_opaque_login(&mut self) -> anyhow::Result<()> {
        let mut rng = OsRng;
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
        let server_response: opaque_ke::CredentialResponse<DefaultCipherSuite> =
            bincode::deserialize(response_bytes).map_err(|_| anyhow::anyhow!("Invalid OpaqueResponse bytes"))?;
        let finish_result = client_state.finish(password, server_response, ClientLoginFinishParameters::default())
            .map_err(|e| anyhow::anyhow!("OPAQUE finish failed: {:?}", e))?;
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
        let (server_setup, server_rec) = p2p_get_server_registration(&self.config.account_uid)?;
        let client_message = bincode::deserialize(start_bytes).map_err(|_| anyhow::anyhow!("Invalid OpaqueStart bytes"))?;
        let start_result = CbServerLogin::start(&mut rng, &server_setup, Some(server_rec), client_message, identifier, ServerLoginStartParameters::default())
            .map_err(|e| anyhow::anyhow!("OPAQUE server start failed: {:?}", e))?;
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
        let server_state = self.opaque_server_state.take().ok_or_else(|| anyhow::anyhow!("Protocol error: Missing server state"))?;
        let client_message = bincode::deserialize(finish_bytes).map_err(|_| anyhow::anyhow!("Invalid OpaqueFinish bytes"))?;
        let _session_key = server_state.finish(client_message).map_err(|e| anyhow::anyhow!("Authentication failed: {:?}", e))?;
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
                    if saved_fp != rfp { anyhow::bail!("TLS_PIN_MISMATCH: saved={}, got={}", saved_fp, rfp); }
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
            let _ = self.send_ctrl(CtrlMsg::Error { reply_to: None, error_code: "TIMEOUT".into(), message: Some("Heartbeat timeout".into()) }).await;
            anyhow::bail!("Heartbeat timeout");
        }
        if self.state == SessionState::Online {
            self.send_ctrl(CtrlMsg::Ping { msg_id: Some(uuid::Uuid::new_v4().to_string()), ts: now_ms() }).await?;
        }
        Ok(())
    }
}