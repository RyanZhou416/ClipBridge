//! 全部 `#[no_mangle] pub extern "C" fn cb_*` 定义，**只写一次**。
//!
//! 三个平台 cdylib（Windows / macOS / Android）都通过
//! `pub use cb_core_ffi::exports::*;` 复用这份完整表面，因此不存在
//! feature flag 或平台子集：33 个函数在三个平台上完全一致。
//!
//! 行为参考实现是 Windows 旧版 `lib.rs`：JSON envelope、
//! `{"ok":true,"data":{"handle":N}}`、`CORE_BUSY` / `CORE_SHUTDOWN`、
//! 幂等 `cb_shutdown`（不 `Box::from_raw`）、每个入口 `catch_unwind`、
//! `cb_logs_*` 的 `i32` + 出参约定，以及 `InitConfigDto.disable_network`
//! → `CoreConfig.disable_network`，全部逐字保留。

use anyhow::Context;
use cb_core::api::{Core, CoreEventSink};
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

pub use crate::handle::cb_handle;

use crate::envelope::{ffi_out_i64, ffi_out_json, ffi_result, ffi_status, ok_json};
use crate::handle::{enter, shutdown, FfiSink};
use crate::marshal::{
	cstr_to_str, parse_cfg, parse_item_id, parse_snapshot, ActivityStatsQueryDto,
	CacheStatsQueryDto, ClearFingerprintDto, EnsureContentDto, HistoryQueryDto, NetStatsQueryDto,
	PeerPolicyDto, ShareMode,
};

/// 释放由本库分配的 C 字符串；空指针安全，调用后指针不得再次使用。
#[no_mangle]
pub extern "C" fn cb_free_string(s: *const c_char) {
	// 释放本身不应 panic；万一 panic（例如指针异常触发的 drop），也不能让它越过 C ABI。
	let _ = panic::catch_unwind(AssertUnwindSafe(|| {
		if s.is_null() {
			return;
		}
		unsafe {
			drop(CString::from_raw(s as *mut c_char));
		}
	}));
}

/// 初始化核心并返回句柄。
///
/// 成功：`{"ok":true,"data":{"handle":<usize>}}`；
/// 失败：JSON 错误 envelope（默认错误码 `INIT_FAILED`）。
#[no_mangle]
pub extern "C" fn cb_init(
	cfg_json: *const c_char,
	on_event: crate::handle::OnEventFn,
	user_data: *mut c_void,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let cfg_s = cstr_to_str(cfg_json)?;
		let cfg = parse_cfg(cfg_s)?;

		let sink: Arc<dyn CoreEventSink> = Arc::new(FfiSink {
			cb: on_event,
			user: user_data,
		});
		let core = Core::init(cfg, sink);

		let h = Box::new(cb_handle::new(core));
		let handle_ptr = Box::into_raw(h) as usize;

		Ok(ok_json(serde_json::json!({ "handle": handle_ptr })))
	}));

	ffi_result("INIT_FAILED", run)
}

/// 关闭句柄：幂等，重复调用返回成功 envelope。
///
/// 句柄盒故意泄漏（有界泄漏），换取向安全：shutdown 之后所有入口返回
/// `CORE_SHUTDOWN`，不会 use-after-free / double-free。
#[no_mangle]
pub extern "C" fn cb_shutdown(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		shutdown(h)?;
		Ok(ok_json(serde_json::json!({})))
	}));

	ffi_result("SHUTDOWN_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_plan_local_ingest(
	h: *mut cb_handle,
	snapshot_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let snap_s = cstr_to_str(snapshot_json)?;
		let (snap, share_mode) = parse_snapshot(snap_s)?;

		let force = matches!(share_mode, ShareMode::Force);

		let hh = unsafe { &mut *h };
		let r = hh.core_mut()?.plan_local_ingest_result(&snap, force)?;
		Ok(ok_json(serde_json::json!({
				"plan": {
				"meta": r.meta,
				"needs_user_confirm": r.needs_user_confirm,
				"strategy": r.strategy
			}
		})))
	}));

	ffi_result("PLAN_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_ingest_local_copy(
	h: *mut cb_handle,
	snapshot_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let snap_s = cstr_to_str(snapshot_json)?;
		let (snap, share_mode) = parse_snapshot(snap_s)?;

		let force = matches!(share_mode, ShareMode::Force);

		let hh = unsafe { &mut *h };
		let meta = hh.core_mut()?.ingest_local_copy_with_force(snap, force)?;
		Ok(ok_json(serde_json::json!({ "meta": meta })))
	}));

	ffi_result("INGEST_FAILED", run)
}

