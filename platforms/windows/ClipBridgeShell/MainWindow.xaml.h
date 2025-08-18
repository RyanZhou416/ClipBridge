#pragma once
#include "MainWindow.g.h"

namespace winrt::ClipBridgeShell::implementation
{
struct MainWindow : MainWindowT<MainWindow>
{
	MainWindow(); // 只声明，不要在这里写 {}

	// ---- public: 供外部/投影访问 ----
	void AppendLog(winrt::hstring const& line);
	void AppendLog(std::wstring const& line);

	// ★ 关键：把这两个从 private 挪到这里（public）
	int32_t MyProperty();
	void	MyProperty(int32_t value);

  private:
	// ---- private: 内部缓存控件指针 ----
	Microsoft::UI::Xaml::Controls::TextBlock m_logBox{nullptr};
};
} // namespace winrt::ClipBridgeShell::implementation

namespace winrt::ClipBridgeShell::factory_implementation
{
struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow>
{
};
} // namespace winrt::ClipBridgeShell::factory_implementation
