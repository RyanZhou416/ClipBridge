#include <jni.h>
#include <string>
#include <mutex>
#include <cstdint>
#include <cstring>
#include <android/log.h>

#include "clipbridge_core.h"

#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, "CB.JNI", __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "CB.JNI", __VA_ARGS__)

// 壳侧期望的 FFI ABI 主版本：必须与 Rust 导出 cb_get_ffi_version 返回的 major 一致。
// 依据：crates/cb_core_ffi/src/exports.rs 返回 1，macOS 壳同样期望 1。
static constexpr uint32_t kExpectedFfiAbiMajor = 1;

// -----------------------------
// UTF-8 <-> UTF-16 转换
// JNI 的 GetStringUTFChars / NewStringUTF 使用「modified UTF-8」：非 BMP 字符（emoji 等）
// 会被拆成代理对各自的 3 字节序列，而 Rust 侧按标准 UTF-8 解析，导致乱码/解析失败。
// 因此输入统一用 GetStringChars 取 UTF-16 再转标准 UTF-8，输出统一用 NewString（UTF-16）。
// -----------------------------

// UTF-8 -> UTF-16；无效字节用 U+FFFD 替换（Rust 返回的 JSON 本身是合法 UTF-8）
static std::u16string utf8_to_utf16(const char* data, size_t len) {
    std::u16string out;
    out.reserve(len);

    size_t i = 0;
    while (i < len) {
        const unsigned char c = static_cast<unsigned char>(data[i]);
        uint32_t cp = 0;
        size_t extra = 0;

        if (c < 0x80) {
            cp = c;
        } else if ((c & 0xE0) == 0xC0) {
            cp = c & 0x1F;
            extra = 1;
        } else if ((c & 0xF0) == 0xE0) {
            cp = c & 0x0F;
            extra = 2;
        } else if ((c & 0xF8) == 0xF0) {
            cp = c & 0x07;
            extra = 3;
        } else {
            out.push_back(0xFFFD);
            ++i;
            continue;
        }

        bool valid = i + extra < len;
        for (size_t k = 1; valid && k <= extra; ++k) {
            const unsigned char cc = static_cast<unsigned char>(data[i + k]);
            if ((cc & 0xC0) != 0x80) {
                valid = false;
                break;
            }
            cp = (cp << 6) | (cc & 0x3F);
        }
        if (!valid) {
            out.push_back(0xFFFD);
            ++i;
            continue;
        }
        i += extra + 1;

        if (cp >= 0xD800 && cp <= 0xDFFF) {          // 孤立的代理码点不合法
            out.push_back(0xFFFD);
        } else if (cp <= 0xFFFF) {
            out.push_back(static_cast<char16_t>(cp));
        } else if (cp <= 0x10FFFF) {
            cp -= 0x10000;
            out.push_back(static_cast<char16_t>(0xD800 + (cp >> 10)));
            out.push_back(static_cast<char16_t>(0xDC00 + (cp & 0x3FF)));
        } else {
            out.push_back(0xFFFD);
        }
    }
    return out;
}

// UTF-16 -> UTF-8；把代理对合成为一个标准 UTF-8 码点
static std::string utf16_to_utf8(const jchar* data, jsize len) {
    std::string out;
    out.reserve(static_cast<size_t>(len));

    for (jsize i = 0; i < len; ++i) {
        uint32_t cp = data[i];
        if (cp >= 0xD800 && cp <= 0xDBFF && i + 1 < len
            && data[i + 1] >= 0xDC00 && data[i + 1] <= 0xDFFF) {
            cp = 0x10000 + ((cp - 0xD800) << 10) + (data[i + 1] - 0xDC00);
            ++i;
        } else if (cp >= 0xD800 && cp <= 0xDFFF) {
            cp = 0xFFFD;
        }

        if (cp < 0x80) {
            out.push_back(static_cast<char>(cp));
        } else if (cp < 0x800) {
            out.push_back(static_cast<char>(0xC0 | (cp >> 6)));
            out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        } else if (cp < 0x10000) {
            out.push_back(static_cast<char>(0xE0 | (cp >> 12)));
            out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        } else {
            out.push_back(static_cast<char>(0xF0 | (cp >> 18)));
            out.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        }
    }
    return out;
}

