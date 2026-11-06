//! C 字符串 helper、所有 JSON DTO 与 `parse_cfg` / `parse_snapshot`。
//!
//! 这是原先三个平台各自 `bridge.rs` 的唯一实现。平台 crate 的 `bridge.rs` 仅保留
//! 一行 `pub use cb_core_ffi::marshal::*;` 兼容 shim，以维持文档中的目录布局。

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cb_core::api::{AppConfig, CoreConfig, GlobalPolicy};
use cb_core::clipboard::{ClipboardFileEntry, ClipboardSnapshot};
use cb_core::policy::SizeLimits;
use serde::Deserialize;
use std::os::raw::c_char;

/// 把 C 侧 NUL 结尾字符串借成 `&str`。
pub(crate) fn cstr_to_str<'a>(p: *const c_char) -> anyhow::Result<&'a str> {
	if p.is_null() {
		anyhow::bail!("null c string");
	}
	let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_str()?;
	Ok(s)
}

#[derive(Deserialize)]
struct LimitsDto {
	#[serde(default)]
	soft_text_bytes: Option<i64>,
	#[serde(default)]
	soft_image_bytes: Option<i64>,
	#[serde(default)]
	soft_file_total_bytes: Option<i64>,
	#[serde(default)]
	hard_text_bytes: Option<i64>,
	#[serde(default)]
	hard_image_bytes: Option<i64>,
	#[serde(default)]
	hard_file_total_bytes: Option<i64>,
	#[serde(default)]
	text_auto_prefetch_bytes: Option<i64>,
}

impl From<LimitsDto> for SizeLimits {
	fn from(val: LimitsDto) -> Self {
		let def = SizeLimits::default();
		SizeLimits {
			soft_text_bytes: val.soft_text_bytes.unwrap_or(def.soft_text_bytes),
			soft_image_bytes: val.soft_image_bytes.unwrap_or(def.soft_image_bytes),
			soft_file_total_bytes: val
				.soft_file_total_bytes
				.unwrap_or(def.soft_file_total_bytes),
			hard_text_bytes: val.hard_text_bytes.unwrap_or(def.hard_text_bytes),
			hard_image_bytes: val.hard_image_bytes.unwrap_or(def.hard_image_bytes),
			hard_file_total_bytes: val
				.hard_file_total_bytes
				.unwrap_or(def.hard_file_total_bytes),
			text_auto_prefetch_bytes: val
				.text_auto_prefetch_bytes
				.unwrap_or(def.text_auto_prefetch_bytes),
		}
	}
}

#[derive(Deserialize)]
struct AppConfigDto {
	#[serde(default)]
	size_limits: Option<LimitsDto>,
	#[serde(default)]
	global_policy: Option<String>,
	#[serde(default)]
	gc_history_max_items: Option<i64>,
	#[serde(default)]
	gc_cas_max_bytes: Option<i64>,
}

#[derive(Deserialize)]
struct InitConfigDto {
	device_id: String,
	device_name: String,
	account_uid: String,
	/// F15：密码在反序列化边界就进入可擦除包装；错误路径/提前返回时 drop 会
	/// 擦除缓冲区，成功路径把它移入 `CoreConfig`（由 `CoreConfig::drop` 擦除）。
	account_password: cb_core::api::SecretString,
	data_dir: String,
	cache_dir: String,

	/// 可选：true 时完全跳过 NetManager（不 bind socket、不启动 mDNS）。
	/// 缺省 false，保持正式应用网络开启。
	#[serde(default)]
	disable_network: bool,

