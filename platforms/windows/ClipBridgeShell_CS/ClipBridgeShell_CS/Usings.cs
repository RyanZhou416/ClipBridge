// WinUIEx 已不再使用：MainWindow 改为直接继承 Microsoft.UI.Xaml.Window，
// 原先依赖 WindowEx 的能力改由 Helpers/WindowExtensions.cs 提供。
// 这里不能再保留 `global using WinUIEx;`：WinUIEx 同样提供了 Window 的 Show/Hide 扩展方法，
// 会与 Helpers 里的同名扩展方法形成二义性（CS0121）。
