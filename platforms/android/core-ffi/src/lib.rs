mod bridge;
mod error;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::Arc;
use std::panic::{self, AssertUnwindSafe}; // 必须引入这个
use anyhow::Context;
use cb_core::api::{Core, CoreEventSink};

use crate::error::{err_json, ok_json};

// --- 辅助宏：全自动捕获 Panic 和 Result ---
// 这段代码是防止崩溃的核心
macro_rules! ffi_safe {
    ($body:block) => {{
        // 1. 捕获 Panic (防止 SIGSEGV)
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            // 2. 执行业务逻辑，返回 anyhow::Result<String>
            let run = (|| -> anyhow::Result<String> {
                $body
            })();

            // 3. 处理 Result (Ok/Err)
            match run {
                Ok(s) => crate::ret(s),
                Err(e) => crate::ret(crate::error::err_json("FFI_ERR", &format!("{e:#}"))),
            }
        }));

        // 4. 处理 Panic 结果
        match result {
            Ok(ptr) => ptr,
            Err(_) => {
                // 打印日志 (在 Android logcat 中通常显示为 stderr)
                eprintln!("CRITICAL: Rust Panic caught in FFI boundary!");
                // 返回一个 JSON 告诉 Java 层发生了恐慌，而不是直接闪退
                crate::ret(crate::error::err_json("PANIC", "Rust panicked internally"))
            }
        }
    }};
}

#[repr(C)]
pub struct cb_handle {
	core: Core,
}

type OnEventFn = extern "C" fn(json: *const c_char, user_data: *mut c_void);

struct FfiSink {
	cb: OnEventFn,
	user: *mut c_void,
}

unsafe impl Send for FfiSink {}
unsafe impl Sync for FfiSink {}

impl CoreEventSink for FfiSink {
	fn emit(&self, event_json: String) {
		let Ok(cstr) = CString::new(event_json) else { return };
		(self.cb)(cstr.as_ptr(), self.user);
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
	cursor: Option<i64>,
}

fn default_limit() -> usize { 20 }

fn ret(s: String) -> *const c_char {
	CString::new(s).unwrap().into_raw()
}

#[no_mangle]
pub extern "C" fn cb_free_string(s: *const c_char) {
	if s.is_null() { return; }
	unsafe { drop(CString::from_raw(s as *mut c_char)); }
}

// ============================================================================
// 下面是应用了 ffi_safe! 宏的导出函数
// ============================================================================

#[no_mangle]
pub extern "C" fn cb_init(cfg_json: *const c_char, on_event: OnEventFn, user_data: *mut c_void) -> *const c_char {
	ffi_safe!({
        let cfg_s = cstr_to_str(cfg_json)?;
        let cfg = bridge::parse_cfg(cfg_s)?;

        let sink: Arc<dyn CoreEventSink> = Arc::new(FfiSink { cb: on_event, user: user_data });
        let core = Core::init(cfg, sink);

        let h = Box::new(cb_handle { core });
        let handle_ptr = Box::into_raw(h) as usize;

        Ok(ok_json(serde_json::json!({ "handle": handle_ptr.to_string() })))
    })
}

#[no_mangle]
pub extern "C" fn cb_shutdown(h: *mut cb_handle) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let boxed = unsafe { Box::from_raw(h) };
        boxed.core.shutdown();
        Ok(ok_json(serde_json::json!({})))
    })
}

#[no_mangle]
pub extern "C" fn cb_plan_local_ingest(h: *mut cb_handle, snapshot_json: *const c_char) -> *const c_char {
	ffi_safe!({
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
    })
}

#[no_mangle]
pub extern "C" fn cb_ingest_local_copy(h: *mut cb_handle, snapshot_json: *const c_char) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let snap_s = cstr_to_str(snapshot_json)?;
        let (snap, share_mode) = bridge::parse_snapshot(snap_s)?;

        let force = matches!(share_mode, bridge::ShareMode::Force);

        let hh = unsafe { &mut *h };
        let meta = hh.core.ingest_local_copy_with_force(snap, force)?;
        Ok(ok_json(serde_json::json!({ "meta": meta })))
    })
}

#[no_mangle]
pub extern "C" fn cb_list_peers(h: *mut cb_handle) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };
        let peers = hh.core.list_peers()?;
        Ok(ok_json(serde_json::json!(peers)))
    })
}

#[no_mangle]
pub extern "C" fn cb_get_status(h: *mut cb_handle) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };
        let status = hh.core.get_status()?;
        Ok(ok_json(status))
    })
}

#[derive(serde::Deserialize)]
struct EnsureContentDto {
	item_id: String,
	file_id: Option<String>,
}

#[no_mangle]
pub extern "C" fn cb_ensure_content_cached(h: *mut cb_handle, req_json: *const c_char) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };

        let json_str = crate::cstr_to_str(req_json)?;
        let dto: EnsureContentDto = serde_json::from_str(json_str).context("invalid json")?;

        let transfer_id = hh.core.ensure_content_cached(&dto.item_id, dto.file_id.as_deref())?;

        Ok(crate::error::ok_json(serde_json::json!({ "transfer_id": transfer_id })))
    })
}

#[no_mangle]
pub extern "C" fn cb_cancel_transfer(h: *mut cb_handle, transfer_id_json: *const c_char) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };

        let json_str = crate::cstr_to_str(transfer_id_json)?;
        let tid: String = serde_json::from_str(json_str).context("invalid json string")?;

        hh.core.cancel_transfer(&tid);
        Ok(crate::error::ok_json(serde_json::json!({})))
    })
}

#[no_mangle]
pub extern "C" fn cb_list_history(h: *mut cb_handle, query_json: *const c_char) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };

        let json_str = crate::cstr_to_str(query_json)?;
        let dto: HistoryQueryDto = serde_json::from_str(json_str).context("invalid query json")?;

        let items = hh.core.list_history(dto.limit, dto.cursor)?;

        let next_cursor = if items.len() >= dto.limit {
            items.last().map(|i| i.created_ts_ms)
        } else {
            None
        };

        let page_data = serde_json::json!({
            "items": items,
            "next_cursor": next_cursor
        });

        Ok(crate::error::ok_json(page_data))
    })
}

#[no_mangle]
pub extern "C" fn cb_get_item_meta(h: *mut cb_handle, item_id_json: *const c_char) -> *const c_char {
	ffi_safe!({
        if h.is_null() { anyhow::bail!("null handle"); }
        let hh = unsafe { &mut *h };

        let json_str = crate::cstr_to_str(item_id_json)?;
        let item_id = if let Ok(s) = serde_json::from_str::<String>(json_str) {
            s
        } else {
            #[derive(serde::Deserialize)]
            struct IdObj { item_id: String }
            let obj: IdObj = serde_json::from_str(json_str).context("invalid item_id json")?;
            obj.item_id
        };

        let meta = hh.core.get_item_meta(&item_id)?;

        match meta {
            Some(m) => Ok(crate::error::ok_json(m)),
            None => Err(anyhow::anyhow!("Item not found")),
        }
    })
}

#[no_mangle]
pub extern "C" fn cb_get_ffi_version(major: *mut u32, minor: *mut u32) {
	unsafe {
		if !major.is_null() { *major = 1; }
		if !minor.is_null() { *minor = 0; }
	}
}
