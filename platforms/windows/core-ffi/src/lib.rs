mod bridge;
mod error;


use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::Arc;
use anyhow::Context;
use cb_core::api::{Core, CoreEventSink};

use crate::error::{err_json, ok_json};

/// 包装 `cb_core::api::Core` 的 C 兼容句柄。
#[repr(C)]
pub struct cb_handle {
    core: Core,
}

/// 壳侧提供的事件回调函数原型。
type OnEventFn = extern "C" fn(json: *const c_char, user_data: *mut c_void);

/// 内部结构，用于将 FFI 回调和用户数据转发给核心层。
struct FfiSink {
    cb: OnEventFn,
    user: *mut c_void,
}

// function pointer + raw pointer：我们保证只“转发调用”，线程安全由壳侧处理
unsafe impl Send for FfiSink {}
unsafe impl Sync for FfiSink {}

impl CoreEventSink for FfiSink {
    fn emit(&self, event_json: String) {
        let Ok(cstr) = CString::new(event_json) else { return };
        (self.cb)(cstr.as_ptr(), self.user);
        // cstr 释放后指针失效；壳侧必须在回调里拷贝
    }
}

fn cstr_to_str<'a>(p: *const c_char) -> anyhow::Result<&'a str> {
    if p.is_null() {
        anyhow::bail!("null c string");
    }
    let s = unsafe { CStr::from_ptr(p) }.to_str()?;
    Ok(s)
}

#[derive(serde::Deserialize)]
struct HistoryQueryDto {
	#[serde(default = "default_limit")]
	limit: usize,
	#[serde(default)]
	cursor: Option<i64>, // 分页游标，可选
}

fn default_limit() -> usize { 20 }

fn ret(s: String) -> *const c_char {
    CString::new(s).unwrap().into_raw()
}


/// # `cb_free_string`
///
/// 此函数用于安全地释放一个在堆上分配的 C 风格字符串。
/// 它被设计为供外部 (C) 代码调用，因此标记了 `#[no_mangle]` 并使用 `extern "C"` 调用约定。
///
/// # 参数
/// - `s`: 指向需要释放的 C 风格字符串 (`*const c_char`) 的指针。
///   该字符串应由核心分配并传给外部代码。
///
/// # 行为
/// - 如果指针 `s` 为空，函数将安全返回，不执行任何操作。
/// - 如果指针不为空，该字符串将通过 `CString::from_raw` 重新构建为 `CString`，
///   然后被丢弃（drop）以释放内存。
///
/// # 安全性
/// - 调用者必须确保传入的指针指向一个有效的、以 null 结尾的 C 风格字符串，且该字符串最初是由 Rust 的 CString 分配的。
/// - 向此函数传递无效或悬空指针会导致未定义行为。
/// - 指针在被此函数释放后不得再次使用。
///
/// # 示例 (C 代码集成)
/// ```c,ignore
/// extern void cb_free_string(const char* s);
///
/// // 假设某个函数返回了由 cb 库分配的字符串
/// char* string = ...;
/// cb_free_string(string);
/// ```
///
/// # 注意事项
/// - 此函数封装了不安全（unsafe）代码，因为它涉及原始指针操作，绕过了 Rust 的典型内存安全检查。
/// - 确保外部代码遵守这些约束，以避免内存泄漏或未定义行为。
#[no_mangle]
pub extern "C" fn cb_free_string(s: *const c_char) {
    if s.is_null() { return; }
    unsafe { drop(CString::from_raw(s as *mut c_char)); }
}

