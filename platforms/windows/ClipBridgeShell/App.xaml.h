// App.xaml.h
#pragma once
#include "App.xaml.g.h"

namespace winrt::ClipBridgeShell::implementation
{
struct App : AppT<App>
{
	App();
	void OnLaunched(Microsoft::UI::Xaml::LaunchActivatedEventArgs const&);

	static void SetMainWindow(winrt::Microsoft::UI::Xaml::Window const& w);
	static winrt::Microsoft::UI::Xaml::Window TryGetMainWindow();

  private:
	winrt::Microsoft::UI::Xaml::Window								  window{nullptr};
	static inline winrt::weak_ref<winrt::Microsoft::UI::Xaml::Window> s_winWeak{nullptr};
};
} // namespace winrt::ClipBridgeShell::implementation
