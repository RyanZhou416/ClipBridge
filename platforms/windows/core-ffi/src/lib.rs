//! ClipBridge C ABI 桥接层（Windows 与 macOS 共用同一份实现）。
//!
//! # 句柄并发契约（重要）
//!
//! - 通过 [`cb_init`] 返回的 `cb_handle` 指针 **不得并发使用**。
//! - 设计契约是“单线程串行调用”：壳侧必须把所有 FFI 调用串行化（例如同一把锁保护）。
//! - 当前每个入口在检查存活标记后都会执行 `unsafe { &mut *h }`；若两个线程同时调用，
//!   就会同时存在两个 `&mut cb_handle`，这在 Rust 中属于**别名未定义行为（UB）**。
//!   要彻底修复，需要把句柄内部改成 `Mutex<Option<Core>>` 一类的同步结构（见 FIX 2 的
//!   存活标记/幂等 shutdown 处理），那是后续独立任务。
//! - 作为过渡，入口用 `busy` 原子标记做一次廉价的“重入检测”：并发调用会返回
//!   `CORE_BUSY` 错误而不是继续制造 UB；`cb_shutdown` 之后所有入口返回 `CORE_SHUTDOWN`。
//!   这不是完整的锁设计，只是把常见的并发误用变成可诊断的错误。

// C ABI 入口必须接收裸指针，且不能声明成 `unsafe fn`（否则头文件语义与 C 侧调用约定都要改）。
// 空指针 / 存活 / 重入检查由每个入口的 `enter()` 统一完成，因此在本 crate 统一关闭该 lint。
// 说明：这条 lint 在 clippy 里是 deny-by-default，不关掉会让 `cargo clippy` 直接编译失败。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod bridge;
mod error;

use anyhow::Context;
use cb_core::api::{Core, CoreEventSink};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::{err_json, ok_json};

/// 包装 `cb_core::api::Core` 的 C 兼容句柄。
#[repr(C)]
pub struct cb_handle {
	/// 句柄是否仍然存活；`cb_shutdown` 会以原子方式置为 `false`。
	alive: AtomicBool,
	/// 廉价的“调用中”标记：用于把并发重入转成 `CORE_BUSY`，而不是别名 UB。
	busy: AtomicBool,
	/// 核心实例；`cb_shutdown` 时被 `take()` 出来执行关闭。
	core: Option<Core>,
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
		let Ok(cstr) = CString::new(event_json) else {
			return;
		};
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

fn default_limit() -> usize {
	20
}

/// 把 Rust `String` 转成 C 侧字符串；**绝不 panic**。
///
/// `CString::new` 只在字符串包含内部 NUL 时失败（例如错误信息里带用户数据），
/// 此时返回一个固定的错误 envelope，而不是 `unwrap()` 导致进程 abort。
fn ret(s: String) -> *const c_char {
	CString::new(s)
		.unwrap_or_else(|_| {
			CString::new(
				"{\"ok\":false,\"error\":{\"code\":\"INVALID_UTF8\",\"message\":\"response contained a NUL byte\"}}",
			)
			.expect("static fallback envelope is NUL-free")
		})
		.into_raw()
}

/// FFI 句柄生命周期错误；`error_code` 会把它们映射为稳定的错误码。
#[derive(Debug)]
enum HandleError {
	/// 句柄已经关闭。
	Shutdown,
	/// 同一句柄上检测到并发/重入调用。
	Busy,
}

impl std::fmt::Display for HandleError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			HandleError::Shutdown => write!(f, "core handle has been shut down"),
			HandleError::Busy => write!(
				f,
				"core handle is busy: concurrent calls are not supported, serialize calls on the shell side"
			),
		}
	}
}

impl std::error::Error for HandleError {}

/// `catch_unwind` 捕获到的 panic payload 类型别名。
type PanicPayload = Box<dyn std::any::Any + Send + 'static>;