/// 获取当前在线设备列表。
///
/// 返回格式：`{"ok":true,"data":[{"device_id":"...","is_online":true,...}]}`
#[no_mangle]
pub extern "C" fn cb_list_peers(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		let peers = hh.core_mut()?.list_peers()?;

		Ok(ok_json(serde_json::json!(peers)))
	}));

	ffi_result("LIST_PEERS_FAILED", run)
}

/// 获取核心状态。
///
/// 返回格式：`{"ok":true,"data":{"status":"Running","device_id":"...",...}}`
#[no_mangle]
pub extern "C" fn cb_get_status(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		let status = hh.core_mut()?.get_status()?;

		Ok(ok_json(status))
	}));

	ffi_result("GET_STATUS_FAILED", run)
}

/// 设置单个设备的共享策略。
///
/// 入参格式：`{"peer_id":"device_uuid","share_to_peer":true,"accept_from_peer":false}`
#[no_mangle]
pub extern "C" fn cb_set_peer_policy(
	h: *mut cb_handle,
	policy_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let policy_str = cstr_to_str(policy_json)?;

		let req: PeerPolicyDto = serde_json::from_str(policy_str)?;
		hh.core_mut()?
			.set_peer_policy(&req.peer_id, req.share_to_peer, req.accept_from_peer)?;

		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("SET_PEER_POLICY_FAILED", run)
}

/// 清除设备指纹（用于重新配对，解决 TLS_PIN_MISMATCH 问题）。
///
/// 入参格式：`{"peer_id":"device_uuid"}`
#[no_mangle]
pub extern "C" fn cb_clear_peer_fingerprint(
	h: *mut cb_handle,
	peer_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let json_str = cstr_to_str(peer_id_json)?;

		let req: ClearFingerprintDto = serde_json::from_str(json_str)?;
		hh.core_mut()?.clear_peer_fingerprint(&req.peer_id)?;

		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_PEER_FINGERPRINT_FAILED", run)
}

/// 清除本地证书（用于重新生成证书，需要重新配对所有设备）。
#[no_mangle]
pub extern "C" fn cb_clear_local_cert(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		hh.core_mut()?.clear_local_cert()?;
		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_LOCAL_CERT_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_ensure_content_cached(
	h: *mut cb_handle,
	req_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		let json_str = cstr_to_str(req_json)?;
		let dto: EnsureContentDto = serde_json::from_str(json_str).context("invalid json")?;

		let transfer_id = hh
			.core_mut()?
			.ensure_content_cached(&dto.item_id, dto.file_id.as_deref())?;

		Ok(ok_json(serde_json::json!({ "transfer_id": transfer_id })))
	}));
	ffi_result("START_FETCH_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_cancel_transfer(
	h: *mut cb_handle,
	transfer_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		let json_str = cstr_to_str(transfer_id_json)?;
		// 假设传入的是一个单纯的字符串 JSON，如 "uuid"
		let tid: String = serde_json::from_str(json_str).context("invalid json string")?;

		hh.core_mut()?.cancel_transfer(&tid);
		Ok(ok_json(serde_json::json!({})))
	}));
	ffi_result("CANCEL_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_list_history(h: *mut cb_handle, query_json: *const c_char) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		// 1. 解析入参
		let json_str = cstr_to_str(query_json)?;
		let dto: HistoryQueryDto = serde_json::from_str(json_str).context("invalid query json")?;

		// 2. 调用 Core 获取列表
		let items = hh.core_mut()?.list_history(dto.limit, dto.cursor)?;

		// 3. 构造分页结构：若返回数量等于 limit，可能还有下一页，取最后一条时间戳做 cursor
		let next_cursor = if items.len() >= dto.limit {
			items.last().map(|i| i.created_ts_ms)
		} else {
			None
		};

		// 构造符合 C# HistoryPage 定义的 JSON 对象
		let page_data = serde_json::json!({
			"items": items,
			"next_cursor": next_cursor
		});

		// 4. 包装返回
		Ok(ok_json(page_data))
	}));
	ffi_result("LIST_HISTORY_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_get_item_meta(
	h: *mut cb_handle,
	item_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };

		// 支持直接传字符串，或者包含 item_id 的对象
		let json_str = cstr_to_str(item_id_json)?;
		let item_id = parse_item_id(json_str)?;

		let meta = hh.core_mut()?.get_item_meta(&item_id)?;

		match meta {
			Some(m) => Ok(ok_json(m)),
			None => Err(anyhow::anyhow!("Item not found")),
		}
	}));

	ffi_result("GET_ITEM_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_delete_item_local(
	h: *mut cb_handle,
	item_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let json_str = cstr_to_str(item_id_json)?;
		let item_id = parse_item_id(json_str)?;
		hh.core_mut()?.delete_item_local(&item_id)?;
		Ok(ok_json(true))
	}));
	ffi_result("DELETE_ITEM_FAILED", run)
}

