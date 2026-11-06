//! C1 每 peer 数据连接池（ADR-002 §4.3 / §5.3）。
//!
//! 设计要点（任务 C1 第 4/5/6 条）：
//!
//! - **池的归属**：一次拉取由*接收方*发起（`ensure_content_cached` → `ContentGet`），
//!   因此"用哪条连接"的选择发生在接收方；被选中的连接在发送方一端由某个
//!   **独立 `Endpoint`** 承载（连接建立后固定在某个 endpoint 的 socket / 驱动上），
//!   从而把发送方的负载摊到多个 endpoint 上。
//! - **池包含控制连接**：每 peer 的池最多 `K` 条连接 = 1 条控制连接 + `K-1` 条
//!   数据连接（`K = CB_QUIC_DATA_ENDPOINTS`，K=1 时不启用池 = 今天的行为）。
//!   大传输可以落在控制连接上（B1 起数据流与控制流本就是不同的 QUIC 流，
//!   控制面语义不变）；小传输仍然只走控制连接。
//! - **发送方轮转**：控制连接的 server 端为每个新接受的 peer 轮转一次自己通告
//!   的 endpoint 列表（控制连接用哨兵 `0` 表示），于是不同 peer 的第 0 条连接
//!   落到发送方不同的 endpoint 上（见 [`advertised_endpoint_list`]）。
//! - **一个传输只走一条连接**：不做跨连接分片（C2 才做），所以并发传输靠
//!   多条连接并行，单传输仍然只占一条。
//! - **小文件跳过池**：`total_bytes < 16 MiB` 继续走控制连接，行为与今天一致。
//! - **懒建立**：只有第一次大传输（或已有连接都满载）才建立新数据连接；
//!   失败只让本次传输回退到控制连接，不影响控制面。
//! - **上限**：`select_slot` 会跳过已有 `MAX_ACTIVE_TRANSFERS_PER_LINK` 个在途
//!   传输的连接，与 `SessionActor` 的每连接 `MAX_CONCURRENT_RECEIVERS = 8`
//!   对齐；所有连接都满时返回 `Unavailable`，调用方回退控制连接。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use crate::session::{SessionCmd, SessionHandle};

/// 只有达到该体积的传输才考虑使用连接池。
///
/// 16 MiB 的依据：ADR-002 §3.3 的冷启动分档以 8 MiB 为界，C1 再保守一档，
/// 避免为几 MB 的图片/文本付出一次完整 QUIC + OPAQUE 握手（~100 ms）。
pub const DATA_LINK_MIN_TRANSFER_BYTES: u64 = 16 * 1024 * 1024;

/// 单条连接允许的在途传输数，与 `SessionActor` 的每连接
/// `MAX_CONCURRENT_RECEIVERS = 8` 对齐。
///
/// 说明（任务 C1 第 6 条"上限按连接还是按 peer"）：**权威上限仍是每连接**
/// （`SessionActor` 未改动：8 个并发 sender / 8 个并发 receiver、每传输 6 条
/// 子流、每会话 24 条子流）。因此一个 peer 在 K 条连接下的理论上限是
/// K × 8；本策略在"分配"这一层再跳过已满 8 个在途传输的连接，避免把新传输
/// 压到已经饱和的链路上。
pub const MAX_ACTIVE_TRANSFERS_PER_LINK: usize = 8;

/// endpoint 通告列表里代表"使用已有控制连接"的哨兵值。
///
/// 真实的 UDP 端口不可能是 0（绑定 0 表示让内核分配），因此用它表示
/// "这一条连接不是新数据端口，而是控制连接本身"。控制连接也占一个池槽位，
/// 这样池的大小恰好是 K（控制 1 + 数据 K-1），轮转后并发 peer 能均匀铺满
/// 发送方的全部 endpoint。
pub const CONTROL_CONNECTION_SENTINEL: u16 = 0;

