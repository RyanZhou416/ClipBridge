// platforms/windows/core-ffi/src/lib.rs
//! ClipBridge Core — Windows FFI Bridge (mixed style)
//! - cb_init: 通过 C 结构体传配置
//! - 其余函数：JSON UTF-8 字符串入/出
//! - 所有由库分配返回的字符串都需调用 cb_free 释放
//!
//! 依赖：
//!   - once_cell = "1"
//!   - serde = { version = "1", features = ["derive"] }
//!   - serde_json = "1"
//!   - 本项目内部 crate: cb_core
//!
//! 生成方式（示例 Cargo.toml 片段见文末）：编译为 cdylib

use std::{
    ffi::{CStr, CString},
    os::raw::{c_char, c_int, c_uint, c_ulonglong, c_ushort},
    sync::{Mutex, Arc},
};

use once_cell::sync::OnceCell;

use cb_core::prelude::*;

// ----------------------------- 全局状态 ---------------------------------------------

struct CoreState {
    core: Option<CbCore>,
    callbacks: Option<Arc<CallbackBridge>>,
}

static GLOBAL: OnceCell<Mutex<CoreState>> = OnceCell::new();

fn state() -> &'static Mutex<CoreState> {
    GLOBAL.get_or_init(|| Mutex::new(CoreState { core: None, callbacks: None }))
}

// ----------------------------- C 侧结构体映射 ---------------------------------------

#[repr(C)]
pub struct cb_config {
    device_name: *const c_char,
    data_dir: *const c_char,
    cache_dir: *const c_char,
    log_dir: *const c_char,

    max_cache_bytes: u64,
    max_cache_items: u32,
    max_history_items: u32,
    item_ttl_secs: i32,

    enable_mdns: c_int,
    service_name: *const c_char,
    port: c_ushort,
    prefer_quic: c_int,

    key_alias: *const c_char,
    trusted_only: c_int,
    require_encryption: c_int,

    reserved1: *const c_char,
    reserved2: u64,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct cb_callbacks {
    device_online: Option<extern "C" fn(*const c_char)>,
    device_offline: Option<extern "C" fn(*const c_char)>,
    new_metadata: Option<extern "C" fn(*const c_char)>,
    transfer_progress: Option<extern "C" fn(*const c_char, c_ulonglong, c_ulonglong)>,
    on_error: Option<extern "C" fn(c_int, *const c_char)>,
}

// ----------------------------- 回调桥（C 指针 → Rust Trait） ------------------------

struct CallbackBridge {
    cbs: cb_callbacks,
}

impl CallbackBridge {
    fn new(cbs: &cb_callbacks) -> Self {
        Self { cbs: *cbs }
    }