/// 初始化回调系统。
///
/// # 参数
/// - `cfg_json`: 指向包含 JSON 配置的 C 风格字符串（以 null 结尾）的指针。
/// - `on_event`: 指向负责处理事件的回调处理函数的指针。
/// - `user_data`: 指向用户数据的指针，该数据将在事件调用期间传回给回调处理函数。
///
/// # 返回值
/// 返回一个指向 C 风格字符串（以 null 结尾）的指针，表示 JSON 格式的结果：
/// - 成功时：包含 "handle" 字段的 JSON 对象，该字段是初始化系统的唯一标识符。
/// - 失败时：包含错误代码和详细描述的错误信息 JSON 对象。
///
/// # 行为
/// - 将输入的 C 风格 JSON 配置转换为 Rust 字符串并进行解析。
/// - 使用解析后的配置和提供的 `on_event` 回调初始化核心系统。
/// - 为初始化的核心系统构建一个句柄并存储在内存中。
/// - 以 JSON 格式返回生成的句柄或相应的错误。
///
/// # 错误
/// 如果发生以下情况，将以 JSON 格式返回错误：
/// - 配置 JSON 无效或无法解析。
/// - 核心初始化由于任何原因失败。
///
/// # 安全性
/// - 调用者负责确保 `cfg_json` 是指向以 null 结尾的 C 风格字符串的有效指针。
/// - `on_event` 指针必须指向一个有效的回调函数。
/// - 只要回调系统在使用中，`user_data` 指针就被预期是有效的。
/// - 未能遵守这些要求可能会导致未定义行为。
///
/// # 示例 (C/C++ 中使用)
/// ```c
/// const char* config_json = "{\"key\": \"value\"}";
/// const char* result = cb_init(config_json, my_event_handler, my_user_data);
/// printf("初始化结果: %s\n", result);
/// ```
#[no_mangle]
pub extern "C" fn cb_init(cfg_json: *const c_char, on_event: OnEventFn, user_data: *mut c_void) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        let cfg_s = cstr_to_str(cfg_json)?;
        let cfg = bridge::parse_cfg(cfg_s)?;

        let sink: Arc<dyn CoreEventSink> = Arc::new(FfiSink { cb: on_event, user: user_data });
        let core = Core::init(cfg, sink);

        let h = Box::new(cb_handle { core });
        let handle_ptr = Box::into_raw(h) as usize;

        Ok(ok_json(serde_json::json!({ "handle": handle_ptr })))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("INIT_FAILED", &format!("{e:#}"))),
    }
}

/// 将原始指针转换为 `cb_handle` 类型的可变引用，并验证其有效性。
///
/// # 参数
///
/// * `handle_ptr` - 以 `usize` 表示的原始指针，预期指向 `cb_handle` 实例。
///
/// # 返回值
///
/// * 成功时，返回包装在 `anyhow::Result` 中的 `cb_handle` 可变引用。
/// * 失败时，如果指针为空或无效，则返回带有 "null handle" 消息的 `anyhow::Error`。
///
/// # 安全性
///
/// 此函数使用 `unsafe` 代码来解引用提供的原始指针。调用者必须确保：
/// - `handle_ptr` 是一个有效的、非空的指针。
/// - 指针引用的内存在返回引用的生命周期内保持有效。
/// - 指针引用的内存是排他性的，且未在其他不安全上下文中被访问。
///
/// 不当使用此函数可能导致未定义行为，包括段错误（segmentation faults）。
///
/// # 错误
///
/// 在以下情况下返回错误：
/// - 如果 `handle_ptr` 为 0 或空。
/// - 如果指针转换导致空引用。
///
/// # 示例
/// ```rust
/// let some_ptr: usize = ...; // 指向有效 `cb_handle` 的指针
/// match get_handle(some_ptr) {
///     Ok(handle) => {
///         // 安全访问 `cb_handle`
///     }
///     Err(err) => {
///         eprintln!("错误: {}", err);
///     }
/// }
/// ```
///
/// 此函数假定调用者传递的是对应于 `cb_handle` 对象的有效指针。
fn get_handle(handle_ptr: usize) -> anyhow::Result<&'static mut cb_handle> {
    if handle_ptr == 0 { anyhow::bail!("null handle"); }
    let p = handle_ptr as *mut cb_handle;
    if p.is_null() { anyhow::bail!("null handle"); }
    Ok(unsafe { &mut *p })
}

/// 从 JSON 字符串中解析 'handle' 字段并将其作为 `usize` 返回。
///
/// 预期 JSON 的结构如下：
/// ```json
/// {
///   "data": {
///     "handle": 123
///   }
/// }
/// ```
///
/// # 参数
/// - `json`: 包含 JSON 数据字符串切片。
///
/// # 返回值
/// - 成功时，返回包含解析出的 "handle" 字段 `usize` 值的 `Result`；
///   如果解析失败，则返回 `anyhow::Error` 类型的错误。
///
/// # 错误
/// - 如果输入的 JSON 字符串格式错误或不符合预期结构，则返回错误。
/// - 如果 "handle" 字段不存在或无法解析为 `usize`，则返回错误。
///
/// # 示例
/// ```
/// let json_data = r#"{ "data": { "handle": 42 } }"#;
/// let handle = parse_handle_from_json(json_data)?;
/// assert_eq!(handle, 42);
/// ```
fn parse_handle_from_json(json: &str) -> anyhow::Result<usize> {
    #[derive(serde::Deserialize)]
    struct H { handle: usize }
    let v: serde_json::Value = serde_json::from_str(json)?;
    let h: H = serde_json::from_value(v["data"].clone())?;
    Ok(h.handle)
}

