#include "pch.h"
#include "App.xaml.h"
#include "MainWindow.xaml.h"


#if __has_include("App.g.cpp")
#include "App.g.cpp"
#endif


#include <winrt/Microsoft.UI.Xaml.h>
#include <winrt/Microsoft.UI.Dispatching.h>
#include <winrt/Microsoft.UI.Windowing.h>
#include <winrt/Microsoft.UI.Xaml.Controls.h>
#include <winrt/Windows.Graphics.h>

#include <windows.h>
#include <shlobj_core.h>
#include <string>

#include "CoreHost.h"

using namespace winrt;
using namespace Microsoft::UI::Xaml;

namespace
{
    // Keep a weak reference to the main Window (no header changes needed)
    winrt::weak_ref<winrt::Microsoft::UI::Xaml::Window> g_mainWindowWeak{ nullptr };

    // ------ small utilities (ASCII only) ------
    std::wstring JoinPath(std::wstring a, std::wstring const& b)
    {
        if (!a.empty() && a.back() != L'\\' && a.back() != L'/') a.push_back(L'\\');
        a.append(b);
        return a;
    }

    bool EnsureDirExists(std::wstring path)
    {
        if (path.empty()) return false;
        for (auto& ch : path) if (ch == L'/') ch = L'\\';

        size_t pos = 0;
        if (path.rfind(L"\\\\", 0) == 0) {
            pos = path.find(L'\\', 2);
            if (pos == std::wstring::npos) return false;
            pos = path.find(L'\\', pos + 1);
            if (pos == std::wstring::npos) return false;
        } else if (path.size() >= 2 && path[1] == L':') {
            pos = 2;
        }

        while (true) {
            pos = path.find(L'\\', pos + 1);
            std::wstring sub = (pos == std::wstring::npos) ? path : path.substr(0, pos);
            if (!sub.empty()) {
                if (!::CreateDirectoryW(sub.c_str(), nullptr)) {
                    DWORD err = ::GetLastError();
                    if (err != ERROR_ALREADY_EXISTS) return false;
                }
            }
            if (pos == std::wstring::npos) break;
        }
        return true;
    }

    std::wstring GetLocalAppData()
    {
        PWSTR raw = nullptr;
        std::wstring out;
        if (SUCCEEDED(::SHGetKnownFolderPath(FOLDERID_LocalAppData, KF_FLAG_DEFAULT, nullptr, &raw))) {
            out = raw;
            ::CoTaskMemFree(raw);
        }
        return out;
    }

    std::wstring GetDeviceName()
    {
        wchar_t buf[256]{};
        DWORD n = static_cast<DWORD>(std::size(buf));
        if (::GetComputerNameExW(ComputerNameDnsHostname, buf, &n)) {
            return std::wstring(buf, buf + n);
        }
        n = static_cast<DWORD>(std::size(buf));
        if (::GetComputerNameW(buf, &n)) {
            return std::wstring(buf, buf + n);
        }
        return L"Windows-PC";
    }

    // Append a log line into TextBlock named "LogBox" on the UI thread
    void AppendLogUI(std::wstring const& line)
    {
        if (auto w = g_mainWindowWeak.get()) {
            auto dq = w.DispatcherQueue();
            dq.TryEnqueue([w, line]() {
                if (auto fe = w.Content().try_as<FrameworkElement>()) {
                    if (auto tb = fe.FindName(L"LogBox").try_as<Microsoft::UI::Xaml::Controls::TextBlock>()) {
                        auto old = tb.Text();
                        tb.Text(old.empty() ? winrt::hstring{ line } : old + L"\n" + winrt::hstring{ line });
                    }
                }
            });
        }
    }
} // anonymous namespace

namespace winrt::ClipBridgeShell::implementation
{
    App::App()
    {
        // NOTE: Do NOT call InitializeComponent() here while XAML codegen is unstable.
        // Window and Core init still work without it for now.
    }

    void App::OnLaunched(LaunchActivatedEventArgs const&)
    {
        // 1) Create and show main window
        auto window = make<MainWindow>();
        g_mainWindowWeak = window;
        window.Activate();

        // Set default size
        window.AppWindow().Resize(winrt::Windows::Graphics::SizeInt32{ 900, 600 });

        // 2) Build Core config and ensure directories
        CoreHost::Config cfg{};
        cfg.device_name = GetDeviceName();

        auto base = JoinPath(GetLocalAppData(), L"ClipBridge");
        cfg.data_dir  = JoinPath(base, L"data");
        cfg.cache_dir = JoinPath(base, L"cache");
        cfg.log_dir   = JoinPath(base, L"logs");

        EnsureDirExists(cfg.data_dir);
        EnsureDirExists(cfg.cache_dir);
        EnsureDirExists(cfg.log_dir);

        cfg.cache_limit_bytes = 1024ull * 1024 * 1024; // 1 GiB
        cfg.history_limit     = 2000;
        cfg.mdns_enabled      = true;
        cfg.mdns_port         = 0;
        cfg.quic_port         = 0;
        cfg.trust_known_devices_only = false;
        cfg.require_encryption       = false;

        // 3) Init Core
        if (!CoreHost::Instance().Init(cfg)) {
            auto err = CoreHost::Instance().LastError();
            std::wstring msg = L"[Core] init failed, code=" + std::to_wstring(err.code);
            if (!err.message.empty()) {
                msg += L", msg=" + err.message;
            }
            AppendLogUI(msg);
        } else {
            AppendLogUI(L"[Core] initialized");
        }

        // 4) Subscribe Core events (log to UI)
        CoreHost::Instance().AddDeviceOnline(
            [&](std::string_view json_dev) {
                AppendLogUI(L"Device online: " + clipbridge::Utf8ToWide(json_dev));
            });
        CoreHost::Instance().AddDeviceOffline(
            [&](std::string_view dev_id) {
                AppendLogUI(L"Device offline: " + clipbridge::Utf8ToWide(dev_id));
            });
        CoreHost::Instance().AddNewMetadata(
            [&](std::string_view json_meta) {
                AppendLogUI(L"New meta: " + clipbridge::Utf8ToWide(json_meta));
            });
        CoreHost::Instance().AddTransferProgress(
            [&](std::string_view item_id, uint64_t done, uint64_t total) {
                std::wstring s = L"Transfer " + clipbridge::Utf8ToWide(item_id)
                               + L": " + std::to_wstring(done) + L"/" + std::to_wstring(total);
                AppendLogUI(s);
            });
        CoreHost::Instance().AddError(
            [&](int code, std::string_view msg) {
                std::wstring s = L"[Core error " + std::to_wstring(code) + L"] "
                               + clipbridge::Utf8ToWide(msg);
                AppendLogUI(s);
            });

        // 5) Shutdown Core when window closes
        window.Closed([](auto&&, auto&&) {
            CoreHost::Instance().Shutdown();
        });

        // UI ready message
        AppendLogUI(L"[UI] MainWindow ready");
    }
}