    fn emit_error(&self, code: c_int, msg: &str) {
        if let Some(f) = self.cbs.on_error {
            let c = CString::new(msg).unwrap_or_else(|_| CString::new("error").unwrap());
            f(code, c.as_ptr());
        }
    }
}

impl CbCallbacks for CallbackBridge {
    fn on_device_online(&self, device: &DeviceInfo) {
        if let Some(f) = self.cbs.device_online {
            if let Ok(js) = serde_json::to_string(device) {
                if let Ok(cs) = CString::new(js) {
                    f(cs.as_ptr());
                }
            }
        }
    }
    fn on_device_offline(&self, device_id: &str) {
        if let Some(f) = self.cbs.device_offline {
            if let Ok(cs) = CString::new(device_id) {
                f(cs.as_ptr());
            }
        }
    }
    fn on_new_metadata(&self, meta: &ItemMeta) {
        if let Some(f) = self.cbs.new_metadata {
            if let Ok(js) = serde_json::to_string(meta) {
                if let Ok(cs) = CString::new(js) {
                    f(cs.as_ptr());
                }
            }
        }
    }
    fn on_transfer_progress(&self, item_id: &str, done: u64, total: u64) {
        if let Some(f) = self.cbs.transfer_progress {
            if let Ok(cs) = CString::new(item_id) {
                f(cs.as_ptr(), done as c_ulonglong, total as c_ulonglong);
            }
        }
    }
    fn on_error(&self, err: &CbError) {
        // 将 Core 的错误转为 (code, message)
        let code = match err.kind {
            CbErrorKind::InvalidArg => 1,
            CbErrorKind::InitFailed => 2,
            CbErrorKind::Storage => 3,
            CbErrorKind::Network => 4,
            CbErrorKind::NotFound => 5,
            CbErrorKind::Paused => 6,
            CbErrorKind::Internal => 7,
        };
        self.emit_error(code, &err.message);
    }
}

// ----------------------------- 工具：C 字符串 ↔ Rust --------------------------------

fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() { return None; }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn cstr_required(_name: &str, ptr: *const c_char) -> Result<String, c_int> {
    if ptr.is_null() {
        return Err(CB_ERR_INVALID_ARG);
    }
    Ok(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string())
}

fn to_cstring_ptr(s: String) -> *mut c_char {
    CString::new(s).unwrap().into_raw()
}

fn set_out_json(out: *mut *mut c_char, s: String) -> c_int {
    if out.is_null() { return CB_ERR_INVALID_ARG; }
    unsafe { *out = to_cstring_ptr(s) };
    CB_OK
}

// ----------------------------- 错误码常量（与头文件一致） ----------------------------

const CB_OK: c_int              = 0;
const CB_ERR_INVALID_ARG: c_int = 1;
const CB_ERR_INIT_FAILED: c_int = 2;
const CB_ERR_STORAGE: c_int     = 3;
const CB_ERR_NETWORK: c_int     = 4;
const CB_ERR_NOT_FOUND: c_int   = 5;
const CB_ERR_PAUSED: c_int      = 6;
const CB_ERR_INTERNAL: c_int    = 7;

fn map_core_err(e: &CbError) -> c_int {
    match e.kind {
        CbErrorKind::InvalidArg => CB_ERR_INVALID_ARG,
        CbErrorKind::InitFailed => CB_ERR_INIT_FAILED,
        CbErrorKind::Storage    => CB_ERR_STORAGE,
        CbErrorKind::Network    => CB_ERR_NETWORK,
        CbErrorKind::NotFound   => CB_ERR_NOT_FOUND,
        CbErrorKind::Paused     => CB_ERR_PAUSED,
        CbErrorKind::Internal   => CB_ERR_INTERNAL,
    }
}

// ----------------------------- extern "C" 导出 --------------------------------------

#[no_mangle]
pub extern "C" fn cb_init(cfg: *const cb_config, cbs: *const cb_callbacks) -> c_int {
    if cfg.is_null() || cbs.is_null() {
        return CB_ERR_INVALID_ARG;
    }

    // 把 C 结构体映射到 CbConfig
    let cfg_ref = unsafe { &*cfg };
    let device_name = match cstr_required("device_name", cfg_ref.device_name) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let data_dir = match cstr_required("data_dir", cfg_ref.data_dir) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let cache_dir = match cstr_required("cache_dir", cfg_ref.cache_dir) {
        Ok(s) => s,
        Err(code) => return code,
    };

    let limits = CacheLimits {
        max_bytes: cfg_ref.max_cache_bytes,
        max_items: cfg_ref.max_cache_items,
    };
    let net = NetOptions {
        enable_mdns: cfg_ref.enable_mdns != 0,
        prefer_quic: cfg_ref.prefer_quic != 0,
    };
    let sec = SecurityOptions {
        trusted_only: cfg_ref.trusted_only != 0,
        require_encryption: cfg_ref.require_encryption != 0,
    };

    let callbacks = unsafe { &*cbs };
    let bridge = Arc::new(CallbackBridge::new(callbacks));

    // 构造 Core
    let core_cfg = CbConfig {
        device_name,
        data_dir: data_dir.into(),
        cache_dir: cache_dir.into(),
        cache_limits: limits,
        net,
        security: sec,
    };

    // 初始化 Core
    let core_res = CbCore::init(
        core_cfg,
        bridge.clone(),
        Arc::new(FileSecureStore::new(
            cstr_opt(cfg_ref.key_alias).unwrap_or_else(|| "default".to_string()),
            // keystore 目录：<data_dir>/keystore
            cstr_required("data_dir", cfg_ref.data_dir).unwrap() + "/keystore",
        )),
    );

    let mut st = state().lock().unwrap();
    match core_res {
        Ok(core) => {
            st.core = Some(core);
            st.callbacks = Some(bridge);
            CB_OK
        }
        Err(e) => {
            // 把错误抛给上层回调
            let code = map_core_err(&e);
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(code, &e.message);
            }
            code
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_shutdown() {
    let mut st = state().lock().unwrap();
    if let Some(core) = st.core.take() {
        core.shutdown();
    }
    st.callbacks = None;
}

#[no_mangle]
pub extern "C" fn cb_get_version_string() -> *mut c_char {
    to_cstring_ptr(cb_core::proto::CORE_SEMVER.to_string())
}

#[no_mangle]
pub extern "C" fn cb_get_protocol_version() -> c_uint {
    cb_core::proto::PROTOCOL_VERSION
}

#[no_mangle]
pub extern "C" fn cb_ingest_local_copy(json_snapshot: *const c_char, out_item_id: *mut *mut c_char) -> c_int {
    let js = match cstr_required("json_snapshot", json_snapshot) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let snap: ClipboardSnapshot = match serde_json::from_str(&js) {
        Ok(v) => v,
        Err(_) => return CB_ERR_INVALID_ARG,
    };

    let st = state().lock().unwrap();
    let core = match st.core.as_ref() {
        Some(c) => c,
        None => return CB_ERR_INIT_FAILED,
    };

    match core.ingest_local_copy(snap) {
        Ok(id) => set_out_json(out_item_id, id),
        Err(e) => {
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(map_core_err(&e), &e.message);
            }
            map_core_err(&e)
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_ingest_remote_metadata(json_meta: *const c_char) -> c_int {
    let js = match cstr_required("json_meta", json_meta) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let meta: ItemMeta = match serde_json::from_str(&js) {
        Ok(v) => v,
        Err(_) => return CB_ERR_INVALID_ARG,
    };

    let st = state().lock().unwrap();
    let core = match st.core.as_ref() {
        Some(c) => c,
        None => return CB_ERR_INIT_FAILED,
    };

    match core.ingest_remote_metadata(&meta) {
        Ok(()) => CB_OK,
        Err(e) => {
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(map_core_err(&e), &e.message);
            }
            map_core_err(&e)
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_ensure_content_cached(
    item_id: *const c_char,
    prefer_mime_or_null: *const c_char,
    out_json_localref: *mut *mut c_char
) -> c_int {
    let id = match cstr_required("item_id", item_id) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let prefer = cstr_opt(prefer_mime_or_null);

    let st = state().lock().unwrap();
    let core = match st.core.as_ref() {
        Some(c) => c,
        None => return CB_ERR_INIT_FAILED,
    };

    match core.ensure_content_cached(&id, prefer.as_deref()) {
        Ok(loc) => {
            match serde_json::to_string(&loc) {
                Ok(js) => set_out_json(out_json_localref, js),
                Err(_) => CB_ERR_INTERNAL,
            }
        }
        Err(e) => {
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(map_core_err(&e), &e.message);
            }
            map_core_err(&e)
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_list_history(limit: c_uint, offset: c_uint, out_json_array: *mut *mut c_char) -> c_int {
    let st = state().lock().unwrap();
    let core = match st.core.as_ref() {
        Some(c) => c,
        None => return CB_ERR_INIT_FAILED,
    };
    let q = HistoryQuery {
        limit,
        offset,
        kind: None,
    };
    match core.list_history(q) {
        Ok(v) => {
            match serde_json::to_string(&v) {
                Ok(js) => set_out_json(out_json_array, js),
                Err(_) => CB_ERR_INTERNAL,
            }
        }
        Err(e) => {
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(map_core_err(&e), &e.message);
            }
            map_core_err(&e)
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_get_item(item_id: *const c_char, out_json_record: *mut *mut c_char) -> c_int {
    let id = match cstr_required("item_id", item_id) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let st = state().lock().unwrap();
    let core = match st.core.as_ref() {
        Some(c) => c,
        None => return CB_ERR_INIT_FAILED,
    };
    match core.get_item(&id) {
        Ok(Some(rec)) => {
            match serde_json::to_string(&rec) {
                Ok(js) => set_out_json(out_json_record, js),
                Err(_) => CB_ERR_INTERNAL,
            }
        }
        Ok(None) => CB_ERR_NOT_FOUND,
        Err(e) => {
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(map_core_err(&e), &e.message);
            }
            map_core_err(&e)
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_pause(yes: c_int) -> c_int {
    let st = state().lock().unwrap();
    if let Some(core) = st.core.as_ref() {
        core.pause(yes != 0);
        CB_OK
    } else {
        CB_ERR_INIT_FAILED
    }
}

#[no_mangle]
pub extern "C" fn cb_prune_cache() -> c_int {
    let st = state().lock().unwrap();
    let core = match st.core.as_ref() {
        Some(c) => c,
        None => return CB_ERR_INIT_FAILED,
    };
    match core.prune_cache() {
        Ok(()) => CB_OK,
        Err(e) => {
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(map_core_err(&e), &e.message);
            }
            map_core_err(&e)
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_prune_history() -> c_int {
    let st = state().lock().unwrap();
    let core = match st.core.as_ref() {
        Some(c) => c,
        None => return CB_ERR_INIT_FAILED,
    };
    match core.prune_history() {
        Ok(()) => CB_OK,
        Err(e) => {
            if let Some(cb) = st.callbacks.as_ref() {
                cb.emit_error(map_core_err(&e), &e.message);
            }
            map_core_err(&e)
        }
    }
}

#[no_mangle]
pub extern "C" fn cb_free(p: *mut c_char) {
    if p.is_null() { return; }
    unsafe { drop(CString::from_raw(p)); }
}

// ----------------------------- FileSecureStore 实现 -------------------------------
// 简单的“文件型安全存储”，写在 <data_dir>/keystore/<alias> 文件里。
// 仅用于 demo；生产建议替换为 DPAPI/Keychain 等系统级方案。

struct FileSecureStore {
    alias: String,
    dir: String,
}

impl FileSecureStore {
    fn new(alias: String, dir: String) -> Self {
        Self { alias, dir }
    }
    fn path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.dir).join(&self.alias)
    }
}

impl SecureStore for FileSecureStore {
    fn get(&self, _key: &str) -> Option<Vec<u8>> {
        let p = self.path();
        std::fs::read(p).ok()
    }
    fn set(&self, _key: &str, value: &[u8]) -> CbResult<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| CbError { kind: CbErrorKind::InitFailed, message: e.to_string() })?;
        std::fs::write(self.path(), value)
            .map_err(|e| CbError { kind: CbErrorKind::InitFailed, message: e.to_string() })?;
        Ok(())
    }
}
