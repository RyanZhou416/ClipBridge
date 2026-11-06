use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
	Text,
	Image,
	FileList,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ItemPreview {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub text: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub image_hint: Option<ImageHint>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub file_count: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageHint {
	pub w: u32,
	pub h: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemContent {
	pub mime: String,
	pub sha256: String, // hex
	pub total_bytes: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMeta {
	pub file_id: String,
	pub rel_name: String,
	pub size_bytes: i64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub sha256: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub local_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemMeta {
	#[serde(rename = "type")]
	pub ty: String, // 固定 "ItemMeta"
	pub item_id: String,
	pub kind: ItemKind,
	pub created_ts_ms: i64,
	pub source_device_id: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source_device_name: Option<String>,
	pub size_bytes: i64,
	pub preview: ItemPreview,
	pub content: ItemContent,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub files: Vec<FileMeta>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub expires_ts_ms: Option<i64>,
}
// [新增] 隐私清洗方法
impl ItemMeta {
	pub fn sanitize_for_broadcast(&mut self) {
		self.source_device_name = None; // 可选：隐藏设备名细节
		self.strip_local_paths(); // 关键：清除所有文件的本地路径
	}

	/// 剥离所有文件的 `local_path`，返回被剥离的数量。
	///
	/// `local_path` 会让本机在 `ContentGet` 时把任意本地文件当作内容发出去
	/// （UNC 路径还会触发 outbound SMB 认证），因此**远端元数据是不可信输入**：
	/// 入站路径必须先调用本方法再落库/转发。返回数量用于记录 hostile input 警告。
	pub fn strip_local_paths(&mut self) -> usize {
		let mut stripped = 0;
		for f in &mut self.files {
			if f.local_path.take().is_some() {
				stripped += 1;
			}
		}
		stripped
	}
}

#[derive(Debug, serde::Serialize)]
pub struct CoreErrorPayload {
	pub code: String,
	pub message: String,
	pub scope: String, // "Session", "Transfer", "Core"
	pub retryable: bool,
	pub affects_session: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub detail: Option<serde_json::Value>,
}
