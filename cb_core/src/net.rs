// cb_core/src/net/mod.rs

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;

use crate::discovery::{DiscoveryEvent, DiscoveryService, PeerCandidate};
use crate::session::{SessionActor, SessionCmd, SessionHandle, SessionRole};
use crate::transport::Transport;
use crate::util::now_ms;
use crate::api::PeerStatus;

/// 网络层管理器
pub struct NetManager {
    config: crate::api::CoreConfig,
    transport: Arc<Transport>,
    discovery: DiscoveryService,

    sessions: Vec<SessionHandle>,

    // 正在拨号中的集合 (防止并发拨号)
    pending_dials: HashSet<String>,

    // 退避记录: device_id -> (失败次数, 下次重试的最早时间戳)
    backoff_map: HashMap<String, BackoffState>,
    known_peers: HashMap<String, PeerCandidate>,

    cmd_rx: mpsc::Receiver<NetCmd>,
    discovery_rx: mpsc::Receiver<DiscoveryEvent>,
    event_sink: Arc<dyn crate::api::CoreEventSink>,
}

/// 管理退避状态的结构体
struct BackoffState {
    fail_count: u32,
    next_retry_ts: i64,
}

#[derive(Debug)]
pub enum NetCmd {
    BroadcastMeta(crate::model::ItemMeta),
    GetPeers(oneshot::Sender<Vec<PeerStatus>>),
    Shutdown,
}