#[no_mangle]
pub extern "C" fn cb_delete_item_global(
	h: *mut cb_handle,
	item_id_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let hh = unsafe { &mut *h };
		let json_str = cstr_to_str(item_id_json)?;
		let item_id = parse_item_id(json_str)?;
		hh.core_mut()?.delete_item_global(&item_id)?;
		Ok(ok_json(true))
	}));
	ffi_result("DELETE_ITEM_FAILED", run)
}

/// 写入日志（多语言版本），`out_id` 返回新日志 id。
#[no_mangle]
pub extern "C" fn cb_logs_write(
	h: *mut cb_handle,
	level: i32,
	component: *const c_char,
	category: *const c_char,
	message_en: *const c_char,
	message_zh_cn: *const c_char,
	exception: *const c_char,
	props_json: *const c_char,
	ts_utc: i64,
	out_id: *mut i64,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<()> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let component_s = cstr_to_str(component)?;
		let category_s = cstr_to_str(category)?;

		// #region agent log
		eprintln!(
			"[cb_logs_write] H_A: Received component={}, category={}, level={}, ts_utc={}",
			component_s, category_s, level, ts_utc
		);
		// #endregion
		let message_en_s = cstr_to_str(message_en)?;
		let message_zh_cn_s = if message_zh_cn.is_null() {
			None
		} else {
			Some(cstr_to_str(message_zh_cn)?)
		};
		let exception_s = if exception.is_null() {
			None
		} else {
			Some(cstr_to_str(exception)?)
		};
		let props_json_s = if props_json.is_null() {
			None
		} else {
			Some(cstr_to_str(props_json)?)
		};

		// 如果 ts_utc <= 0，使用当前时间；否则使用提供的时间戳（用于暂存日志回写）
		let ts_utc = if ts_utc <= 0 {
			cb_core::util::now_ms()
		} else {
			ts_utc
		};

		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let id = log_store.write_multilang(
			ts_utc,
			level,
			component_s,
			category_s,
			message_en_s,
			message_zh_cn_s,
			exception_s,
			props_json_s,
		)?;

		unsafe {
			if !out_id.is_null() {
				*out_id = id;
			}
		}
		Ok(())
	}));

	ffi_status(run)
}

/// 查询增量日志 (after_id)。
#[no_mangle]
pub extern "C" fn cb_logs_query_after_id(
	h: *mut cb_handle,
	after_id: i64,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries = log_store.query_after_id(after_id, level_min, like_s, limit, lang_s)?;

		Ok(ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 查询最新的日志（从新到旧排序，用于初始加载）。
#[no_mangle]
pub extern "C" fn cb_logs_query_latest(
	h: *mut cb_handle,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries = log_store.query_latest(level_min, like_s, limit, lang_s)?;

		Ok(ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 查询 before_id 之前的日志（用于向上滚动加载更早的日志）。
#[no_mangle]
pub extern "C" fn cb_logs_query_before_id(
	h: *mut cb_handle,
	before_id: i64,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries = log_store.query_before_id(before_id, level_min, like_s, limit, lang_s)?;

		Ok(ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 查询时间范围内的日志。
#[no_mangle]
pub extern "C" fn cb_logs_query_range(
	h: *mut cb_handle,
	start_ms: i64,
	end_ms: i64,
	level_min: i32,
	like: *const c_char,
	limit: i32,
	offset: i32,
	lang: *const c_char,
	out_json: *mut *const c_char,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let like_s = if like.is_null() {
			None
		} else {
			Some(cstr_to_str(like)?)
		};
		let lang_s = if lang.is_null() {
			None
		} else {
			Some(cstr_to_str(lang)?)
		};

		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let entries =
			log_store.query_range(start_ms, end_ms, level_min, like_s, limit, offset, lang_s)?;

		Ok(ok_json(entries))
	}));

	ffi_out_json("QUERY_FAILED", "Failed to query logs", out_json, run)
}

/// 获取日志统计。
#[no_mangle]
pub extern "C" fn cb_logs_stats(h: *mut cb_handle, out_json: *mut *const c_char) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let stats = log_store.stats()?;

		Ok(ok_json(stats))
	}));

	ffi_out_json("STATS_FAILED", "Failed to get log stats", out_json, run)
}

