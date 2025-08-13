use std::ffi::c_char;

/// 一个最小的 C ABI 导出，方便 C++ 侧 DllImport/GetProcAddress 调用进行连通性测试。
#[no_mangle]
pub extern "C" fn cb_core_ping() -> *const c_char {
    // 返回一个以 \0 结尾的静态字符串指针（C 风格）
    b"cb core ffi (windows) ok\0".as_ptr() as *const c_char
}
