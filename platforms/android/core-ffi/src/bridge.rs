use anyhow::Context;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cb_core::api::{AppConfig, CoreConfig, GlobalPolicy};
use cb_core::clipboard::{ClipboardFileEntry, ClipboardSnapshot};
use cb_core::policy::SizeLimits;
use serde::Deserialize;

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

impl Into<SizeLimits> for LimitsDto {
	fn into(self) -> SizeLimits {
		let def = SizeLimits::default();
		SizeLimits {
			soft_text_bytes: self.soft_text_bytes.unwrap_or(def.soft_text_bytes),
			soft_image_bytes: self.soft_image_bytes.unwrap_or(def.soft_image_bytes),
			soft_file_total_bytes: self
				.soft_file_total_bytes
				.unwrap_or(def.soft_file_total_bytes),
			hard_text_bytes: self.hard_text_bytes.unwrap_or(def.hard_text_bytes),
			hard_image_bytes: self.hard_image_bytes.unwrap_or(def.hard_image_bytes),
			hard_file_total_bytes: self
				.hard_file_total_bytes
				.unwrap_or(def.hard_file_total_bytes),
			text_auto_prefetch_bytes: self
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
	account_password: String,
	data_dir: String,
	cache_dir: String,

	#[serde(default)]
	app_config: Option<AppConfigDto>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
	Default,
	LocalOnly,
	Force,
}
impl Default for ShareMode {
	fn default() -> Self {
		ShareMode::Default
	}
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

pub fn parse_cfg(json: &str) -> anyhow::Result<cb_core::api::CoreConfig> {
	let dto: InitConfigDto = serde_json::from_str(json).context("invalid cfg_json")?;

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

	Ok(CoreConfig {
		device_id: dto.device_id,
		device_name: dto.device_name,
		account_uid: dto.account_uid,
		account_password: dto.account_password,
		data_dir: dto.data_dir,
		cache_dir: dto.cache_dir,
		app_config,
	})
}

pub fn parse_snapshot(json: &str) -> anyhow::Result<(ClipboardSnapshot, ShareMode)> {
	let dto: ClipboardSnapshotDto = serde_json::from_str(json).context("invalid snapshot_json")?;

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
