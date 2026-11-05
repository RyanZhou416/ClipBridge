//! 兼容 shim：DTO、`parse_cfg` / `parse_snapshot` 已统一到 `cb_core_ffi::marshal`。
//! 保留本文件是为了维持文档中的 `core-ffi/src/bridge.rs` 目录布局（实现只写一份）。

#![allow(unused_imports)]
pub use cb_core_ffi::marshal::*;
