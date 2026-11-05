//! 兼容 shim：错误码映射与 JSON envelope 已统一到 `cb_core_ffi::errors` / `envelope`。
//! 保留本文件是为了维持文档中的 `core-ffi/src/error.rs` 目录布局（实现只写一份）。

#![allow(unused_imports)]
pub use cb_core_ffi::errors::*;
