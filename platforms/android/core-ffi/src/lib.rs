use std::ffi::c_char;

/// 最小 JNI/NDK 侧可用的 C ABI 导出（后续你可以改成 JNI 命名）
/// 先只返回一个 C 风格的静态字符串，确保能编过。
#[no_mangle]
pub extern "C" fn cb_core_ping() -> *const c_char {
    b"cb core ffi (android) ok\0".as_ptr() as *const c_char
}
