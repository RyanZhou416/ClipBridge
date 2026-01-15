#pragma once
#include <stdint.h>

#ifdef _WIN32
  #define CB_API __declspec(dllexport)
#else
  #define CB_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

// 事件回调：json 是临时指针，仅在回调期间有效；壳侧必须立刻拷贝
typedef void (*cb_on_event_fn)(const char* json, void* user_data);

// 不透明句柄
typedef struct cb_handle cb_handle;

// 统一 JSON envelope：{"ok":true,"data":...} / {"ok":false,"error":{"code":...,"message":...}} :contentReference[oaicite:2]{index=2}
CB_API const char* cb_init(const char* cfg_json, cb_on_event_fn on_event, void* user_data);
CB_API const char* cb_shutdown(cb_handle* h);


// share_mode 在 snapshot_json 中表达
CB_API const char* cb_plan_local_ingest(cb_handle* h, const char* snapshot_json);
CB_API const char* cb_ingest_local_copy(cb_handle* h, const char* snapshot_json);


// 释放由 core-ffi 返回的字符串
CB_API void cb_free_string(const char* s);

// 返回 {"ok":true, "data":[{"device_id":"...", "is_online":true}, ...]}
CB_API const char* cb_list_peers(cb_handle* h);

// 返回 {"ok":true, "data":{"status":"Running", ...}}
CB_API const char* cb_get_status(cb_handle* h);

// 设置单个设备的共享策略
// policy_json: {"peer_id": "device_uuid", "share_to_peer": true, "accept_from_peer": false}
// 返回 {"ok": true}
CB_API const char* cb_set_peer_policy(cb_handle* h, const char* policy_json);

// 清除设备指纹（用于重新配对，解决 TLS_PIN_MISMATCH 问题）
// peer_id_json: {"peer_id": "device_uuid"}
// 返回 {"ok": true}
CB_API const char* cb_clear_peer_fingerprint(cb_handle* h, const char* peer_id_json);

// 清除本地证书（用于重新生成证书，需要重新配对所有设备）
// 无需参数
// 返回 {"ok": true}
CB_API const char* cb_clear_local_cert(cb_handle* h);

// M3: 确保内容缓存 (Lazy Fetch)
// req_json: { "item_id": "...", "file_id": "opt", "prefer_peer": "opt" }
CB_API const char* cb_ensure_content_cached(cb_handle* h, const char* req_json);

// M3: 取消传输
// transfer_id_json: "uuid-string"
CB_API const char* cb_cancel_transfer(cb_handle* h, const char* transfer_id_json);

// 历史查询
CB_API const char* cb_list_history(cb_handle* h, const char* query_json);

// 查询单条元数据
CB_API const char* cb_get_item_meta(cb_handle* h, const char* item_id_json);

// 日志系统
CB_API int cb_logs_write(cb_handle* h, int level, const char* component, const char* category, const char* message_en, const char* message_zh_cn, const char* exception, const char* props_json, long long ts_utc, long long* out_id);
CB_API int cb_logs_query_latest(cb_handle* h, int level_min, const char* like, int limit, const char* lang, const char** out_json);
CB_API int cb_logs_query_after_id(cb_handle* h, long long after_id, int level_min, const char* like, int limit, const char* lang, const char** out_json);
CB_API int cb_logs_query_before_id(cb_handle* h, long long before_id, int level_min, const char* like, int limit, const char* lang, const char** out_json);
CB_API int cb_logs_query_range(cb_handle* h, long long start_ms, long long end_ms, int level_min, const char* like, int limit, int offset, const char* lang, const char** out_json);
CB_API int cb_logs_stats(cb_handle* h, const char** out_json);
CB_API int cb_logs_delete_before(cb_handle* h, long long cutoff_ms, long long* out_deleted);

// 数据库清空
CB_API const char* cb_clear_core_db(cb_handle* h);
CB_API const char* cb_clear_logs_db(cb_handle* h);
CB_API const char* cb_clear_stats_db(cb_handle* h);
CB_API const char* cb_clear_cache(cb_handle* h);

// 统计查询
CB_API const char* cb_query_cache_stats(cb_handle* h, const char* query_json);
CB_API const char* cb_query_net_stats(cb_handle* h, const char* query_json);
CB_API const char* cb_query_activity_stats(cb_handle* h, const char* query_json);

#ifdef __cplusplus
}
#endif
