#include "pch.h"
#include "MainWindow.xaml.h"
#if __has_include("MainWindow.g.cpp")
#include "MainWindow.g.cpp"
#endif

using namespace winrt;
using namespace Microsoft::UI::Xaml;

namespace winrt::ClipBridgeShell::implementation
{
    MainWindow::MainWindow()
    {
	    InitializeComponent();
	    Title(L"ClipBridge");
    }

    void MainWindow::AppendLog(winrt::hstring const& line)
    {
	    using Microsoft::UI::Xaml::FrameworkElement;
	    using Microsoft::UI::Xaml::Controls::TextBlock;

	    if (!m_logBox)
	    {
		    if (auto fe = this->Content().try_as<FrameworkElement>())
			    m_logBox = fe.FindName(L"LogBox").try_as<TextBlock>();
	    }
	    if (auto tb = m_logBox)
	    {
		    auto old = tb.Text();
		    tb.Text(old.empty() ? line : old + L"\n" + line);
	    }
    }

    void MainWindow::AppendLog(std::wstring const& line)
    {
	    AppendLog(winrt::hstring(line));
    }

    int32_t MainWindow::MyProperty()
    {
	    throw hresult_not_implemented();
    }
    void MainWindow::MyProperty(int32_t)
    {
	    throw hresult_not_implemented();
    }
} // namespace winrt::ClipBridgeShell::implementation