	// 接受嵌套的 app_config
	#[serde(default)]
	app_config: Option<AppConfigDto>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ShareMode {
	#[default]
	Default,
	LocalOnly,
	Force,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotKind {
	Text,
	Image,
	FileList,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TextDto {
	#[serde(default)]
	mime: Option<String>,
	utf8: String,
}

#[derive(Debug, Deserialize)]
struct ImageDto {
	mime: String,
	bytes_b64: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClipboardSnapshotDto {
	#[serde(rename = "type")]
	ty: Option<String>,

	ts_ms: i64,

	#[serde(default)]
	source_app: Option<String>,

	kind: SnapshotKind,

	#[serde(default)]
	share_mode: ShareMode,

	#[serde(default)]
	text: Option<TextDto>,
	#[serde(default)]
	image: Option<ImageDto>,
	#[serde(default)]
	files: Vec<ClipboardFileEntry>,
}

/// `cb_list_history` 查询 DTO。
#[derive(Deserialize)]
pub(crate) struct HistoryQueryDto {
	#[serde(default = "default_limit")]
	pub(crate) limit: usize,
	#[serde(default)]
	pub(crate) cursor: Option<i64>, // 分页游标，可选
}

fn default_limit() -> usize {
	20
}

/// `cb_ensure_content_cached` 请求 DTO。
#[derive(Deserialize)]
pub(crate) struct EnsureContentDto {
	pub(crate) item_id: String,
	pub(crate) file_id: Option<String>,
	// prefer_peer: Option<String>, // 预留，Core API 升级后可传入
}

/// `cb_set_peer_policy` 请求 DTO。
#[derive(Deserialize)]
pub(crate) struct PeerPolicyDto {
	pub(crate) peer_id: String,
	#[serde(default)]
	pub(crate) share_to_peer: Option<bool>,
	#[serde(default)]
	pub(crate) accept_from_peer: Option<bool>,
}

/// `cb_clear_peer_fingerprint` 请求 DTO。
#[derive(Deserialize)]
pub(crate) struct ClearFingerprintDto {
	pub(crate) peer_id: String,
}

#[derive(Deserialize)]
struct ItemIdDto {
	item_id: String,
}

/// `cb_get_item_meta` / `cb_delete_item_*` 的入参兼容：JSON 字符串或 `{"item_id": ...}`。
pub(crate) fn parse_item_id(json_str: &str) -> anyhow::Result<String> {
	if let Ok(s) = serde_json::from_str::<String>(json_str) {
		Ok(s)
	} else {
		let obj: ItemIdDto = serde_json::from_str(json_str).context("invalid item_id json")?;
		Ok(obj.item_id)
	}
}

/// `cb_query_cache_stats` 查询 DTO（默认 bucket 10s）。
#[derive(Deserialize)]
pub(crate) struct CacheStatsQueryDto {
	#[serde(default)]
	pub(crate) start_ts_ms: i64,
	#[serde(default)]
	pub(crate) end_ts_ms: i64,
	#[serde(default = "default_bucket_10")]
	pub(crate) bucket_sec: i32,
}

fn default_bucket_10() -> i32 {
	10
}

/// `cb_query_net_stats` 查询 DTO（默认 bucket 10s）。
#[derive(Deserialize)]
pub(crate) struct NetStatsQueryDto {
	#[serde(default)]
	pub(crate) start_ts_ms: i64,
	#[serde(default)]
	pub(crate) end_ts_ms: i64,
	#[serde(default = "default_bucket_10")]
	pub(crate) bucket_sec: i32,
}

/// `cb_query_activity_stats` 查询 DTO（默认 bucket 60s）。
#[derive(Deserialize)]
pub(crate) struct ActivityStatsQueryDto {
	#[serde(default)]
	pub(crate) start_ts_ms: i64,
	#[serde(default)]
	pub(crate) end_ts_ms: i64,
	#[serde(default = "default_bucket_60")]
	pub(crate) bucket_sec: i32,
}

fn default_bucket_60() -> i32 {
	60
}

/// 判断账号密码是否属于“需要提醒但必须接受”的弱密码集合。
///
/// 空/纯空白密码由 [`parse_cfg`] 单独拒绝；本函数返回 `None` 表示未命中提醒条件。
/// 返回值是稳定的机器可读原因，便于日志检索：
/// - `"same_as_username"`：去掉首尾空白后与账号 uid 相同；
/// - `"too_short"`：不足 12 个字符。
///
/// 只用于**告警**，绝不用于拒绝：core 无法区分“登录已有账号”与“创建新账号”，
/// 硬拒绝会让历史短密码用户升级后直接无法启动。真正的熵约束在壳侧。
fn password_weakness_reason(password: &str, uid: &str) -> Option<&'static str> {
	if password.trim() == uid.trim() {
		return Some("same_as_username");
	}
	if password.chars().count() < 12 {
		return Some("too_short");
	}
	None
}

pub fn parse_cfg(json: &str) -> anyhow::Result<cb_core::api::CoreConfig> {
	// 1. 使用 InitConfigDto 进行反序列化 (支持 app_config 嵌套和数字类型的 GC 配置)
	let dto: InitConfigDto = serde_json::from_str(json).context("invalid cfg_json")?;

	// 1.1 拒绝空/纯空白密码（审计 F11，纵深防御）。
	// 空密码会让 core 注册一个“空密码即可认证”的 OPAQUE 账号，任何 LAN peer 都能通过
	// 握手并被自动 TOFU 固定。复杂度策略仍由壳侧负责，这里只做“绝不接受空凭据”的兜底，
	// 且在 Core::init / OPAQUE setup / 证书初始化之前失败。
	if dto.account_password.expose().trim().is_empty() {
		anyhow::bail!("account_password must not be empty");
	}

	// 1.2 弱密码只告警、不拒绝（审计 F3 的兼容性妥协；parse_cfg 每次初始化只调用一次）。
	//
	// 背景：OPAQUE 的 ServerSetup 由共享账号密码确定性派生，攻击者观察到一次握手即可
	// 对候选密码做离线验证（约 2×Argon2id/猜测）。在没有带外配对码的前提下，真正让
	// 离线猜测不可行的是**密码熵**，硬约束落在壳侧：创建/修改账号时强制 ≥12 字符 +
	// 熵下限，并提供 160-bit 生成器。这里只是纵深防御的第二道提醒——一旦硬拒绝，
	// 历史短密码用户升级后会直接无法启动，而且 core 无法区分“登录”与“创建/修改”。
	if let Some(reason) = password_weakness_reason(dto.account_password.expose(), &dto.account_uid)
	{
		eprintln!(
			"[cb_core_ffi][WARN] weak account_password detected ({reason}); accepting for \
			 backward compatibility. Use a strong account secret generated by the ClipBridge \
			 shell (at least 12 characters, 160-bit generator)."
		);
	}

	// 2. 构造 AppConfig
	let app_config = if let Some(app) = dto.app_config {
		let policy = match app.global_policy.as_deref() {
			Some("DenyAll") => GlobalPolicy::DenyAll,
			_ => GlobalPolicy::AllowAll,
		};
		AppConfig {
			size_limits: app.size_limits.map(|l| l.into()).unwrap_or_default(),
			global_policy: policy,
			gc_history_max_items: app.gc_history_max_items.unwrap_or(50_000),
			gc_cas_max_bytes: app.gc_cas_max_bytes.unwrap_or(1024 * 1024 * 1024),
		}
	} else {
		AppConfig::default()
	};

	// 3. 返回 CoreConfig
	// 密码从 SecretString 取出所有权后立刻移入 CoreConfig；中间不产生第三份副本。
	let account_password = dto.account_password.into_plain_string();
	Ok(CoreConfig {
		device_id: dto.device_id,
		device_name: dto.device_name,
		account_uid: dto.account_uid,
		account_password,
		data_dir: dto.data_dir,
		cache_dir: dto.cache_dir,
		app_config,
		disable_network: dto.disable_network,
	})
}

pub fn parse_snapshot(json: &str) -> anyhow::Result<(ClipboardSnapshot, ShareMode)> {
	let dto: ClipboardSnapshotDto = serde_json::from_str(json).context("invalid snapshot_json")?;

	// type 字段：建议严格
	if dto.ty.as_deref() != Some("ClipboardSnapshot") {
		anyhow::bail!("invalid snapshot.type: expected ClipboardSnapshot");
	}

	let ts_ms = dto.ts_ms;

	let snap = match dto.kind {
		SnapshotKind::Text => {
			let t = dto.text.context("kind=text requires .text")?;
			ClipboardSnapshot::Text {
				text_utf8: t.utf8,
				ts_ms,
			}
		}
		SnapshotKind::Image => {
			let i = dto.image.context("kind=image requires .image")?;
			let bytes = B64
				.decode(i.bytes_b64.as_bytes())
				.context("invalid image.bytes_b64")?;
			ClipboardSnapshot::Image {
				bytes,
				mime: i.mime,
				ts_ms,
			}
		}
		SnapshotKind::FileList => {
			if dto.files.is_empty() {
				anyhow::bail!("kind=file_list requires non-empty files[]");
			}
			ClipboardSnapshot::FileList {
				files: dto.files,
				ts_ms,
			}
		}
	};

	Ok((snap, dto.share_mode))
}

#[cfg(test)]
mod tests {
	use super::{parse_cfg, password_weakness_reason};

	/// 构造一份字段齐全、仅密码可变的合法 cfg JSON（parse_cfg 不访问文件系统）。
	fn cfg_json(password: &str) -> String {
		serde_json::json!({
			"device_id": "dev-test",
			"device_name": "test-device",
			"account_uid": "acct-test",
			"account_password": password,
			"data_dir": "C:/tmp/clipbridge-test/data",
			"cache_dir": "C:/tmp/clipbridge-test/cache",
		})
		.to_string()
	}

	#[test]
	fn parse_cfg_rejects_empty_account_password() {
		let err = parse_cfg(&cfg_json("")).expect_err("empty account_password must be rejected");
		assert!(
			err.to_string()
				.contains("account_password must not be empty"),
			"unexpected error: {err:#}"
		);
	}

	#[test]
	fn parse_cfg_rejects_whitespace_only_account_password() {
		let err = parse_cfg(&cfg_json(" \t\r\n "))
			.expect_err("whitespace-only password must be rejected");
		assert!(
			err.to_string()
				.contains("account_password must not be empty"),
			"unexpected error: {err:#}"
		);
	}

	#[test]
	fn parse_cfg_accepts_non_empty_account_password() {
		let cfg = parse_cfg(&cfg_json("correct horse battery staple"))
			.expect("non-empty account_password must be accepted");
		assert_eq!(cfg.account_password, "correct horse battery staple");
		assert_eq!(cfg.account_uid, "acct-test");
		assert_eq!(cfg.device_id, "dev-test");
	}

	#[test]
	fn password_weakness_reason_flags_short_password() {
		assert_eq!(
			password_weakness_reason("short", "acct-test"),
			Some("too_short")
		);
	}

	#[test]
	fn password_weakness_reason_flags_password_equal_to_uid() {
		// 首尾空白先 trim 再比较；同一密码同时命中两种条件时优先报更严重的“等于账号名”。
		assert_eq!(
			password_weakness_reason("  acct-test  ", "acct-test"),
			Some("same_as_username")
		);
		assert_eq!(
			password_weakness_reason("acct-test", "acct-test"),
			Some("same_as_username")
		);
	}

	#[test]
	fn password_weakness_reason_accepts_long_password() {
		assert_eq!(
			password_weakness_reason("correct horse battery staple", "acct-test"),
			None
		);
	}

	#[test]
	fn parse_cfg_accepts_short_account_password_but_marks_it_weak() {
		// 兼容性：短密码仍然接受（不能被 F3 的缓解变成启动阻断），只走告警路径。
		let cfg = parse_cfg(&cfg_json("short")).expect("short password must stay accepted");
		assert_eq!(cfg.account_password, "short");
		assert_eq!(
			password_weakness_reason(&cfg.account_password, &cfg.account_uid),
			Some("too_short")
		);
	}
}