/// 把 `anyhow::Error` 映射成 FFI 错误码（句柄错误优先于调用方默认码）。
fn error_code<'a>(e: &'a anyhow::Error, fallback: &'a str) -> &'a str {
	match e.downcast_ref::<HandleError>() {
		Some(HandleError::Shutdown) => "CORE_SHUTDOWN",
		Some(HandleError::Busy) => "CORE_BUSY",
		None => fallback,
	}
}

impl cb_handle {
	/// 通过共享引用读取存活标记（必须在任何 `&mut *h` 之前调用）。
	fn is_alive(&self) -> bool {
		self.alive.load(Ordering::Acquire)
	}

	/// 取得 `Core` 的可变引用；句柄已关闭时返回 `CORE_SHUTDOWN` 语义的错误。
	fn core_mut(&mut self) -> anyhow::Result<&mut Core> {
		if !self.is_alive() {
			return Err(anyhow::Error::new(HandleError::Shutdown));
		}
		self.core
			.as_mut()
			.ok_or_else(|| anyhow::Error::new(HandleError::Shutdown))
	}
}

/// 入口守卫：持有期间 `busy` 为 `true`，`Drop`（包括 panic 展开）时释放。
///
/// 只保存裸指针（而不是 `&cb_handle`），避免在随后创建 `&mut *h` 时产生别名冲突。
struct CallGuard {
	h: *mut cb_handle,
}

impl Drop for CallGuard {
	fn drop(&mut self) {
		// SAFETY: shutdown 时句柄盒被故意泄漏（见 cb_shutdown），因此指针始终有效；
		// 此时本函数内的 `&mut *h` 借用已经结束。
		unsafe {
			(*self.h).busy.store(false, Ordering::Release);
		}
	}
}

/// 所有带句柄入口的第一步：占住重入标记，并在任何 `&mut *h` 之前检查 `alive`。
fn enter(h: *mut cb_handle) -> anyhow::Result<CallGuard> {
	if h.is_null() {
		anyhow::bail!("null handle");
	}
	// SAFETY: 调用者保证 h 是 cb_init 返回且未被释放的句柄。这里只读取原子标记。
	if unsafe {
		(*h).busy
			.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
			.is_err()
	} {
		return Err(anyhow::Error::new(HandleError::Busy));
	}
	// 守卫负责在返回/panic 时把 busy 复位。
	let guard = CallGuard { h };
	// 共享读取存活标记；必须在任何 `&mut *h` 之前完成，且不跨越后续借用。
	let alive = unsafe { (&*h).is_alive() };
	if !alive {
		return Err(anyhow::Error::new(HandleError::Shutdown));
	}
	Ok(guard)
}

/// 捕获 `extern "C"` 边界上的 panic，统一转成错误 envelope，绝不让 panic 越过 C ABI。
fn ffi_result(code: &str, run: Result<anyhow::Result<String>, PanicPayload>) -> *const c_char {
	match run {
		Ok(Ok(s)) => ret(s),
		Ok(Err(e)) => ret(err_json(error_code(&e, code), &format!("{e:#}"))),
		Err(_) => {
			eprintln!("CRITICAL: Rust panic caught at the ClipBridge C ABI boundary");
			ret(err_json("PANIC", "internal panic"))
		}
	}
}

/// `i32` + JSON 出参型入口的 panic 边界：panic 时写入 `PANIC` envelope（out 非空时）并返回 1。
fn ffi_out_json(
	code: &str,
	fallback_message: &str,
	out_json: *mut *const c_char,
	run: Result<anyhow::Result<String>, PanicPayload>,
) -> i32 {
	let (json, rc) = match run {
		Ok(Ok(s)) => (s, 0),
		Ok(Err(e)) => {
			let message = match e.downcast_ref::<HandleError>() {
				Some(_) => format!("{e:#}"),
				None => fallback_message.to_owned(),
			};
			(err_json(error_code(&e, code), &message), 1)
		}
		Err(_) => {
			eprintln!("CRITICAL: Rust panic caught at the ClipBridge C ABI boundary");
			(err_json("PANIC", "internal panic"), 1)
		}
	};
	unsafe {
		if !out_json.is_null() {
			*out_json = ret(json);
		}
	}
	rc
}

