//! C1 数据端点端口规划（ADR-002 §4.3 / §5.3）。
//!
//! 规则（任务 C1 第 1 条）：
//! - 控制/发现端口仍然是 P（mDNS 广播的端口，保持不变）；
//! - 额外数据端点从 **P+1** 起顺序分配，数量 = `CB_QUIC_DATA_ENDPOINTS - 1`
//!   （K=1 时没有额外端点，行为与今天完全一致）；
//! - 端口被占用时**跳过并继续向后找**，实际绑定的端口会被通告给对端
//!   （见 `proto::CtrlMsg::Hello/HelloAck` 的 `data_ports` 字段）；
//! - 端口号上溢（P 接近 65535）时停止分配：宁可退化为单连接，也不回绕到
//!   低端口（低端口可能被系统进程占用且需要特权）。
//!
//! 本模块只做**纯计算**：不 bind socket，因此可以 socket-free 单测。真正
//! `UdpSocket::bind` 在 [`crate::transport::Transport::new`] 里通过注入的
//! `is_free` 闭包完成。

/// 数据端点总数的环境变量名。
pub const DATA_ENDPOINT_COUNT_ENV: &str = "CB_QUIC_DATA_ENDPOINTS";

/// 数据端点总数默认值（1 个控制端点 + 1 个数据端点）。
pub const DEFAULT_DATA_ENDPOINT_COUNT: usize = 2;

/// 数据端点总数下限：1 = 只有控制端点（今天的行为）。
pub const MIN_DATA_ENDPOINT_COUNT: usize = 1;

/// 数据端点总数上限：4（ADR-002 §5.3 实测 4 个独立 Endpoint 收益已接近饱和，
/// 再增加只会放大内存/调度开销，且端口占用与防火墙暴露面线性增长）。
pub const MAX_DATA_ENDPOINT_COUNT: usize = 4;

/// 端口冲突时最多向后扫描多少个候选端口。
///
/// 只影响"多端点是否拿得满"，不影响正确性：扫描不到足够的空闲端口时，
/// 少开几个数据端点（甚至退化为单连接）而不是让启动失败。
pub const MAX_PORT_SCAN: u16 = 64;

/// 解析 `CB_QUIC_DATA_ENDPOINTS` 的原始值（纯函数，便于单测）。
///
/// 语义：
/// - 未设置 / 空串 / 非整数 → 默认 [`DEFAULT_DATA_ENDPOINT_COUNT`]；
/// - 数值会被夹到 `[MIN, MAX]`（`0` 等价于 1，`9` 等价于 4）；
/// - 夹取只影响"开几个端点"，不会 panic。
pub fn parse_data_endpoint_count(raw: Option<&str>) -> usize {
	let Some(raw) = raw else {
		return DEFAULT_DATA_ENDPOINT_COUNT;
	};
	let trimmed = raw.trim();
	if trimmed.is_empty() {
		return DEFAULT_DATA_ENDPOINT_COUNT;
	}
	match trimmed.parse::<usize>() {
		Ok(value) => value.clamp(MIN_DATA_ENDPOINT_COUNT, MAX_DATA_ENDPOINT_COUNT),
		Err(_) => DEFAULT_DATA_ENDPOINT_COUNT,
	}
}

/// 从环境变量读取数据端点总数（每次构造 [`crate::transport::Transport`] 读一次）。
///
/// 非法/越界值会打印一行提示，避免"配置了但没生效"却毫无线索。
pub fn data_endpoint_count_from_env() -> usize {
	let raw = std::env::var(DATA_ENDPOINT_COUNT_ENV).ok();
	let parsed = parse_data_endpoint_count(raw.as_deref());
	if let Some(raw) = raw.as_deref() {
		let trimmed = raw.trim();
		let numeric = trimmed.parse::<usize>().ok();
		if numeric.is_some_and(|value| value != parsed)
			|| (numeric.is_none() && !trimmed.is_empty())
		{
			eprintln!(
				"[Transport] {DATA_ENDPOINT_COUNT_ENV}={raw:?} 已解析为 {parsed}（允许范围 \
				 {MIN_DATA_ENDPOINT_COUNT}..={MAX_DATA_ENDPOINT_COUNT}，默认 \
				 {DEFAULT_DATA_ENDPOINT_COUNT}）"
			);
		}
	}
	parsed
}

/// 生成数据端口的候选序列：从 `control_port + 1` 开始，最多 `limit` 个，
/// 且横向扫描不超过 `max_scan` 个端口号。
///
/// 结果**不包含**控制端口 P 本身；`control_port == u16::MAX` 时返回空列表
/// （没有可用的后继端口，绝不回绕）。
pub fn data_port_candidates(control_port: u16, limit: usize, max_scan: u16) -> Vec<u16> {
	if limit == 0 {
		return Vec::new();
	}
	let mut out = Vec::with_capacity(limit.min(usize::from(max_scan)));
	let last = u32::from(control_port)
		.saturating_add(u32::from(max_scan))
		.min(u32::from(u16::MAX));
	let mut candidate = u32::from(control_port) + 1;
	while candidate <= last && out.len() < limit {
		// candidate 已限制在 u16 范围内，转换必然成功。
		if let Ok(port) = u16::try_from(candidate) {
			out.push(port);
		}
		candidate += 1;
	}
	out
}

