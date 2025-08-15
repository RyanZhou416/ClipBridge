use std::os::raw::c_char;

// 对外只导出这一个 C 接口
#[no_mangle]
pub extern "C" fn cb_core_ping() -> *const c_char {
    cb_core::ping_ptr() // <-- 调用上面 cb_core 里的函数名
}
