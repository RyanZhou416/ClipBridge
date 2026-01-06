#include <jni.h>
#include <string>
#include <mutex>
#include <android/log.h>

#include "clipbridge_core.h"

#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, "CB.JNI", __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "CB.JNI", __VA_ARGS__)

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
        listener_local = g_listener;
        mid = g_onCoreEvent;
    }

    if (listener_local && mid) {
        jstring jjson = env->NewStringUTF(copied.c_str());
        env->CallVoidMethod(listener_local, mid, jjson);
        env->DeleteLocalRef(jjson);

        if (env->ExceptionCheck()) {
            env->ExceptionClear();
        }
    }

    if (need_detach) {
        g_vm->DetachCurrentThread();
    }
}

// helper：把 core-ffi 返回的 const char* 转成 jstring，并 cb_free_string
static jstring take_core_string(JNIEnv* env, const char* s) {
    if (!s) return env->NewStringUTF("");
    jstring out = env->NewStringUTF(s);
    cb_free_string(s);
    return out;
}

// helper：jstring -> std::string
static std::string jstr(JNIEnv* env, jstring js) {
    if (!js) return {};
    const char* utf = env->GetStringUTFChars(js, nullptr);
    std::string out = utf ? utf : "";
    env->ReleaseStringUTFChars(js, utf);
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
                return env->NewStringUTF("{\"ok\":false,\"error\":{\"code\":\"JNI_LISTENER\",\"message\":\"listener missing onCoreEvent(String)\"}}");
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
