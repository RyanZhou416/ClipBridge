//! JSON envelope 与 `catch_unwind` 边界。
//!
//! 所有 `cb_*` 入口共用这里的结果包装，确保：
//! 1. 返回值永远是 NUL 结尾的 JSON 字符串（`ret` 遇到内部 NUL 时回退到固定 envelope）；
//! 2. 任何 panic 都在 C ABI 边界被捕获，转成 `{"ok":false,"error":{...}}`，
//!    而不是让 panic 越过 `extern "C"`（那会是进程级 abort/UB）。
//!
//! Windows 的 verbatim 行为（`CORE_BUSY` / `CORE_SHUTDOWN` / `PANIC`）是唯一参考实现；
//! Android 旧版的 `ffi_safe!` 宏 panic 守卫与这里等价，能力没有丢失。

use crate::errors::error_code;
use crate::handle::HandleError;
use std::ffi::CString;
use std::os::raw::c_char;

/// 构造失败 envelope：`{"ok":false,"error":{"code":...,"message":...}}`。
pub fn err_json(code: &str, message: &str) -> String {
	serde_json::json!({
		"ok": false,
		"error": { "code": code, "message": message }
	})
	.to_string()
}

/// 构造成功 envelope：`{"ok":true,"data":...}`。
pub fn ok_json<T: serde::Serialize>(data: T) -> String {
	serde_json::json!({
		"ok": true,
		"data": data
	})
	.to_string()
}

/// 把 Rust `String` 转成 C 侧字符串；**绝不 panic**。
///
/// `CString::new` 只在字符串包含内部 NUL 时失败（例如错误信息里带用户数据），
/// 此时返回一个固定的错误 envelope，而不是 `unwrap()` 导致进程 abort。
pub(crate) fn ret(s: String) -> *const c_char {
	CString::new(s)
		.unwrap_or_else(|_| {
			CString::new(
				"{\"ok\":false,\"error\":{\"code\":\"INVALID_UTF8\",\"message\":\"response contained a NUL byte\"}}",
			)
			.expect("static fallback envelope is NUL-free")
		})
		.into_raw()
}

/// `catch_unwind` 捕获到的 panic payload 类型别名。
pub(crate) type PanicPayload = Box<dyn std::any::Any + Send + 'static>;

/// 捕获 `extern "C"` 边界上的 panic，统一转成错误 envelope，绝不让 panic 越过 C ABI。
pub(crate) fn ffi_result(
	code: &str,
	run: Result<anyhow::Result<String>, PanicPayload>,
) -> *const c_char {
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
pub(crate) fn ffi_out_json(
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
pub(crate) fn ffi_status(run: Result<anyhow::Result<()>, PanicPayload>) -> i32 {
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
pub(crate) fn ffi_out_i64(out: *mut i64, run: Result<anyhow::Result<i64>, PanicPayload>) -> i32 {
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
