// cb_core/src/net/mod.rs

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;

use crate::api::{PeerConnectionState, PeerStatus};
use crate::discovery::{DiscoveryEvent, DiscoveryService, PeerCandidate};
use crate::logs::LogStore;
use crate::session::{SessionActor, SessionCmd, SessionHandle, SessionRole};
use crate::store::Store;
use crate::transport::Transport;
use crate::util::now_ms;
use std::sync::Mutex;

/// 网络层管理器
pub struct NetManager {
	config: crate::api::CoreConfig,
	transport: Arc<Transport>,
	discovery: DiscoveryService,
	store: Arc<Mutex<Store>>,
	log_store: Arc<Mutex<LogStore>>,
	sessions: Vec<SessionHandle>,

	// 正在拨号中的集合 (防止并发拨号)
	pending_dials: HashSet<String>,

	// 退避记录: device_id -> (失败次数, 下次重试的最早时间戳)
	backoff_map: HashMap<String, BackoffState>,
	known_peers: HashMap<String, PeerCandidate>,
	cas: crate::cas::Cas,
	cmd_rx: mpsc::Receiver<NetCmd>,
	discovery_rx: mpsc::Receiver<DiscoveryEvent>,
	/// 拨号任务结果回传通道。拨号在独立 `tokio::spawn` 中执行，
	/// 主循环只在这里收取结果，绝不在 `connect` 上阻塞。
	dial_tx: mpsc::Sender<DialOutcome>,
	dial_rx: mpsc::Receiver<DialOutcome>,
	event_sink: Arc<dyn crate::api::CoreEventSink>,
}

/// 拨号超时（详见 `perform_dial` 与 `NetManager::spawn` 的说明）。
///
/// quinn 的 `Endpoint::connect` 返回的 `Connecting` 没有应用层超时，实际由
/// TransportConfig 的 idle timeout 兜底：本项目只给 server 配置了 10 s，
/// 客户端 `ClientConfig` 未设置 `max_idle_timeout`，因此走 quinn 默认（约 30 s）。
/// 对防火墙丢包地址拨号时，旧实现会在单线程 runtime 里最多挂住 30 s，
/// accept / mDNS 发现 / 心跳全部停摆。这里显式限制为 3 s：局域网内
/// QUIC 握手（TLS 1.3）正常在 100 ms 量级，3 s 已留足重传余量；超时即认为
/// 链路不可达，放弃该地址并由主循环写入退避表（见 `handle_dial_outcome`），
/// 等退避到期或下一次 discovery 信号再重试。
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// 后台拨号任务完成后回传的结果。
enum DialOutcome {
	/// 某个地址拨号成功，携带已建立的 QUIC 连接
	Connected {
		device_id: String,
		addr: std::net::SocketAddr,
		conn: crate::transport::Connection,
	},
	/// 所有候选地址都失败/超时
	Failed {
		device_id: String,
		last_error: Option<String>,
	},
}

/// 广播 / 多会话路径的**非阻塞**投递。
///
/// 返回 `true` 表示命令成功进入该会话的有界命令队列；`false` 表示消息被丢弃：
/// - `Full`：该会话的 actor 消费不过来（队列容量 32 已满）。这里绝不等待，
///   否则一个慢会话会把整个 NetManager 的 select 循环堵住，进而拖住所有对等设备。
/// - `Closed`：actor 已结束，消息无处可去；句柄随后由 `maintain_sessions` 回收。
fn try_send_to_session(
	session: &SessionHandle,
	cmd: SessionCmd,
	log_store: &Mutex<LogStore>,
) -> bool {
	match session.cmd_tx.try_send(cmd) {
		Ok(()) => true,
		Err(mpsc::error::TrySendError::Full(_cmd)) => {
			let device_id = session.device_id();
			let mut log_store = log_store.lock().unwrap();
			let _ = log_store.log_warn(
				"Network",
				&format!(
					"Session command queue full; dropping broadcast message: device_id={device_id}"
				),
				Some(&format!(
					"会话命令队列已满，丢弃广播消息: 设备ID={device_id}"
				)),
			);
			false
		}
		Err(mpsc::error::TrySendError::Closed(_cmd)) => {
			let device_id = session.device_id();
			let mut log_store = log_store.lock().unwrap();
			let _ = log_store.log_debug(
				"Network",
				&format!(
					"Session already closed; dropping broadcast message: device_id={device_id}"
				),
				Some(&format!("会话已关闭，丢弃广播消息: 设备ID={device_id}")),
			);
			false
		}
	}
}

