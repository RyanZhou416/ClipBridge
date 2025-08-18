// platforms/windows/core-ffi/src/models_api.rs
use cb_core::models::Envelope;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

#[no_mangle]
pub extern "C" fn cb_make_text_envelope(device_id: *const c_char, text: *const c_char) -> *mut c_char {
    unsafe {
        let dev = CStr::from_ptr(device_id).to_string_lossy().to_string();
        let txt = CStr::from_ptr(text).to_string_lossy().to_string();
        let env = Envelope::make_text(dev, txt);
        let json = env.to_json_bytes();
        CString::new(json).unwrap().into_raw()
    }
}

#[no_mangle]
pub extern "C" fn cb_free_string(p: *mut c_char) {
    if !p.is_null() {
        unsafe { drop(CString::from_raw(p)); }
    }
}
