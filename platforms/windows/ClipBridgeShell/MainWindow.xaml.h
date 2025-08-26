#pragma once

#if __has_include("MainWindow.g.h")
  #include "MainWindow.g.h"
#elif __has_include("winrt/ClipBridgeShell.MainWindow.g.h")
  #include "winrt/ClipBridgeShell.MainWindow.g.h"
#elif __has_include("winrt/impl/ClipBridgeShell.MainWindow.g.h")
  #include "winrt/impl/ClipBridgeShell.MainWindow.g.h"
#else
  #include <winrt/Microsoft.UI.Xaml.h>
  #include <winrt/Microsoft.UI.Xaml.Markup.h>
  namespace winrt::ClipBridgeShell::implementation {
    template <typename D, typename... I>
    struct MainWindow_base : ::winrt::Microsoft::UI::Xaml::WindowT<D, I...>
    {
      using window_t = ::winrt::Microsoft::UI::Xaml::WindowT<D, I...>;
      using window_t::window_t;                       // 继承 WindowT 的构造
      using base_type  = MainWindow_base<D, I...>;    // ★ 让 base_type 成为“直接基类”
      using class_type = D;                           // 实现类类型
    };
  }
#endif

#if __has_include("MainWindow.xaml.g.h")
  #include "MainWindow.xaml.g.h"
#else
  #error "MainWindow.xaml.g.h not found. Make sure XAML build ran."
#endif

#include <memory>
#include <string>

namespace ClipBridgeShell { class ClipboardWatcher; }

namespace winrt::ClipBridgeShell::implementation
{
    struct MainWindow : MainWindowT<MainWindow>
    {
        MainWindow();

        int32_t MyProperty();
        void MyProperty(int32_t value);

        void AppendLog(winrt::hstring const& line);

        void OnTestPasteClick(winrt::Windows::Foundation::IInspectable const&,
                              winrt::Microsoft::UI::Xaml::RoutedEventArgs const&);
        void OnPauseClick(winrt::Windows::Foundation::IInspectable const&,
                          winrt::Microsoft::UI::Xaml::RoutedEventArgs const&);
        void OnPruneCacheClick(winrt::Windows::Foundation::IInspectable const&,
                               winrt::Microsoft::UI::Xaml::RoutedEventArgs const&);
        void OnPruneHistoryClick(winrt::Windows::Foundation::IInspectable const&,
                                 winrt::Microsoft::UI::Xaml::RoutedEventArgs const&);

    private:
        std::unique_ptr<::ClipBridgeShell::ClipboardWatcher> m_clip;
        std::string  m_lastItemId;   // UTF-8
        int32_t      m_myProperty{0};

        void SetLastItemId(std::string id_utf8);
        static std::wstring Utf8ToWide(std::string const& s);
        void TryScrollToEnd();
    };
}

namespace winrt::ClipBridgeShell::factory_implementation
{
    struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow>{};
}
