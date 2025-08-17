// cb_ffi.h — ClipBridge FFI v1
#pragma once
#include <stdint.h>

#ifdef _WIN32
  #define CB_CALL __cdecl
#else
  #define CB_CALL
#endif

#define CB_API_VERSION 1u

// -------------------- 基础类型 --------------------
typedef struct CbStr {
    const char* ptr;   // UTF-8; 可为 NULL
    uint32_t    len;   // 字节长度；ptr 为 NULL 时需为 0
} CbStr;

typedef struct CbBytes {
    const uint8_t* ptr;
    uint32_t       len;
} CbBytes;

typedef struct CbStrList {
    const struct CbStr* items; // 连续数组
    uint32_t            len;
} CbStrList;

// -------------------- 设备 / 配置 --------------------
typedef struct CbDevice {
    CbStr device_id;              // UUID/ULID
    CbStr account_id;             // 可为空（无账号模式）
    CbStr name;                   // 设备显示名
    CbStr pubkey_fingerprint;     // 可选：用于校验
} CbDevice;

typedef struct CbConfig {
    CbStr device_name;            // 本机名称
    int32_t listen_port;          // 0 = 自动
    uint32_t api_version;         // 传 CB_API_VERSION
} CbConfig;

// -------------------- 元数据（精简骨架版） --------------------
typedef struct CbMeta {
    CbStr   item_id;              // 主键
    CbStr   owner_device_id;
    CbStr   owner_account_id;     // 可为空
    CbStrList kinds;              // 例如: ["text","image","file"]
    CbStrList mimes;              // 例如: ["text/plain","image/png"]
    CbStr   preferred_mime;       // 例如: "text/plain"
    uint64_t size_bytes;
    CbStr   sha256;               // 可为空（未知时）
    uint64_t created_at;          // epoch seconds
    uint64_t expires_at;          // 0 = 不设置
    // 预览、文件列表等后续按需扩展
} CbMeta;

// -------------------- 回调 --------------------
typedef void (CB_CALL *CbOnDeviceOnline)(const CbDevice* dev);
typedef void (CB_CALL *CbOnDeviceOffline)(const CbStr* device_id);
typedef void (CB_CALL *CbOnNewMetadata)(const CbMeta* meta);
typedef void (CB_CALL *CbOnTransferProgress)(const CbStr* item_id, uint64_t sent, uint64_t total);
typedef void (CB_CALL *CbOnError)(int code, const CbStr* msg);

typedef struct CbCallbacks {
    CbOnDeviceOnline      on_device_online;     // 可为 NULL
    CbOnDeviceOffline     on_device_offline;
    CbOnNewMetadata       on_new_metadata;
    CbOnTransferProgress  on_transfer_progress;
    CbOnError             on_error;
} CbCallbacks;

// -------------------- 导出函数（C ABI） --------------------
#ifdef __cplusplus
extern "C" {
#endif

// 版本号（便于握手）
uint32_t CB_CALL cb_get_version(void);

// 初始化核心；保存回调；启动发现/网络等（本骨架里先不启网络）
int CB_CALL cb_init(const CbConfig* cfg, const CbCallbacks* cbs);

// 发送“我这儿有新剪贴板元数据”
int CB_CALL cb_send_metadata(const CbMeta* meta);

// 请求正文（懒取）
int CB_CALL cb_request_content(const CbStr* item_id, const CbStr* mime);

// 暂停/恢复（1=暂停，0=恢复）
int CB_CALL cb_pause(int32_t pause);

// 关闭与清理
void CB_CALL cb_shutdown(void);

// 若有从 core 返回的堆内存，统一用它释放（本骨架暂不需要）
void CB_CALL cb_free(void* p);

#ifdef __cplusplus
}
#endif