/// 向所有在线会话非阻塞广播命令，返回 `(成功入队数, 丢弃数)`。
fn broadcast_to_online(
	sessions: &[SessionHandle],
	log_store: &Mutex<LogStore>,
	mut make_cmd: impl FnMut() -> SessionCmd,
) -> (usize, usize) {
	let mut sent = 0;
	let mut dropped = 0;
	for session in sessions {
		if !session.is_online() {
			continue;
		}
		if try_send_to_session(session, make_cmd(), log_store) {
			sent += 1;
		} else {
			dropped += 1;
		}
	}
	(sent, dropped)
}

/// 管理退避状态的结构体
struct BackoffState {
	fail_count: u32,
	next_retry_ts: i64,
}

#[derive(Debug)]
pub enum NetCmd {
	BroadcastMeta(Box<crate::model::ItemMeta>),
	GetPeers(oneshot::Sender<Vec<PeerStatus>>),
	Shutdown,
	/// 发起内容拉取请求 (Core -> Session)
	EnsureContentCached {
		item_id: String,
		// FileList 需要指定 file_id，Image/Text 为 None
		file_id: Option<String>,
		// 强制重传？通常 false
		force: bool,
		// 返回 transfer_id (即 req_id)
		reply: oneshot::Sender<anyhow::Result<String>>,
	},

	/// 取消传输
	CancelTransfer {
		transfer_id: String,
	},

	/// 广播删除：从所有设备删除指定 item
	BroadcastDelete {
		item_id: String,
	},
}