/// 是否值得为大传输启用连接池。
///
/// 三个条件缺一不可：
/// 1. `total_bytes >= 16 MiB`（小传输走控制连接）；
/// 2. 对端通告了 endpoint 列表（旧对端/K=1 对端为空 → 单连接）；
/// 3. 本端至少有一个额外数据端点（`CB_QUIC_DATA_ENDPOINTS >= 2`）。
pub fn should_use_data_link(
	total_bytes: u64,
	peer_endpoints: &[u16],
	local_data_endpoints: usize,
) -> bool {
	total_bytes >= DATA_LINK_MIN_TRANSFER_BYTES
		&& !peer_endpoints.is_empty()
		&& local_data_endpoints > 0
}

/// 按 `rotation` 轮转列表（控制连接哨兵也参与轮转）。
pub fn rotate_ports(ports: &[u16], rotation: usize) -> Vec<u16> {
	if ports.is_empty() {
		return Vec::new();
	}
	let offset = rotation % ports.len();
	ports[offset..]
		.iter()
		.chain(ports[..offset].iter())
		.copied()
		.collect()
}

/// 构造通告给对端的 endpoint 列表：`[P+1, P+2, ..., 0]` 再轮转。
///
/// 最后一项是 [`CONTROL_CONNECTION_SENTINEL`]；K=1（`data_ports` 为空）时
/// 返回空列表——对端会退化为单连接，行为与今天一致。
pub fn advertised_endpoint_list(data_ports: &[u16], rotation: usize) -> Vec<u16> {
	if data_ports.is_empty() {
		return Vec::new();
	}
	let mut endpoints = data_ports.to_vec();
	endpoints.push(CONTROL_CONNECTION_SENTINEL);
	rotate_ports(&endpoints, rotation)
}

/// 本端每 peer 最多维护的连接数：`min(本端 endpoint 总数, 对端通告数)`。
///
/// `local_data_endpoints + 1` 就是本端 endpoint 总数（含控制连接）。
pub fn slot_count(local_data_endpoints: usize, advertised_len: usize) -> usize {
	(local_data_endpoints + 1).min(advertised_len)
}

/// 槽位 `slot` 的数据连接应该从本端第几个数据 endpoint 拨出。
///
/// 轮转后的列表顺序与本地 endpoint 序号没有语义关系；这里用取模做确定性
/// 映射，仅用于把拨号分散到本端不同数据 endpoint（对端看到的来源端口因此也
/// 是分散的）。`local_data_endpoints == 0` 时返回 0（该路径不会被使用）。
pub fn local_endpoint_for_slot(slot: usize, local_data_endpoints: usize) -> usize {
	if local_data_endpoints == 0 {
		0
	} else {
		slot % local_data_endpoints
	}
}

/// 选择要承载下一个传输的连接槽位。
///
/// 规则（确定性）：
/// 1. **最不忙优先**：在途传输数最少者优先（`None` = 连接尚未建立，视为 0）；
/// 2. 在途数相同 → 从 `cursor` 开始的**循环距离**最小者优先（同一 peer 的并发
///    传输因此轮转着铺开，而不是全部压在槽位 0）；
/// 3. 在途数达到 `cap` 的槽位直接跳过；
/// 4. 所有槽位都满/都不可用 → `None`（调用方回退控制连接）。
pub fn select_slot(loads: &[Option<usize>], cursor: usize, cap: usize) -> Option<usize> {
	if loads.is_empty() {
		return None;
	}
	let len = loads.len();
	let start = cursor % len;
	let mut best: Option<(usize, usize, usize)> = None;
	for (index, load) in loads.iter().enumerate() {
		let active = load.unwrap_or(0);
		if active >= cap {
			continue;
		}
		let distance = (index + len - start) % len;
		let key = (active, distance, index);
		if best.is_none_or(|current| key < current) {
			best = Some(key);
		}
	}
	best.map(|(_, _, index)| index)
}

