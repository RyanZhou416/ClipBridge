#pragma once

#include <windows.h>
#include <functional>
#include <string>
#include <string_view>
#include <optional>

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.ApplicationModel.DataTransfer.h>
#include <winrt/Microsoft.UI.Dispatching.h>

namespace ClipBridgeShell
{
    class ClipboardWatcher
    {
    public:
        // Core-interactive ctor: report new item_id via onItemId; logs via onLog
        using OnItemId = std::function<void(std::string)>;                 // UTF-8 item_id
        using OnLog    = std::function<void(const wchar_t*)>;              // UI-friendly log

        ClipboardWatcher(OnItemId onItemId, OnLog onLog);
        ~ClipboardWatcher();

        void Start();
        void Stop();

    private:
        // WinRT subscription token and state
        winrt::event_token m_token{};
        bool m_running{false};

        // Callbacks
        OnItemId m_onItemId;
        OnLog    m_onLog;

        // Core FFI dynamic binding (loaded on demand)
        HMODULE m_core{nullptr};
        using fn_cb_ingest_local_copy = int(__cdecl*)(
            const char* snapshot_json,           // UTF-8
            const uint8_t* blob, size_t blob_len,// optional raw payload (here we pass text bytes)
            char* out_item_id, size_t out_cap    // UTF-8 item_id buffer
        );
        fn_cb_ingest_local_copy p_cb_ingest_local_copy{nullptr};

        // Event handler
        void OnClipboardChanged(winrt::Windows::Foundation::IInspectable const&,
                                winrt::Windows::Foundation::IInspectable const&);

        // Helpers
        bool  TryLoadCore(); // load core_ffi_windows.dll and resolve symbol
        static std::string  Utf8(std::wstring const& w);
        static std::string  EscapeJson(std::string_view s);
        static std::string  BuildTextSnapshotJson(std::string const& text_utf8);
    };
}
