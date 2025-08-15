use std::os::raw::c_char;

/// 返回一个进程内静态的 UTF-8 字符串指针
pub fn ping_ptr() -> *const c_char {
    b"clipbridge-core ok\0".as_ptr() as *const c_char
}
