// cb_core/src/net/mod.rs

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::discovery::{DiscoveryEvent, DiscoveryService};
use crate::session::{SessionActor, SessionCmd, SessionHandle, SessionRole};
use crate::transport::Transport;
use crate::util::now_ms;

/// 网络层管理器
pub struct NetManager {
    config: crate::api::CoreConfig,
    transport: Arc<Transport>,
    discovery: DiscoveryService,

    sessions: Vec<SessionHandle>,

    // 正在拨号中的集合 (防止并发拨号)
    pending_dials: HashSet<String>,

    // 退避记录: device_id -> (失败次数, 下次重试的最早时间戳)
    backoff_map: HashMap<String, (u32, i64)>,

    cmd_rx: mpsc::Receiver<NetCmd>,
    discovery_rx: mpsc::Receiver<DiscoveryEvent>,
    event_sink: Arc<dyn crate::api::CoreEventSink>,
}

#[derive(Debug)]
pub enum NetCmd {
    BroadcastMeta(crate::model::ItemMeta),
    Shutdown,
}

impl NetManager {
    pub fn spawn(
        config: crate::api::CoreConfig,
        event_sink: Arc<dyn crate::api::CoreEventSink>,
    ) -> anyhow::Result<mpsc::Sender<NetCmd>> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);

        // 端口 0 = 随机端口
        let transport = Arc::new(Transport::new(0)?);
        let port = transport.local_port()?;

        let (disc_tx, disc_rx) = mpsc::channel(32);
        let discovery = DiscoveryService::spawn(config.clone(), port, disc_tx)?;

        let manager = Self {
            config,
            transport,
            discovery,
            sessions: Vec::new(),
            pending_dials: HashSet::new(),
            backoff_map: HashMap::new(),
            cmd_rx,
            discovery_rx: disc_rx,
            event_sink,
        };

        tokio::spawn(async move {
            manager.run().await;
        });

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
                            self.event_sink.clone()
                        );
                        self.sessions.push(handle);
                    }
                }

                // 4. 定期维护 (清理死链 + 管理退避)
                _ = cleanup_ticker.tick() => {
                    self.maintain_sessions();
                }
            }
        }
    }

    fn maintain_sessions(&mut self) {
        let now = now_ms();

        // --- A. 识别并清理断开的会话 ---
        let mut dead_ids = Vec::new();
        self.sessions.retain(|s| {
            if !s.is_online() && !s.device_id.starts_with("pending") {
                // 如果会话不再 Online 且不是刚建立的 pending 状态，说明断开了
                dead_ids.push(s.device_id.clone());
                false // 移除
            } else {
                true // 保留
            }
        });

        // --- B. 记录退避 ---
        for did in dead_ids {
            // 计算指数退避: 1s, 2s, 4s, 8s ... 64s
            let entry = self.backoff_map.entry(did.clone()).or_insert((0, 0));
            entry.0 += 1; // 失败次数 +1
            let delay_secs = 2u64.pow(entry.0.min(6));
            entry.1 = now + (delay_secs * 1000) as i64;

            // 清理 pending 状态，允许未来重连
            self.pending_dials.remove(&did);

            println!("[Net] Device {} disconnected. Backoff {}s", did, delay_secs);
        }

        // --- C. 检查退避到期 ---
        // 如果退避时间到了，我们将其从 pending_dials 中移除 (如果还在里面的话)，
        // 这样下一次 DiscoveryEvent 就能触发重连。
        // 注意：这里我们不主动发起重连，而是依赖 DiscoveryService 持续广播/扫描带来的 CandidateFound 事件。
        let mut to_reset = Vec::new();
        for (did, (_cnt, next_ts)) in &self.backoff_map {
            if now >= *next_ts {
                to_reset.push(did.clone());
            }
        }
        for did in to_reset {
            // 退避结束：从 pending 移除，给下一次 Discovery 机会
            self.pending_dials.remove(&did);
            // 既然退避结束了，可以重置失败计数吗？
            // 策略：只有连接 *成功* 后才重置失败计数。这里只是允许重试。
            // 如果下次又失败，计数会继续增加。
        }
    }

    async fn handle_discovery_event(&mut self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::CandidateFound(peer) => {
                // 1. 策略: Low ID Dials High ID
                if self.config.device_id >= peer.device_id {
                    return;
                }

                // 2. 查重: 是否已连接
                if self.sessions.iter().any(|s| s.device_id == peer.device_id) {
                    return;
                }

                // 3. 查重: 是否正在拨号
                if self.pending_dials.contains(&peer.device_id) {
                    return;
                }

                // 4. 检查是否在退避期
                if let Some((_, next_retry)) = self.backoff_map.get(&peer.device_id) {
                    if now_ms() < *next_retry {
                        return; // 还在冷却中，忽略
                    }
                }

                // 5. 执行连接
                self.pending_dials.insert(peer.device_id.clone());

                // 尝试所有地址
                let mut success = false;
                for addr in peer.addrs {
                    match self.transport.connect(&addr).await {
                        Ok(conn) => {
                            let mut handle = SessionActor::spawn(
                                SessionRole::Client,
                                conn,
                                self.config.clone(),
                                self.event_sink.clone()
                            );
                            // Client 侧在握手前就知道对方 ID，手动修正 handle
                            handle.device_id = peer.device_id.clone();
                            self.sessions.push(handle);

                            // 连接成功，清除退避记录
                            self.backoff_map.remove(&peer.device_id);
                            success = true;
                            break;
                        }
                        Err(e) => {
                            // eprintln!("[Net] Dial {} failed: {}", addr, e);
                        }
                    }
                }

                if !success {
                    // 如果所有地址都连不上，立即移除 pending，以便下次重试（或让 maintain_sessions 处理）
                    self.pending_dials.remove(&peer.device_id);
                }
            }
            DiscoveryEvent::CandidateLost(_) => {
                // 依赖心跳超时断开
            }
        }
    }

    async fn broadcast_meta(&self, meta: crate::model::ItemMeta) {
        for session in &self.sessions {
            let _ = session.cmd_tx.send(SessionCmd::SendMeta(meta.clone())).await;
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