/// `i32` 状态型入口的 panic 边界（无 JSON 出参）。
fn ffi_status(run: Result<anyhow::Result<()>, PanicPayload>) -> i32 {
	match run {
		Ok(Ok(())) => 0,
		Ok(Err(_)) => 1,
		Err(_) => {
			eprintln!("CRITICAL: Rust panic caught at the ClipBridge C ABI boundary");
			1
		}
	}
}

/// `i32` + `*mut i64` 出参型入口的 panic 边界。
fn ffi_out_i64(out: *mut i64, run: Result<anyhow::Result<i64>, PanicPayload>) -> i32 {
	match run {
		Ok(Ok(v)) => {
			unsafe {
				if !out.is_null() {
					*out = v;
				}
			}
			0
		}
		Ok(Err(_)) => 1,
		Err(_) => {
			eprintln!("CRITICAL: Rust panic caught at the ClipBridge C ABI boundary");
			1
		}
	}
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
	// 释放本身不应 panic；万一 panic（例如指针异常触发的 drop），也不能让它越过 C ABI。
	let _ = panic::catch_unwind(AssertUnwindSafe(|| {
		if s.is_null() {
			return;
		}
		unsafe {
			drop(CString::from_raw(s as *mut c_char));
		}
	}));
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
pub extern "C" fn cb_init(
	cfg_json: *const c_char,
	on_event: OnEventFn,
	user_data: *mut c_void,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let cfg_s = cstr_to_str(cfg_json)?;
		let cfg = bridge::parse_cfg(cfg_s)?;

		let sink: Arc<dyn CoreEventSink> = Arc::new(FfiSink {
			cb: on_event,
			user: user_data,
		});
		let core = Core::init(cfg, sink);

		let h = Box::new(cb_handle {
			alive: AtomicBool::new(true),
			busy: AtomicBool::new(false),
			core: Some(core),
		});
		let handle_ptr = Box::into_raw(h) as usize;

		Ok(ok_json(serde_json::json!({ "handle": handle_ptr })))
	}));

	ffi_result("INIT_FAILED", run)
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
/// 2. 原子地把 `alive` 置为 `false`，并用 `take()` 取出 `Core` 执行 `shutdown()`。
///    句柄盒本身**故意泄漏**（有界泄漏），以换取并发/重复调用时的 use-after-free 安全；
///    详细权衡见实现内注释（FIX 2）。
/// 3. 调用与句柄关联的核心对象上的 `shutdown()` 方法以启动关闭过程。
/// 4. 成功后，序列化一个空的 JSON 对象 (`{}`) 以指示成功关闭。
/// 5. 如果在任何阶段发生错误，它将返回包含错误代码 (`"SHUTDOWN_FAILED"`) 和描述性错误消息的 JSON 对象。
///
/// ## 安全性
/// - 调用者必须确保 `h` 是指向先前分配的 `cb_handle` 对象的有效、非空指针。
/// - 该函数**幂等**：重复调用只会生效一次，第二次及以后返回成功 envelope。
/// - 关闭后句柄内存不被释放（故意泄漏），但所有其它入口都会返回 `CORE_SHUTDOWN` 错误，
///   因此调用者在 `cb_shutdown` 之后误用 `h` 不会再触发 double-free / use-after-free。
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
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		if h.is_null() {
			anyhow::bail!("null handle");
		}
		// 与 enter() 相同的重入保护：避免 shutdown 与正在进行的调用并发执行。
		// 句柄可能在调用进行中被关闭，因此这里有限次重试而不是立即放弃。
		let mut acquired = false;
		for _ in 0..500 {
			if unsafe {
				(*h).busy
					.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
					.is_ok()
			} {
				acquired = true;
				break;
			}
			std::thread::yield_now();
		}
		if !acquired {
			return Err(anyhow::Error::new(HandleError::Busy));
		}
		let _guard = CallGuard { h };

		// 幂等：第二次及以后的调用直接返回 ok。
		if !unsafe { (*h).alive.load(Ordering::Acquire) } {
			return Ok(ok_json(serde_json::json!({})));
		}

		// 先置 false：此后任何入口都会拿到 CORE_SHUTDOWN（而不是 UAF）。
		unsafe {
			(*h).alive.store(false, Ordering::Release);
		}

		// 取走 Core 并关闭。注意：这里 **不** 调用 Box::from_raw(h)。
		// 句柄盒被故意泄漏：它很小且结构固定，用这点有界泄漏换取
		// “shutdown 之后迟到的调用仍然可以安全解引用并得到 CORE_SHUTDOWN” 的保障。
		// 若真要回收，需要额外的跨线程生命周期协议（Mutex/Arc 设计），不在本次范围内。
		let core = unsafe { (*h).core.take() };
		if let Some(core) = core {
			core.shutdown();
		}

		Ok(ok_json(serde_json::json!({})))
	}));

	ffi_result("SHUTDOWN_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_plan_local_ingest(
	h: *mut cb_handle,
	snapshot_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let snap_s = cstr_to_str(snapshot_json)?;
		let (snap, share_mode) = bridge::parse_snapshot(snap_s)?;

		let force = matches!(share_mode, bridge::ShareMode::Force);

		let hh = unsafe { &mut *h };
		let r = hh.core_mut()?.plan_local_ingest_result(&snap, force)?;
		Ok(ok_json(serde_json::json!({
				"plan": {
				"meta": r.meta,
				"needs_user_confirm": r.needs_user_confirm,
				"strategy": r.strategy
			}
		})))
	}));

	ffi_result("PLAN_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_ingest_local_copy(
	h: *mut cb_handle,
	snapshot_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let snap_s = cstr_to_str(snapshot_json)?;
		let (snap, share_mode) = bridge::parse_snapshot(snap_s)?;

		let force = matches!(share_mode, bridge::ShareMode::Force);

		let hh = unsafe { &mut *h };
		let meta = hh.core_mut()?.ingest_local_copy_with_force(snap, force)?;
		Ok(ok_json(serde_json::json!({ "meta": meta })))
	}));

	ffi_result("INGEST_FAILED", run)
}

