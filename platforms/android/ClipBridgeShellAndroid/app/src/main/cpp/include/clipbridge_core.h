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

#ifdef __cplusplus
}
#endif
