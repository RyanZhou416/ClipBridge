use super::super::*;
use std::ffi::{CStr, CString};
use std::{fs, thread};
use std::os::raw::{c_char, c_void};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn test_target_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(p);
    }
    // core-ffi crate 在 platforms/windows/core-ffi，workspace target 默认在仓库根目录/target
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target")
}

fn unique_test_root(sub: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    // 获取当前线程 ID，确保并发测试绝对不会重名
    let thread_id = format!("{:?}", thread::current().id())
        .replace("ThreadId(", "")
        .replace(")", "");

    test_target_dir()
        .join("debug")
        .join("clipbridge_tests")
        .join(sub)
        // 路径格式：run_时间戳_线程ID
        .join(format!("run_{}_{}", nanos, thread_id))
}

fn cleanup_dir(p: &PathBuf) {
    if std::env::var_os("CB_TEST_KEEP").is_some() {
        // 需要保留现场排查：CB_TEST_KEEP=1 cargo test ...
        return;
    }
    // SQLite WAL / 文件句柄偶尔会延迟释放：重试几次更稳
    for _ in 0..10 {
        if !p.exists() {
            return;
        }
        if fs::remove_dir_all(p).is_ok() {
            return;
        }
        sleep(Duration::from_millis(20));
    }
}

extern "C" fn on_event(_json: *const c_char, _user: *mut c_void) {
    // 测试里先不处理事件
}

unsafe fn take_json(p: *const c_char) -> String {
    assert!(!p.is_null());
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    cb_free_string(p);
    s
}

fn now_ms() -> i64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    d.as_millis() as i64
}

fn mk_cfg_json() -> (CString, PathBuf) {
    let root = unique_test_root("ffi");
    let data_dir = root.join("data");
    let cache_dir = root.join("cache");

    let cfg = format!(
        r#"{{
          "device_id":"dev-1",
          "device_name":"dev1",
          "account_uid":"acct-1",
          "account_tag":"acctTag",
          "data_dir":"{}",
          "cache_dir":"{}",
          "gc_history_max_items":"1000000",
          "gc_cas_max_bytes":"1099511627776"
        }}"#,
        data_dir.display(),
        cache_dir.display()
    );

    (CString::new(cfg).unwrap(), root)
}

unsafe fn init_handle() -> (*mut cb_handle, PathBuf) {
    let (cfg, root) = mk_cfg_json();
    let out = cb_init(cfg.as_ptr(), on_event, std::ptr::null_mut());
    let json = take_json(out);

    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["ok"].as_bool().unwrap());
    let handle = v["data"]["handle"].as_u64().unwrap() as usize;
    (handle as *mut cb_handle, root)
}

#[test]
fn ffi_plan_text_ok() {
    unsafe {
        let (h, root) = init_handle();
        let ts = now_ms();
        let snap = CString::new(format!(
            r#"{{
              "type":"ClipboardSnapshot",
              "ts_ms":{ts},
              "kind":"text",
              "share_mode":"default",
              "text":{{"mime":"text/plain","utf8":"hello"}}
            }}"#
        ))
            .unwrap();

        let out = cb_plan_local_ingest(h, snap.as_ptr());
        let json = take_json(out);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["ok"].as_bool().unwrap());
        assert_eq!(
            v["data"]["plan"]["needs_user_confirm"].as_bool().unwrap(),
            false
        );

        let _ = take_json(cb_shutdown(h));
        cleanup_dir(&root);
    }
}

#[test]
fn ffi_plan_text_over_soft_needs_confirm_then_force() {
    unsafe {
        let (h, root) = init_handle();
        let ts = now_ms();
        // soft_text_bytes 默认 1MB：构造一个稍微更大的文本
        let big = "a".repeat(1 * 1024 * 1024 + 10);

        let snap1 = CString::new(format!(
            r#"{{
              "type":"ClipboardSnapshot",
              "ts_ms":{ts},
              "kind":"text",
              "share_mode":"default",
              "text":{{"utf8":{}}}
            }}"#,
            serde_json::to_string(&big).unwrap()
        ))
            .unwrap();

        let v1: serde_json::Value =
            serde_json::from_str(&take_json(cb_plan_local_ingest(h, snap1.as_ptr()))).unwrap();
        assert!(v1["ok"].as_bool().unwrap());
        assert_eq!(
            v1["data"]["plan"]["needs_user_confirm"].as_bool().unwrap(),
            true
        );

        let snap2 = CString::new(format!(
            r#"{{
              "type":"ClipboardSnapshot",
              "ts_ms":{ts},
              "kind":"text",
              "share_mode":"force",
              "text":{{"utf8":{}}}
            }}"#,
            serde_json::to_string(&big).unwrap()
        ))
            .unwrap();

        let v2: serde_json::Value =
            serde_json::from_str(&take_json(cb_plan_local_ingest(h, snap2.as_ptr()))).unwrap();
        assert!(v2["ok"].as_bool().unwrap());
        assert_eq!(
            v2["data"]["plan"]["needs_user_confirm"].as_bool().unwrap(),
            false
        );

        let _ = take_json(cb_shutdown(h));
        cleanup_dir(&root);
    }
}

#[test]
fn ffi_plan_file_list_ok() {
    unsafe {
        let (h, root) = init_handle();
        let ts = now_ms();
        let snap = CString::new(format!(
            r#"{{
              "type":"ClipboardSnapshot",
              "ts_ms":{ts},
              "kind":"file_list",
              "share_mode":"default",
              "files":[
                {{"rel_name":"a.txt","size_bytes":100,"sha256":null}},
                {{"rel_name":"b.bin","size_bytes":200,"sha256":null}}
              ]
            }}"#
        ))
            .unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&take_json(cb_plan_local_ingest(h, snap.as_ptr()))).unwrap();
        assert!(v["ok"].as_bool().unwrap());
        assert_eq!(v["data"]["plan"]["meta"]["kind"].as_str().unwrap(), "file_list");
        assert_eq!(v["data"]["plan"]["meta"]["size_bytes"].as_i64().unwrap(), 300);
        assert_eq!(
            v["data"]["plan"]["meta"]["preview"]["file_count"].as_u64().unwrap(),
            2
        );

        let _ = take_json(cb_shutdown(h));
        cleanup_dir(&root);
    }
}

#[test]
fn ffi_ingest_text_smoke() {
    unsafe {
        let (h, root) = init_handle();
        let ts = now_ms();
        let snap = CString::new(format!(
            r#"{{
              "type":"ClipboardSnapshot",
              "ts_ms":{ts},
              "kind":"text",
              "share_mode":"default",
              "text":{{"mime":"text/plain","utf8":"hello"}}
            }}"#
        ))
            .unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&take_json(cb_ingest_local_copy(h, snap.as_ptr()))).unwrap();
        assert!(v["ok"].as_bool().unwrap());
        assert_eq!(v["data"]["meta"]["kind"].as_str().unwrap(), "text");

        let _ = take_json(cb_shutdown(h));
        cleanup_dir(&root);
    }
}