/// 获取当前在线设备列表
///
/// 返回格式：{"ok": true, "data": [{"device_id": "...", "is_online": true, ...}]}
#[no_mangle]
pub extern "C" fn cb_list_peers(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		// 调用 Core 的 list_peers
		let peers = hh.core_mut()?.list_peers()?;

		// 序列化结果
		Ok(ok_json(serde_json::json!(peers)))
	}));

	ffi_result("LIST_PEERS_FAILED", run)
}

/// 获取核心状态
///
/// 返回格式：{"ok": true, "data": {"status": "Running", "device_id": "...", ...}}
#[no_mangle]
pub extern "C" fn cb_get_status(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		let status = hh.core_mut()?.get_status()?;

		Ok(ok_json(status))
	}));

	ffi_result("GET_STATUS_FAILED", run)
}

/// 设置单个设备的共享策略
///
/// 入参格式：{"peer_id": "device_uuid", "share_to_peer": true, "accept_from_peer": false}
/// 返回格式：{"ok": true}
#[no_mangle]
pub extern "C" fn cb_set_peer_policy(
	h: *mut cb_handle,
	policy_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let policy_str = crate::cstr_to_str(policy_json)?;

		#[derive(serde::Deserialize)]
		struct PolicyRequest {
			peer_id: String,
			#[serde(default)]
			share_to_peer: Option<bool>,
			#[serde(default)]
			accept_from_peer: Option<bool>,
		}

		let req: PolicyRequest = serde_json::from_str(policy_str)?;
		hh.core_mut()?
			.set_peer_policy(&req.peer_id, req.share_to_peer, req.accept_from_peer)?;

		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("SET_PEER_POLICY_FAILED", run)
}

/// 清除设备指纹（用于重新配对，解决 TLS_PIN_MISMATCH 问题）
///
/// 入参格式：{"peer_id": "device_uuid"}
/// 返回格式：{"ok": true}
#[no_mangle]
pub extern "C" fn cb_clear_peer_fingerprint(
	h: *mut cb_handle,
	peer_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let json_str = crate::cstr_to_str(peer_id_json)?;

		#[derive(serde::Deserialize)]
		struct ClearFingerprintRequest {
			peer_id: String,
		}

		let req: ClearFingerprintRequest = serde_json::from_str(json_str)?;
		hh.core_mut()?.clear_peer_fingerprint(&req.peer_id)?;

		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_PEER_FINGERPRINT_FAILED", run)
}