// 标准 UTF-8 -> Java String（内部走 UTF-16，绕开 NewStringUTF 的 modified UTF-8）
static jstring make_jstring(JNIEnv* env, const char* data, size_t len) {
    const std::u16string utf16 = utf8_to_utf16(data, len);
    return env->NewString(reinterpret_cast<const jchar*>(utf16.data()),
                          static_cast<jsize>(utf16.size()));
}

// -----------------------------
// 全局：JavaVM + Listener 引用
// -----------------------------
static JavaVM* g_vm = nullptr;
static std::mutex g_mu;

// 你 Java 侧的监听接口对象（GlobalRef）
static jobject g_listener = nullptr;
// 监听接口的方法 ID：void onCoreEvent(String json)
static jmethodID g_onCoreEvent = nullptr;

static void clear_listener_locked(JNIEnv* env) {
    if (g_listener) {
        env->DeleteGlobalRef(g_listener);
        g_listener = nullptr;
    }
    g_onCoreEvent = nullptr;
}

// 事件回调：json 只在回调期间有效，所以这里立刻拷贝到 std::string
static void on_event_cb(const char* json, void* /*user_data*/) {
    if (!json) return;

    std::string copied(json);

    // 拿 JNIEnv
    JNIEnv* env = nullptr;
    bool need_detach = false;

    if (g_vm->GetEnv(reinterpret_cast<void**>(&env), JNI_VERSION_1_6) != JNI_OK) {
        if (g_vm->AttachCurrentThread(&env, nullptr) != JNI_OK) {
            return;
        }
        need_detach = true;
    }

    jobject listener_local = nullptr;
    jmethodID mid = nullptr;

    {
        std::lock_guard<std::mutex> lk(g_mu);
        // 在锁内取 NewLocalRef：nativeShutdown 可能并发 DeleteGlobalRef(g_listener)，
        // 只拷贝裸指针会在解锁后 use-after-free；本地引用保证本次回调期间对象存活。
        if (g_listener) {
            listener_local = env->NewLocalRef(g_listener);
        }
        mid = g_onCoreEvent;
    }

    if (listener_local && mid) {
        jstring jjson = make_jstring(env, copied.data(), copied.size());
        if (jjson) {
            env->CallVoidMethod(listener_local, mid, jjson);
            env->DeleteLocalRef(jjson);

            if (env->ExceptionCheck()) {
                env->ExceptionClear();
            }
        }
        env->DeleteLocalRef(listener_local);
    }

    if (need_detach) {
        g_vm->DetachCurrentThread();
    }
}

// helper：把 core-ffi 返回的 const char* 转成 jstring，并 cb_free_string
static jstring take_core_string(JNIEnv* env, const char* s) {
    if (!s) return make_jstring(env, "", 0);
    jstring out = make_jstring(env, s, std::strlen(s));
    cb_free_string(s);
    return out;
}

// helper：jstring -> 标准 UTF-8 std::string
static std::string jstr(JNIEnv* env, jstring js) {
    if (!js) return {};
    const jchar* chars = env->GetStringChars(js, nullptr);
    if (!chars) return {};
    const jsize len = env->GetStringLength(js);
    std::string out = utf16_to_utf8(chars, len);
    env->ReleaseStringChars(js, chars);
    return out;
}

// -----------------------------
// JNI 生命周期
// -----------------------------
JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void* /*reserved*/) {
    g_vm = vm;
    return JNI_VERSION_1_6;
}