/// 删除指定时间之前的日志，`out_deleted` 返回删除条数。
#[no_mangle]
pub extern "C" fn cb_logs_delete_before(
	h: *mut cb_handle,
	cutoff_ms: i64,
	out_deleted: *mut i64,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<i64> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let deleted = log_store.delete_before(cutoff_ms)?;
		Ok(deleted)
	}));

	ffi_out_i64(out_deleted, run)
}

/// 按 ID 列表删除日志，`out_deleted` 返回删除条数。
#[no_mangle]
pub extern "C" fn cb_logs_delete_by_ids(
	h: *mut cb_handle,
	ids_json: *const c_char,
	out_deleted: *mut i64,
) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<i64> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let ids_json_str = cstr_to_str(ids_json)?;
		let ids: Vec<i64> = serde_json::from_str(ids_json_str)?;

		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let deleted = log_store.delete_by_ids(&ids)?;
		Ok(deleted)
	}));

	ffi_out_i64(out_deleted, run)
}

/// 获取来源统计（component 和 category 的列表及计数）。
#[no_mangle]
pub extern "C" fn cb_logs_source_stats(h: *mut cb_handle, out_json: *mut *const c_char) -> i32 {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		let stats = log_store.get_source_stats()?;

		Ok(ok_json(stats))
	}));

	ffi_out_json("STATS_FAILED", "Failed to get source stats", out_json, run)
}

/// 清空核心数据库。
#[no_mangle]
pub extern "C" fn cb_clear_core_db(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut store = handle.core_mut()?.inner.store.lock().unwrap();
		store.clear_core_db()?;
		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 清空日志数据库。
#[no_mangle]
pub extern "C" fn cb_clear_logs_db(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut log_store = handle.core_mut()?.inner.log_store.lock().unwrap();
		log_store.clear_logs_db()?;
		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 清空统计数据库。
#[no_mangle]
pub extern "C" fn cb_clear_stats_db(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let mut stats_store = handle.core_mut()?.inner.stats_store.lock().unwrap();
		stats_store.clear_stats_db()?;
		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 清空缓存（CAS blobs、临时文件等）。
#[no_mangle]
pub extern "C" fn cb_clear_cache(h: *mut cb_handle) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		handle.core_mut()?.clear_cache()?;
		Ok(ok_json(serde_json::json!({ "ok": true })))
	}));

	ffi_result("CLEAR_FAILED", run)
}

/// 查询缓存统计。
#[no_mangle]
pub extern "C" fn cb_query_cache_stats(
	h: *mut cb_handle,
	query_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let query_str = cstr_to_str(query_json)?;

		let query: CacheStatsQueryDto = serde_json::from_str(query_str)?;
		let start_ts = if query.start_ts_ms == 0 && query.end_ts_ms == 0 {
			// 默认窗口：最近60分钟
			cb_core::util::now_ms() - 60 * 60 * 1000
		} else {
			query.start_ts_ms
		};
		let end_ts = if query.end_ts_ms == 0 {
			cb_core::util::now_ms()
		} else {
			query.end_ts_ms
		};

		let core = handle.core_mut()?;
		let stats_store = core.inner.stats_store.lock().unwrap();
		let entries = stats_store.query_cache_stats(start_ts, end_ts, query.bucket_sec)?;

		// 解析 data_json 并构建 series
		let mut series = Vec::new();
		for entry in entries {
			let data: serde_json::Value = serde_json::from_str(&entry.data_json)?;
			series.push(serde_json::json!({
				"ts_ms": entry.ts_ms,
				"cache_bytes": data.get("cache_bytes").and_then(|v| v.as_i64()).unwrap_or(0)
			}));
		}

		// 获取当前缓存大小（从 store）
		let store = core.inner.store.lock().unwrap();
		let current_cache_bytes = store.sum_present_bytes().unwrap_or(0);

		Ok(ok_json(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": query.bucket_sec
			},
			"series": series,
			"current_cache_bytes": current_cache_bytes
		})))
	}));

	ffi_result("QUERY_FAILED", run)
}

