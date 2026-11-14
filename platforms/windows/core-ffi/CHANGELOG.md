# Changelog

## [0.2.0](https://github.com/RyanZhou416/ClipBridge/compare/core-ffi-windows-0.1.0...core-ffi-windows-0.2.0) (2026-11-14)


### Features

* history lock/select sync, language fix, UI improvements ([ed10cf3](https://github.com/RyanZhou416/ClipBridge/commit/ed10cf3acaa364e0256cdf88c5fd0cae360855ea))
* implement missing M3 FFI bindings and Core API updates ([fea2bcd](https://github.com/RyanZhou416/ClipBridge/commit/fea2bcd333397e9f17698a27a889ebfb2b660eb0))


### Bug Fixes

* **ffi:** C ABI panic 隔离与句柄生命周期加固 ([1df9ae2](https://github.com/RyanZhou416/ClipBridge/commit/1df9ae297e5f56a964ce08465605af69b8538476))


### Refactors

* **ffi:** 抽取共享 C ABI 实现，三平台统一导出面 ([8f1e1e6](https://github.com/RyanZhou416/ClipBridge/commit/8f1e1e6cb58d8879b0c92f72c3c272cd977381cf))
* 升级 .NET 10 + WinAppSDK 2.0 + 全面修复编译警告与自启重构 ([c5b1bc3](https://github.com/RyanZhou416/ClipBridge/commit/c5b1bc377ecb8500e69c31b3dedeba23cfb6c4ad))


### Chores

* **deps:** 更新 Rust 依赖并整理 manifest ([aae0c33](https://github.com/RyanZhou416/ClipBridge/commit/aae0c33490597c7121c6bc03904993a359f9184e))
