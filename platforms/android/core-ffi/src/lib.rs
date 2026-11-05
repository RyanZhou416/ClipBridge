//! ClipBridge C ABI 桥接层（Windows / macOS / Android 三个平台共用同一份实现）。
//!
//! # 句柄并发契约（重要）
//!
//! - 通过 `cb_init` 返回的 `cb_handle` 指针 **不得并发使用**。
//! - 设计契约是“单线程串行调用”：壳侧必须把所有 FFI 调用串行化（例如同一把锁保护）。
//! - `cb_core_ffi::handle` 里的 `busy` 原子标记做一次廉价的“重入检测”：并发调用会返回
//!   `CORE_BUSY` 错误，而不是继续制造别名 UB；`cb_shutdown` 之后所有入口返回 `CORE_SHUTDOWN`。
//!
//! # 实现位置
//!
//! 真正的 C ABI 实现只存在于 `cb_core_ffi`（`crates/cb_core_ffi`），只写一次。本 crate
//! 仍然是独立的 cdylib / ABI 边界包（路径、包名、`crate-type` 与头文件都不变），只是通过
//! `pub use cb_core_ffi::exports::*;` 把同一份完整导出表面挂到这里。
//!
//! `bridge.rs` / `error.rs` 保留为一行 re-export shim，以维持文档中的目录布局。

pub use cb_core_ffi::exports::*;

mod bridge;
mod error;

#[cfg(test)]
mod tests;