/// # `cb_shutdown` 函数
///
/// 此函数提供了一个外部函数接口 (FFI)，供非 Rust 语言调用。
/// 它处理给定句柄（以原始指针 `*mut cb_handle` 表示）的关闭过程。
/// 它确保资源的正确清理，并返回指示成功或失败的 JSON 字符串。
///
/// ## 参数
/// - `h: *mut cb_handle`
///     - 指向 `cb_handle` 的原始指针。这代表需要关闭的上下文或对象。
///       注意此指针不得为空。传递空句柄将导致错误。
///
/// ## 返回值
/// - `*const c_char`
///     - 指向包含操作结果（JSON 格式）的 C 风格 null 结尾字符串的指针：
///         - 成功时：指示成功的 JSON 对象（例如 `{}`）。
///         - 失败时：包含错误代码和描述性消息的 JSON 对象（例如 `{"code": "SHUTDOWN_FAILED", "message": "<错误详情>"}`）。
///
/// ## 行为
/// 1. 检查提供的句柄 (`h`) 是否为空：
///     - 如果为空，函数立即返回指示 `"SHUTDOWN_FAILED"` 的错误 JSON。
/// 2. 使用 `Box::from_raw(h)` 安全地将原始句柄指针转换为 Box 包装的句柄。内存的所有权移交给此函数。
/// 3. 调用与句柄关联的核心对象上的 `shutdown()` 方法以启动关闭过程。
/// 4. 成功后，序列化一个空的 JSON 对象 (`{}`) 以指示成功关闭。
/// 5. 如果在任何阶段发生错误，它将返回包含错误代码 (`"SHUTDOWN_FAILED"`) 和描述性错误消息的 JSON 对象。
///
/// ## 安全性
/// - 调用者必须确保 `h` 是指向先前分配的 `cb_handle` 对象的有效、非空指针。
/// - 底层内存的所有权将转移到此函数，该函数将释放内存。
///   调用者在将 `h` 传递给此函数后不得再使用或访问它，以避免未定义行为。
/// - 该函数依赖于 FFI 安全类型，并确保返回值已正确格式化为 null 结尾的 C 字符串以便互操作。
///
/// ## 示例用法 (在 C 或类似语言中)
/// ```c
/// cb_handle *handle = ...; // 假设句柄已正确初始化
/// const char* result = cb_shutdown(handle);
/// printf("关闭结果: %s\n", result);
/// // 调用 cb_shutdown 后不要再使用 `handle`。
/// ```
///
/// ## 错误
/// - 如果句柄为空，函数返回：
///   `{"code": "SHUTDOWN_FAILED", "message": "null handle"}`。
/// - 如果由于内部错误导致关闭过程失败，详细的错误信息将包含在返回 JSON 的 `"message"` 字段中。
///
/// ## 注意事项
/// - 假定 `ok_json` 和 `err_json` 工具函数分别生成成功和错误情况的 JSON 字符串。
/// - 假定 `ret` 函数将 Rust `String` 转换为 C 兼容的 null 结尾 `*const c_char`。
#[no_mangle]
pub extern "C" fn cb_shutdown(h: *mut cb_handle) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let boxed = unsafe { Box::from_raw(h) };
        boxed.core.shutdown();
        Ok(ok_json(serde_json::json!({})))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("SHUTDOWN_FAILED", &format!("{e:#}"))),
    }
}


#[no_mangle]
pub extern "C" fn cb_plan_local_ingest(h: *mut cb_handle, snapshot_json: *const c_char) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let snap_s = cstr_to_str(snapshot_json)?;
        let (snap, share_mode) = bridge::parse_snapshot(snap_s)?;

        let force = matches!(share_mode, bridge::ShareMode::Force);

        let hh = unsafe { &mut *h };
        let r = hh.core.plan_local_ingest_result(&snap, force)?;
        Ok(ok_json(serde_json::json!({
        "plan": {
        "meta": r.meta,
        "needs_user_confirm": r.needs_user_confirm,
        "strategy": r.strategy
    }
})))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("PLAN_FAILED", &format!("{e:#}"))),
    }
}



#[no_mangle]
pub extern "C" fn cb_ingest_local_copy(h: *mut cb_handle, snapshot_json: *const c_char) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let snap_s = cstr_to_str(snapshot_json)?;
        let (snap, share_mode) = bridge::parse_snapshot(snap_s)?;

        let force = matches!(share_mode, bridge::ShareMode::Force);

        let hh = unsafe { &mut *h };
        let meta = hh.core.ingest_local_copy_with_force(snap, force)?;
        Ok(ok_json(serde_json::json!({ "meta": meta })))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("INGEST_FAILED", &format!("{e:#}"))),
    }
}

/// 获取当前在线设备列表
///
/// 返回格式：{"ok": true, "data": [{"device_id": "...", "is_online": true, ...}]}
#[no_mangle]
pub extern "C" fn cb_list_peers(h: *mut cb_handle) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };

        // 调用 Core 的 list_peers
        let peers = hh.core.list_peers()?;

        // 序列化结果
        Ok(ok_json(serde_json::json!(peers)))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("LIST_PEERS_FAILED", &format!("{e:#}"))),
    }
}