/// 清除本地证书（用于重新生成证书，需要重新配对所有设备）
///
/// 无需参数
/// 返回格式：{"ok": true}
#[no_mangle]
pub extern "C" fn cb_clear_local_cert(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		hh.core_mut()?.clear_local_cert()?;
		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_LOCAL_CERT_FAILED", run)
}

#[derive(serde::Deserialize)]
struct EnsureContentDto {
	item_id: String,
	file_id: Option<String>,
	// prefer_peer: Option<String>, // 预留，Core API 升级后可传入
}

#[no_mangle]
pub extern "C" fn cb_ensure_content_cached(
	h: *mut cb_handle,
	req_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		let json_str = crate::cstr_to_str(req_json)?;
		let dto: EnsureContentDto = serde_json::from_str(json_str).context("invalid json")?;

		// 调用 Core API
		let transfer_id = hh
			.core_mut()?
			.ensure_content_cached(&dto.item_id, dto.file_id.as_deref())?;

		Ok(crate::error::ok_json(
			serde_json::json!({ "transfer_id": transfer_id }),
		))
	}));
	ffi_result("START_FETCH_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_cancel_transfer(
	h: *mut cb_handle,
	transfer_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		let json_str = crate::cstr_to_str(transfer_id_json)?;
		// 假设传入的是一个单纯的字符串 JSON，如 "uuid"
		let tid: String = serde_json::from_str(json_str).context("invalid json string")?;

		hh.core_mut()?.cancel_transfer(&tid);
		Ok(crate::error::ok_json(serde_json::json!({})))
	}));
	ffi_result("CANCEL_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_list_history(h: *mut cb_handle, query_json: *const c_char) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		// 1. 解析入参
		let json_str = crate::cstr_to_str(query_json)?;
		let dto: HistoryQueryDto = serde_json::from_str(json_str).context("invalid query json")?;

		// 2. 调用 Core 获取列表
		let items = hh.core_mut()?.list_history(dto.limit, dto.cursor)?;

		// 3. 【关键修改】构造分页结构
		// 因为 Core 目前没有直接返回 next_cursor，我们这里简单推算：
		// 如果返回数量等于 limit，说明可能还有下一页，取最后一条的时间戳做 cursor
		let next_cursor = if items.len() >= dto.limit {
			items.last().map(|i| i.created_ts_ms)
		} else {
			None
		};

		// 构造符合 C# HistoryPage 定义的 JSON 对象
		let page_data = serde_json::json!({
			"items": items,
			"next_cursor": next_cursor
		});

		// 4. 包装返回
		Ok(crate::error::ok_json(page_data))
	}));
	ffi_result("LIST_HISTORY_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_get_item_meta(
	h: *mut cb_handle,
	item_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
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
			struct IdObj {
				item_id: String,
			}
			let obj: IdObj = serde_json::from_str(json_str).context("invalid item_id json")?;
			obj.item_id
		};

		// 2. 调用 Core
		let meta = hh.core_mut()?.get_item_meta(&item_id)?;

		// 3. 包装返回
		match meta {
			Some(m) => Ok(crate::error::ok_json(m)),
			None => Err(anyhow::anyhow!("Item not found")),
		}
	}));

	ffi_result("GET_ITEM_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_delete_item_local(
	h: *mut cb_handle,
	item_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let json_str = crate::cstr_to_str(item_id_json)?;
		let item_id = if let Ok(s) = serde_json::from_str::<String>(json_str) {
			s
		} else {
			#[derive(serde::Deserialize)]
			struct IdObj {
				item_id: String,
			}
			let obj: IdObj = serde_json::from_str(json_str).context("invalid item_id json")?;
			obj.item_id
		};
		hh.core_mut()?.delete_item_local(&item_id)?;
		Ok(crate::error::ok_json(true))
	}));
	ffi_result("DELETE_ITEM_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_delete_item_global(
	h: *mut cb_handle,
	item_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let json_str = crate::cstr_to_str(item_id_json)?;
		let item_id = if let Ok(s) = serde_json::from_str::<String>(json_str) {
			s
		} else {
			#[derive(serde::Deserialize)]
			struct IdObj {
				item_id: String,
			}
			let obj: IdObj = serde_json::from_str(json_str).context("invalid item_id json")?;
			obj.item_id
		};
		hh.core_mut()?.delete_item_global(&item_id)?;
		Ok(crate::error::ok_json(true))
	}));
	ffi_result("DELETE_ITEM_FAILED", run)
}

