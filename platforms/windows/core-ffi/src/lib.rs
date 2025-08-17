// platforms/windows/core-ffi/src/lib.rs
#![allow(non_camel_case_types)]

use std::{
    ffi::c_char,
    slice,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use cb_core::{self, ClipMeta as CoreClipMeta};

// ===== C 对齐结构体（保持与 cb_ffi.h 一致） =====
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CbStr {
    pub ptr: *const c_char,
    pub len: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CbBytes {
    pub ptr: *const u8,
    pub len: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CbStrList {
    pub items: *const CbStr,
    pub len: u32,
}

#[repr(C)]
pub struct CbDevice {
    pub device_id: CbStr,
    pub account_id: CbStr,
    pub name: CbStr,
    pub pubkey_fingerprint: CbStr,
}
#[repr(C)]
pub struct CbConfig {
    pub device_name: CbStr,
    pub listen_port: i32,
    pub api_version: u32,
}
#[repr(C)]
pub struct CbMeta {
    pub item_id: CbStr,
    pub owner_device_id: CbStr,
    pub owner_account_id: CbStr,
    pub kinds: CbStrList,
    pub mimes: CbStrList,
    pub preferred_mime: CbStr,
    pub size_bytes: u64,
    pub sha256: CbStr,
    pub created_at: u64,
    pub expires_at: u64,
}

// ===== 回调类型 =====
type OnDeviceOnline      = Option<extern "C" fn(*const CbDevice)>;
type OnDeviceOffline     = Option<extern "C" fn(*const CbStr)>;
type OnNewMetadata       = Option<extern "C" fn(*const CbMeta)>;
type OnTransferProgress  = Option<extern "C" fn(*const CbStr, u64, u64)>;
type OnError             = Option<extern "C" fn(i32, *const CbStr)>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CbCallbacks {
    pub on_device_online: OnDeviceOnline,
    pub on_device_offline: OnDeviceOffline,
    pub on_new_metadata: OnNewMetadata,
    pub on_transfer_progress: OnTransferProgress,
    pub on_error: OnError,
}

// ===== 全局状态 =====
static STORE: OnceLock<Mutex<State>> = OnceLock::new();
struct State {
    callbacks: Option<CbCallbacks>,
    paused: bool,
}
fn state() -> &'static Mutex<State> {
    STORE.get_or_init(|| Mutex::new(State { callbacks: None, paused: false }))
}

// ===== ☆ 你问到的两个“辅助转换函数”就放这里（C → Rust Owned） =====
unsafe fn cbstr_to_string(s: CbStr) -> String {
    if s.ptr.is_null() || s.len == 0 { return String::new(); }
    let bytes = slice::from_raw_parts(s.ptr as *const u8, s.len as usize);
    String::from_utf8_lossy(bytes).to_string()
}
unsafe fn list_to_vec_strings(list: CbStrList) -> Vec<String> {
    if list.items.is_null() || list.len == 0 { return vec![]; }
    let items = slice::from_raw_parts(list.items, list.len as usize);
    items.iter().map(|s| cbstr_to_string(*s)).collect()
}

// 小工具：把 &str 包成 CbStr（注意：仅本栈帧有效，供回调临时使用）
fn s(s: &str) -> CbStr { CbStr { ptr: s.as_ptr() as *const c_char, len: s.len() as u32 } }
fn sl(items: &[CbStr]) -> CbStrList { CbStrList { items: items.as_ptr(), len: items.len() as u32 } }

// ===== 导出 API =====
#[no_mangle]
pub extern "C" fn cb_get_version() -> u32 { 1 }

// 初始化：这里会初始化 SQLite（cb_core::init），并保存回调
#[no_mangle]
pub extern "C" fn cb_init(cfg: *const CbConfig, cbs: *const CbCallbacks) -> i32 {
    if cfg.is_null() || cbs.is_null() { return -1; }

    // 1) 初始化 SQLite 到平台默认目录（Windows: %LOCALAPPDATA%\ClipBridge\db\clipbridge.sqlite）
    if let Err(e) = cb_core::init(None) {
        eprintln!("core init failed: {e:?}");
        return -2;
    }

    // 2) 保存回调
    let mut st = state().lock().unwrap();
    unsafe { st.callbacks = Some((*cbs).clone()); }
    st.paused = false;

    // 3) 演示：0.8s 后回调一次 device+meta，方便前端验证事件通路
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(800));
        let guard = state().lock().unwrap();
        if let Some(ref c) = guard.callbacks {
            if let Some(f) = c.on_device_online {
                let dev = CbDevice {
                    device_id: s("device-A"),
                    account_id: s("account-1"),
                    name: s("My-PC"),
                    pubkey_fingerprint: s("fp-001"),
                };
                f(&dev as *const _);
            }
            if let Some(fm) = c.on_new_metadata {
                let kinds_arr = [s("text")];
                let mimes_arr = [s("text/plain")];
                let meta = CbMeta {
                    item_id: s("item-123"),
                    owner_device_id: s("device-A"),
                    owner_account_id: s("account-1"),
                    kinds: sl(&kinds_arr),
                    mimes: sl(&mimes_arr),
                    preferred_mime: s("text/plain"),
                    size_bytes: 14,
                    sha256: s(""),
                    created_at: 1_720_000_000,
                    expires_at: 0,
                };
                fm(&meta as *const _);
            }
        }
    });

    0
}

#[no_mangle]
pub extern "C" fn cb_pause(pause: i32) -> i32 {
    let mut st = state().lock().unwrap();
    st.paused = pause != 0;
    0
}

#[no_mangle]
pub extern "C" fn cb_shutdown() {
    let mut st = state().lock().unwrap();
    st.callbacks = None;
    st.paused = false;
}

