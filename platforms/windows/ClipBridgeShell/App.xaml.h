#pragma once
#include "App.xaml.g.h"
#include <winrt/Microsoft.UI.Xaml.h>

namespace winrt::ClipBridgeShell::implementation
{
    struct App : winrt::Microsoft::UI::Xaml::ApplicationT<App>
    {
        App();
        void OnLaunched(Microsoft::UI::Xaml::LaunchActivatedEventArgs const&);
    };
}

namespace winrt::ClipBridgeShell::factory_implementation
{
    struct App : winrt::Microsoft::UI::Xaml::ApplicationT<App, implementation::App> {};
}