/// 获取核心状态
///
/// 返回格式：{"ok": true, "data": {"status": "Running", "device_id": "...", ...}}
#[no_mangle]
pub extern "C" fn cb_get_status(h: *mut cb_handle) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };

        let status = hh.core.get_status()?;

        Ok(ok_json(status))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("GET_STATUS_FAILED", &format!("{e:#}"))),
    }
}

#[derive(serde::Deserialize)]
struct EnsureContentDto {
	item_id: String,
	file_id: Option<String>,
	// prefer_peer: Option<String>, // 预留，Core API 升级后可传入
}

#[no_mangle]
pub extern "C" fn cb_ensure_content_cached(h: *mut cb_handle, req_json: *const c_char) -> *const c_char {
	let run = (|| -> anyhow::Result<String> {
		if h.is_null() { anyhow::bail!("null handle"); }
		let hh = unsafe { &mut *h };

		let json_str = crate::cstr_to_str(req_json)?;
		let dto: EnsureContentDto = serde_json::from_str(json_str).context("invalid json")?;

		// 调用 Core API
		let transfer_id = hh.core.ensure_content_cached(&dto.item_id, dto.file_id.as_deref())?;

		Ok(crate::error::ok_json(serde_json::json!({ "transfer_id": transfer_id })))
	})();
	match run {
		Ok(s) => crate::ret(s),
		Err(e) => crate::ret(crate::error::err_json("START_FETCH_FAILED", &format!("{e:#}"))),
	}
}

#[no_mangle]
pub extern "C" fn cb_cancel_transfer(h: *mut cb_handle, transfer_id_json: *const c_char) -> *const c_char {
	let run = (|| -> anyhow::Result<String> {
		if h.is_null() { anyhow::bail!("null handle"); }
		let hh = unsafe { &mut *h };

		let json_str = crate::cstr_to_str(transfer_id_json)?;
		// 假设传入的是一个单纯的字符串 JSON，如 "uuid"
		let tid: String = serde_json::from_str(json_str).context("invalid json string")?;

		hh.core.cancel_transfer(&tid);
		Ok(crate::error::ok_json(serde_json::json!({})))
	})();
	match run {
		Ok(s) => crate::ret(s),
		Err(e) => crate::ret(crate::error::err_json("CANCEL_FAILED", &format!("{e:#}"))),
	}
}

#[no_mangle]
pub extern "C" fn cb_list_history(h: *mut cb_handle, query_json: *const c_char) -> *const c_char {
	let run = (|| -> anyhow::Result<String> {
		if h.is_null() { anyhow::bail!("null handle"); }
		let hh = unsafe { &mut *h };

		// 1. 解析入参
		let json_str = crate::cstr_to_str(query_json)?;
		let dto: HistoryQueryDto = serde_json::from_str(json_str).context("invalid query json")?;

		// 2. 调用 Core
		let items = hh.core.list_history(dto.limit, dto.cursor)?;

		// 3. 包装返回
		Ok(crate::error::ok_json(items))
	})();

	match run {
		Ok(s) => crate::ret(s),
		Err(e) => crate::ret(crate::error::err_json("LIST_HISTORY_FAILED", &format!("{e:#}"))),
	}
}

#[no_mangle]
pub extern "C" fn cb_get_item_meta(h: *mut cb_handle, item_id_json: *const c_char) -> *const c_char {
	let run = (|| -> anyhow::Result<String> {
		if h.is_null() { anyhow::bail!("null handle"); }
		let hh = unsafe { &mut *h };

		// 1. 解析入参 (假设传入的是单纯的字符串 "uuid..."，或者是 { "item_id": "..." })
		// 这里为了兼容性，支持直接传字符串，或者包含 item_id 的对象
		let json_str = crate::cstr_to_str(item_id_json)?;

		// 尝试解析为字符串
		let item_id = if let Ok(s) = serde_json::from_str::<String>(json_str) {
			s
		} else {
			// 尝试解析为对象
			#[derive(serde::Deserialize)]
			struct IdObj { item_id: String }
			let obj: IdObj = serde_json::from_str(json_str).context("invalid item_id json")?;
			obj.item_id
		};

		// 2. 调用 Core
		let meta = hh.core.get_item_meta(&item_id)?;

		// 3. 包装返回
		match meta {
			Some(m) => Ok(crate::error::ok_json(m)),
			None => Err(anyhow::anyhow!("Item not found")),
		}
	})();

	match run {
		Ok(s) => crate::ret(s),
		Err(e) => crate::ret(crate::error::err_json("GET_ITEM_FAILED", &format!("{e:#}"))),
	}
}

#[cfg(test)]
mod tests;
