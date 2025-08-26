#pragma once
//
// CoreHost.h — ClipBridge Windows Shell ↔ Core FFI 网关
//

#include <windows.h>
#include <cstdint>
#include <string>
#include <string_view>
#include <mutex>
#include <optional>
#include <winrt/base.h>


#if __has_include("cb_ffi.h")
#include "cb_ffi.h"
#else
#error "cb_ffi.h not found (set Additional Include Directories)"
#endif

namespace clipbridge
{

// UTF-16 → UTF-8
std::string WideToUtf8(std::wstring_view w);

// UTF-8 → UTF-16
std::wstring Utf8ToWide(const char* s);
std::wstring Utf8ToWide(std::string_view s);

// 托管 core 返回的 char*，析构调用 cb_free
struct OwnedStr
{
	char* p{nullptr};
	OwnedStr() = default;
	explicit OwnedStr(char* s) : p(s) {}
	OwnedStr(OwnedStr&& o) noexcept : p(o.p) { o.p = nullptr; }
	OwnedStr& operator=(OwnedStr&& o) noexcept
	{
		if (this != &o)
		{
			reset();
			p	= o.p;
			o.p = nullptr;
		}
		return *this;
	}
	~OwnedStr() { reset(); }
	void reset(char* s = nullptr)
	{
		if (p)
			cb_free(p);
		p = s;
	}
	const char* c_str() const { return p ? p : ""; }
	bool		empty() const { return !p || !*p; }
};

struct CoreResult
{
	int			 code{CB_OK};
	std::wstring message;
	explicit	 operator bool() const { return code == CB_OK; }
};

} // namespace clipbridge

class CoreHost
{
  public:
	struct Config
	{
		std::wstring device_name;
		std::wstring data_dir;
		std::wstring cache_dir;
		std::wstring log_dir;

		uint64_t cache_limit_bytes = 512ull * 1024 * 1024; // 512 MiB
		uint32_t history_limit	   = 2000;

		bool	 mdns_enabled = true;
		uint16_t mdns_port	  = 0;
		uint16_t quic_port	  = 0;

		bool trust_known_devices_only = false;
		bool require_encryption		  = false;

		std::wstring dll_name = L"core_ffi_windows.dll";
	};

	using DeviceOnlineHandler	  = winrt::delegate<void(std::string_view)>;
	using DeviceOfflineHandler	  = winrt::delegate<void(std::string_view)>;
	using NewMetadataHandler	  = winrt::delegate<void(std::string_view)>;
	using TransferProgressHandler = winrt::delegate<void(std::string_view, uint64_t, uint64_t)>;
	using ErrorHandler			  = winrt::delegate<void(int, std::string_view)>;

	winrt::event_token AddDeviceOnline(DeviceOnlineHandler const& h)
	{
		return m_onDeviceOnline.add(h);
	}
	void RemoveDeviceOnline(winrt::event_token const& t) { m_onDeviceOnline.remove(t); }
	winrt::event_token AddDeviceOffline(DeviceOfflineHandler const& h)
	{
		return m_onDeviceOffline.add(h);
	}
	void RemoveDeviceOffline(winrt::event_token const& t) { m_onDeviceOffline.remove(t); }
	winrt::event_token AddNewMetadata(NewMetadataHandler const& h)
	{
		return m_onNewMetadata.add(h);
	}
	void			   RemoveNewMetadata(winrt::event_token const& t) { m_onNewMetadata.remove(t); }
	winrt::event_token AddTransferProgress(TransferProgressHandler const& h)
	{
		return m_onTransferProgress.add(h);
	}
	void RemoveTransferProgress(winrt::event_token const& t) { m_onTransferProgress.remove(t); }
	winrt::event_token AddError(ErrorHandler const& h) { return m_onError.add(h); }
	void			   RemoveError(winrt::event_token const& t) { m_onError.remove(t); }

	static CoreHost& Instance();

	bool Init(Config const& cfg);
	void Shutdown();

	clipbridge::CoreResult LastError() const;

	std::string IngestLocalCopy(std::string_view snapshot_json);
	std::string EnsureContentCached(std::string_view item_id, std::string_view prefer_mime);
	std::string ListHistory(uint32_t limit, uint32_t offset);
	std::string GetItem(std::string_view item_id);
	bool		Pause(bool yes);
	bool		PruneCache();
	bool		PruneHistory();

	CoreHost(CoreHost const&)			 = delete;
	CoreHost& operator=(CoreHost const&) = delete;

  private:
	CoreHost()	= default;
	~CoreHost() = default;

	bool LoadDll(std::wstring const& path_or_name);
	void UnloadDll();
	void SetLastError(int code, std::wstring msg);

	using PFN_cb_init					= int(__cdecl*)(const cb_config*, const cb_callbacks*);
	using PFN_cb_shutdown				= void(__cdecl*)(void);
	using PFN_cb_ingest_local_copy		= int(__cdecl*)(const char*, char**);
	using PFN_cb_ingest_remote_metadata = int(__cdecl*)(const char*);
	using PFN_cb_ensure_content_cached	= int(__cdecl*)(const char*, const char*, char**);
	using PFN_cb_list_history			= int(__cdecl*)(uint32_t, uint32_t, char**);
	using PFN_cb_get_item				= int(__cdecl*)(const char*, char**);
	using PFN_cb_pause					= int(__cdecl*)(int);
	using PFN_cb_prune_cache			= int(__cdecl*)(void);
	using PFN_cb_prune_history			= int(__cdecl*)(void);

	HMODULE						  m_mod{nullptr};
	PFN_cb_init					  p_cb_init{nullptr};
	PFN_cb_shutdown				  p_cb_shutdown{nullptr};
	PFN_cb_ingest_local_copy	  p_cb_ingest_local_copy{nullptr};
	PFN_cb_ingest_remote_metadata p_cb_ingest_remote_metadata{nullptr};
	PFN_cb_ensure_content_cached  p_cb_ensure_content_cached{nullptr};
	PFN_cb_list_history			  p_cb_list_history{nullptr};
	PFN_cb_get_item				  p_cb_get_item{nullptr};
	PFN_cb_pause				  p_cb_pause{nullptr};
	PFN_cb_prune_cache			  p_cb_prune_cache{nullptr};
	PFN_cb_prune_history		  p_cb_prune_history{nullptr};

	winrt::event<DeviceOnlineHandler>	  m_onDeviceOnline;
	winrt::event<DeviceOfflineHandler>	  m_onDeviceOffline;
	winrt::event<NewMetadataHandler>	  m_onNewMetadata;
	winrt::event<TransferProgressHandler> m_onTransferProgress;
	winrt::event<ErrorHandler>			  m_onError;

	static CoreHost* s_self;
	static void __cdecl OnDeviceOnline_C(const char* json_device);
	static void __cdecl OnDeviceOffline_C(const char* device_id);
	static void __cdecl OnNewMetadata_C(const char* json_meta);
	static void __cdecl OnTransferProgress_C(const char* item_id, uint64_t done, uint64_t total);
	static void __cdecl OnError_C(int code, const char* message);

	cb_callbacks m_callbacks{};

	mutable std::mutex	   m_mu;
	clipbridge::CoreResult m_lastErr;

	std::optional<Config> m_cfg;
};