/// 写入日志（多语言版本）
#[no_mangle]
pub extern "C" fn cb_logs_write(
	h: *mut cb_handle,
	level: i32,
	component: *const c_char,
	category: *const c_char,
	message_en: *const c_char,
	message_zh_cn: *const c_char,
	exception: *const c_char,
	props_json: *const c_char,
	ts_utc: i64,
	out_id: *mut i64,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<()> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let component_s = cstr_to_str(component)?;
		let category_s = cstr_to_str(category)?;

		// #region agent log
		eprintln!(
			"[cb_logs_write] H_A: Received component={}, category={}, level={}, ts_utc={}",
			component_s, category_s, level, ts_utc
		);
		// #endregion
		let message_en_s = cstr_to_str(message_en)?;
		let message_zh_cn_s = if message_zh_cn.is_null() {
			None
		} else {
			Some(cstr_to_str(message_zh_cn)?)
		};
		let exception_s = if exception.is_null() {
			None
		} else {
			Some(cstr_to_str(exception)?)
		};
		let props_json_s = if props_json.is_null() {
			None
		} else {
			Some(cstr_to_str(props_json)?)
		};

		// 如果 ts_utc <= 0，使用当前时间；否则使用提供的时间戳（用于暂存日志回写）
		let ts_utc = if ts_utc <= 0 {
			cb_core::util::now_ms()
		} else {
			ts_utc
		};

		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let id = log_store.write_multilang(
			ts_utc,
			level,
			component_s,
			category_s,
			message_en_s,
			message_zh_cn_s,
			exception_s,
			props_json_s,
		)?;

		unsafe {
			if !out_id.is_null() {
				*out_id = id;
			}
		}
		Ok(())
	}));

	ffi_status(run)
}

/// 查询增量日志 (after_id)
#[no_mangle]
pub extern "C" fn cb_logs_query_after_id(
	h: *mut cb_handle,
	after_id: i64,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries = log_store.query_after_id(after_id, level_min, like_s, limit, lang_s)?;

		Ok(crate::error::ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 查询最新的日志（从新到旧排序，用于初始加载）
#[no_mangle]
pub extern "C" fn cb_logs_query_latest(
	h: *mut cb_handle,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries = log_store.query_latest(level_min, like_s, limit, lang_s)?;

		Ok(crate::error::ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 查询 before_id 之前的日志（用于向上滚动加载更早的日志）
#[no_mangle]
pub extern "C" fn cb_logs_query_before_id(
	h: *mut cb_handle,
	before_id: i64,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries = log_store.query_before_id(before_id, level_min, like_s, limit, lang_s)?;

		Ok(crate::error::ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 查询时间范围内的日志
#[no_mangle]
pub extern "C" fn cb_logs_query_range(
	h: *mut cb_handle,
	start_ms: i64,
	end_ms: i64,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	offset: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries =
			log_store.query_range(start_ms, end_ms, level_min, like_s, limit, offset, lang_s)?;

		Ok(crate::error::ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 获取日志统计
#[no_mangle]
pub extern "C" fn cb_logs_stats(h: *mut cb_handle, out_json: *mut *const c_char) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let stats = log_store.stats()?;

		Ok(crate::error::ok_json(stats))
	}));

	ffi_out_json("STATS_FAILED", "Failed to get log stats", out_json, run)
}

/// 删除指定时间之前的日志
#[no_mangle]
pub extern "C" fn cb_logs_delete_before(
	h: *mut cb_handle,
	cutoff_ms: i64,
	out_deleted: *mut i64,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<i64> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let deleted = log_store.delete_before(cutoff_ms)?;
		Ok(deleted)
	}));

	ffi_out_i64(out_deleted, run)
}

/// 按 ID 列表删除日志
#[no_mangle]
pub extern "C" fn cb_logs_delete_by_ids(
	h: *mut cb_handle,
	ids_json: *const c_char,
	out_deleted: *mut i64,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<i64> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let ids_json_str = cstr_to_str(ids_json)?;
		let ids: Vec<i64> = serde_json::from_str(ids_json_str)?;

		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let deleted = log_store.delete_by_ids(&ids)?;
		Ok(deleted)
	}));

	ffi_out_i64(out_deleted, run)
}

