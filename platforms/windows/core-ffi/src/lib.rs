mod bridge;
mod error;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::Arc;

use cb_core::api::{Core, CoreEventSink};

use crate::error::{err_json, ok_json};

#[repr(C)]
pub struct cb_handle {
    core: Core,
}

type OnEventFn = extern "C" fn(json: *const c_char, user_data: *mut c_void);

struct FfiSink {
    cb: OnEventFn,
    user: *mut c_void,
}

// function pointer + raw pointer：我们保证只“转发调用”，线程安全由壳侧处理
unsafe impl Send for FfiSink {}
unsafe impl Sync for FfiSink {}

impl CoreEventSink for FfiSink {
    fn emit(&self, event_json: String) {
        let Ok(cstr) = CString::new(event_json) else { return };
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

fn ret(s: String) -> *const c_char {
    CString::new(s).unwrap().into_raw()
}

#[no_mangle]
pub extern "C" fn cb_free_string(s: *const c_char) {
    if s.is_null() { return; }
    unsafe { drop(CString::from_raw(s as *mut c_char)); }
}

#[no_mangle]
pub extern "C" fn cb_init(cfg_json: *const c_char, on_event: OnEventFn, user_data: *mut c_void) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        let cfg_s = cstr_to_str(cfg_json)?;
        let cfg = bridge::parse_cfg(cfg_s)?;

        let sink: Arc<dyn CoreEventSink> = Arc::new(FfiSink { cb: on_event, user: user_data });
        let core = Core::init(cfg, sink);

        let h = Box::new(cb_handle { core });
        let handle_ptr = Box::into_raw(h) as usize;

        Ok(ok_json(serde_json::json!({ "handle": handle_ptr })))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("INIT_FAILED", &format!("{e:#}"))),
    }
}

fn get_handle(handle_ptr: usize) -> anyhow::Result<&'static mut cb_handle> {
    if handle_ptr == 0 { anyhow::bail!("null handle"); }
    let p = handle_ptr as *mut cb_handle;
    if p.is_null() { anyhow::bail!("null handle"); }
    Ok(unsafe { &mut *p })
}

fn parse_handle_from_json(json: &str) -> anyhow::Result<usize> {
    #[derive(serde::Deserialize)]
    struct H { handle: usize }
    let v: serde_json::Value = serde_json::from_str(json)?;
    let h: H = serde_json::from_value(v["data"].clone())?;
    Ok(h.handle)
}

#[no_mangle]
pub extern "C" fn cb_shutdown(h: *mut cb_handle) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let boxed = unsafe { Box::from_raw(h) };
        boxed.core.shutdown();
        Ok(ok_json(serde_json::json!({})))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("SHUTDOWN_FAILED", &format!("{e:#}"))),
    }
}

#[no_mangle]
pub extern "C" fn cb_plan_local_ingest(h: *mut cb_handle, snapshot_json: *const c_char, force: i32) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let snap_s = cstr_to_str(snapshot_json)?;
        let snap = bridge::parse_snapshot(snap_s)?;

        let hh = unsafe { &mut *h };
        let r = hh.core.plan_local_ingest_result(&snap, force != 0)?;

        Ok(ok_json(serde_json::json!({
            "meta": r.meta,
            "needs_user_confirm": r.needs_user_confirm,
            "strategy": r.strategy
        })))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("PLAN_FAILED", &format!("{e:#}"))),
    }
}

#[no_mangle]
pub extern "C" fn cb_ingest_local_copy_with_force(h: *mut cb_handle, snapshot_json: *const c_char, force: i32) -> *const c_char {
    let run = (|| -> anyhow::Result<String> {
        if h.is_null() { anyhow::bail!("null handle"); }
        let snap_s = cstr_to_str(snapshot_json)?;
        let snap = bridge::parse_snapshot(snap_s)?;

        let hh = unsafe { &mut *h };
        let meta = hh.core.ingest_local_copy_with_force(snap, force != 0)?;
        Ok(ok_json(serde_json::json!({ "meta": meta })))
    })();

    match run {
        Ok(s) => ret(s),
        Err(e) => ret(err_json("INGEST_FAILED", &format!("{e:#}"))),
    }
}
