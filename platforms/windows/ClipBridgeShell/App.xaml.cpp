#include "pch.h"
#include "App.xaml.h"
#include "MainWindow.xaml.h"

#include <Windows.h>
#include <string>
#include <cstring>

using namespace winrt;
using namespace Microsoft::UI::Xaml;



// ---------- 与 cb_ffi.h 对齐的最小 C 结构/回调声明（你也可以直接 #include 头文件） ----------
struct CbStr
{
	const char* ptr;
	uint32_t	len;
};
struct CbStrList
{
	const CbStr* items;
	uint32_t	 len;
};
struct CbDevice
{
	CbStr device_id, account_id, name, pubkey_fingerprint;
};
struct CbConfig
{
	CbStr	 device_name;
	int32_t	 listen_port;
	uint32_t api_version;
};
struct CbMeta
{
	CbStr	  item_id, owner_device_id, owner_account_id;
	CbStrList kinds, mimes;
	CbStr	  preferred_mime;
	uint64_t  size_bytes;
	CbStr	  sha256;
	uint64_t  created_at;
	uint64_t  expires_at;
};

// 现在再声明函数指针类型（此时 CbMeta 已经是已知类型了）
using cb_store_metadata_t = int(__cdecl*)(const CbMeta*);
using cb_history_list_t	  = int(__cdecl*)(uint64_t, uint32_t);

// 全局函数指针（只在这里声明一次）
cb_store_metadata_t g_cb_store_metadata = nullptr;
cb_history_list_t	g_cb_history_list	= nullptr;

// 回调类型
using CbOnDeviceOnline	   = void(__cdecl*)(const CbDevice*);
using CbOnDeviceOffline	   = void(__cdecl*)(const CbStr*);
using CbOnNewMetadata	   = void(__cdecl*)(const CbMeta*);
using CbOnTransferProgress = void(__cdecl*)(const CbStr*, uint64_t, uint64_t);
using CbOnError			   = void(__cdecl*)(int, const CbStr*);

struct CbCallbacks
{
	CbOnDeviceOnline	 on_device_online;
	CbOnDeviceOffline	 on_device_offline;
	CbOnNewMetadata		 on_new_metadata;
	CbOnTransferProgress on_transfer_progress;
	CbOnError			 on_error;
};

// 核心导出函数指针
using cb_get_version_t = uint32_t(__cdecl*)();
using cb_init_t		   = int(__cdecl*)(const CbConfig*, const CbCallbacks*);
using cb_shutdown_t	   = void(__cdecl*)();

