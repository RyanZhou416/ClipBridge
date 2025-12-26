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
	cb_free_string(p); // 确保 lib.rs 中导出了这个函数
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

	// [修改点] 适配 AppConfig 结构，将配置项嵌套入 app_config
	// 同时将数字改为 JSON Number 类型而非 String
	let cfg = format!(
		r#"{{
          "device_id":"dev-1",
          "device_name":"dev1",
          "account_uid":"acct-1",
          "account_tag":"acctTag",
          "data_dir":"{}",
          "cache_dir":"{}",
          "app_config": {{
              "gc_history_max_items": 1000000,
              "gc_cas_max_bytes": 1099511627776
          }}
        }}"#,
		data_dir.display().to_string().replace('\\', "\\\\"), // Windows 路径转义
		cache_dir.display().to_string().replace('\\', "\\\\")
	);

	(CString::new(cfg).unwrap(), root)
}

unsafe fn init_handle() -> (*mut cb_handle, PathBuf) {
	let (cfg, root) = mk_cfg_json();
	let out = cb_init(cfg.as_ptr(), on_event, std::ptr::null_mut());
	let json = take_json(out);

	let v: serde_json::Value = serde_json::from_str(&json).unwrap();

	// [修改点] 增加错误详情打印
	if !v["ok"].as_bool().unwrap() {
		panic!("cb_init failed: {}", v["error"]);
	}

	// 注意：init_handle 返回的 JSON 结构取决于 lib.rs 实现
	// 假设是 {"ok": true, "data": { "handle": 123 }} 或者 {"ok": true, "data": 123}
	// 这里兼容一下代码：你的 lib.rs 目前好像是直接返回 data: id
	// 如果之前的测试代码是 v["data"]["handle"]，说明 lib.rs 返回了对象
	// 如果现在的 lib.rs 返回的是 {"data": id}，则需要改为 v["data"].as_u64()
	// 下面保留你原始代码的路径，如果报错请根据 lib.rs 调整
	let handle = if let Some(h) = v["data"]["handle"].as_u64() {
		h as usize
	} else if let Some(h) = v["data"].as_u64() {
		h as usize
	} else {
		panic!("Unknown handle format: {:?}", v);
	};

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

		if !v["ok"].as_bool().unwrap() {
			panic!("plan failed: {}", v["error"]);
		}

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
		// soft_text_bytes 默认 1MB：构造一个稍微更大的文本 (1MB + 10 bytes)
		let big = "a".repeat(1 * 1024 * 1024 + 10);

		// 1. share_mode: default -> 应该触发 needs_user_confirm
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

		let out1 = cb_plan_local_ingest(h, snap1.as_ptr());
		let v1: serde_json::Value = serde_json::from_str(&take_json(out1)).unwrap();

		if !v1["ok"].as_bool().unwrap() {
			panic!("plan1 failed: {}", v1["error"]);
		}

		assert_eq!(
			v1["data"]["plan"]["needs_user_confirm"].as_bool().unwrap(),
			true,
			"Should require confirmation for >1MB text"
		);

		// 2. share_mode: force -> 应该跳过确认
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

		let out2 = cb_plan_local_ingest(h, snap2.as_ptr());
		let v2: serde_json::Value = serde_json::from_str(&take_json(out2)).unwrap();

		if !v2["ok"].as_bool().unwrap() {
			panic!("plan2 failed: {}", v2["error"]);
		}

		assert_eq!(
			v2["data"]["plan"]["needs_user_confirm"].as_bool().unwrap(),
			false,
			"Should NOT require confirmation when forced"
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

		let out = cb_plan_local_ingest(h, snap.as_ptr());
		let v: serde_json::Value = serde_json::from_str(&take_json(out)).unwrap();

		if !v["ok"].as_bool().unwrap() {
			panic!("plan file list failed: {}", v["error"]);
		}

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

		let out = cb_ingest_local_copy(h, snap.as_ptr());
		let v: serde_json::Value = serde_json::from_str(&take_json(out)).unwrap();

		if !v["ok"].as_bool().unwrap() {
			panic!("ingest failed: {}", v["error"]);
		}

		assert_eq!(v["data"]["meta"]["kind"].as_str().unwrap(), "text");

		let _ = take_json(cb_shutdown(h));
		cleanup_dir(&root);
	}
}

#[test]
fn ffi_list_history_and_get_item() {
	unsafe {
		let (h, root) = init_handle();
		let ts = now_ms();

		// 1. 先插入一条数据
		let snap = CString::new(format!(
			r#"{{
              "type":"ClipboardSnapshot",
              "ts_ms":{ts},
              "kind":"text",
              "share_mode":"default",
              "text":{{"utf8":"history_test_item"}}
            }}"#
		)).unwrap();

		let out_ingest = cb_ingest_local_copy(h, snap.as_ptr());
		let v_ingest: serde_json::Value = serde_json::from_str(&take_json(out_ingest)).unwrap();
		assert!(v_ingest["ok"].as_bool().unwrap());
		let item_id = v_ingest["data"]["meta"]["item_id"].as_str().unwrap().to_string();

		// 2. 测试 list_history
		// 构造查询参数：limit=10
		let query = CString::new(r#"{"limit": 10}"#).unwrap();
		let out_list = cb_list_history(h, query.as_ptr());
		let v_list: serde_json::Value = serde_json::from_str(&take_json(out_list)).unwrap();

		assert!(v_list["ok"].as_bool().unwrap(), "list_history failed");
		let items = v_list["data"].as_array().unwrap();
		assert!(!items.is_empty(), "history should not be empty");
		assert_eq!(items[0]["item_id"], item_id, "first item should match");

		// 3. 测试 get_item_meta
		// 构造查询参数：直接传 item_id 的 JSON 字符串
		let id_json = CString::new(serde_json::to_string(&item_id).unwrap()).unwrap();
		let out_get = cb_get_item_meta(h, id_json.as_ptr());
		let v_get: serde_json::Value = serde_json::from_str(&take_json(out_get)).unwrap();

		assert!(v_get["ok"].as_bool().unwrap(), "get_item_meta failed");
		assert_eq!(v_get["data"]["item_id"], item_id);
		assert_eq!(v_get["data"]["preview"]["text"], "history_test_item");

		// 4. 清理
		let _ = take_json(cb_shutdown(h));
		cleanup_dir(&root);
	}
}