impl NetManager {
    pub fn spawn(
        config: crate::api::CoreConfig,
        event_sink: Arc<dyn crate::api::CoreEventSink>,
    ) -> anyhow::Result<mpsc::Sender<NetCmd>> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);

        std::thread::Builder::new()
            .name("cb-net-manager".to_string())
            .spawn(move || {
                // 1. 创建专用的 Runtime
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build NetManager runtime");

                rt.block_on(async move {
                    // 2. 在 Runtime 内部进行初始化 (Transport 需要绑定 Socket)
                    match Transport::new(0) {
                        Ok(transport) => {
                            let transport = Arc::new(transport);
                            let port = transport.local_port().unwrap_or(0);

                            // 3. 启动 Discovery
                            let (disc_tx, disc_rx) = mpsc::channel(32);
                            match DiscoveryService::spawn(config.clone(), port, disc_tx) {
                                Ok(discovery) => {
                                    let manager = Self {
                                        config,
                                        transport,
                                        discovery,
                                        sessions: Vec::new(),
                                        pending_dials: HashSet::new(),
                                        backoff_map: HashMap::new(),
                                        known_peers: HashMap::new(),
                                        cmd_rx,
                                        discovery_rx: disc_rx,
                                        event_sink,
                                    };
                                    // 4. 运行主循环
                                    manager.run().await;
                                }
                                Err(e) => eprintln!("[NetManager] Discovery spawn failed: {}", e),
                            }
                        }
                        Err(e) => eprintln!("[NetManager] Transport init failed: {}", e),
                    }
                });
            })?;

        Ok(cmd_tx)
    }

    async fn run(mut self) {
        // 每秒检查一次，用于快速响应重连
        let mut cleanup_ticker = interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                // 1. 上层命令
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(NetCmd::BroadcastMeta(meta)) => self.broadcast_meta(meta).await,
                        Some(NetCmd::GetPeers(reply_tx)) => {
                            // [New] 处理查询请求
                            let peers = self.get_peers_info();
                            let _ = reply_tx.send(peers);
                        }
                        Some(NetCmd::Shutdown) => {
                            self.shutdown().await;
                            break;
                        }
                        None => break,
                    }
                }

                // 2. 发现事件
                evt = self.discovery_rx.recv() => {
                    if let Some(event) = evt {
                        self.handle_discovery_event(event).await;
                    }
                }

                // 3. 入站连接
                conn = self.transport.accept() => {
                    if let Some(conn) = conn {
                        let handle = SessionActor::spawn(
                            SessionRole::Server,
                            conn,
                            self.config.clone(),
                            self.event_sink.clone(),
                            None
                        );
                        self.sessions.push(handle);
                    }
                }

                // 4. 定期维护 (清理死链 + 管理退避)
                _ = cleanup_ticker.tick() => {
                    self.maintain_sessions().await;
                }
            }
        }
    }

    /// 收集当前会话状态
    fn get_peers_info(&self) -> Vec<PeerStatus> {
        let mut peers = Vec::new();

        // 1. 先把 Session 里的加进去
        for s in &self.sessions {
            peers.push(PeerStatus {
                device_id: s.device_id(),
                state: s.public_state(),
            });
        }

        // 2. 再把 known_peers 里有但 session 里没有的加为 "Discovered" (可选，文档建议展示所有已发现设备)
        for (did, _) in &self.known_peers {
            if !peers.iter().any(|p| &p.device_id == did) {
                peers.push(PeerStatus {
                    device_id: did.clone(),
                    state: crate::api::PeerConnectionState::Discovered,
                });
            }
        }

        peers
    }

    async fn maintain_sessions(&mut self) {
        let now = now_ms();

        // --- A. 成功连接的“真正”判定 ---
        // 只有当 Session 状态变为 Online 时，才清除退避记录（清零）
        // 如果只是 Handshaking，不要清零，万一握手失败还得继续退避
        for s in &self.sessions {
            if s.is_online() {
                if self.backoff_map.contains_key(&s.device_id()) {
                    println!("[Net] Session {} is stable (Online). Resetting backoff.", s.device_id());
                    self.backoff_map.remove(&s.device_id());
                }
            }
        }

        // --- B. 清理死链 & 生成/升级退避 ---
        let mut dead_ids = Vec::new();
        self.sessions.retain(|s| {
            if s.is_finished() { // 彻底挂了
                if !s.device_id().starts_with("pending") {
                    dead_ids.push(s.device_id().clone());
                }
                false
            } else {
                true
            }
        });

        for did in dead_ids {
            let entry = self.backoff_map.entry(did.clone()).or_insert(BackoffState {
                fail_count: 0,
                next_retry_ts: 0,
            });

            entry.fail_count += 1;
            // 指数退避：1s, 2s, 4s, 8s...
            let delay_secs = 2u64.pow(entry.fail_count.min(6));
            entry.next_retry_ts = now + (delay_secs * 1000) as i64;

            self.pending_dials.remove(&did);
            println!("[Net] Device {} disconnected. Fail count: {}. Backoff {}s", did, entry.fail_count, delay_secs);
        }

        // --- C. 检查退避到期 & 执行重连 ---
        // 只有当 (当前时间 > 重试时间) 且 (不在正在拨号列表) 时才尝试
        let mut peers_to_dial = Vec::new();

        for (did, state) in &self.backoff_map {
            if now >= state.next_retry_ts && !self.pending_dials.contains(did) {
                // 【关键修复】不再依赖 cached_candidate，而是去地址簿(known_peers)里查
                if let Some(candidate) = self.known_peers.get(did) {
                    peers_to_dial.push(candidate.clone());
                } else {
                    // 极端情况：由于还没收到过 Discovery 就连过了（不太可能），或者数据丢失
                    // 只能等下一次 Discovery
                    // println!("[Net] Backoff expired for {} but no known address.", did);
                }
            }
        }

        for peer in peers_to_dial {
            println!("[Net] Backoff expired for {}. Retrying...", peer.device_id);
            // 重试时更新下一次时间，防止下一帧重复触发（直到再次失败进入 B 步骤，或者成功进入 A 步骤）
            if let Some(state) = self.backoff_map.get_mut(&peer.device_id) {
                // 临时推迟一点点，避免在此次拨号尚未完成时重复进入此循环
                state.next_retry_ts = now + 5000;
            }
            self.perform_dial(peer).await;
        }
    }

    async fn handle_discovery_event(&mut self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::CandidateFound(peer) => {
                // 【新增】更新地址簿：这是我们唯一的记忆来源
                self.known_peers.insert(peer.device_id.clone(), peer.clone());

                if self.config.device_id >= peer.device_id { return; }
                if self.sessions.iter().any(|s| s.device_id() == peer.device_id) { return; }
                if self.pending_dials.contains(&peer.device_id) { return; }

                // 如果收到 mDNS 发现信号，即使在退避期，也认为是一个“值得重试”的新时机。
                // 我们不清除 fail_count（如果这次又失败了，下次惩罚会更重），
                // 但我们允许 bypass 这一次的时间检查。
                if let Some(state) = self.backoff_map.get_mut(&peer.device_id) {
                    if now_ms() < state.next_retry_ts {
                        println!("[Net] ⚡️ Discovery signal received for {}. Bypassing backoff wait (was waiting until {}).",
                                 peer.device_id, state.next_retry_ts);
                        // 不 return，直接往下走去 perform_dial
                    }
                }

                self.perform_dial(peer).await;
            }
            _ => {}
        }
    }

    async fn broadcast_meta(&self, meta: crate::model::ItemMeta) {
        if self.config.global_policy == crate::api::GlobalPolicy::DenyAll {
            println!("[Policy] Broadcast DENIED by DenyAll policy.");
            // 这里可以发一个 PolicyDeny 事件给 UI
            return;
        }

        for session in &self.sessions {
            if session.is_online() {
                let _ = session.cmd_tx.send(SessionCmd::SendMeta(meta.clone())).await;
            }
        }
    }

    async fn perform_dial(&mut self, peer: PeerCandidate) {
        // 1. 获取本机 Socket 的“血统”
        let i_am_v4 = self.transport.is_ipv4();

        // 2. 智能过滤：只保留跟我血统一致的地址
        let valid_addrs: Vec<std::net::SocketAddr> = peer.addrs.iter()
            .filter_map(|s| s.parse().ok())
            .filter(|addr: &std::net::SocketAddr| {
                // 如果我是 v4，我只要 v4；如果我是 v6，我只要 v6
                addr.is_ipv4() == i_am_v4
            })
            .collect();

        // 3. 如果 mDNS 这次只发了不匹配的地址（比如我是v4，对方只发了v6）
        // 直接忽略，不要报错，不要 Backoff，静静等待下一波更新
        if valid_addrs.is_empty() {
            // println!("[Net] Skipped {} (Protocol mismatch: I am v4={}, Peer has {:?})",
            //          peer.device_id, i_am_v4, peer.addrs);
            return;
        }

        println!("[Net] Initiating connection to {} (Compatible Addrs: {:?})...", peer.device_id, valid_addrs);
        self.pending_dials.insert(peer.device_id.clone());

        let mut success = false;

        // 3. 【修改】只遍历有效的 valid_addrs
        for addr in valid_addrs {
            match self.transport.connect(&addr.to_string()).await { // 注意：quinn connect 接受 &str 或 SocketAddr
                Ok(conn) => {
                    let handle = SessionActor::spawn(
                        SessionRole::Client,
                        conn,
                        self.config.clone(),
                        self.event_sink.clone(),
                        Some(peer.device_id.clone())
                    );
                    self.sessions.push(handle);
                    success = true;
                    break;
                }
                Err(e) => {
                    println!("[Net] Failed to connect to {}: {}", addr, e);
                }
            }
        }

        // 4. 处理结果
        if !success {
            self.pending_dials.remove(&peer.device_id);

            // 只有当“真的尝试了 IPv4 地址但连不上”时，才触发退避
            let now = now_ms();
            let entry = self.backoff_map.entry(peer.device_id.clone()).or_insert(BackoffState { fail_count: 0, next_retry_ts: 0 });
            entry.fail_count += 1;
            let delay = 2u64.pow(entry.fail_count.min(6));
            entry.next_retry_ts = now + (delay * 1000) as i64;

            println!("[Net] Dial failed for {}. Backoff {}s", peer.device_id, delay);
        }
    }

    async fn shutdown(&self) {
        self.discovery.shutdown().await;
        self.transport.shutdown();
        for s in &self.sessions {
            s.shutdown().await;
        }
    }
}