namespace
{
// UTF-8 -> UTF-16（有长度）
std::wstring Utf8ToWide(const CbStr& s)
{
	if (!s.ptr || s.len == 0)
		return L"";
	int			 wlen = MultiByteToWideChar(CP_UTF8, 0, s.ptr, (int)s.len, nullptr, 0);
	std::wstring w(wlen, L'\0');
	MultiByteToWideChar(CP_UTF8, 0, s.ptr, (int)s.len, w.data(), wlen);
	return w;
}

// ---------- 全局句柄与函数指针 ----------
HMODULE			 g_core			  = nullptr;
cb_get_version_t g_cb_get_version = nullptr;
cb_init_t		 g_cb_init		  = nullptr;
cb_shutdown_t	 g_cb_shutdown	  = nullptr;

// ---------- 回调实现（注意：来自工作线程，实际项目里请切回 UI 线程再更新界面） ----------
void __cdecl OnDeviceOnline(const CbDevice* dev)
{
	auto name = Utf8ToWide(dev->name);
	MessageBoxW(nullptr, (L"Device online: " + name).c_str(), L"CB", MB_OK | MB_ICONINFORMATION);
}
void __cdecl OnNewMetadata(const CbMeta* meta)
{
	auto id = Utf8ToWide(meta->item_id);
	MessageBoxW(nullptr, (L"New meta: " + id).c_str(), L"CB", MB_OK | MB_ICONINFORMATION);
}
void __cdecl OnTransferProgress(const CbStr* id, uint64_t sent, uint64_t total)
{
	// 占位：可改为状态栏/通知气泡
	(void)id;
	(void)sent;
	(void)total;
}
void __cdecl OnError(int code, const CbStr* msg)
{
	auto		 w = Utf8ToWide(*msg);
	std::wstring t = L"Core error " + std::to_wstring(code) + L": " + w;
	MessageBoxW(nullptr, t.c_str(), L"CB", MB_OK | MB_ICONERROR);
}

// ---------- 启动核心并注册回调 ----------
void StartCoreFFI()
{
	// 1) 计算 AppX 运行目录（WinUI 桌面调试常见路径）
	wchar_t exePath[MAX_PATH];
	GetModuleFileNameW(nullptr, exePath, MAX_PATH);
	std::wstring dir = exePath;
	size_t		 pos = dir.find_last_of(L"\\/");
	if (pos != std::wstring::npos)
		dir.resize(pos + 1);

	std::wstring full = dir + L"core_ffi_windows.dll";

	// 2) 限定搜索目录并加载 DLL
	SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
	AddDllDirectory(dir.c_str());
	g_core = LoadLibraryW(full.c_str());
	if (!g_core)
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

	// 3) 解析导出
	g_cb_get_version = (cb_get_version_t)GetProcAddress(g_core, "cb_get_version");
	g_cb_init		 = (cb_init_t)GetProcAddress(g_core, "cb_init");
	g_cb_shutdown	 = (cb_shutdown_t)GetProcAddress(g_core, "cb_shutdown");
	g_cb_store_metadata = (cb_store_metadata_t)GetProcAddress(g_core, "cb_store_metadata");
	g_cb_history_list	= (cb_history_list_t)GetProcAddress(g_core, "cb_history_list");
	if (!g_cb_store_metadata || !g_cb_history_list)
	{
		MessageBoxW(nullptr,
					L"missing exports: cb_store_metadata or cb_history_list",
					L"FFI",
					MB_OK | MB_ICONERROR);
	}
	if (!g_cb_get_version || !g_cb_init || !g_cb_shutdown)
	{
		MessageBoxW(
			nullptr, L"GetProcAddress failed: missing core exports", L"FFI", MB_OK | MB_ICONERROR);
		return;
	}
	

	// 4) 版本握手（可选）
	uint32_t ver = g_cb_get_version();
	if (ver != 1)
	{
		MessageBoxW(nullptr, L"Core API version mismatch", L"FFI", MB_OK | MB_ICONERROR);
		return;
	}

	// 5) 组装回调与配置并初始化
	CbCallbacks cbs{OnDeviceOnline,
					nullptr, // on_device_offline（先不实现）
					OnNewMetadata,
					OnTransferProgress,
					OnError};

	const char* name_utf8 = "Windows-PC";
	CbConfig	cfg{{name_utf8, (uint32_t)std::strlen(name_utf8)}, 0, 1};

	int rc = g_cb_init(&cfg, &cbs);
	if (rc != 0)
	{
		MessageBoxW(nullptr, L"cb_init failed", L"FFI", MB_OK | MB_ICONERROR);
		return;
	}
	// ---- 写入一条最小元数据（测试用） ----
	if (g_cb_store_metadata)
	{
		const char* id	= "item-demo-001";
		const char* dev = "device-A";
		const char* acc = "account-1";
		const char* pm	= "text/plain";

		CbStr	  id_s{id, (uint32_t)strlen(id)};
		CbStr	  dev_s{dev, (uint32_t)strlen(dev)};
		CbStr	  acc_s{acc, (uint32_t)strlen(acc)};
		CbStr	  pm_s{pm, (uint32_t)strlen(pm)};
		CbStr	  sha{nullptr, 0};
		CbStrList empty{nullptr, 0};

		// created_at/ expires_at 随便填；size_bytes 这里写 5
		CbMeta m{
			id_s,		// item_id
			dev_s,		// owner_device_id（先用作来源）
			acc_s,		// owner_account_id
			empty,		// kinds
			empty,		// mimes
			pm_s,		// preferred_mime
			5,			// size_bytes
			sha,		// sha256
			1720000000, // created_at
			0			// expires_at
		};
		g_cb_store_metadata(&m);
	}

	if (g_cb_store_metadata)
	{
		// 组装一条最小元数据（UTF-8）
		const char* id	  = "item-demo-001";
		const char* dev	  = "device-A";
		const char* acc	  = "account-1";
		const char* pmime = "text/plain";
		CbStr		id_s{id, (uint32_t)strlen(id)};
		CbStr		dev_s{dev, (uint32_t)strlen(dev)};
		CbStr		acc_s{acc, (uint32_t)strlen(acc)};
		CbStr		pm_s{pmime, (uint32_t)strlen(pmime)};
		CbStrList	empty{nullptr, 0};
		CbStr		sha{nullptr, 0};

		CbMeta m{id_s, dev_s, acc_s, empty, empty, pm_s, 5, sha, 1720000000, 0};
		g_cb_store_metadata(&m);
	}

	// 然后再：
	if (g_cb_history_list)
		g_cb_history_list(0, 20);
}

// （可选）应用退出时清理
void StopCoreFFI()
{
	if (g_cb_shutdown)
		g_cb_shutdown();
	if (g_core)
	{
		FreeLibrary(g_core);
		g_core = nullptr;
	}
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

	// 退出时释放 core_ffi_windows.dll
	window.Closed([](auto&&, auto&&) { StopCoreFFI(); });

	StartCoreFFI();
}
} // namespace winrt::ClipBridgeShell::implementation