/// 获取来源统计（component 和 category 的列表及计数）
#[no_mangle]
pub extern "C" fn cb_logs_source_stats(h: *mut cb_handle, out_json: *mut *const c_char) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let stats = log_store.get_source_stats()?;

		Ok(crate::error::ok_json(stats))
	}));

	ffi_out_json("STATS_FAILED", "Failed to get source stats", out_json, run)
}

/// 清空核心数据库
#[no_mangle]
pub extern "C" fn cb_clear_core_db(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut store = handle.core_mut()?.inner.store.lock().unwrap();
		store.clear_core_db()?;
		Ok(crate::error::ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 清空日志数据库
#[no_mangle]
pub extern "C" fn cb_clear_logs_db(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		log_store.clear_logs_db()?;
		Ok(crate::error::ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 清空统计数据库
#[no_mangle]
pub extern "C" fn cb_clear_stats_db(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut stats_store = handle.core_mut()?.inner.stats_store.lock().unwrap();
		stats_store.clear_stats_db()?;
		Ok(crate::error::ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 清空缓存（CAS blobs、临时文件等）
#[no_mangle]
pub extern "C" fn cb_clear_cache(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		handle.core_mut()?.clear_cache()?;
		Ok(crate::error::ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 查询缓存统计
#[no_mangle]
pub extern "C" fn cb_query_cache_stats(
	h: *mut cb_handle,
	query_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let query_str = cstr_to_str(query_json)?;

		#[derive(serde::Deserialize)]
		struct StatsQuery {
			#[serde(default)]
			start_ts_ms: i64,
			#[serde(default)]
			end_ts_ms: i64,
			#[serde(default = "default_bucket")]
			bucket_sec: i32,
		}
		fn default_bucket() -> i32 {
			10
		}

		let query: StatsQuery = serde_json::from_str(query_str)?;
		let start_ts = if query.start_ts_ms == 0 && query.end_ts_ms == 0 {
			// 默认窗口：最近60分钟
			cb_core::util::now_ms() - 60 * 60 * 1000
		} else {
			query.start_ts_ms
		};
		let end_ts = if query.end_ts_ms == 0 {
			cb_core::util::now_ms()
		} else {
			query.end_ts_ms
		};

		let core = handle.core_mut()?;
		let stats_store = core.inner.stats_store.lock().unwrap();
		let entries = stats_store.query_cache_stats(start_ts, end_ts, query.bucket_sec)?;

		// 解析data_json并构建series
		let mut series = Vec::new();
		for entry in entries {
			let data: serde_json::Value = serde_json::from_str(&entry.data_json)?;
			series.push(serde_json::json!({
				"ts_ms": entry.ts_ms,
				"cache_bytes": data.get("cache_bytes").and_then(|v| v.as_i64()).unwrap_or(0)
			}));
		}

		// 获取当前缓存大小（从store）
		let store = core.inner.store.lock().unwrap();
		let current_cache_bytes = store.sum_present_bytes().unwrap_or(0);

		Ok(crate::error::ok_json(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": query.bucket_sec
			},
			"series": series,
			"current_cache_bytes": current_cache_bytes
		})))
	}));

	ffi_result("QUERY_FAILED", run)
}

/// 查询网络统计
#[no_mangle]
pub extern "C" fn cb_query_net_stats(
	h: *mut cb_handle,
	query_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let query_str = cstr_to_str(query_json)?;

		#[derive(serde::Deserialize)]
		struct StatsQuery {
			#[serde(default)]
			start_ts_ms: i64,
			#[serde(default)]
			end_ts_ms: i64,
			#[serde(default = "default_bucket")]
			bucket_sec: i32,
		}
		fn default_bucket() -> i32 {
			10
		}

		let query: StatsQuery = serde_json::from_str(query_str)?;
		let start_ts = if query.start_ts_ms == 0 && query.end_ts_ms == 0 {
			cb_core::util::now_ms() - 60 * 60 * 1000
		} else {
			query.start_ts_ms
		};
		let end_ts = if query.end_ts_ms == 0 {
			cb_core::util::now_ms()
		} else {
			query.end_ts_ms
		};

		let stats_store = handle.core_mut()?.inner.stats_store.lock().unwrap();
		let entries = stats_store.query_net_stats(start_ts, end_ts, query.bucket_sec)?;

		let mut series = Vec::new();
		for entry in entries {
			let data: serde_json::Value = serde_json::from_str(&entry.data_json)?;
			series.push(serde_json::json!({
				"ts_ms": entry.ts_ms,
				"bytes_sent": data.get("bytes_sent").and_then(|v| v.as_i64()).unwrap_or(0),
				"bytes_recv": data.get("bytes_recv").and_then(|v| v.as_i64()).unwrap_or(0)
			}));
		}

		Ok(crate::error::ok_json(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": query.bucket_sec
			},
			"series": series
		})))
	}));

	ffi_result("QUERY_FAILED", run)
}

