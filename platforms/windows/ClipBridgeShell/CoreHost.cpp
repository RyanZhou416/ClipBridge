#include "pch.h"
#include "CoreHost.h"
#include <cstring>

using namespace winrt;

namespace clipbridge {

std::string WideToUtf8(std::wstring_view w) {
    if (w.empty()) return {};
    int len = ::WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), nullptr, 0, nullptr, nullptr);
    std::string out; out.resize(len > 0 ? len : 0);
    if (len > 0) ::WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), out.data(), len, nullptr, nullptr);
    return out;
}
std::wstring Utf8ToWide(const char* s) {
    if (!s || !*s) return {};
    int len = ::MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
    std::wstring out; out.resize(len ? (len - 1) : 0);
    if (len) ::MultiByteToWideChar(CP_UTF8, 0, s, -1, out.data(), len);
    return out;
}
std::wstring Utf8ToWide(std::string_view s) { return Utf8ToWide(std::string(s).c_str()); }

} // namespace clipbridge

CoreHost* CoreHost::s_self = nullptr;

CoreHost& CoreHost::Instance() {
    static CoreHost g;
    return g;
}

clipbridge::CoreResult CoreHost::LastError() const {
    std::lock_guard<std::mutex> _g(m_mu);
    return m_lastErr;
}
void CoreHost::SetLastError(int code, std::wstring msg) {
    std::lock_guard<std::mutex> _g(m_mu);
    m_lastErr.code = code;
    m_lastErr.message = std::move(msg);
}

template <typename T>
static bool LoadSym(HMODULE mod, const char* name, T& fn) {
    fn = reinterpret_cast<T>(::GetProcAddress(mod, name));
    return fn != nullptr;
}

bool CoreHost::LoadDll(std::wstring const& path_or_name) {
    if (m_mod) return true;
    m_mod = ::LoadLibraryW(path_or_name.c_str());
    if (!m_mod) { SetLastError(CB_ERR_INIT_FAILED, L"无法加载 core_ffi_windows.dll"); return false; }

    bool ok = true;
    ok &= LoadSym(m_mod, "cb_init",                   p_cb_init);
    ok &= LoadSym(m_mod, "cb_shutdown",               p_cb_shutdown);
    ok &= LoadSym(m_mod, "cb_ingest_local_copy",      p_cb_ingest_local_copy);
    ok &= LoadSym(m_mod, "cb_ingest_remote_metadata", p_cb_ingest_remote_metadata);
    ok &= LoadSym(m_mod, "cb_ensure_content_cached",  p_cb_ensure_content_cached);
    ok &= LoadSym(m_mod, "cb_list_history",           p_cb_list_history);
    ok &= LoadSym(m_mod, "cb_get_item",               p_cb_get_item);
    ok &= LoadSym(m_mod, "cb_pause",                  p_cb_pause);
    ok &= LoadSym(m_mod, "cb_prune_cache",            p_cb_prune_cache);
    ok &= LoadSym(m_mod, "cb_prune_history",          p_cb_prune_history);

    if (!ok) { SetLastError(CB_ERR_INIT_FAILED, L"core FFI 符号缺失或版本不匹配"); UnloadDll(); return false; }
    return true;
}
void CoreHost::UnloadDll() {
    if (m_mod) { ::FreeLibrary(m_mod); m_mod = nullptr; }
    p_cb_init = nullptr; p_cb_shutdown = nullptr; p_cb_ingest_local_copy = nullptr;
    p_cb_ingest_remote_metadata = nullptr; p_cb_ensure_content_cached = nullptr;
    p_cb_list_history = nullptr; p_cb_get_item = nullptr; p_cb_pause = nullptr;
    p_cb_prune_cache = nullptr; p_cb_prune_history = nullptr;
}

bool CoreHost::Init(Config const& cfg) {
    std::lock_guard<std::mutex> _g(m_mu);
    if (!LoadDll(cfg.dll_name)) return false;

    cb_config c{};
    auto dev = clipbridge::WideToUtf8(cfg.device_name);
    auto dd  = clipbridge::WideToUtf8(cfg.data_dir);
    auto cd  = clipbridge::WideToUtf8(cfg.cache_dir);
    auto ld  = clipbridge::WideToUtf8(cfg.log_dir);

    c.device_name = dev.c_str();
    c.data_dir    = dd.c_str();
    c.cache_dir   = cd.c_str();
    c.log_dir     = ld.c_str();

    c.max_cache_bytes   = cfg.cache_limit_bytes;
    c.max_cache_items   = 0;
    c.max_history_items = cfg.history_limit;
    c.item_ttl_secs     = -1;

    c.enable_mdns  = cfg.mdns_enabled ? 1 : 0;
    c.service_name = "_clipbridge._tcp";
    c.port         = cfg.mdns_port ? cfg.mdns_port : 0;
    c.prefer_quic  = 1;

    c.key_alias          = nullptr;
    c.trusted_only       = cfg.trust_known_devices_only ? 1 : 0;
    c.require_encryption = cfg.require_encryption ? 1 : 0;

    m_callbacks = {};
    m_callbacks.device_online     = &CoreHost::OnDeviceOnline_C;
    m_callbacks.device_offline    = &CoreHost::OnDeviceOffline_C;
    m_callbacks.new_metadata      = &CoreHost::OnNewMetadata_C;
    m_callbacks.transfer_progress = &CoreHost::OnTransferProgress_C;
    m_callbacks.on_error          = &CoreHost::OnError_C;

    s_self = this;
    int rc = p_cb_init(&c, &m_callbacks);
    if (rc != CB_OK) { s_self = nullptr; SetLastError(rc, L"cb_init 失败"); return false; }

    m_cfg = cfg;
    m_lastErr = {};
    return true;
}

