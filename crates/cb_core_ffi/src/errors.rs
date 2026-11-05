//! 稳定错误码映射（历史 `error.rs` 内容）。
//!
//! JSON envelope 的规范实现在 [`crate::envelope`]；这里保留 `error` 模块路径下的
//! `ok_json` / `err_json` 再导出，并负责把内部错误映射为 C 侧稳定错误码。

use crate::handle::HandleError;

pub use crate::envelope::{err_json, ok_json};

/// 把 `anyhow::Error` 映射成 FFI 错误码（句柄错误优先于调用方默认码）。
pub(crate) fn error_code<'a>(e: &'a anyhow::Error, fallback: &'a str) -> &'a str {
	match e.downcast_ref::<HandleError>() {
		Some(HandleError::Shutdown) => "CORE_SHUTDOWN",
		Some(HandleError::Busy) => "CORE_BUSY",
		None => fallback,
	}
}
