// cb_core/src/net/mod.rs

mod data_link;
pub mod ports;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;

use crate::api::{PeerConnectionState, PeerStatus};
use crate::discovery::{DiscoveryEvent, DiscoveryService, PeerCandidate};
use crate::logs::LogSink;
use crate::session::{SessionActor, SessionCmd, SessionHandle, SessionKind, SessionRole};
use crate::store::Store;
use crate::transport::Transport;
use crate::util::now_ms;
use std::sync::Mutex;

use data_link::{
	advertised_endpoint_list, local_endpoint_for_slot, should_use_data_link, slot_count, DataPool,
	ReserveOutcome,
};

/// 建立一条数据连接（拨号 + 完整应用层握手）的总预算。
///
/// 局域网 QUIC + OPAQUE 正常在 100~300 ms；5 s 只是兜底。超时后本次传输
/// 回退到控制连接，绝不让调用方（`ensure_content_cached`）无限等待。
const DATA_LINK_DIAL_TIMEOUT: Duration = Duration::from_secs(3);
const DATA_LINK_ONLINE_TIMEOUT: Duration = Duration::from_secs(5);

/// 本进程同时保留的**入站**数据会话上限。
///
/// 数据端口是公开监听的，认证前的连接也会占用一个会话槽位/一次握手 CPU。
/// 控制连接有既有上限（每设备一条 + 退避），数据连接没有天然上限，因此这里
/// 再加一道资源闸门：超过上限的新连接直接 close（不影响已有传输）。
const MAX_INBOUND_DATA_SESSIONS: usize = 64;

/// 入站数据连接：`(数据端点槽位, 已完成的 QUIC 连接)`。
type AcceptedDataConnection = (usize, crate::transport::Connection);

/// 网络层管理器
pub struct NetManager {
	config: crate::api::CoreConfig,
	transport: Arc<Transport>,
	discovery: DiscoveryService,
	store: Arc<Mutex<Store>>,
	log_sink: LogSink,
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