/// 每个 peer 的连接槽位状态。
#[derive(Debug)]
struct PeerLinks {
	peer_ip: IpAddr,
	/// 对端通告的完整 endpoint 列表（含控制连接哨兵）。
	endpoints: Vec<u16>,
	/// 只保存**数据连接**句柄；控制连接槽位恒为 `None`（句柄由 NetManager 持有）。
	slots: Vec<Option<SessionHandle>>,
	/// 正在建立中的槽位：选择时按"已满"处理，避免同一槽位并发建两条连接。
	creating: Vec<bool>,
	/// 槽位是否被分配过传输。
	///
	/// 用途：区分冷启动与稳态——冷启动时"尚未建立"的连接不应压过控制连接
	/// （否则轮转失效）；已经有连接被用过之后，"尚未建立"就要让位于可复用
	/// 的空闲连接（避免每次传输都重新握手，见 [`DataPool::reserve`] 的负载规则）。
	ever_used: Vec<bool>,
	cursor: usize,
}

impl PeerLinks {
	fn new(peer_ip: IpAddr, endpoints: &[u16], slot_count: usize) -> Self {
		Self {
			peer_ip,
			endpoints: endpoints.to_vec(),
			slots: vec![None; slot_count],
			creating: vec![false; slot_count],
			ever_used: vec![false; slot_count],
			cursor: 0,
		}
	}

	/// 端口/地址变化（对端重连后重新轮转了通告）或槽位数变化时重建。
	///
	/// 只关闭**数据连接**；控制连接槽位不持有句柄，绝不能在池重建时误关控制面。
	fn reset(&mut self, peer_ip: IpAddr, endpoints: &[u16], slot_count: usize) {
		for handle in self.slots.drain(..).flatten() {
			let _ = handle.cmd_tx.try_send(SessionCmd::Shutdown);
		}
		*self = Self::new(peer_ip, endpoints, slot_count);
	}

	fn live_handle(&mut self, index: usize) -> Option<SessionHandle> {
		let stale = self.slots[index]
			.as_ref()
			.is_some_and(|handle| handle.is_finished() || handle.cmd_tx.is_closed());
		if stale {
			self.slots[index] = None;
			self.creating[index] = false;
		}
		self.slots[index].clone()
	}
}

/// 槽位预留结果。
#[derive(Debug)]
pub enum ReserveOutcome {
	/// 已有数据连接可用。
	Existing { slot: usize, handle: SessionHandle },
	/// 选中了控制连接槽位：本次大传输走控制连接（这是设计内的分配，
	/// 不是失败回退）。
	Control { slot: usize },
	/// 需要先建立该槽位的数据连接；调用方随后必须 `mark_created` / `mark_failed`。
	Create { slot: usize },
	/// 没有可用槽位（全部满载/正在建立/端口为空），调用方回退控制连接。
	Unavailable,
}

/// 每 peer 连接池。
///
/// 由 `NetManager` 主循环与"建立数据连接"的后台任务共享；锁只在内存操作期间
/// 持有，绝不在持锁状态下做网络等待。
#[derive(Debug, Default)]
pub struct DataPool {
	inner: Mutex<HashMap<String, PeerLinks>>,
}

