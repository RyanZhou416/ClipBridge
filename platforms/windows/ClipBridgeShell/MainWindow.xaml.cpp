#include "pch.h"
#include "MainWindow.xaml.h"

#if __has_include("MainWindow.g.cpp")
#include "MainWindow.g.cpp"
#endif

#include <string>
#include <sstream>

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.ApplicationModel.DataTransfer.h>
#include <winrt/Microsoft.UI.Xaml.h>
#include <winrt/Microsoft.UI.Xaml.Controls.h>
#include <winrt/Microsoft.UI.Dispatching.h>

#include "ClipboardWatcher.h"

using namespace winrt;
using namespace Microsoft::UI::Xaml;
using namespace Microsoft::UI::Xaml::Controls;

namespace winrt::ClipBridgeShell::implementation
{
    // -------- helpers --------
    std::wstring MainWindow::Utf8ToWide(std::string const& s)
    {
        if (s.empty()) return {};
        int len = ::MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
        std::wstring out; out.resize(len ? len - 1 : 0);
        if (len) ::MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, out.data(), len);
        return out;
    }

    void MainWindow::TryScrollToEnd()
    {
        // no-op placeholder
    }

    // -------- ctor --------
    MainWindow::MainWindow()
    {
        InitializeComponent();
        Title(L"ClipBridge");

        // create core-interactive ClipboardWatcher
        m_clip = std::make_unique<::ClipBridgeShell::ClipboardWatcher>(
            // OnItemId
            [this](std::string id) {
                this->DispatcherQueue().TryEnqueue([this, id = std::move(id)]() {
                    SetLastItemId(id);
                    std::wstring msg = L"[Copy->Core] item_id=" + Utf8ToWide(this->m_lastItemId);
                    AppendLog(hstring{ msg });
                });
            },
            // OnLog
            [this](const wchar_t* line) {
                this->DispatcherQueue().TryEnqueue([this, line]() {
                    AppendLog(hstring{ line ? line : L"" });
                });
            }
        );

        AppendLog(L"[UI] MainWindow ready");
    }

    // -------- property (minimal) --------
    int32_t MainWindow::MyProperty() { return m_myProperty; }
    void MainWindow::MyProperty(int32_t value) { m_myProperty = value; }

    // -------- logging --------
    void MainWindow::AppendLog(hstring const& line)
    {
        if (auto tb = this->Content().try_as<FrameworkElement>()
                        .FindName(L"LogBox").try_as<TextBlock>()) {
            std::wstring oldw = tb.Text().c_str();
            std::wstring lw = line.c_str();
            std::wstring combined = oldw.empty() ? lw : (oldw + L"\n" + lw);
            tb.Text(hstring{ combined });
        }
        TryScrollToEnd();
    }

    // -------- last item id --------
    void MainWindow::SetLastItemId(std::string id_utf8)
    {
        m_lastItemId = std::move(id_utf8);
        if (auto t = this->Content().try_as<FrameworkElement>()
                         .FindName(L"LastIdBox").try_as<TextBlock>()) {
            t.Text(Utf8ToWide(m_lastItemId));
        }
    }

    // -------- UI events --------
    void MainWindow::OnTestPasteClick(IInspectable const&, RoutedEventArgs const&)
    {
        AppendLog(L"[Paste] test clicked (provider not wired yet)");
    }

    void MainWindow::OnPauseClick(IInspectable const&, RoutedEventArgs const&)
    {
        static bool paused = false;
        paused = !paused;
        if (paused) {
            if (m_clip) m_clip->Stop();
            AppendLog(L"[Watcher] paused");
        } else {
            if (m_clip) m_clip->Start();
            AppendLog(L"[Watcher] resumed");
        }
    }

    void MainWindow::OnPruneCacheClick(IInspectable const&, RoutedEventArgs const&)
    {
        AppendLog(L"[Core] prune cache (not implemented in this window)");
    }

    void MainWindow::OnPruneHistoryClick(IInspectable const&, RoutedEventArgs const&)
    {
        AppendLog(L"[Core] prune history (not implemented in this window)");
    }
}
