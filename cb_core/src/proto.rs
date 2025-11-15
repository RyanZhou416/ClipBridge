// cb_core/src/proto.rs
//! 协议与数据模型（Protocol & DTO）
//!
//! 设计目标：
//! 1. **稳定**：字段兼容性优先；新字段默认 `Option<T>`，老端可忽略；
//! 2. **中立**：不夹带平台细节（Win32 格式、UTI 等放到平台层适配）；
//! 3. **可直序列化**：全部 `serde` 可序列化/反序列化，跨端传输统一 JSON；
//! 4. **小而全**：v1 只包含“Lazy Fetch”所需的最小字段，但预留扩展位；
//!
//! 命名约定：
//! - `*Snapshot`：本机“复制”时采集到的局部信息（供 `ingest_local_copy` 用）；
//! - `ItemMeta` ：对外广播/存储的**元数据**（不含正文），粘贴时据此去拉取正文；
//! - `LocalContentRef`：本地缓存（CAS）的定位结果，粘贴时据此读真实数据；
//!
//! 时间单位：除非特别说明，均为 **epoch milliseconds (i64)**。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 协议版本（面向**互操作**）。
/// - **仅在破坏性变更时 +1**（字段删除/语义变化/加密握手不兼容等）；
/// - 新增可选字段不应提升此版本，只提升语义版本 [`CORE_SEMVER`]。
pub const PROTOCOL_VERSION: u32 = 1;

/// 核心库语义版本（面向**实现/发布**）。
/// - 采用语义化版本规则：MAJOR.MINOR.PATCH。
pub const CORE_SEMVER: &str = "1.0.0";

/// 设备平台类型（用于日志/可观测；不参与协议分支逻辑）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// 微软 Windows（Win32/WinRT/WinUI）
    Windows,
    /// Apple iOS（含 iPadOS）
    Ios,
    /// Apple macOS
    Macos,
    /// Android
    Android,
    /// GNU/Linux（含各发行版）
    Linux,
    /// 其他或未知
    Other,
}

impl Default for Platform {
    fn default() -> Self {
        Self::Other
    }
}

/// 设备信息（握手/诊断/可观测用途；可持久化到“信任设备表”）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 设备唯一 ID（本地生成并持久化；建议 `uuid_v4` 文本形式）。
    pub device_id: String,
    /// 展示名称（例如 “My-PC” / “Ryan’s iPhone”）。
    pub device_name: String,
    /// 平台标识（仅用于显示与统计）。
    #[serde(default)]
    pub platform: Platform,
    /// 应用版本（可选；例如外壳版本或 FFI 桥版本）。
    #[serde(default)]
    pub app_version: Option<String>,
    /// 此设备支持的协议主版本（用于握手时的最小兼容判断）。
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
}

fn default_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

/// 本机在“复制”动作发生时采集到的**剪贴板快照**（不含大正文）。
/// 该结构是 `ingest_local_copy` 的输入，用于生成广播用的 [`ItemMeta`]。
///
/// 典型流程：
/// - 用户在 A 设备复制 → 壳层读取剪贴板 → 组装 `ClipboardSnapshot` → 调 core；
/// - core 去重/生成 `item_id`/计算摘要 → 产出 `ItemMeta` → 广播给同网段设备。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardSnapshot {
    /// 项目标识（如未提供由 core 生成；使用 `uuid_v4` 文本可避免跨端冲突）。
    #[serde(default)]
    pub item_id: Option<String>,

    /// 该条目包含的 **MIME 类型列表**（优先级从高到低）。
    /// 例如：`["text/plain; charset=utf-8", "text/html", "application/rtf"]`
    pub mimes: Vec<String>,

    /// 正文的 **总大小（字节）**。文本按 UTF-8 编码长度统计；图片/文件为实际字节数。
    pub size_bytes: u64,

    /// 正文内容的 **sha256（十六进制小写）**。
    /// - 文本：对 UTF-8 字节数组求哈希；
    /// - 图片/文件：对原始字节求哈希；
    /// - 大对象：对完整体求哈希（v1 不拆块计算）。
    pub sha256_hex: String,

    /// 预览信息（小体量元数据），用于列表/通知/UI 渲染，不含大正文。
    /// 例如：文本前 120 字符、图片宽高/像素、文件名/数量等。
    /// 约定：由壳层或平台适配层生成，**大小建议 ≤ 2 KB**。
    #[serde(default)]
    pub preview_json: serde_json::Value,

    /// 本机采集时间（ms since epoch）。
    pub created_at: i64,
}