impl DataPool {
	/// 为 `peer_id` 预留一个槽位。
	///
	/// `endpoints` 是对端在 Hello/HelloAck 里通告的完整列表（含控制哨兵），
	/// `control` 是当前控制会话（其负载也参与"最不忙优先"的选择）。
	/// 若对端通告发生变化（典型场景：控制连接重连后发送方重新轮转了列表），
	/// 旧的数据连接会被关闭并重建槽位。
	pub fn reserve(
		&self,
		peer_id: &str,
		peer_ip: IpAddr,
		endpoints: &[u16],
		slot_count: usize,
		control: Option<&SessionHandle>,
	) -> ReserveOutcome {
		let mut inner = self.inner.lock().unwrap();
		let needs_reset = inner.get(peer_id).is_some_and(|entry| {
			entry.peer_ip != peer_ip
				|| entry.endpoints != endpoints
				|| entry.slots.len() != slot_count
		});
		if needs_reset {
			if let Some(entry) = inner.get_mut(peer_id) {
				entry.reset(peer_ip, endpoints, slot_count);
			}
		}
		let entry = inner
			.entry(peer_id.to_string())
			.or_insert_with(|| PeerLinks::new(peer_ip, endpoints, slot_count));

		if entry.slots.is_empty() {
			return ReserveOutcome::Unavailable;
		}

		// 先回收已死连接，再计算负载。
		let handle_states: Vec<Option<SessionHandle>> = (0..entry.slots.len())
			.map(|index| entry.live_handle(index))
			.collect();
		let has_used_any = entry.ever_used.iter().any(|used| *used);
		let has_used_data = entry
			.endpoints
			.iter()
			.zip(entry.ever_used.iter())
			.any(|(endpoint, used)| *endpoint != CONTROL_CONNECTION_SENTINEL && *used);

		// 负载规则（越小越优先）：
		// - 已建立的数据连接：当前活跃传输数（空闲 = 0）；
		// - 尚未建立的数据连接：冷启动（本 peer 还没有任何分配）时为 0，让轮转
		//   决定先连哪个 endpoint；一旦有过分配就记 1，让位于可复用的空闲连接，
		//   避免"每次传输都重新握手"；
		// - 控制连接：当前活跃传输数；只要本 peer 已经用过数据连接就 +1，
		//   使同负载时优先走专用数据 endpoint（控制连接只做容量补位）；
		// - 正在建立：视为满载，跳过。
		let loads: Vec<Option<usize>> = (0..entry.slots.len())
			.map(|index| {
				if entry.endpoints[index] == CONTROL_CONNECTION_SENTINEL {
					Some(control.map_or(MAX_ACTIVE_TRANSFERS_PER_LINK, |handle| {
						handle.load.active() + usize::from(has_used_data)
					}))
				} else if entry.creating[index] {
					Some(MAX_ACTIVE_TRANSFERS_PER_LINK)
				} else if let Some(handle) = handle_states[index].as_ref() {
					Some(handle.load.active())
				} else {
					Some(usize::from(has_used_any))
				}
			})
			.collect();

		let Some(slot) = select_slot(&loads, entry.cursor, MAX_ACTIVE_TRANSFERS_PER_LINK) else {
			return ReserveOutcome::Unavailable;
		};
		entry.cursor = (slot + 1) % entry.slots.len();
		entry.ever_used[slot] = true;

		if entry.endpoints[slot] == CONTROL_CONNECTION_SENTINEL {
			if control.is_some() {
				ReserveOutcome::Control { slot }
			} else {
				ReserveOutcome::Unavailable
			}
		} else if let Some(handle) = handle_states[slot].clone() {
			ReserveOutcome::Existing { slot, handle }
		} else {
			entry.creating[slot] = true;
			ReserveOutcome::Create { slot }
		}
	}

	/// 建连成功：把 handle 放进数据槽位。返回 `false` 表示池已失效（端口变化/
	/// peer 已被移除/槽位是控制连接），调用方应关闭该连接并回退控制连接。
	pub fn mark_created(&self, peer_id: &str, slot: usize, handle: SessionHandle) -> bool {
		let mut inner = self.inner.lock().unwrap();
		let Some(entry) = inner.get_mut(peer_id) else {
			let _ = handle.cmd_tx.try_send(SessionCmd::Shutdown);
			return false;
		};
		if slot >= entry.slots.len()
			|| entry.endpoints[slot] == CONTROL_CONNECTION_SENTINEL
			|| entry.slots[slot].is_some()
		{
			let _ = handle.cmd_tx.try_send(SessionCmd::Shutdown);
			return false;
		}
		entry.creating[slot] = false;
		entry.slots[slot] = Some(handle);
		true
	}