/// 在候选序列里挑出 `needed` 个**可用**端口（按升序、确定性地跳过被占用端口）。
///
/// `is_free` 由调用方注入：生产路径里它是"尝试 bind 并成功"（见
/// `Transport::new`），单测里可以是任意集合判断。同一个候选端口只会被询问
/// 一次，因此调用方可以在闭包里把成功绑定的 socket 收集起来。
pub fn allocate_data_ports(
	control_port: u16,
	needed: usize,
	max_scan: u16,
	mut is_free: impl FnMut(u16) -> bool,
) -> Vec<u16> {
	if needed == 0 {
		return Vec::new();
	}
	// 最多需要 needed 个成功端口；额外多扫 max_scan 个候选用于跳过占用。
	let candidate_limit = needed.saturating_add(usize::from(max_scan));
	let mut out = Vec::with_capacity(needed);
	for port in data_port_candidates(control_port, candidate_limit, max_scan) {
		if is_free(port) {
			out.push(port);
			if out.len() == needed {
				break;
			}
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn endpoint_count_parsing_defaults_and_clamps() {
		assert_eq!(parse_data_endpoint_count(None), DEFAULT_DATA_ENDPOINT_COUNT);
		assert_eq!(
			parse_data_endpoint_count(Some("")),
			DEFAULT_DATA_ENDPOINT_COUNT
		);
		assert_eq!(
			parse_data_endpoint_count(Some("   ")),
			DEFAULT_DATA_ENDPOINT_COUNT
		);
		assert_eq!(
			parse_data_endpoint_count(Some("jumbo")),
			DEFAULT_DATA_ENDPOINT_COUNT
		);
		assert_eq!(
			parse_data_endpoint_count(Some("-1")),
			DEFAULT_DATA_ENDPOINT_COUNT
		);

		// 合法区间原样采用（允许前后空白）。
		assert_eq!(parse_data_endpoint_count(Some("1")), 1);
		assert_eq!(parse_data_endpoint_count(Some(" 2 ")), 2);
		assert_eq!(parse_data_endpoint_count(Some("3")), 3);
		assert_eq!(parse_data_endpoint_count(Some("4")), 4);

		// 越界夹取：0 -> 1（今天的单端点行为），>4 -> 4。
		assert_eq!(
			parse_data_endpoint_count(Some("0")),
			MIN_DATA_ENDPOINT_COUNT
		);
		assert_eq!(
			parse_data_endpoint_count(Some("5")),
			MAX_DATA_ENDPOINT_COUNT
		);
		assert_eq!(
			parse_data_endpoint_count(Some("999999")),
			MAX_DATA_ENDPOINT_COUNT
		);
	}

	#[test]
	fn candidates_start_after_control_port_and_are_deterministic() {
		assert_eq!(data_port_candidates(5000, 3, 8), vec![5001, 5002, 5003]);
		// 相同输入必须给出相同结果（确定性）。
		assert_eq!(
			data_port_candidates(5000, 3, 8),
			data_port_candidates(5000, 3, 8)
		);
		// 控制端口本身永远不在候选里。
		assert!(!data_port_candidates(5000, 8, 8).contains(&5000));
		// 候选数量受 limit 限制。
		assert_eq!(data_port_candidates(5000, 0, 8), Vec::<u16>::new());
	}

	#[test]
	fn candidates_stop_at_u16_max_without_wrapping() {
		// 65534 之后只有一个合法端口；绝不回绕到 1/2 这类低端口。
		assert_eq!(data_port_candidates(65534, 3, 8), vec![65535]);
		// 控制端口已是最大值：没有任何可用的后继端口。
		assert_eq!(
			data_port_candidates(65535, 3, 8),
			Vec::<u16>::new(),
			"端口上溢时必须返回空列表而不是回绕"
		);
	}

	#[test]
	fn allocation_skips_busy_ports_and_keeps_order() {
		// 5001 被占用 -> 依次尝试 5002、5003……
		let got = allocate_data_ports(5000, 2, 8, |port| port != 5001);
		assert_eq!(got, vec![5002, 5003]);

		// 连续占用多个候选时继续向后找。
		let busy = [5001u16, 5002, 5005];
		let got = allocate_data_ports(5000, 2, 8, |port| !busy.contains(&port));
		assert_eq!(got, vec![5003, 5004]);

		// 全部占用时宁可少开（返回空）也不失败。
		assert_eq!(
			allocate_data_ports(5000, 2, 8, |_| false),
			Vec::<u16>::new()
		);

		// 需要 0 个数据端口时直接返回空（K=1 的路径）。
		assert_eq!(allocate_data_ports(5000, 0, 8, |_| true), Vec::<u16>::new());
	}

	#[test]
	fn allocation_is_deterministic_and_respects_scan_window() {
		// 扫描窗口之外的端口不应被考虑（max_scan=2 -> 最多 5001/5002）。
		let got = allocate_data_ports(5000, 2, 2, |_| true);
		assert_eq!(got, vec![5001, 5002]);

		// 确定性：同样的输入两次结果一致。
		let is_free = |port: u16| port.is_multiple_of(2);
		assert_eq!(
			allocate_data_ports(7000, 3, 16, is_free),
			allocate_data_ports(7000, 3, 16, is_free)
		);
	}
}