/// 查询网络统计。
#[no_mangle]
pub extern "C" fn cb_query_net_stats(
	h: *mut cb_handle,
	query_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let query_str = cstr_to_str(query_json)?;

		let query: NetStatsQueryDto = serde_json::from_str(query_str)?;
		let start_ts = if query.start_ts_ms == 0 && query.end_ts_ms == 0 {
			cb_core::util::now_ms() - 60 * 60 * 1000
		} else {
			query.start_ts_ms
		};
		let end_ts = if query.end_ts_ms == 0 {
			cb_core::util::now_ms()
		} else {
			query.end_ts_ms
		};

		let stats_store = handle.core_mut()?.inner.stats_store.lock().unwrap();
		let entries = stats_store.query_net_stats(start_ts, end_ts, query.bucket_sec)?;

		let mut series = Vec::new();
		for entry in entries {
			let data: serde_json::Value = serde_json::from_str(&entry.data_json)?;
			series.push(serde_json::json!({
				"ts_ms": entry.ts_ms,
				"bytes_sent": data.get("bytes_sent").and_then(|v| v.as_i64()).unwrap_or(0),
				"bytes_recv": data.get("bytes_recv").and_then(|v| v.as_i64()).unwrap_or(0)
			}));
		}

		Ok(ok_json(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": query.bucket_sec
			},
			"series": series
		})))
	}));

	ffi_result("QUERY_FAILED", run)
}

/// 查询活动统计。
#[no_mangle]
pub extern "C" fn cb_query_activity_stats(
	h: *mut cb_handle,
	query_json: *const c_char,
) -> *const c_char {
	let run = panic::catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<String> {
		let _guard = enter(h)?;
		let handle = unsafe { &mut *h };
		let query_str = cstr_to_str(query_json)?;

		let query: ActivityStatsQueryDto = serde_json::from_str(query_str)?;
		let start_ts = if query.start_ts_ms == 0 && query.end_ts_ms == 0 {
			// 默认窗口：最近24小时
			cb_core::util::now_ms() - 24 * 60 * 60 * 1000
		} else {
			query.start_ts_ms
		};
		let end_ts = if query.end_ts_ms == 0 {
			cb_core::util::now_ms()
		} else {
			query.end_ts_ms
		};

		let stats_store = handle.core_mut()?.inner.stats_store.lock().unwrap();
		let entries = stats_store.query_activity_stats(start_ts, end_ts, query.bucket_sec)?;

		let mut series = Vec::new();
		for entry in entries {
			let data: serde_json::Value = serde_json::from_str(&entry.data_json)?;
			series.push(serde_json::json!({
				"ts_ms": entry.ts_ms,
				"text_count": data.get("text_count").and_then(|v| v.as_i64()).unwrap_or(0),
				"image_count": data.get("image_count").and_then(|v| v.as_i64()).unwrap_or(0),
				"files_count": data.get("files_count").and_then(|v| v.as_i64()).unwrap_or(0)
			}));
		}

		Ok(ok_json(serde_json::json!({
			"window": {
				"start_ts_ms": start_ts,
				"end_ts_ms": end_ts,
				"bucket_sec": query.bucket_sec
			},
			"series": series
		})))
	}));

	ffi_result("QUERY_FAILED", run)
}

/// 获取 FFI ABI 版本，用于壳侧兼容性检查。
#[no_mangle]
pub extern "C" fn cb_get_ffi_version(major: *mut u32, minor: *mut u32) {
	let _ = panic::catch_unwind(AssertUnwindSafe(|| {
		unsafe {
			if !major.is_null() {
				*major = 1; // 必须匹配 C# CB_FFI_ABI_MAJOR
			}
			if !minor.is_null() {
				*minor = 0;
			}
		}
	}));
}
