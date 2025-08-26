// cb_ffi.h — ClipBridge Core FFI (mixed style)
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// error codes
#define CB_OK                 0
#define CB_ERR_INVALID_ARG    1
#define CB_ERR_INIT_FAILED    2
#define CB_ERR_STORAGE        3
#define CB_ERR_NETWORK        4
#define CB_ERR_NOT_FOUND      5
#define CB_ERR_PAUSED         6
#define CB_ERR_INTERNAL       7

// config struct (MUST match Rust repr(C) layout)
typedef struct {
  const char* device_name;     // UTF-8
  const char* data_dir;        // UTF-8 path
  const char* cache_dir;       // UTF-8 path
  const char* log_dir;         // optional

  uint64_t    max_cache_bytes;
  uint32_t    max_cache_items;
  uint32_t    max_history_items;
  int32_t     item_ttl_secs;   // -1 = none

  int         enable_mdns;     // 0/1
  const char* service_name;    // optional
  uint16_t    port;            // 0 = auto
  int         prefer_quic;     // 0/1

  const char* key_alias;       // optional
  int         trusted_only;    // 0/1
  int         require_encryption; // 0/1

  const char* reserved1;
  uint64_t    reserved2;
} cb_config;

// callbacks
typedef struct {
  void (*device_online)(const char* json_device);
  void (*device_offline)(const char* device_id);
  void (*new_metadata)(const char* json_meta);
  void (*transfer_progress)(const char* item_id, unsigned long long done, unsigned long long total);
  void (*on_error)(int code, const char* message);
} cb_callbacks;

// API
int  cb_init(const cb_config* cfg, const cb_callbacks* cbs);
void cb_shutdown(void);

const char* cb_get_version_string(void); // must free with cb_free
uint32_t    cb_get_protocol_version(void);

int cb_ingest_local_copy(const char* json_snapshot, char** out_item_id);
int cb_ingest_remote_metadata(const char* json_meta);
int cb_ensure_content_cached(const char* item_id, const char* prefer_mime_or_null, char** out_json_localref);
int cb_list_history(uint32_t limit, uint32_t offset, char** out_json_array);
int cb_get_item(const char* item_id, char** out_json_record);
int cb_pause(int yes);
int cb_prune_cache(void);
int cb_prune_history(void);

// free strings returned by the library
void cb_free(void* p);

#ifdef __cplusplus
}
#endif
