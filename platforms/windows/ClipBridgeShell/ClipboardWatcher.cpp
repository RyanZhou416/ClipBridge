#include "pch.h"
#include "ClipboardWatcher.h"

using namespace winrt;
using namespace Windows::ApplicationModel::DataTransfer;

namespace ClipBridgeShell
{
    ClipboardWatcher::ClipboardWatcher(OnItemId onItemId, OnLog onLog)
        : m_onItemId(std::move(onItemId)), m_onLog(std::move(onLog))
    {
        TryLoadCore(); // best-effort: ok if it fails (we will degrade)
    }

    ClipboardWatcher::~ClipboardWatcher()
    {
        Stop();
        if (m_core) { ::FreeLibrary(m_core); m_core = nullptr; }
    }

    void ClipboardWatcher::Start()
    {
        if (m_running) return;
        m_token = Clipboard::ContentChanged({ this, &ClipboardWatcher::OnClipboardChanged });
        m_running = true;
        if (m_onLog) m_onLog(L"Clipboard watcher started");
    }

    void ClipboardWatcher::Stop()
    {
        if (!m_running) return;
        Clipboard::ContentChanged(m_token);
        m_running = false;
        if (m_onLog) m_onLog(L"Clipboard watcher stopped");
    }

    bool ClipboardWatcher::TryLoadCore()
    {
        if (p_cb_ingest_local_copy) return true;
        if (!m_core)
        {
            // Search in current dir first; LoadLibraryW will also search PATH.
            m_core = ::LoadLibraryW(L"core_ffi_windows.dll");
        }
        if (!m_core) return false;

        // Resolve symbol (best guess FFI name; ok if absent)
        p_cb_ingest_local_copy = reinterpret_cast<fn_cb_ingest_local_copy>(
            ::GetProcAddress(m_core, "cb_ingest_local_copy"));
        return p_cb_ingest_local_copy != nullptr;
    }

    void ClipboardWatcher::OnClipboardChanged(
        winrt::Windows::Foundation::IInspectable const&,
        winrt::Windows::Foundation::IInspectable const&)
    {
        try
        {
            auto view = Clipboard::GetContent();
            if (!view) return;

            // Only handle text for the minimal viable path
            if (!view.Contains(StandardDataFormats::Text())) return;

            winrt::hstring hs   = view.GetTextAsync().get();
            std::wstring  textW = std::wstring(hs.c_str());
            std::string   textU = Utf8(textW);

            // Build minimal snapshot JSON (metadata-only)
            std::string snapshot = BuildTextSnapshotJson(textU);

            std::string item_id;
            item_id.resize(64); // typical UUID length fits; buffer will be trimmed

            bool sent_to_core = false;
            if (TryLoadCore())
            {
                int rc = p_cb_ingest_local_copy(
                    snapshot.c_str(),
                    reinterpret_cast<const uint8_t*>(textU.data()),
                    textU.size(),
                    item_id.data(), item_id.size());

                if (rc == 0)
                {
                    // trim to C-string end if core wrote shorter id
                    item_id.assign(item_id.c_str());
                    sent_to_core = true;
                }
                else
                {
                    if (m_onLog) m_onLog(L"cb_ingest_local_copy failed");
                }
            }
            else
            {
                if (m_onLog) m_onLog(L"core_ffi_windows.dll not found; local-only mode");
            }

            // Report item_id (from core if available; otherwise synthesize)
            if (!sent_to_core)
            {
                // Fall back: simple hash-like tag for UI continuity
                char buf[64]{};
                _snprintf_s(buf, _TRUNCATE, "local-%u", (unsigned)::GetTickCount());
                item_id = buf;
            }
            if (m_onItemId) m_onItemId(item_id);
        }
        catch (...)
        {
            if (m_onLog) m_onLog(L"Clipboard handler threw an exception");
        }
    }

    // --- Helpers ---

    std::string ClipboardWatcher::Utf8(std::wstring const& w)
    {
        if (w.empty()) return {};
        int len = ::WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), nullptr, 0, nullptr, nullptr);
        std::string out; out.resize(len);
        ::WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), out.data(), len, nullptr, nullptr);
        return out;
    }

    std::string ClipboardWatcher::EscapeJson(std::string_view s)
    {
        std::string o; o.reserve(s.size() + 16);
        for (unsigned char c : s)
        {
            switch (c)
            {
            case '\\': o += "\\\\"; break;
            case '"':  o += "\\\""; break;
            case '\b': o += "\\b";  break;
            case '\f': o += "\\f";  break;
            case '\n': o += "\\n";  break;
            case '\r': o += "\\r";  break;
            case '\t': o += "\\t";  break;
            default:
                if (c < 0x20) { char buf[7]; _snprintf_s(buf, _TRUNCATE, "\\u%04X", (unsigned)c); o += buf; }
                else { o += static_cast<char>(c); }
            }
        }
        return o;
    }

    std::string ClipboardWatcher::BuildTextSnapshotJson(std::string const& text_utf8)
    {
        // Minimal fields for v1: protocol_version, mimes, preview, size
        const char* mime = "text/plain; charset=utf-8";
        std::string preview = text_utf8.substr(0, 64);
        std::string json;
        json += "{";
        json += "\"protocol_version\":1,";
        json += "\"mimes\":[\""; json += mime; json += "\"],";
        json += "\"size\":"; json += std::to_string(text_utf8.size()); json += ",";
        json += "\"preview\":\""; json += EscapeJson(preview); json += "\"";
        json += "}";
        return json;
    }
}
