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
    pub sha256: String,      // hex
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
		// 关键：清除所有文件的本地路径
		for f in &mut self.files {
			f.local_path = None;
		}
	}
}