	/// 建连失败：释放预留（下次使用时重试）。若槽位已被 `mark_created` 填上
	/// （例如握手成功后投递请求失败），也一并清空，避免池里留着不可用连接。
	pub fn mark_failed(&self, peer_id: &str, slot: usize) {
		let mut inner = self.inner.lock().unwrap();
		if let Some(entry) = inner.get_mut(peer_id) {
			if slot < entry.creating.len() {
				entry.creating[slot] = false;
				entry.slots[slot] = None;
			}
		}
	}

	/// 移除某个 peer 的全部数据连接（控制连接断开时调用），返回需要关闭的句柄。
	pub fn drop_peer(&self, peer_id: &str) -> Vec<SessionHandle> {
		let mut inner = self.inner.lock().unwrap();
		inner
			.remove(peer_id)
			.map(|entry| entry.slots.into_iter().flatten().collect())
			.unwrap_or_default()
	}

	/// 当前池中所有已建立的数据连接（用于取消广播/关闭）。
	pub fn sessions(&self) -> Vec<SessionHandle> {
		let inner = self.inner.lock().unwrap();
		inner
			.values()
			.flat_map(|entry| entry.slots.iter().flatten().cloned())
			.collect()
	}

	/// 清空池并返回全部数据连接（`NetManager::shutdown` 用）。
	pub fn take_all(&self) -> Vec<SessionHandle> {
		let mut inner = self.inner.lock().unwrap();
		inner
			.drain()
			.flat_map(|(_, entry)| entry.slots.into_iter().flatten())
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn peer_ip() -> IpAddr {
		IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
	}

	/// 一个 K=3 的发送方通告：数据端口 5001/5002 + 控制哨兵。
	fn endpoints() -> Vec<u16> {
		vec![5001, 5002, CONTROL_CONNECTION_SENTINEL]
	}

	#[test]
	fn small_transfers_bypass_the_pool() {
		let endpoints = endpoints();
		assert!(!should_use_data_link(
			DATA_LINK_MIN_TRANSFER_BYTES - 1,
			&endpoints,
			1
		));
		assert!(should_use_data_link(
			DATA_LINK_MIN_TRANSFER_BYTES,
			&endpoints,
			1
		));
		assert!(should_use_data_link(256 * 1024 * 1024, &endpoints, 3));
	}

	#[test]
	fn no_advertised_endpoints_or_no_local_endpoints_means_single_connection() {
		let bytes = 256 * 1024 * 1024;
		// 旧对端 / K=1：没有通告 endpoint。
		assert!(!should_use_data_link(bytes, &[], 3));
		// 本端只有控制端点（CB_QUIC_DATA_ENDPOINTS=1）。
		assert!(!should_use_data_link(bytes, &[5001], 0));
		// 两端都启用才用池。
		assert!(should_use_data_link(bytes, &[5001], 1));
	}

	#[test]
	fn rotate_ports_is_deterministic_and_cycles() {
		assert_eq!(rotate_ports(&[5001, 5002, 5003], 0), vec![5001, 5002, 5003]);
		assert_eq!(rotate_ports(&[5001, 5002, 5003], 1), vec![5002, 5003, 5001]);
		assert_eq!(rotate_ports(&[5001, 5002, 5003], 2), vec![5003, 5001, 5002]);
		assert_eq!(rotate_ports(&[5001, 5002, 5003], 3), vec![5001, 5002, 5003]);
		assert_eq!(rotate_ports(&[5001, 5002, 5003], 4), vec![5002, 5003, 5001]);
		assert_eq!(rotate_ports(&[], 7), Vec::<u16>::new());
		assert_eq!(rotate_ports(&[6001], 9), vec![6001]);
	}

	/// 通告列表 = 数据端口 + 控制哨兵，再按接受顺序轮转。
	#[test]
	fn advertised_endpoint_list_appends_control_sentinel_and_rotates() {
		assert_eq!(
			advertised_endpoint_list(&[5001, 5002, 5003], 0),
			vec![5001, 5002, 5003, CONTROL_CONNECTION_SENTINEL]
		);
		// 4 个 peer 依次接受：第 0 条连接分别落到 4 个不同 endpoint。
		assert_eq!(
			advertised_endpoint_list(&[5001, 5002, 5003], 1),
			vec![5002, 5003, CONTROL_CONNECTION_SENTINEL, 5001]
		);
		assert_eq!(
			advertised_endpoint_list(&[5001, 5002, 5003], 2),
			vec![5003, CONTROL_CONNECTION_SENTINEL, 5001, 5002]
		);
		assert_eq!(
			advertised_endpoint_list(&[5001, 5002, 5003], 3),
			vec![CONTROL_CONNECTION_SENTINEL, 5001, 5002, 5003]
		);
		// K=1（没有数据端口）不产生任何通告：单连接行为不变。
		assert_eq!(advertised_endpoint_list(&[], 5), Vec::<u16>::new());
	}

	#[test]
	fn slot_count_caps_at_local_endpoint_total() {
		// 本端 K=4（3 个数据端点 + 控制）最多 4 条连接。
		assert_eq!(slot_count(3, 4), 4);
		// 本端 K=2（1 个数据端点 + 控制）即使对端通告 4 个也只维护 2 条。
		assert_eq!(slot_count(1, 4), 2);
		// 本端 K=1 只有控制端点（`should_use_data_link` 会直接拒绝）。
		assert_eq!(slot_count(0, 4), 1);
		assert_eq!(slot_count(3, 0), 0);
	}

	#[test]
	fn local_endpoint_for_slot_is_deterministic_and_in_range() {
		assert_eq!(local_endpoint_for_slot(0, 3), 0);
		assert_eq!(local_endpoint_for_slot(1, 3), 1);
		assert_eq!(local_endpoint_for_slot(3, 3), 0);
		assert_eq!(local_endpoint_for_slot(5, 3), 2);
		assert_eq!(local_endpoint_for_slot(0, 0), 0);
	}

	#[test]
	fn select_slot_prefers_least_loaded_then_rotates() {
		// 最不忙优先。
		assert_eq!(
			select_slot(
				&[Some(3), Some(1), Some(2)],
				0,
				MAX_ACTIVE_TRANSFERS_PER_LINK
			),
			Some(1)
		);
		// 相同负载 -> 从 cursor 开始的循环距离最小者。
		assert_eq!(
			select_slot(
				&[Some(1), Some(1), Some(1)],
				1,
				MAX_ACTIVE_TRANSFERS_PER_LINK
			),
			Some(1)
		);
		assert_eq!(
			select_slot(
				&[Some(1), Some(1), Some(1)],
				2,
				MAX_ACTIVE_TRANSFERS_PER_LINK
			),
			Some(2)
		);
		// 未建立（None）按 0 在途处理，优先于已建立但忙碌的连接。
		assert_eq!(
			select_slot(&[Some(2), None], 0, MAX_ACTIVE_TRANSFERS_PER_LINK),
			Some(1)
		);
	}

	#[test]
	fn select_slot_skips_links_at_the_per_connection_cap() {
		// 每连接在途上限 8：满载槽位被跳过。未建立的槽位（None）按 0 在途处理，
		// 所以在"槽位 0 满载 + 槽位 2 未建立"时选未建立的 2（懒建立新连接），
		// 而不是压在已有 2 个在途的槽位 1 上。
		assert_eq!(
			select_slot(&[Some(8), Some(2), None], 0, MAX_ACTIVE_TRANSFERS_PER_LINK),
			Some(2)
		);
		// 已建立的槽位之间：跳过满载、选负载更低的。
		assert_eq!(
			select_slot(
				&[Some(8), Some(2), Some(3)],
				0,
				MAX_ACTIVE_TRANSFERS_PER_LINK
			),
			Some(1)
		);
		// 全部满载 -> 不分配（回退控制连接）。
		assert_eq!(
			select_slot(&[Some(8), Some(8)], 0, MAX_ACTIVE_TRANSFERS_PER_LINK),
			None
		);
		assert_eq!(select_slot(&[], 0, MAX_ACTIVE_TRANSFERS_PER_LINK), None);
	}

	/// 轮转后的第 0 个槽位在 4 个并发 peer 上分别是：数据 0 / 数据 1 / 数据 2 /
	/// 控制连接 —— 这正是发送方 4 个 endpoint 被均匀使用的依据。
	#[test]
	fn rotated_first_slots_spread_across_all_endpoints() {
		let data_ports = [5001u16, 5002, 5003];
		let first_slots: Vec<u16> = (0..4)
			.map(|rotation| advertised_endpoint_list(&data_ports, rotation)[0])
			.collect();
		assert_eq!(
			first_slots,
			vec![5001, 5002, 5003, CONTROL_CONNECTION_SENTINEL]
		);
	}

	#[test]
	fn pool_lazily_creates_then_reuses_and_spreads() {
		let pool = DataPool::default();
		let endpoints = endpoints(); // [5001, 5002, 0]
		let control = SessionHandle::for_test("peer-a");
		let slots = slot_count(3, endpoints.len());

		// 第 0 槽是数据端口：预留创建。
		let ReserveOutcome::Create { slot: first } =
			pool.reserve("peer-a", peer_ip(), &endpoints, slots, Some(&control))
		else {
			panic!("first reserve must create a data link");
		};
		assert_eq!(first, 0);
		// 第 1 槽也是数据端口：第二个并发传输换槽位（不会并发建同一条）。
		let ReserveOutcome::Create { slot: second } =
			pool.reserve("peer-a", peer_ip(), &endpoints, slots, Some(&control))
		else {
			panic!("second reserve must create the other link");
		};
		assert_eq!(second, 1);
		// 第 2 槽是控制连接：第三个并发传输直接走控制连接。
		assert!(matches!(
			pool.reserve("peer-a", peer_ip(), &endpoints, slots, Some(&control)),
			ReserveOutcome::Control { slot: 2 }
		));

		// 数据连接建立完成后按负载复用。
		let handle0 = SessionHandle::for_test("peer-a");
		let handle1 = SessionHandle::for_test("peer-a");
		assert!(pool.mark_created("peer-a", 0, handle0));
		assert!(pool.mark_created("peer-a", 1, handle1));
		let ReserveOutcome::Existing { slot, handle } =
			pool.reserve("peer-a", peer_ip(), &endpoints, slots, Some(&control))
		else {
			panic!("existing data link should be reused");
		};
		assert!(slot < 2);
		assert_eq!(handle.device_id(), "peer-a");
	}

	/// 一次传输结束后，下一条传输必须复用**已建立的空闲连接**，而不是再拨一条
	/// 新连接（否则每次大传输都要付一次 QUIC + OPAQUE 握手）。
	#[test]
	fn pool_reuses_an_idle_established_link_instead_of_dialing_again() {
		let pool = DataPool::default();
		let control = SessionHandle::for_test("peer-e");
		let endpoints = vec![5001u16, 5002, CONTROL_CONNECTION_SENTINEL];
		let slots = slot_count(3, endpoints.len());
		let ReserveOutcome::Create { slot } =
			pool.reserve("peer-e", peer_ip(), &endpoints, slots, Some(&control))
		else {
			panic!("first reserve must create");
		};
		assert!(pool.mark_created("peer-e", slot, SessionHandle::for_test("peer-e")));
		// 句柄的 load 为 0（空闲）。再次分配应命中同一槽位。
		match pool.reserve("peer-e", peer_ip(), &endpoints, slots, Some(&control)) {
			ReserveOutcome::Existing { slot: reused, .. } => assert_eq!(reused, slot),
			other => panic!("expected reuse of slot {slot}, got {other:?}"),
		}
	}

	/// 并发传输在"还没有任何连接"时按 cursor 依次铺开到不同槽位（含控制槽位的
	/// 轮转），这是 C1 把多个传输分散到多个 endpoint 的核心行为。
	#[test]
	fn pool_spreads_concurrent_transfers_across_available_slots() {
		let pool = DataPool::default();
		let control = SessionHandle::for_test("peer-f");
		let endpoints = vec![5001u16, 5002, 5003, CONTROL_CONNECTION_SENTINEL];
		let slots = slot_count(3, endpoints.len());
		let mut created = Vec::new();
		for _ in 0..3 {
			let ReserveOutcome::Create { slot } =
				pool.reserve("peer-f", peer_ip(), &endpoints, slots, Some(&control))
			else {
				panic!("concurrent reserves must create data links");
			};
			created.push(slot);
		}
		assert_eq!(created, vec![0, 1, 2]);
		// 三个数据槽位都在建立中：第 4 个并发传输走控制连接（第 4 个 endpoint）。
		assert!(matches!(
			pool.reserve("peer-f", peer_ip(), &endpoints, slots, Some(&control)),
			ReserveOutcome::Control { slot: 3 }
		));
	}

	#[test]
	fn pool_resets_when_advertised_endpoints_change() {
		let pool = DataPool::default();
		let control = SessionHandle::for_test("peer-b");
		let endpoints = vec![5001u16, CONTROL_CONNECTION_SENTINEL];
		let ReserveOutcome::Create { slot } =
			pool.reserve("peer-b", peer_ip(), &endpoints, 2, Some(&control))
		else {
			panic!("reserve must create");
		};
		assert!(pool.mark_created("peer-b", slot, SessionHandle::for_test("peer-b")));
		assert_eq!(pool.sessions().len(), 1);

		// 控制连接重连后发送方轮转了列表：池必须重建，旧数据连接被关闭。
		let new_endpoints = vec![CONTROL_CONNECTION_SENTINEL, 5001u16];
		assert!(matches!(
			pool.reserve("peer-b", peer_ip(), &new_endpoints, 2, Some(&control)),
			ReserveOutcome::Control { slot: 0 }
		));
		assert!(pool.sessions().is_empty());
	}

	#[test]
	fn pool_drop_peer_returns_only_data_handles() {
		let pool = DataPool::default();
		let control = SessionHandle::for_test("peer-c");
		let endpoints = vec![5001u16, CONTROL_CONNECTION_SENTINEL];
		let ReserveOutcome::Create { slot } =
			pool.reserve("peer-c", peer_ip(), &endpoints, 2, Some(&control))
		else {
			panic!("reserve must create");
		};
		assert!(pool.mark_created("peer-c", slot, SessionHandle::for_test("peer-c")));
		let handles = pool.drop_peer("peer-c");
		assert_eq!(handles.len(), 1, "控制连接不属于池的所有权");
		assert!(pool.sessions().is_empty());
		assert!(pool.drop_peer("peer-c").is_empty());
	}

	/// 池重建时绝不能关闭控制连接：控制槽位不持有句柄，`reset` 只 drain 数据句柄。
	#[test]
	fn pool_reset_never_shuts_down_control_connection() {
		let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(4);
		let control = SessionHandle {
			cmd_tx: control_tx,
			..SessionHandle::for_test("peer-d")
		};
		let pool = DataPool::default();
		let old = vec![5001u16, CONTROL_CONNECTION_SENTINEL];
		let _ = pool.reserve("peer-d", peer_ip(), &old, 2, Some(&control));
		let new = vec![5002u16, CONTROL_CONNECTION_SENTINEL];
		let _ = pool.reserve("peer-d", peer_ip(), &new, 2, Some(&control));
		// 控制句柄的队列里不应出现 Shutdown。
		assert!(
			control_rx.try_recv().is_err(),
			"pool reset must not send Shutdown to the control session"
		);
	}
}