void CoreHost::Shutdown() {
    std::lock_guard<std::mutex> _g(m_mu);
    if (p_cb_shutdown) p_cb_shutdown();
    s_self = nullptr;
    m_cfg.reset();
    UnloadDll();
}

std::string CoreHost::IngestLocalCopy(std::string_view snapshot_json) {
    if (!p_cb_ingest_local_copy) { SetLastError(CB_ERR_INIT_FAILED, L"core 未初始化"); return {}; }
    clipbridge::OwnedStr out{};
    int rc = p_cb_ingest_local_copy(snapshot_json.data(), &out.p);
    if (rc != CB_OK) { SetLastError(rc, L"ingest_local_copy 失败"); return {}; }
    return out.c_str();
}

std::string CoreHost::EnsureContentCached(std::string_view item_id, std::string_view prefer_mime) {
    if (!p_cb_ensure_content_cached) { SetLastError(CB_ERR_INIT_FAILED, L"core 未初始化"); return {}; }
    clipbridge::OwnedStr out{};
    const char* pref = prefer_mime.empty() ? nullptr : prefer_mime.data();
    int rc = p_cb_ensure_content_cached(item_id.data(), pref, &out.p);
    if (rc != CB_OK) { SetLastError(rc, L"ensure_content_cached 失败"); return {}; }
    return out.c_str();
}

std::string CoreHost::ListHistory(uint32_t limit, uint32_t offset) {
    if (!p_cb_list_history) { SetLastError(CB_ERR_INIT_FAILED, L"core 未初始化"); return {}; }
    clipbridge::OwnedStr out{};
    int rc = p_cb_list_history(limit, offset, &out.p);
    if (rc != CB_OK) { SetLastError(rc, L"list_history 失败"); return {}; }
    return out.c_str();
}

std::string CoreHost::GetItem(std::string_view item_id) {
    if (!p_cb_get_item) { SetLastError(CB_ERR_INIT_FAILED, L"core 未初始化"); return {}; }
    clipbridge::OwnedStr out{};
    int rc = p_cb_get_item(item_id.data(), &out.p);
    if (rc != CB_OK) { SetLastError(rc, L"get_item 失败"); return {}; }
    return out.c_str();
}

bool CoreHost::Pause(bool yes) {
    if (!p_cb_pause) { SetLastError(CB_ERR_INIT_FAILED, L"core 未初始化"); return false; }
    int rc = p_cb_pause(yes ? 1 : 0);
    if (rc != CB_OK) { SetLastError(rc, L"pause 失败"); return false; }
    return true;
}
bool CoreHost::PruneCache() {
    if (!p_cb_prune_cache) { SetLastError(CB_ERR_INIT_FAILED, L"core 未初始化"); return false; }
    int rc = p_cb_prune_cache();
    if (rc != CB_OK) { SetLastError(rc, L"prune_cache 失败"); return false; }
    return true;
}
bool CoreHost::PruneHistory() {
    if (!p_cb_prune_history) { SetLastError(CB_ERR_INIT_FAILED, L"core 未初始化"); return false; }
    int rc = p_cb_prune_history();
    if (rc != CB_OK) { SetLastError(rc, L"prune_history 失败"); return false; }
    return true;
}

static std::string_view sv_or_empty(const char* s){ return (s && *s) ? std::string_view{s} : std::string_view{}; }

void __cdecl CoreHost::OnDeviceOnline_C(const char* json_device) {
    if (!s_self) return; s_self->m_onDeviceOnline(sv_or_empty(json_device));
}
void __cdecl CoreHost::OnDeviceOffline_C(const char* device_id) {
    if (!s_self) return; s_self->m_onDeviceOffline(sv_or_empty(device_id));
}
void __cdecl CoreHost::OnNewMetadata_C(const char* json_meta) {
    if (!s_self) return; s_self->m_onNewMetadata(sv_or_empty(json_meta));
}
void __cdecl CoreHost::OnTransferProgress_C(const char* item_id, uint64_t done, uint64_t total) {
    if (!s_self) return; s_self->m_onTransferProgress(sv_or_empty(item_id), done, total);
}
void __cdecl CoreHost::OnError_C(int code, const char* message) {
    if (!s_self) return; s_self->m_onError(code, sv_or_empty(message));
}