// -----------------------------
// 你 Java 包名/类名：请按实际改
// 这里假设：com.ryan416.clipbridgeshellandroid.core.CoreInterop
// -----------------------------
extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeInit(
        JNIEnv* env, jclass /*cls*/, jstring cfg_json, jobject listener) {

    // FFI ABI 探针：cb_get_ffi_version 不需要 handle，先校验再 init。
    // 主版本不匹配直接返回错误 envelope（Java 侧 init() 会保存 code/message 并拒绝进入 Ready）。
    {
        uint32_t ffi_major = 0;
        uint32_t ffi_minor = 0;
        cb_get_ffi_version(&ffi_major, &ffi_minor);
        if (ffi_major != kExpectedFfiAbiMajor) {
            LOGE("FFI ABI mismatch: core reports %u.%u, shell requires major %u",
                 ffi_major, ffi_minor, kExpectedFfiAbiMajor);
            std::string err =
                "{\"ok\":false,\"error\":{\"code\":\"FFI_ABI_MISMATCH\",\"message\":\"FFI ABI mismatch: core reports "
                + std::to_string(ffi_major) + "." + std::to_string(ffi_minor)
                + ", shell requires major " + std::to_string(kExpectedFfiAbiMajor) + "\"}}";
            return make_jstring(env, err.data(), err.size());
        }
        LOGI("FFI ABI: %u.%u", ffi_major, ffi_minor);
    }

    std::string cfg = jstr(env, cfg_json);

    // 注册 listener（GlobalRef）
    {
        std::lock_guard<std::mutex> lk(g_mu);
        clear_listener_locked(env);

        if (listener) {
            jclass itf = env->GetObjectClass(listener);
            // 方法签名：void onCoreEvent(String)
            g_onCoreEvent = env->GetMethodID(itf, "onCoreEvent", "(Ljava/lang/String;)V");
            env->DeleteLocalRef(itf);

            if (!g_onCoreEvent) {
                // listener 没实现该方法，直接返回错误 envelope（由 Java 处理）
                static const char kListenerMissing[] =
                    "{\"ok\":false,\"error\":{\"code\":\"JNI_LISTENER\",\"message\":\"listener missing onCoreEvent(String)\"}}";
                return make_jstring(env, kListenerMissing, sizeof(kListenerMissing) - 1);
            }
            g_listener = env->NewGlobalRef(listener);
        }
    }

    const char* res = cb_init(cfg.c_str(), on_event_cb, nullptr);
    return take_core_string(env, res);
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeShutdown(
        JNIEnv* env, jclass /*cls*/, jlong handle_ptr) {

    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    const char* res = cb_shutdown(h);

    // shutdown 后清理 listener
    {
        std::lock_guard<std::mutex> lk(g_mu);
        clear_listener_locked(env);
    }

    return take_core_string(env, res);
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativePlanLocalIngest(
        JNIEnv* env, jclass, jlong handle_ptr, jstring snapshot_json) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    std::string snap = jstr(env, snapshot_json);
    return take_core_string(env, cb_plan_local_ingest(h, snap.c_str()));
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeIngestLocalCopy(
        JNIEnv* env, jclass, jlong handle_ptr, jstring snapshot_json) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    std::string snap = jstr(env, snapshot_json);
    return take_core_string(env, cb_ingest_local_copy(h, snap.c_str()));
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeListPeers(
        JNIEnv* env, jclass, jlong handle_ptr) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    return take_core_string(env, cb_list_peers(h));
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeGetStatus(
        JNIEnv* env, jclass, jlong handle_ptr) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    return take_core_string(env, cb_get_status(h));
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeEnsureContentCached(
        JNIEnv* env, jclass, jlong handle_ptr, jstring req_json) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    std::string req = jstr(env, req_json);
    return take_core_string(env, cb_ensure_content_cached(h, req.c_str()));
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeCancelTransfer(
        JNIEnv* env, jclass, jlong handle_ptr, jstring transfer_id_json) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    std::string tid = jstr(env, transfer_id_json);
    return take_core_string(env, cb_cancel_transfer(h, tid.c_str()));
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeListHistory(
        JNIEnv* env, jclass, jlong handle_ptr, jstring query_json) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    std::string q = jstr(env, query_json);
    return take_core_string(env, cb_list_history(h, q.c_str()));
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_ryan416_clipbridgeshellandroid_core_CoreInterop_nativeGetItemMeta(
        JNIEnv* env, jclass, jlong handle_ptr, jstring item_id_json) {
    auto* h = reinterpret_cast<cb_handle*>(static_cast<intptr_t>(handle_ptr));
    std::string idj = jstr(env, item_id_json);
    return take_core_string(env, cb_get_item_meta(h, idj.c_str()));
}
