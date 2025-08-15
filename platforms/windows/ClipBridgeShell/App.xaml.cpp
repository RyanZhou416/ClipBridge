#include "pch.h"
#include "App.xaml.h"
#include "MainWindow.xaml.h"

#include <Windows.h>
#include <string>

using namespace winrt;
using namespace Microsoft::UI::Xaml;

namespace
{
// UTF-8 -> UTF-16
std::wstring Utf8ToWide(const char* s)
{
	if (!s)
		return L"(null)";
	int			 len = MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
	std::wstring w(len, L'\0');
	MultiByteToWideChar(CP_UTF8, 0, s, -1, w.data(), len);
	if (!w.empty() && w.back() == L'\0')
		w.pop_back();
	return w;
}

using cb_core_ping_t = const char*(__cdecl*)();

void TestRustFFI()
{
	
	wchar_t exePath[MAX_PATH];
	GetModuleFileNameW(nullptr, exePath, MAX_PATH);
	std::wstring dir = exePath;
	size_t		 pos = dir.find_last_of(L"\\/");
	if (pos != std::wstring::npos)
		dir.resize(pos + 1);

	std::wstring full = dir + L"core_ffi_windows.dll";

	
	SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
	AddDllDirectory(dir.c_str());

	HMODULE h = LoadLibraryW(full.c_str());
	if (!h)
	{
		DWORD	ec = GetLastError();
		wchar_t buf[512];
		FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
					   nullptr,
					   ec,
					   0,
					   buf,
					   512,
					   nullptr);
		std::wstring msg =
			L"LoadLibrary failed:\n" + full + L"\n\nError " + std::to_wstring(ec) + L": " + buf;
		MessageBoxW(nullptr, msg.c_str(), L"FFI", MB_OK | MB_ICONERROR);
		return;
	}

	using cb_core_ping_t = const char*(__cdecl*)();
	auto fn				 = reinterpret_cast<cb_core_ping_t>(GetProcAddress(h, "cb_core_ping"));
	if (!fn)
	{
		DWORD	ec = GetLastError();
		wchar_t buf[512];
		FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
					   nullptr,
					   ec,
					   0,
					   buf,
					   512,
					   nullptr);
		std::wstring msg =
			L"GetProcAddress failed: cb_core_ping\nError " + std::to_wstring(ec) + L": " + buf;
		MessageBoxW(nullptr, msg.c_str(), L"FFI", MB_OK | MB_ICONERROR);
		return;
	}

	const char*	 utf8 = fn();
	std::wstring w	  = Utf8ToWide(utf8);
	MessageBoxW(nullptr, w.c_str(), L"Rust FFI OK", MB_OK | MB_ICONINFORMATION);
}
} // namespace

namespace winrt::ClipBridgeShell::implementation
{
App::App()
{
#if defined _DEBUG && !defined DISABLE_XAML_GENERATED_BREAK_ON_UNHANDLED_EXCEPTION
	UnhandledException(
		[](IInspectable const&, UnhandledExceptionEventArgs const& e)
		{
			if (IsDebuggerPresent())
			{
				auto errorMessage = e.Message();
				__debugbreak();
			}
		});
#endif
}

void App::OnLaunched([[maybe_unused]] LaunchActivatedEventArgs const& e)
{
	window = make<MainWindow>();
	window.Activate();

	TestRustFFI(); // expect a message box "clipbridge-core ok"
}
} // namespace winrt::ClipBridgeShell::implementation
