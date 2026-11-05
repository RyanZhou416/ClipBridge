//! ClipBridge 共享 C ABI 实现（shared C ABI implementation）。
//!
//! 这是 Windows、macOS、Android 三个 `platforms/*/core-ffi` cdylib 共同依赖的
//! **唯一实现**。三个平台包仍然各自是独立的 cdylib / ABI 边界包，只是用
//! `pub use cb_core_ffi::exports::*;` 把这里的 `cb_*` 导出到自己的 cdylib 中。
//!
//! # 为什么是 rlib
//!
//! 本 crate 只包含实现，不产生 cdylib；真正的 ABI 产物（DLL / dylib / .so）仍由
//! 三个平台 crate 各自生成，因此头文件、包名、路径和发布流程都不变。
//!
//! # 句柄并发契约（重要）
//!
//! - 通过 [`exports::cb_init`] 返回的 `cb_handle` 指针 **不得并发使用**。
//! - 设计契约是“单线程串行调用”：壳侧必须把所有 FFI 调用串行化（例如同一把锁保护）。
//! - 每个入口在检查存活标记后都会执行 `unsafe { &mut *h }`；若两个线程同时调用，
//!   就会同时存在两个 `&mut cb_handle`，这在 Rust 中属于**别名未定义行为（UB）**。
//! - 作为过渡，入口用 [`handle`] 里的 `busy` 原子标记做一次廉价的“重入检测”：并发调用会
//!   返回 `CORE_BUSY` 错误而不是继续制造 UB；`cb_shutdown` 之后所有入口返回 `CORE_SHUTDOWN`。
//!   这不是完整的锁设计，只是把常见的并发误用变成可诊断的错误。
//!
//! # 模块
//!
//! - [`envelope`]：JSON envelope、`ret()` 与 `catch_unwind` 边界，保证 panic 不越过 C ABI。
//! - [`handle`]：`cb_handle`、重入守卫与幂等 shutdown 语义。
//! - [`marshal`]：C 字符串/DTO 解析（历史 `bridge.rs` 内容）。
//! - [`errors`]：稳定错误码映射（历史 `error.rs` 内容）。
//! - [`exports`]：全部 `#[no_mangle] pub extern "C" fn cb_*`，只写一次。

// C ABI 入口必须接收裸指针，且不能声明成 `unsafe fn`（否则头文件语义与 C 侧调用约定都要改）。
// 空指针 / 存活 / 重入检查由每个入口的 `enter()` 统一完成，因此在本 crate 统一关闭该 lint。
// 说明：这条 lint 在 clippy 里是 deny-by-default，不关掉会让 `cargo clippy` 直接编译失败。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod envelope;
pub mod errors;
pub mod exports;
pub mod handle;
pub mod marshal;