// ===== ☆ 你问到的两个导出函数：cb_store_metadata / cb_history_list =====

// 把一条元数据写入 SQLite 历史
#[no_mangle]
pub extern "C" fn cb_store_metadata(meta: *const CbMeta) -> i32 {
    if meta.is_null() { return -1; }
    let m = unsafe { &*meta };

    // C → Rust Owned
    let item_id          = unsafe { cbstr_to_string(m.item_id) };
    let owner_device_id  = unsafe { cbstr_to_string(m.owner_device_id) };
    let owner_account_id = unsafe { let s = cbstr_to_string(m.owner_account_id); if s.is_empty() { None } else { Some(s) } };
    let kinds_vec        = unsafe { list_to_vec_strings(m.kinds) };
    let mimes_vec        = unsafe { list_to_vec_strings(m.mimes) };
    let preferred_mime   = unsafe { cbstr_to_string(m.preferred_mime) };
    let sha256           = unsafe { let s = cbstr_to_string(m.sha256); if s.is_empty() { None } else { Some(s) } };

    let row = CoreClipMeta {
        item_id,
        source_device_id: owner_device_id,  // 先用 owner_device_id 作为来源
        owner_account_id,
        kinds_json: serde_json::to_string(&kinds_vec).unwrap_or_else(|_| "[]".to_string()),
        mimes_json: serde_json::to_string(&mimes_vec).unwrap_or_else(|_| "[]".to_string()),
        preferred_mime,
        size_bytes: m.size_bytes as i64,
        sha256,
        preview_text: None,
        files_json: None,
        created_at: m.created_at as i64,
        expires_at: if m.expires_at == 0 { None } else { Some(m.expires_at as i64) },
        seen_ts: chrono::Utc::now().timestamp(),
    };

    match cb_core::store_meta(&row) {
        Ok(_) => 0,
        Err(e) => { eprintln!("store_meta failed: {e:?}"); -2 }
    }
}

// 查询历史并“逐条通过 on_new_metadata 回调喂给外壳”
#[no_mangle]
pub extern "C" fn cb_history_list(since_ts: u64, limit: u32) -> i32 {
    // 1) 读库
    let rows = match cb_core::history_since(since_ts as i64, limit) {
        Ok(v) => v,
        Err(e) => { eprintln!("history_since failed: {e:?}"); return -2; }
    };

    // 2) 取回调
    let st = state().lock().unwrap();
    let Some(cb) = &st.callbacks else { return -3; };
    let Some(on_meta) = cb.on_new_metadata else { return 0; };

    // 3) 逐条拼装并回调
    for r in rows {
        // —— 先把所有“被引用的数据”放到稳定的本地变量里（保证在回调期间活着）——
        let item_id_s        = r.item_id;                     // String
        let owner_dev_s      = r.source_device_id;            // String
        let owner_acc_s      = r.owner_account_id.unwrap_or_default(); // String
        let preferred_mime_s = r.preferred_mime;              // String
        let sha_s            = r.sha256.unwrap_or_default();  // String

        // kinds/mimes 反序列化成 Vec<String>
        let kinds_vec: Vec<String> = serde_json::from_str(&r.kinds_json).unwrap_or_default();
        let mimes_vec: Vec<String> = serde_json::from_str(&r.mimes_json).unwrap_or_default();

        // 把 Vec<String> 映射为 Vec<CbStr>（指向上面的字符串数据）
        let kinds_store: Vec<CbStr> = kinds_vec.iter()
            .map(|t| CbStr { ptr: t.as_ptr() as *const c_char, len: t.len() as u32 })
            .collect();
        let mimes_store: Vec<CbStr> = mimes_vec.iter()
            .map(|t| CbStr { ptr: t.as_ptr() as *const c_char, len: t.len() as u32 })
            .collect();

        // 再把顶层 String 也转成 CbStr（仍指向上面的变量数据）
        let item_id_cb       = CbStr { ptr: item_id_s.as_ptr() as *const c_char,       len: item_id_s.len() as u32 };
        let owner_dev_cb     = CbStr { ptr: owner_dev_s.as_ptr() as *const c_char,     len: owner_dev_s.len() as u32 };
        let owner_acc_cb     = CbStr { ptr: owner_acc_s.as_ptr() as *const c_char,     len: owner_acc_s.len() as u32 };
        let preferred_mime_cb= CbStr { ptr: preferred_mime_s.as_ptr() as *const c_char,len: preferred_mime_s.len() as u32 };
        let sha_cb           = CbStr { ptr: sha_s.as_ptr() as *const c_char,           len: sha_s.len() as u32 };

        let kinds_cb = CbStrList { items: kinds_store.as_ptr(), len: kinds_store.len() as u32 };
        let mimes_cb = CbStrList { items: mimes_store.as_ptr(), len: mimes_store.len() as u32 };

        // 组装 CbMeta（仅包含对上面本地变量的指针；生命周期受本作用域控制）
        let meta = CbMeta {
            item_id:           item_id_cb,
            owner_device_id:   owner_dev_cb,
            owner_account_id:  owner_acc_cb,
            kinds:             kinds_cb,
            mimes:             mimes_cb,
            preferred_mime:    preferred_mime_cb,
            size_bytes:        r.size_bytes as u64,
            sha256:            sha_cb,
            created_at:        r.created_at as u64,
            expires_at:        r.expires_at.unwrap_or(0) as u64,
        };

        // 在这些局部变量仍然活着的时候回调
        on_meta(&meta as *const CbMeta);
        // 回调结束后自然继续下一条；局部变量在本次循环末尾释放
    }

    0
}


// 预留：统一释放跨 FFI 分配的内存（当前未用）
#[no_mangle]
pub extern "C" fn cb_free(_p: *mut core::ffi::c_void) {}