	// --- C1（ADR-002 §4.3）：多数据端点 / 每 peer 数据连接池 ---
	/// 每 peer 数据连接池（拨号侧/拉取发起侧使用；被 dial 的发送方由各自
	/// endpoint 承担负载，连接在建立时即绑定到某个数据 Endpoint）。
	data_pool: Arc<DataPool>,
	/// 本端**入站**数据会话（其他 peer 拨到我们的数据端口）。与控制会话分开
	/// 维护：它们不参与 `list_peers`、不广播元数据、不发 PEER_ONLINE/OFFLINE。
	data_sessions: Vec<SessionHandle>,
	/// 各数据 endpoint 的 accept loop 汇总到这里，由主循环 spawn 数据会话。
	data_accept_rx: mpsc::Receiver<AcceptedDataConnection>,
	/// 本端实际绑定的数据端口（通告给对端；空 = 只有控制连接）。
	local_data_ports: Vec<u16>,
	/// 控制连接 server 端按接受顺序轮转数据端口用的计数器。
	next_data_port_rotation: usize,
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
fn try_send_to_session(session: &SessionHandle, cmd: SessionCmd, log_sink: &LogSink) -> bool {
	match session.cmd_tx.try_send(cmd) {
		Ok(()) => true,
		Err(mpsc::error::TrySendError::Full(_cmd)) => {
			let device_id = session.device_id();
			log_sink.log_warn(
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
			log_sink.log_debug(
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
	log_sink: &LogSink,
	mut make_cmd: impl FnMut() -> SessionCmd,
) -> (usize, usize) {
	let mut sent = 0;
	let mut dropped = 0;
	for session in sessions {
		if !session.is_online() {
			continue;
		}
		if try_send_to_session(session, make_cmd(), log_sink) {
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

/// 把 `RequestTransfer` 投递给某个会话；队列已关闭时把 `reply_tx` 还回来，
/// 让调用方可以回退到控制连接或在本地报错（绝不把调用方永久挂在 oneshot 上）。
async fn try_send_transfer_request(
	handle: &SessionHandle,
	item_id: String,
	file_id: Option<String>,
	reply_tx: oneshot::Sender<anyhow::Result<String>>,
) -> Result<(), oneshot::Sender<anyhow::Result<String>>> {
	let cmd = SessionCmd::RequestTransfer {
		item_id,
		file_id,
		reply_tx,
	};
	match handle.cmd_tx.send(cmd).await {
		Ok(()) => Ok(()),
		Err(mpsc::error::SendError(SessionCmd::RequestTransfer { reply_tx, .. })) => Err(reply_tx),
		Err(_) => unreachable!("仅发送 RequestTransfer，错误命令不可能是别的变体"),
	}
}

/// C1：数据连接不可用时的安全回退——把本次传输交给控制连接。
///
/// 回退是**静默且安全**的：控制连接上的一切语义与引入 C1 之前完全相同，
/// 只是大传输不会得到多 endpoint 的并行收益。
async fn fallback_to_control(
	control: &SessionHandle,
	item_id: String,
	file_id: Option<String>,
	reply: oneshot::Sender<anyhow::Result<String>>,
) {
	if let Err(reply_tx) = try_send_transfer_request(control, item_id, file_id, reply).await {
		let _ = reply_tx.send(Err(anyhow::anyhow!(
			"control session command queue closed before transfer request"
		)));
	}
}

/// 后台建立一条 C1 数据连接并投递传输请求所需的全部上下文。
///
/// 建立流程放在独立 `tokio::spawn` 里：拨号 + QUIC + OPAQUE/TOFU 握手可能要
/// 几百毫秒，绝不能在 `NetManager` 主循环里 await（会拖住 accept/发现/心跳）。
struct DataLinkJob {
	transport: Arc<Transport>,
	pool: Arc<DataPool>,
	control: SessionHandle,
	peer_id: String,
	peer_ip: IpAddr,
	peer_port: u16,
	/// 连接池槽位（用于 `mark_created` / `mark_failed`）。
	slot: usize,
	/// 拨号使用的本端数据 endpoint 序号（与池槽位解耦：轮转后的列表顺序
	/// 与本地 endpoint 序号没有语义关系）。
	data_endpoint: usize,
	local_data_ports: Vec<u16>,
	config: crate::api::CoreConfig,
	event_sink: Arc<dyn crate::api::CoreEventSink>,
	store: Arc<Mutex<Store>>,
	cas: crate::cas::Cas,
	log_sink: LogSink,
	item_id: String,
	file_id: Option<String>,
	total_bytes: u64,
	reply: oneshot::Sender<anyhow::Result<String>>,
}

/// C1：为一次大传输建立（或失败后重建）数据连接，并把 `ContentGet` 投过去。
///
/// 失败路径统一回退控制连接：数据连接的问题只影响"本次传输用哪条连接"，
/// 不会影响控制会话，也不会让调用方卡死。
async fn establish_data_link_and_request(job: DataLinkJob) {
	let DataLinkJob {
		transport,
		pool,
		control,
		peer_id,
		peer_ip,
		peer_port,
		slot,
		data_endpoint,
		local_data_ports,
		config,
		event_sink,
		store,
		cas,
		log_sink,
		item_id,
		file_id,
		total_bytes,
		reply,
	} = job;

	let addr = std::net::SocketAddr::new(peer_ip, peer_port);

	// 1. 从第 `data_endpoint` 个本地数据端点拨号，连到对端通告的数据端口。
	//    连接一旦建立就绑定在这个本地 Endpoint 的 socket/驱动上（负载分摊来源）。
	let connected = tokio::time::timeout(
		DATA_LINK_DIAL_TIMEOUT,
		transport.connect_data(data_endpoint, &addr.to_string()),
	)
	.await;
	let conn = match connected {
		Ok(Ok(conn)) => conn,
		Ok(Err(error)) => {
			pool.mark_failed(&peer_id, slot);
			log_sink.log_warn(
				"Network",
				&format!(
					"C1 data link dial failed: peer={peer_id}, slot={slot}, addr={addr}, error={error}"
				),
				Some(&format!(
					"C1 数据连接拨号失败，回退控制连接: 设备ID={peer_id}，槽位={slot}，地址={addr}，错误={error}"
				)),
			);
			fallback_to_control(&control, item_id, file_id, reply).await;
			return;
		}
		Err(_) => {
			pool.mark_failed(&peer_id, slot);
			log_sink.log_warn(
				"Network",
				&format!(
					"C1 data link dial timed out: peer={peer_id}, slot={slot}, addr={addr}, timeout={DATA_LINK_DIAL_TIMEOUT:?}"
				),
				Some(&format!(
					"C1 数据连接拨号超时，回退控制连接: 设备ID={peer_id}，槽位={slot}，地址={addr}"
				)),
			);
			fallback_to_control(&control, item_id, file_id, reply).await;
			return;
		}
	};

	// 2. 数据连接跑**与控制连接完全相同**的 SessionActor 握手（TLS/OPAQUE/TOFU），
	//    只是标记为 Data：不对外发 PEER_ONLINE/OFFLINE、不进 list_peers。
	let link = SessionActor::spawn_with_sink(
		SessionRole::Client,
		conn,
		config,
		event_sink,
		store,
		cas,
		Some(peer_id.clone()),
		log_sink.clone(),
		SessionKind::Data,
		local_data_ports,
	);

	let deadline = tokio::time::Instant::now() + DATA_LINK_ONLINE_TIMEOUT;
	loop {
		if link.is_online() {
			break;
		}
		if link.is_finished() || link.cmd_tx.is_closed() {
			break;
		}
		if tokio::time::Instant::now() >= deadline {
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}

	if !link.is_online() {
		pool.mark_failed(&peer_id, slot);
		let _ = link.cmd_tx.try_send(SessionCmd::Shutdown);
		log_sink.log_warn(
			"Network",
			&format!("C1 data link handshake failed: peer={peer_id}, slot={slot}, remote={addr}"),
			Some(&format!(
				"C1 数据连接握手失败，回退控制连接: 设备ID={peer_id}，槽位={slot}，远端={addr}"
			)),
		);
		fallback_to_control(&control, item_id, file_id, reply).await;
		return;
	}

	if !pool.mark_created(&peer_id, slot, link.clone()) {
		// 池在此期间被重建（控制连接重连/端口变化）：这条连接已无意义。
		let _ = link.cmd_tx.try_send(SessionCmd::Shutdown);
		log_sink.log_warn(
			"Network",
			&format!("C1 data link invalidated by pool reset: peer={peer_id}, slot={slot}"),
			Some(&format!(
				"C1 数据连接因连接池重建而作废: 设备ID={peer_id}，槽位={slot}"
			)),
		);
		fallback_to_control(&control, item_id, file_id, reply).await;
		return;
	}

	let local_port = transport
		.data_ports()
		.get(data_endpoint)
		.copied()
		.unwrap_or(0);
	log_sink.log_info(
		"Network",
		&format!(
			"C1 data link established: peer={peer_id}, slot={slot}, local_data_endpoint={data_endpoint}, local_data_port={local_port}, peer_data_port={peer_port}, bytes={total_bytes}"
		),
		Some(&format!(
			"C1 数据连接已建立: 设备ID={peer_id}，槽位={slot}，本地数据端点={data_endpoint}，本地数据端口={local_port}，对端数据端口={peer_port}，字节={total_bytes}"
		)),
	);

	let fallback_item = item_id.clone();
	let fallback_file = file_id.clone();
	if let Err(reply_tx) = try_send_transfer_request(&link, item_id, file_id, reply).await {
		// 连接在握手完成后、投递前死亡：清掉槽位，等下次使用重建。
		pool.mark_failed(&peer_id, slot);
		let _ = link.cmd_tx.try_send(SessionCmd::Shutdown);
		fallback_to_control(&control, fallback_item, fallback_file, reply_tx).await;
	}
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
		log_sink: LogSink,
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
				// worker_threads 取值：min(available_parallelism, 8)，且不低于 2；
				// 取不到并行度时回退 2。
				//
				// 上限从 4 提到 8 的依据（release 构建、32 逻辑核、回环、纯 QUIC 基线）：
				// - 4 条并发连接：4 workers = 402 MiB/s → 8 workers = 478 MiB/s（+19%）；
				// - 4 个 endpoint：401 MiB/s → 534 MiB/s（+33%）；
				// - 单连接：195 MiB/s → 210 MiB/s（+7%）；
				// - 16 workers 相对 8 workers 无进一步收益，因此封顶 8（不取 16）。
				// 上限仍与 min(available_parallelism, …) 结合：手机 / 低核设备并行度低，
				// 会自然夹到更小的值；下限 2 保证“一个 worker 被慢任务占住时，
				// 仍有另一个 worker 推进 accept / 发现 / 心跳”。
				let worker_threads =
					std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
				let rt = tokio::runtime::Builder::new_multi_thread()
					.worker_threads(worker_threads)
					.enable_all()
					.build()
					.expect("Failed to build NetManager runtime");

				rt.block_on(async move {
					// 2. 在 Runtime 内部进行初始化 (Transport 需要绑定 Socket)
					match Transport::new(
						0,
						&config.data_dir,
						&config.device_id,
						&config.account_uid,
					) {
						Ok(transport) => {
							let transport = Arc::new(transport);
							let port = transport.local_port().unwrap_or(0);
							let is_ipv4 = transport.is_ipv4();

							// 记录 Transport 初始化成功
							log_sink.log_info(
								"Network",
								&format!(
									"Transport initialized, local port: {port}, IPv4: {is_ipv4}"
								),
								Some(&format!(
									"传输层已初始化，本地端口: {port}，IPv4: {is_ipv4}"
								)),
							);

							// 3. 启动 Discovery
							let (disc_tx, disc_rx) = mpsc::channel(32);
							match DiscoveryService::spawn(&config, port, disc_tx) {
								Ok(discovery) => {
									// 记录 Discovery 启动成功
									log_sink.log_info(
                                        "Network",
									&format!("mDNS discovery service started, listening on port: {port}"),
                                    Some(&format!("mDNS 发现服务已启动，监听端口: {port}")),
                                    );

									// 3b. C1：为每个数据 endpoint 启动独立的 accept loop。
									//
									// 每个 loop 只认领一个 Endpoint 的入站连接（quinn 每个
									// Endpoint 本来就有独立的 socket/接收循环，正是 ADR-002
									// §5.3 收益的来源）。连接在这里只完成 QUIC 握手，随后交给
									// 主循环 spawn 正常的 SessionActor（TLS/OPAQUE/TOFU 与控制
									// 会话完全相同）。
									let (data_accept_tx, data_accept_rx) = mpsc::channel(32);
									for slot in 0..transport.data_endpoint_count() {
										let transport = transport.clone();
										let tx = data_accept_tx.clone();
										tokio::spawn(async move {
											loop {
												match transport.accept_data(slot).await {
													Some(conn) => {
														if tx.send((slot, conn)).await.is_err() {
															// 主循环已退出：释放 Endpoint 引用。
															break;
														}
													}
													None => {
														// None 既可能是握手失败（endpoint 仍健康），
														// 也可能是端点已关闭；只有后者才退出。
														if transport.is_shutdown() {
															break;
														}
													}
												}
											}
										});
									}
									drop(data_accept_tx);

									let local_data_ports = transport.data_ports().to_vec();
									log_sink.log_info(
										"Network",
										&format!(
											"C1 data endpoints ready: count={}, ports={:?}",
											transport.data_endpoint_count(),
											local_data_ports
										),
										Some(&format!(
											"C1 数据端点已就绪: 数量={}，端口={:?}",
											transport.data_endpoint_count(),
											local_data_ports
										)),
									);

									let manager = Self {
										config,
										transport,
										discovery,
										store,
										log_sink,
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
										data_pool: Arc::new(DataPool::default()),
										data_sessions: Vec::new(),
										data_accept_rx,
										local_data_ports,
										next_data_port_rotation: 0,
									};
									// 4. 运行主循环
									manager.run().await;
								}
								Err(e) => {
									log_sink.log_error(
										"Network",
										&format!("mDNS discovery service failed to start: {e}"),
										Some(&format!("mDNS 发现服务启动失败: {e}")),
										Some(&e.to_string()),
									);
								}
							}
						}
						Err(e) => {
							log_sink.log_error(
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
							let peers = self.get_peers_info().await;
							let _ = reply_tx.send(peers);
						}
						Some(NetCmd::Shutdown) => {
							self.shutdown().await;
							break;
						}

						Some(NetCmd::EnsureContentCached { item_id, file_id, force: _, reply }) => {
							// 1. 查库找 owner (A 的 device_id) 与传输体积。
							// DB 查询放进 spawn_blocking：SQLite busy/慢盘不能卡住主循环。
							//
							// C1 需要 `total_bytes` 才能在"小传输走控制连接 / 大传输走数据连接池"
							// 之间做选择（阈值 16 MiB，见 net::data_link）。
							let store = self.store.clone();
							let lookup_item_id = item_id.clone();
							let lookup_file_id = file_id.clone();
							let (owner_res, total_bytes) = tokio::task::spawn_blocking(move || {
								let store = store.lock().unwrap();
								let owner = store.get_item_owner(&lookup_item_id).ok().flatten();
								let total_bytes = store
									.get_item_meta_by_id(&lookup_item_id)
									.ok()
									.flatten()
									.map_or(0u64, |meta| {
										if let Some(file_id) = lookup_file_id.as_deref() {
											meta.files
												.iter()
												.find(|file| file.file_id == file_id)
												.and_then(|file| u64::try_from(file.size_bytes).ok())
												.unwrap_or_else(|| {
													u64::try_from(meta.content.total_bytes).unwrap_or(0)
												})
										} else {
											u64::try_from(meta.content.total_bytes).unwrap_or(0)
										}
									});
								(owner, total_bytes)
							})
							.await
							.unwrap_or((None, 0));

							if let Some(device_id) = owner_res {
								// 2. 找对应的控制会话（C1：数据会话不参与这里的查找，
								//    控制连接永远是 peer 的代表）。
								let control = self
									.sessions
									.iter()
									.find(|s| s.device_id() == device_id && s.is_online())
									.cloned();

								if let Some(control) = control {
									// C1：对端通告的 endpoint 列表（含控制连接哨兵 0）。
									let peer_endpoints = control.peer_data_ports();
									let peer_ip = control.remote_addr.map(|addr| addr.ip());
									let local_endpoints = self.transport.data_endpoint_count();
									let use_pool = should_use_data_link(
										total_bytes,
										&peer_endpoints,
										local_endpoints,
									) && peer_ip.is_some();

									if use_pool {
										let peer_ip = peer_ip.expect("use_pool 已要求 remote_addr 存在");
										let slots = slot_count(local_endpoints, peer_endpoints.len());
										match self.data_pool.reserve(
											&device_id,
											peer_ip,
											&peer_endpoints,
											slots,
											Some(&control),
										) {
											ReserveOutcome::Existing { slot, handle } => {
												self.log_sink.log_info(
													"Network",
													&format!(
														"C1 transfer routed to existing data link: peer={device_id}, slot={slot}, bytes={total_bytes}"
													),
													Some(&format!(
														"C1 传输已分配到已有数据连接: 设备ID={device_id}，槽位={slot}，字节={total_bytes}"
													)),
												);
												// 投递失败（连接刚好断开）→ 回退控制连接；
												// 绝不因为数据连接问题让大传输直接失败。
												let fallback_item = item_id.clone();
												let fallback_file = file_id.clone();
												if let Err(reply_tx) = try_send_transfer_request(
													&handle,
													item_id,
													file_id,
													reply,
												)
												.await
												{
													fallback_to_control(
														&control,
														fallback_item,
														fallback_file,
														reply_tx,
													)
													.await;
												}
											}
											ReserveOutcome::Control { slot } => {
												// 轮转把控制连接分配给了本次传输：直接走控制连接
												// （与引入 C1 之前的路径相同，控制面语义不变）。
												self.log_sink.log_info(
													"Network",
													&format!(
														"C1 transfer routed to control connection: peer={device_id}, slot={slot}, bytes={total_bytes}"
													),
													Some(&format!(
														"C1 传输已分配到控制连接: 设备ID={device_id}，槽位={slot}，字节={total_bytes}"
													)),
												);
												if let Err(reply_tx) = try_send_transfer_request(
													&control,
													item_id,
													file_id,
													reply,
												)
												.await
												{
													let _ = reply_tx.send(Err(anyhow::anyhow!(
														"control session command queue closed before transfer request"
													)));
												}
											}
											ReserveOutcome::Create { slot } => {
												// 懒建立：后台拨号 + 完整握手，完成后把传输投给
												// 新连接；失败则回退控制连接。
												let peer_port = peer_endpoints[slot];
												let data_endpoint =
													local_endpoint_for_slot(slot, local_endpoints);
												let job = DataLinkJob {
													transport: self.transport.clone(),
													pool: self.data_pool.clone(),
													control: control.clone(),
													peer_id: device_id.clone(),
													peer_ip,
													peer_port,
													slot,
													data_endpoint,
													local_data_ports: advertised_endpoint_list(
														&self.local_data_ports,
														0,
													),
													config: self.config.clone(),
													event_sink: self.event_sink.clone(),
													store: self.store.clone(),
													cas: self.cas.clone(),
													log_sink: self.log_sink.clone(),
													item_id,
													file_id,
													total_bytes,
													reply,
												};
												tokio::spawn(establish_data_link_and_request(job));
											}
											ReserveOutcome::Unavailable => {
												// 所有连接都在建立中/满载：本次走控制连接，
												// 行为与今天一致（不会阻塞或失败）。
												fallback_to_control(
													&control,
													item_id,
													file_id,
													reply,
												)
												.await;
											}
										}
									} else {
										// 小传输 / 对端不通告数据端口 / 本端 K=1：
										// 保持引入 C1 之前的行为（控制连接）。
										if let Err(reply_tx) = try_send_transfer_request(
											&control,
											item_id,
											file_id,
											reply,
										)
										.await
										{
											let _ = reply_tx.send(Err(anyhow::anyhow!(
												"session command queue closed before transfer request"
											)));
										}
									}
								} else {
									let _ = reply.send(Err(anyhow::anyhow!(
										"Device {device_id} not online"
									)));
								}
							} else {
								 let _ = reply.send(Err(anyhow::anyhow!("Item not found or no owner info")));
							}
						}

						Some(NetCmd::CancelTransfer { transfer_id }) => {
							// 广播给所有会话尝试取消（NetManager 不记录 transfer_id 属于哪个会话，
							// SessionActor 发现不是自己的会忽略）。C1 的数据会话也必须覆盖，
							// 否则分配到大传输数据连接上的取消会静默失效。
							// 多会话路径：非阻塞投递，队列满时丢弃并记日志，不阻塞主循环。
							let data_pool_sessions = self.data_pool.sessions();
							for s in self
								.sessions
								.iter()
								.chain(self.data_sessions.iter())
								.chain(data_pool_sessions.iter())
							{
								let _ = try_send_to_session(
									s,
									SessionCmd::CancelTransfer { transfer_id: transfer_id.clone() },
									&self.log_sink,
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

				// 4. 入站控制连接
				conn = self.transport.accept() => {
					if let Some(conn) = conn {
						let addr = conn.remote_address();
						self.log_sink.log_info(
							"Network",
						&format!("Incoming connection accepted from: {addr}, spawning server session"),
						Some(&format!("已接受入站连接: {addr}，正在创建服务器会话")),
						);

						// C1：控制连接的 server 端按接受顺序轮转自己通告的 endpoint
						// 列表（含控制连接哨兵），让并发 peer 的第 0 条连接落到
						// 不同的 endpoint。
						let advertised = self.next_advertised_endpoints();
						let handle = SessionActor::spawn_with_sink(
							SessionRole::Server,
							conn,
							self.config.clone(),
							self.event_sink.clone(),
							self.store.clone(),
							self.cas.clone(),
							None,
							self.log_sink.clone(),
							SessionKind::Control,
							advertised,
						);
						self.sessions.push(handle);
					}
				}

				// 4b. 入站数据连接（C1）：每个数据 endpoint 的 accept loop 汇总到这里。
				// 数据连接同样跑完整握手/认证，只是标记为 Data，不当成 peer 的代表。
				accepted = self.data_accept_rx.recv(), if !self.data_accept_rx.is_closed() => {
					if let Some((slot, conn)) = accepted {
						let addr = conn.remote_address();
						if self.data_sessions.len() >= MAX_INBOUND_DATA_SESSIONS {
							self.log_sink.log_warn(
								"Network",
								&format!(
									"Rejecting inbound data connection: limit reached ({MAX_INBOUND_DATA_SESSIONS}), from={addr}, slot={slot}"
								),
								Some(&format!(
									"拒绝入站数据连接：已达上限 {MAX_INBOUND_DATA_SESSIONS}，来源={addr}，槽位={slot}"
								)),
							);
							conn.close(0u32.into(), b"too many data sessions");
						} else {
							self.log_sink.log_info(
								"Network",
								&format!(
									"Inbound data connection accepted: from={addr}, data_endpoint_slot={slot}, spawning data session"
								),
								Some(&format!(
									"已接受入站数据连接: 来源={addr}，数据端点槽位={slot}，正在创建数据会话"
								)),
							);
							let handle = SessionActor::spawn_with_sink(
								SessionRole::Server,
								conn,
								self.config.clone(),
								self.event_sink.clone(),
								self.store.clone(),
								self.cas.clone(),
								None,
								self.log_sink.clone(),
								SessionKind::Data,
								// 数据连接也通告本端 endpoint（对端可能反向复用
								// 这条连接的身份信息；未轮转即可）。
								advertised_endpoint_list(&self.local_data_ports, 0),
							);
							self.data_sessions.push(handle);
						}
					}
				}

				// 5. 定期维护 (清理死链 + 管理退避)
				_ = cleanup_ticker.tick() => {
					self.maintain_sessions();
				}
			}
		}
	}

	/// C1：为一条新接受的控制连接生成要通告的 endpoint 列表。
	///
	/// 列表 = `[P+1, ..., P+(K-1), 0]` 再按本 NetManager **接受控制连接的顺序**
	/// 轮转（`0` 是控制连接哨兵，见 `data_link::CONTROL_CONNECTION_SENTINEL`）。
	/// 第 n 个 peer 看到的列表从 `n % K` 开始，于是多个 peer 并发拉取时，它们
	/// 各自的"第 0 条连接"会落到本机不同的 endpoint 上（这是 C1 吞吐收益的
	/// 来源；4 个 peer 时刚好铺满 4 个 endpoint）。
	///
	/// 没有数据端点（K=1）时返回空列表：对端会退化为单连接，行为与今天一致。
	fn next_advertised_endpoints(&mut self) -> Vec<u16> {
		if self.local_data_ports.is_empty() {
			return Vec::new();
		}
		let rotation = self.next_data_port_rotation;
		self.next_data_port_rotation = self.next_data_port_rotation.wrapping_add(1);
		advertised_endpoint_list(&self.local_data_ports, rotation)
	}

	/// 收集当前会话状态。
	///
	/// 只读策略查询放进 `spawn_blocking`：SQLite 的 `busy_timeout` 最长 5 s，
	/// 绝不能让它占住 NetManager 主循环（心跳/accept/发现都在同一 loop 上）。
	async fn get_peers_info(&self) -> Vec<PeerStatus> {
		let now = now_ms();
		let account_uid = self.config.account_uid.clone();

		// 1. 先在内存里收集 (device_id, state)：session 在前，再补 known_peers。
		let mut base: Vec<(String, PeerConnectionState)> = Vec::new();
		for s in &self.sessions {
			base.push((s.device_id(), s.public_state()));
		}
		for did in self.known_peers.keys() {
			if base.iter().any(|(id, _)| id == did) {
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

			base.push((did.clone(), state));
		}

		// 2. 一次性在 blocking 线程查询所有策略（不存在则默认都允许）。
		let store = self.store.clone();
		let device_ids: Vec<String> = base.iter().map(|(id, _)| id.clone()).collect();
		let rule_count = device_ids.len();
		let rules = tokio::task::spawn_blocking(move || {
			let store = store.lock().unwrap();
			device_ids
				.iter()
				.map(|did| match store.get_peer_rule(&account_uid, did) {
					Ok(Some(rule)) => (rule.share_to_peer, rule.accept_from_peer),
					_ => (true, true), // 默认都允许
				})
				.collect::<Vec<_>>()
		})
		.await
		.unwrap_or_else(|_| vec![(true, true); rule_count]);

		base.into_iter()
			.zip(rules)
			.map(
				|((device_id, state), (share_to_peer, accept_from_peer))| PeerStatus {
					device_id,
					device_name: None, // TODO: 从 HELLO 消息或事件中获取设备名称
					state,
					last_seen_ts_ms: now, // TODO: 从 session 或 known_peers 中获取真实时间
					share_to_peer,
					accept_from_peer,
				},
			)
			.collect()
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

		// C1：控制连接断开的 peer，其数据连接池一并回收（数据连接本身多半也
		// 已经断了；留着只会让后续传输选中死连接）。连接池与实时数据会话是两套
		// 视图：池持有"我们主动拨出的"数据会话，data_sessions 持有"别人拨进来的"。
		for did in &dead_ids {
			for handle in self.data_pool.drop_peer(did) {
				let _ = handle.cmd_tx.try_send(SessionCmd::Shutdown);
			}
		}
		self.data_sessions
			.retain(|s| !(s.is_finished() || s.cmd_tx.is_closed()));

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
			self.log_sink.log_info(
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
			self.log_sink.log_warn(
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
			self.log_sink.log_debug(
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

		self.log_sink.log_info(
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

		// 多会话广播：逐个 try_send，绝不阻塞等待某个会话的队列。
		// 队列满（Full）或 actor 已结束（Closed）时丢弃该条消息并记日志。
		let _ = broadcast_to_online(&self.sessions, &self.log_sink, || {
			SessionCmd::SendMeta(Box::new(meta.clone()))
		});
	}

	fn broadcast_delete(&self, item_id: &str) {
		let online_count = self.sessions.iter().filter(|s| s.is_online()).count();
		self.log_sink.log_info(
			"Network",
			&format!("Broadcasting delete to {online_count} online peers: item_id={item_id}"),
			Some(&format!(
				"正在向 {online_count} 个在线对等设备广播删除: 项目ID={item_id}",
			)),
		);

		// 多会话广播：同 broadcast_meta，使用非阻塞 try_send。
		let _ = broadcast_to_online(&self.sessions, &self.log_sink, || SessionCmd::SendDelete {
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
				// C1：拨号侧也按顺序轮转自己通告的 endpoint 列表。很多拓扑里
				// "id 较小的一端拨号"（本仓库 `handle_discovery_event` 就是如此），
				// 此时**发送方**往往正是拨号方；它无法在 Hello 之前知道对端 id，
				// 但可以按拨号顺序轮转，让第 n 个 peer 从第 n % K 个 endpoint 开始。
				let advertised = self.next_advertised_endpoints();
				let handle = SessionActor::spawn_with_sink(
					SessionRole::Client,
					conn,
					self.config.clone(),
					self.event_sink.clone(),
					self.store.clone(),
					self.cas.clone(),
					Some(device_id),
					self.log_sink.clone(),
					SessionKind::Control,
					advertised,
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
				self.log_sink.log_warn(
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
		// 先关控制会话与数据会话（它们持有 Connection），再关 Transport 的
		// 全部 Endpoint；最后清空连接池，保证没有遗留的数据连接。
		for s in &self.sessions {
			s.shutdown().await;
		}
		for s in &self.data_sessions {
			s.shutdown().await;
		}
		for s in self.data_pool.take_all() {
			s.shutdown().await;
		}
		self.transport.shutdown();
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
			kind: SessionKind::Control,
			remote_addr: None,
			peer_data_ports: Arc::new(Mutex::new(Vec::new())),
			load: Arc::new(crate::session::TransferLoad::default()),
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
		let log_store = Arc::new(Mutex::new(
			crate::logs::LogStore::open(tmp.path()).expect("open log store"),
		));
		let log_sink = LogSink::new(log_store);
		// 预热一次日志入队（writer 线程异步落盘，不参与 50 ms 断言）。
		log_sink.log_warn("Network", "broadcast test warmup", None);

		let (session, mut cmd_rx) = online_session_with_full_queue("dev-queue-full");

		let broadcast = tokio::spawn(async move {
			// 与 broadcast_meta / broadcast_delete 走同一个广播辅助函数。
			broadcast_to_online(&[session], &log_sink, || SessionCmd::SendDelete {
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