/// 查询活动统计
#[no_mangle]
pub extern "C" fn cb_query_activity_stats(
	h: *mut cb_handle,
	query_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let query_str = cstr_to_str(query_json)?;

		#[derive(serde::Deserialize)]
		struct StatsQuery {
			#[serde(default)]
			start_ts_ms: i64,
			#[serde(default)]
			end_ts_ms: i64,
			#[serde(default = "default_bucket")]
			bucket_sec: i32,
		}
		fn default_bucket() -> i32 {
			60
		}

		let query: StatsQuery = serde_json::from_str(query_str)?;
		let start_ts = if query.start_ts_ms == 0 && query.end_ts_ms == 0 {
			// 默认窗口：最近24小时
			cb_core::util::now_ms() - 24 * 60 * 60 * 1000
		} else {
			query.start_ts_ms
		};
		let end_ts = if query.end_ts_ms == 0 {
			cb_core::util::now_ms()
		} else {
			query.end_ts_ms
		};

		let stats_store = handle.core_mut()?.inner.stats_store.lock().unwrap();
		let entries = stats_store.query_activity_stats(start_ts, end_ts, query.bucket_sec)?;

		let mut series = Vec::new();
		for entry in entries {
			let data: serde_json::Value = serde_json::from_str(&entry.data_json)?;
			series.push(serde_json::json!({
				"ts_ms": entry.ts_ms,
				"text_count": data.get("text_count").and_then(|v| v.as_i64()).unwrap_or(0),
				"image_count": data.get("image_count").and_then(|v| v.as_i64()).unwrap_or(0),
				"files_count": data.get("files_count").and_then(|v| v.as_i64()).unwrap_or(0)
			}));
		}

		Ok(crate::error::ok_json(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": query.bucket_sec
			},
			"series": series
		})))
	}));

	ffi_result("QUERY_FAILED", run)
}

/// 获取 FFI ABI 版本，用于 C# 侧兼容性检查
#[no_mangle]
pub extern "C" fn cb_get_ffi_version(major: *mut u32, minor: *mut u32) {
	let _ = panic::catch_unwind(AssertUnwindSafe(|| {
		unsafe {
			if !major.is_null() {
				*major = 1; // 必须匹配 C# CB_FFI_ABI_MAJOR
			}
			if !minor.is_null() {
				*minor = 0;
			}
		}
	}));
}

#[cfg(test)]
mod tests;