impl NetManager {
	pub fn spawn(
		config: crate::api::CoreConfig,
		event_sink: Arc<dyn crate::api::CoreEventSink>,
		store: Arc<Mutex<Store>>,
		cas: crate::cas::Cas,
		log_store: Arc<Mutex<LogStore>>,
	) -> anyhow::Result<mpsc::Sender<NetCmd>> {
		let (cmd_tx, cmd_rx) = mpsc::channel(32);
		let (dial_tx, dial_rx) = mpsc::channel(32);

		std::thread::Builder::new()
            .name("cb-net-manager".to_string())
            .spawn(move || {
                // 1. 创建专用的 Runtime
                //
                // P1 审计：旧实现是 new_current_thread()，accept 循环、mDNS 发现、
                // 全部会话 actor、接收器与上传器都挤在同一个 OS 线程上，任何一个
                // 对等设备的同步阻塞都会让所有对等设备一起卡住（超过 6 s 心跳
                // 超时即全体掉线）。这里改为多线程运行时，让不同会话的任务真正
                // 并行推进。
                //
                // worker_threads 取值：min(available_parallelism, 4)，且不低于 2；
                // 取不到并行度时回退 2。上限压到 4 的理由：本工作负载是 I/O 密集
                // （QUIC 收发、磁盘 CAS、mDNS），线程再多也主要增加上下文切换与
                // 栈内存，收益远小于成本；下限 2 保证“一个 worker 被慢任务占住时，
                // 仍有另一个 worker 推进 accept / 发现 / 心跳”。
                let worker_threads = std::thread::available_parallelism()
                    .map_or(2, |n| n.get().clamp(2, 4));
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(worker_threads)
                    .enable_all()
                    .build()
                    .expect("Failed to build NetManager runtime");

                rt.block_on(async move {
                    // 2. 在 Runtime 内部进行初始化 (Transport 需要绑定 Socket)
                    match Transport::new(0, &config.data_dir, &config.device_id, &config.account_uid) {
                        Ok(transport) => {
                            let transport = Arc::new(transport);
                            let port = transport.local_port().unwrap_or(0);
                            let is_ipv4 = transport.is_ipv4();

                            // 记录 Transport 初始化成功
                            {
                                let mut log_store = log_store.lock().unwrap();
                                let _ = log_store.log_info(
                                    "Network",
								&format!("Transport initialized, local port: {port}, IPv4: {is_ipv4}"),
                                    Some(&format!("传输层已初始化，本地端口: {port}，IPv4: {is_ipv4}")),
                                );
                            }

                            // 3. 启动 Discovery
                            let (disc_tx, disc_rx) = mpsc::channel(32);
							match DiscoveryService::spawn(&config, port, disc_tx) {
                                Ok(discovery) => {
                                    // 记录 Discovery 启动成功
                                    {
                                        let mut log_store = log_store.lock().unwrap();
                                        let _ = log_store.log_info(
                                            "Network",
									&format!("mDNS discovery service started, listening on port: {port}"),
                                        Some(&format!("mDNS 发现服务已启动，监听端口: {port}")),
                                        );
                                    }

                                    let manager = Self {
                                        config,
                                        transport,
                                        discovery,
                                        store,
                                        log_store,
                                        cas,
                                        sessions: Vec::new(),
                                        pending_dials: HashSet::new(),
                                        backoff_map: HashMap::new(),
                                        known_peers: HashMap::new(),
                                        cmd_rx,
                                        discovery_rx: disc_rx,
                                        dial_tx,
                                        dial_rx,
                                        event_sink,
                                    };
                                    // 4. 运行主循环
                                    manager.run().await;
                                }
                                Err(e) => {
                                    let mut log_store = log_store.lock().unwrap();
                                    let _ = log_store.log_error(
                                        "Network",
								&format!("mDNS discovery service failed to start: {e}"),
                                        Some(&format!("mDNS 发现服务启动失败: {e}")),
                                        Some(&e.to_string()),
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            let mut log_store = log_store.lock().unwrap();
                            let _ = log_store.log_error(
                                "Network",
							&format!("Transport initialization failed: {e}"),
                                Some(&format!("传输层初始化失败: {e}")),
                                Some(&e.to_string()),
                            );
                        }
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
						Some(NetCmd::BroadcastMeta(meta)) => self.broadcast_meta(&meta),
						Some(NetCmd::GetPeers(reply_tx)) => {
							// [New] 处理查询请求
							let peers = self.get_peers_info();
							let _ = reply_tx.send(peers);
						}
						Some(NetCmd::Shutdown) => {
							self.shutdown().await;
							break;
						}

						Some(NetCmd::EnsureContentCached { item_id, file_id, force: _, reply }) => {
							// 1. 查库找 owner (A 的 device_id)
							let owner_res = {
								let store = self.store.lock().unwrap();
								// 注意：这行末尾【不要】加分号
								store.get_item_owner(&item_id).ok().flatten()
							};

							if let Some(device_id) = owner_res {
								// 2. 找对应的 Session
								if let Some(session) = self.sessions.iter().find(|s| s.device_id() == device_id && s.is_online()) {
									// 3. 发送命令给 SessionActor
									//
									// 这里保留 send().await：这是针对**单个**目标会话的直接请求，
									// 调用方（ensure_content_cached）正阻塞在 reply_tx 上等待该
									// 传输的结果，命令必须送达这个会话。注意：若该 actor 长时间
									// 不消费（队列满），本 select 循环仍可能被拖住——这是已知的
									// 残留风险，后续可改为 try_send + 立即回复 "session busy"。
									let _ = session.cmd_tx.send(SessionCmd::RequestTransfer {
										item_id,
										file_id,
										reply_tx: reply,
									}).await;
								} else {
									let _ = reply.send(Err(anyhow::anyhow!("Device {device_id} not online")));
								}
							} else {
								 let _ = reply.send(Err(anyhow::anyhow!("Item not found or no owner info")));
							}
						}

						Some(NetCmd::CancelTransfer { transfer_id }) => {
							// 广播给所有 session 尝试取消 (因为 NetManager 不记录 transfer_id 属于哪个 session)
							// 或者 SessionHandle 可以返回它正在处理的 transfer_ids?
							// 简单做法：群发，SessionActor 发现不是自己的会忽略
							// 多会话路径：非阻塞投递，队列满时丢弃并记日志，不阻塞主循环。
							for s in &self.sessions {
								let _ = try_send_to_session(
									s,
									SessionCmd::CancelTransfer { transfer_id: transfer_id.clone() },
									&self.log_store,
								);
							}
						}

						Some(NetCmd::BroadcastDelete { item_id }) => {
							self.broadcast_delete(&item_id);
						}

						None => break,
					}
				}

				// 2. 发现事件
				evt = self.discovery_rx.recv() => {
					if let Some(event) = evt {
						self.handle_discovery_event(event);
					}
				}

				// 3. 拨号任务结果（拨号在独立 spawn 中执行，主循环不等待）
				outcome = self.dial_rx.recv() => {
					if let Some(outcome) = outcome {
						self.handle_dial_outcome(outcome);
					}
				}

				// 4. 入站连接
				conn = self.transport.accept() => {
					if let Some(conn) = conn {
						let addr = conn.remote_address();
						let mut log_store = self.log_store.lock().unwrap();
						let _ = log_store.log_info(
							"Network",
						&format!("Incoming connection accepted from: {addr}, spawning server session"),
						Some(&format!("已接受入站连接: {addr}，正在创建服务器会话")),
						);
						drop(log_store);

						let handle = SessionActor::spawn(
							SessionRole::Server,
							conn,
							self.config.clone(),
							self.event_sink.clone(),
							self.store.clone(),
							self.cas.clone(),
							None,
							self.log_store.clone(),
						);
						self.sessions.push(handle);
					}
				}

				// 5. 定期维护 (清理死链 + 管理退避)
				_ = cleanup_ticker.tick() => {
					self.maintain_sessions();
				}
			}
		}
	}

	/// 收集当前会话状态
	fn get_peers_info(&self) -> Vec<PeerStatus> {
		let mut peers = Vec::new();
		let now = now_ms();
		let account_uid = &self.config.account_uid;

		// 获取 store 锁用于查询策略（只读查询）
		let store = self.store.lock().unwrap();

		// 1. 先把 Session 里的加进去
		for s in &self.sessions {
			let device_id = s.device_id();
			let state = s.public_state();

			// 查询策略（如果不存在则使用默认值）
			let (share_to, accept_from) = match store.get_peer_rule(account_uid, &device_id) {
				Ok(Some(rule)) => (rule.share_to_peer, rule.accept_from_peer),
				_ => (true, true), // 默认都允许
			};

			peers.push(PeerStatus {
				device_id,
				device_name: None, // TODO: 从 HELLO 消息或事件中获取设备名称
				state,
				last_seen_ts_ms: now, // TODO: 从 session 或 known_peers 中获取真实时间
				share_to_peer: share_to,
				accept_from_peer: accept_from,
			});
		}

		// 2. 再把 known_peers 里有但 session 里没有的加为 "Discovered" (可选，文档建议展示所有已发现设备)
		for did in self.known_peers.keys() {
			if peers.iter().any(|p| &p.device_id == did) {
				continue;
			}

			// 优先级：Backoff > Connecting > Discovered
			let state = if let Some(bo) = self.backoff_map.get(did) {
				if now < bo.next_retry_ts {
					PeerConnectionState::Backoff
				} else if self.pending_dials.contains(did) {
					PeerConnectionState::Connecting
				} else {
					// 到点但还没发起拨号：也可以认为是 Connecting 或 Discovered
					PeerConnectionState::Connecting
				}
			} else if self.pending_dials.contains(did) {
				PeerConnectionState::Connecting
			} else {
				PeerConnectionState::Discovered
			};

			// 查询策略（如果不存在则使用默认值）
			let (share_to, accept_from) = match store.get_peer_rule(account_uid, did) {
				Ok(Some(rule)) => (rule.share_to_peer, rule.accept_from_peer),
				_ => (true, true), // 默认都允许
			};

			peers.push(PeerStatus {
				device_id: did.clone(),
				device_name: None, // TODO: 从 discovery 或事件中获取设备名称
				state,
				last_seen_ts_ms: now, // TODO: 从 known_peers 中获取真实时间
				share_to_peer: share_to,
				accept_from_peer: accept_from,
			});
		}

		peers
	}

	fn maintain_sessions(&mut self) {
		let now = now_ms();

		// --- A. 成功连接的“真正”判定 ---
		// 只有当 Session 状态变为 Online 时，才清除退避记录（清零）
		// 如果只是 Handshaking，不要清零，万一握手失败还得继续退避
		for s in &self.sessions {
			if s.is_online() && self.backoff_map.contains_key(&s.device_id()) {
				println!(
					"[Net] Session {} is stable (Online). Resetting backoff.",
					s.device_id()
				);
				self.backoff_map.remove(&s.device_id());
			}
		}

		// --- B. 清理死链 & 生成/升级退避 ---
		let mut dead_ids = Vec::new();
		self.sessions.retain(|s| {
			// 防御网：除了正常路径的 Terminated，actor 任务已结束
			// （cmd_rx 被 drop → cmd_tx.is_closed()）的句柄也必须回收。
			// 否则残留句柄会让 handle_discovery_event 一直认为“该设备已有会话”
			// 而拒绝重连，直到进程重启。
			if s.is_finished() || s.cmd_tx.is_closed() {
				// 彻底挂了
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
			let mut log_store = self.log_store.lock().unwrap();
			let _ = log_store.log_info(
				"Network",
				&format!(
					"Peer in backoff period: device_id={}, retry_after={}, fail_count={}",
					did, entry.next_retry_ts, entry.fail_count
				),
				Some(&format!(
					"对等设备处于退避期: 设备ID={}，重试时间={}，失败次数={}",
					did, entry.next_retry_ts, entry.fail_count
				)),
			);
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
			self.perform_dial(peer);
		}
	}

	fn handle_discovery_event(&mut self, event: DiscoveryEvent) {
		if let DiscoveryEvent::CandidateFound(peer) = event {
			self.known_peers
				.insert(peer.device_id.clone(), peer.clone());

			if self.config.device_id >= peer.device_id {
				return;
			}
			if self
				.sessions
				.iter()
				.any(|s| s.device_id() == peer.device_id)
			{
				return;
			}
			if self.pending_dials.contains(&peer.device_id) {
				return;
			}

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

			self.perform_dial(peer);
		}
	}

	fn broadcast_meta(&self, meta: &crate::model::ItemMeta) {
		if self.config.app_config.global_policy == crate::policy::GlobalPolicy::DenyAll {
			let mut log_store = self.log_store.lock().unwrap();
			let _ = log_store.log_warn(
				"Network",
				&format!(
					"Metadata broadcast denied by DenyAll policy: item_id={}",
					meta.item_id
				),
				Some(&format!(
					"元数据广播被 DenyAll 策略拒绝: 项目ID={}",
					meta.item_id
				)),
			);
			return;
		}

		let online_count = self.sessions.iter().filter(|s| s.is_online()).count();
		if online_count == 0 {
			let mut log_store = self.log_store.lock().unwrap();
			let _ = log_store.log_debug(
				"Network",
				&format!(
					"No online peers available for metadata broadcast: item_id={}",
					meta.item_id
				),
				Some(&format!(
					"无在线对等设备可用于元数据广播: 项目ID={}",
					meta.item_id
				)),
			);
			return;
		}

		{
			let mut log_store = self.log_store.lock().unwrap();
			let _ = log_store.log_info(
				"Network",
				&format!(
					"Broadcasting metadata to {} online peers: item_id={}",
					online_count, meta.item_id
				),
				Some(&format!(
					"正在向 {} 个在线对等设备广播元数据: 项目ID={}",
					online_count, meta.item_id
				)),
			);
		}

		// 多会话广播：逐个 try_send，绝不阻塞等待某个会话的队列。
		// 队列满（Full）或 actor 已结束（Closed）时丢弃该条消息并记日志。
		let _ = broadcast_to_online(&self.sessions, &self.log_store, || {
			SessionCmd::SendMeta(Box::new(meta.clone()))
		});
	}

	fn broadcast_delete(&self, item_id: &str) {
		let online_count = self.sessions.iter().filter(|s| s.is_online()).count();
		{
			let mut log_store = self.log_store.lock().unwrap();
			let _ = log_store.log_info(
				"Network",
				&format!("Broadcasting delete to {online_count} online peers: item_id={item_id}"),
				Some(&format!(
					"正在向 {online_count} 个在线对等设备广播删除: 项目ID={item_id}",
				)),
			);
		}

		// 多会话广播：同 broadcast_meta，使用非阻塞 try_send。
		let _ = broadcast_to_online(&self.sessions, &self.log_store, || SessionCmd::SendDelete {
			item_id: item_id.to_string(),
		});
	}

	/// 发起一次拨号。
	///
	/// 本函数只做地址过滤与“登记”，真正的 `connect` 放在独立 `tokio::spawn`
	/// 任务中执行，结果通过 `dial_tx` 回传给主循环（见 `handle_dial_outcome`）。
	/// 这样即使目标地址被防火墙丢包、拨号要等到 3 s 超时，accept / mDNS 发现 /
	/// 心跳 tick 也不会被阻塞——这正是旧单线程实现的 P1 缺陷之一。
	fn perform_dial(&mut self, peer: PeerCandidate) {
		// 1. 获取本机 Socket 的“血统”
		let i_am_v4 = self.transport.is_ipv4();

		// 2. 智能过滤：只保留跟我血统一致的地址。
		//    这里直接解构 PeerCandidate，把字段 move 进来（device_id 随任务一起
		//    移动），避免为了一次拨号复制整份候选信息。
		let PeerCandidate {
			device_id, addrs, ..
		} = peer;
		let valid_addrs: Vec<std::net::SocketAddr> = addrs
			.into_iter()
			.filter_map(|s| s.parse().ok())
			.filter(|addr: &std::net::SocketAddr| {
				// 如果我是 v4，我只要 v4；如果我是 v6，我只要 v6
				addr.is_ipv4() == i_am_v4
			})
			.collect();

		// 3. 如果 mDNS 这次只发了不匹配的地址（比如我是v4，对方只发了v6）
		// 直接忽略，不要报错，不要 Backoff，静静等待下一波更新
		if valid_addrs.is_empty() {
			return;
		}

		println!(
			"[Net] Initiating connection to {device_id} (Compatible Addrs: {valid_addrs:?})..."
		);
		// 先占位，防止同一 peer 在拨号任务未完成期间被重复拨号
		self.pending_dials.insert(device_id.clone());

		let transport = self.transport.clone();
		let dial_tx = self.dial_tx.clone();

		tokio::spawn(async move {
			let mut last_error = None;

			// 4. 【修改】只遍历有效的 valid_addrs；每个地址最多等 DIAL_TIMEOUT
			for addr in valid_addrs {
				let addr_text = addr.to_string();
				match tokio::time::timeout(DIAL_TIMEOUT, transport.connect(&addr_text)).await {
					// 注意：quinn connect 接受 &str 或 SocketAddr
					Ok(Ok(conn)) => {
						let _ = dial_tx
							.send(DialOutcome::Connected {
								device_id,
								addr,
								conn,
							})
							.await;
						return;
					}
					Ok(Err(e)) => {
						println!("[Net] Failed to connect to {addr}: {e}");
						last_error = Some(format!("{addr}: {e}"));
					}
					Err(_) => {
						// 超时：放弃该地址。等所有地址都处理完后回传 Failed，
						// 由主循环写入退避表（失败路径见 handle_dial_outcome）。
						println!(
							"[Net] Connect to {addr} timed out after {}s",
							DIAL_TIMEOUT.as_secs()
						);
						last_error = Some(format!(
							"{addr}: dial timeout after {}s",
							DIAL_TIMEOUT.as_secs()
						));
					}
				}
			}

			let _ = dial_tx
				.send(DialOutcome::Failed {
					device_id,
					last_error,
				})
				.await;
		});
	}

	/// 处理后台拨号任务的结果。
	///
	/// 成功：创建客户端会话（与旧实现一致，只有第一个连通的地址生效）。
	/// 失败/超时：移出 pending_dials 并写入退避表（指数退避，1s/2s/4s… 上限 64s），
	/// 之后要么由退避到期重试，要么由新的 discovery 信号提前触发。
	fn handle_dial_outcome(&mut self, outcome: DialOutcome) {
		match outcome {
			DialOutcome::Connected {
				device_id,
				addr,
				conn,
			} => {
				println!("[Net] Connected to {device_id} at {addr}, spawning client session");
				let handle = SessionActor::spawn(
					SessionRole::Client,
					conn,
					self.config.clone(),
					self.event_sink.clone(),
					self.store.clone(),
					self.cas.clone(),
					Some(device_id),
					self.log_store.clone(),
				);
				self.sessions.push(handle);
			}
			DialOutcome::Failed {
				device_id,
				last_error,
			} => {
				self.pending_dials.remove(&device_id);

				let now = now_ms();
				let entry = self
					.backoff_map
					.entry(device_id.clone())
					.or_insert(BackoffState {
						fail_count: 0,
						next_retry_ts: 0,
					});
				entry.fail_count += 1;
				let delay = 2u64.pow(entry.fail_count.min(6));
				entry.next_retry_ts = now + (delay * 1000) as i64;

				println!("[Net] Dial failed for {device_id}. Backoff {delay}s");
				let mut log_store = self.log_store.lock().unwrap();
				let _ = log_store.log_warn(
					"Network",
					&format!(
						"Dial failed for device_id={device_id} (last error: {last_error:?}); retry in {delay}s"
					),
					Some(&format!(
						"拨号失败: 设备ID={device_id}（最后错误: {last_error:?}），{delay} 秒后重试"
					)),
				);
			}
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

#[cfg(test)]
mod tests {
	use super::*;

	/// 构造一个状态为 Online、但命令队列已满的会话句柄。
	fn online_session_with_full_queue(
		device_id: &str,
	) -> (SessionHandle, mpsc::Receiver<SessionCmd>) {
		let (cmd_tx, cmd_rx) = mpsc::channel(1);
		// 容量 1：先填满，之后对该会话的 try_send 必然返回 Full。
		cmd_tx
			.try_send(SessionCmd::Shutdown)
			.expect("prefill session command queue");

		let handle = SessionHandle {
			initial_id: device_id.to_string(),
			peer_id: Arc::new(Mutex::new(Some(device_id.to_string()))),
			state: Arc::new(Mutex::new(crate::session::SessionState::Online)),
			cmd_tx,
		};
		(handle, cmd_rx)
	}

	/// 广播路径必须是非阻塞的：某个会话的命令队列已满时，广播应立即返回并把
	/// 该会话计为 dropped，而不是卡在队列上拖住整个 NetManager 主循环。
	///
	/// 用 multi_thread runtime + 独立 task 执行广播：如果实现退化成阻塞等待，
	/// 计时器仍能在另一个 worker 上推进，测试会以 50 ms 超时失败而不是挂死。
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn broadcast_does_not_block_when_session_queue_is_full() {
		let tmp = tempfile::tempdir().expect("tempdir for logs.db");
		let log_store = Mutex::new(LogStore::open(tmp.path()).expect("open log store"));
		// 预热一次日志写入，避免把 SQLite 首次落盘的抖动算进 50 ms 断言。
		log_store
			.lock()
			.unwrap()
			.log_warn("Network", "broadcast test warmup", None)
			.expect("warmup log write");

		let (session, mut cmd_rx) = online_session_with_full_queue("dev-queue-full");

		let broadcast = tokio::spawn(async move {
			// 与 broadcast_meta / broadcast_delete 走同一个广播辅助函数。
			broadcast_to_online(&[session], &log_store, || SessionCmd::SendDelete {
				item_id: "item-1".to_string(),
			})
		});

		let result = tokio::time::timeout(Duration::from_millis(50), broadcast)
			.await
			.expect("broadcast must return promptly when a session queue is full")
			.expect("broadcast task must not panic");

		assert_eq!(result, (0, 1), "full session must be counted as dropped");
		// 被丢弃的广播没有进入队列：队列里仍只有预填的那条 Shutdown。
		assert!(matches!(cmd_rx.try_recv(), Ok(SessionCmd::Shutdown)));
		assert!(
			cmd_rx.try_recv().is_err(),
			"dropped message must not be queued"
		);
	}
}