/// 广播/存储的**元数据**（不含正文）。
/// 粘贴时，接收端据此发起 **Lazy Fetch** 去拉取正文（通过 QUIC/HTTP3 等）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemMeta {
    /// 协议主版本（用于兼容性检查）。默认与 [`PROTOCOL_VERSION`] 同步。
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,

    /// 唯一 `item_id`（文本 `uuid_v4` 建议）。
    pub item_id: String,

    /// 所有者设备 ID（来源设备）。
    pub owner_device_id: String,

    /// （可选）来源设备展示名，用于 UI 列表更友好。
    #[serde(default)]
    pub owner_device_name: Option<String>,

    /// 支持的 MIME 列表（按偏好排序，和 [`ClipboardSnapshot::mimes`] 一致或其子集）。
    pub mimes: Vec<String>,

    /// 正文总大小（字节）。
    pub size_bytes: u64,

    /// 正文 sha256（十六进制小写）。
    pub sha256_hex: String,

    /// 过期时间（ms since epoch）。`None` 表示不设过期（由接收端策略决定保留期）。
    #[serde(default)]
    pub expires_at: Option<i64>,

    /// 预览信息（与快照保持一致；接收端可直接用于列表/通知渲染）。
    #[serde(default)]
    pub preview_json: serde_json::Value,

    /// 拉取定位 URI（逻辑地址），供网络层解析为实际连接参数。
    /// 规范建议：`cb://{owner_device_id}/item/{item_id}`
    pub uri: String,

    /// 生成该元数据的时间（ms since epoch）。
    pub created_at: i64,
}

/// 本地内容定位结果（CAS 缓存命中后返回）。
/// 粘贴时，壳层据此从 `path` 读取真实正文并注入系统剪贴板。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalContentRef {
    /// 内容 sha256（十六进制小写），与 `ItemMeta.sha256_hex` 一致。
    pub sha256_hex: String,
    /// 文件实际落盘路径（绝对路径）。
    pub path: PathBuf,
    /// 选择的 MIME（当一个条目支持多种表示时，表明本次使用的那一种）。
    pub mime: String,
    /// 实际字节数（用于二次校验/统计）。
    pub size_bytes: u64,
}

/// 便捷构造：空的 `preview_json`。
#[must_use]
pub fn empty_preview() -> serde_json::Value {
    serde_json::json!({})
}

/// 工具：将 MIME 规范化（去空白/小写类型与子类型，保留参数顺序）。
/// 仅用于 UI/日志；**不会改变协议语义**。
#[must_use]
pub fn normalize_mime(mime: &str) -> String {
    let mut parts = mime.split(';');
    let head = parts
        .next()
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mut out = head;
    for p in parts {
        // 参数原样保留但去掉首尾空白
        out.push(';');
        out.push_str(p.trim());
    }
    out
}

/// 工具：判断是否为“文本类” MIME（便于 UI 优先显示文本预览）。
#[must_use]
pub fn is_text_mime(mime: &str) -> bool {
    let m = mime.trim().to_ascii_lowercase();
    m.starts_with("text/") || m.starts_with("application/json") || m.starts_with("application/xml")
}

/// 工具：根据 MIME 粗分类型（UI 可据此选用图标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoarseKind {
    /// 纯文本/半结构化文本（html/xml/json 等）
    TextLike,
    /// 位图（png/jpeg/webp/heif 等）
    Image,
    /// 文件/多文件（uri-list/自定义容器）
    File,
    /// 其他
    Other,
}

/// MIME → 粗分类型映射（仅用于 UI/日志）。
#[must_use]
pub fn coarse_kind(mime: &str) -> CoarseKind {
    let m = mime.trim().to_ascii_lowercase();
    if m.starts_with("text/")
        || m.starts_with("application/json")
        || m.starts_with("application/xml")
        || m.starts_with("application/rtf")
        || m.starts_with("text/html")
    {
        CoarseKind::TextLike
    } else if m.starts_with("image/") {
        CoarseKind::Image
    } else if m == "application/uri-list" || m == "application/x-cb-files" {
        CoarseKind::File
    } else {
        CoarseKind::Other
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogRow {
    pub id: i64,
    pub time_unix: i64,         // ms
    pub level: i32,             // 0..6
    pub category: String,
    pub message: String,
    pub exception: Option<String>,
    pub props_json: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct LogStats {
    pub count: i64,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub by_level: [i64; 7],     // 0..6
}
