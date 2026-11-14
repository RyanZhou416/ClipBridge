# Changelog

## [0.2.0](https://github.com/RyanZhou416/ClipBridge/compare/clipbridge-windows-0.1.0...clipbridge-windows-0.2.0) (2026-11-14)


### Features

* **auth:** gate new account secrets on entropy and warn on short passwords (F3) ([3d8b847](https://github.com/RyanZhou416/ClipBridge/commit/3d8b847da67a0bf09e80f15ecdbb7d7a739afa99))
* **backup:** one-way export of core.db, logs.db, manifest and CAS content ([d44083e](https://github.com/RyanZhou416/ClipBridge/commit/d44083e521a2f516cb5e5e46a97690dfd144278b))
* history lock/select sync, language fix, UI improvements ([ed10cf3](https://github.com/RyanZhou416/ClipBridge/commit/ed10cf3acaa364e0256cdf88c5fd0cae360855ea))
* **windows:** FrameTrace 增加 1%/0.1% low 帧与超预算帧分布 ([c3e05c2](https://github.com/RyanZhou416/ClipBridge/commit/c3e05c24ddb24e7bdf657dcb0c7bf5265eff7c60))
* **windows:** FrameTrace 增加 drag 场景与亚克力 A/B ([3090dda](https://github.com/RyanZhou416/ClipBridge/commit/3090ddaa5ea85aa431b264b29e7b3cf73b9a4bd9))
* **windows:** FrameTrace 增加 manual 手动拖拽场景与 Mica A/B 开关 ([691731e](https://github.com/RyanZhou416/ClipBridge/commit/691731ec3b3243c9e111f3f18e47986cec8e5de0))
* **windows:** FrameTrace 增加每帧实际成本采样 ([fc8de10](https://github.com/RyanZhou416/ClipBridge/commit/fc8de102f6cb30894b2a8c7e0058897fc341eccf))
* **windows:** portable ZIP with ClipBridge.portable data root ([25c1849](https://github.com/RyanZhou416/ClipBridge/commit/25c184921b9c82360025c3addef0f85626d88ea5))
* **windows:** self-contained portable user data (DPAPI credentials + portable settings) ([38aa79b](https://github.com/RyanZhou416/ClipBridge/commit/38aa79b7bea53704cc7ddd2731e12b66f070b409))
* **windows:** 增加可选的运行时帧时序诊断 CB_FRAME_TRACE ([18492e5](https://github.com/RyanZhou416/ClipBridge/commit/18492e5f1973bc915721d24675426c00613920d7))


### Bug Fixes

* core性能优化、UI布局修复、设置下拉框初始化修复 ([218fd0c](https://github.com/RyanZhou416/ClipBridge/commit/218fd0cf67fe564a120afe54c4e477f7d3814e6a))
* **ffi:** Windows/Android 壳侧校验 FFI ABI 主版本，不匹配即拒绝初始化 ([65433e5](https://github.com/RyanZhou416/ClipBridge/commit/65433e5fa0e5243590990f41d886ae52bc0bf79f))
* **security:** fail closed when account password is unavailable (F11) ([073834f](https://github.com/RyanZhou416/ClipBridge/commit/073834fa2169b967c402f5f9e633bdc53b1f7831))
* **shell:** 剪贴板单入口、采集开关生效与传输事件接线 ([d486a17](https://github.com/RyanZhou416/ClipBridge/commit/d486a17764845fc4b59cf017c055facc6bbd9033))
* **ui:** 恢复首页英雄图的透明度遮罩（半透明效果消失是我引入的回归） ([db25a64](https://github.com/RyanZhou416/ClipBridge/commit/db25a642797b9ccb40f06c98fdacca0860ab43d2))
* **windows:** CONTENT_CACHED 将传输落为 completed 终态 ([66be844](https://github.com/RyanZhou416/ClipBridge/commit/66be8440108b43a5695fa67be44e3c8937ab0b2f))
* **windows:** ContentFetchAwaiter 增加 10s 超时并缓存早到失败 ([9f9f788](https://github.com/RyanZhou416/ClipBridge/commit/9f9f78833bbf0918742ee81c01b46142dd5fde3b))
* **windows:** ingest 解析 envelope，仅成功时推进去重 ([e9ac61c](https://github.com/RyanZhou416/ClipBridge/commit/e9ac61cccf203d261ca14f02532eadf6ab0c22de))
* **windows:** stop creating dead data dirs; ask before deleting user data on uninstall; add MSI build script ([3e740a1](https://github.com/RyanZhou416/ClipBridge/commit/3e740a1dd8f903c856f0316dc19e44f2ec51488b))
* **windows:** 删除锁定项时统一解锁，避免剪贴板被已删内容劫持 ([7f4e953](https://github.com/RyanZhou416/ClipBridge/commit/7f4e953af3a2bba8b2198a73d61eb451d425d508))
* **windows:** 快照 JSON 对齐 Rust ClipboardSnapshotDto 契约 ([da4e52c](https://github.com/RyanZhou416/ClipBridge/commit/da4e52c11c2d6cd268979ba1a73e4977b0a58803))
* **windows:** 恢复首页滚动视差效果 ([23cff40](https://github.com/RyanZhou416/ClipBridge/commit/23cff40f378093cc87130f7ff5ca2bf9af0e6cea))
* **windows:** 视差速度 1.1 → 0.2，修正仪表盘滚动"过于灵敏" ([94e1986](https://github.com/RyanZhou416/ClipBridge/commit/94e198627f5eb411ec2e224fdd3d2fa580752348))


### Performance

* **shell:** trim working set only on tray hide; minimize stays instant ([6536ecc](https://github.com/RyanZhou416/ClipBridge/commit/6536eccc570f38d720be45d0b5aa0920a640a3b9))
* **shell:** trim working set when window is minimized or hidden to tray ([a716250](https://github.com/RyanZhou416/ClipBridge/commit/a716250c472e8d0981e407ed61bbc070bbf55dde))
* **shell:** 主题设置读取与窗口构造重叠，可安全时推迟到首帧后套用（LEVER 2，约 -68ms） ([72ac18d](https://github.com/RyanZhou416/ClipBridge/commit/72ac18dba603bc4cd68bb64f5d4a163c25a9cb23))
* **shell:** 修复渲染开销、数据流放大与订阅泄漏 ([09d15fb](https://github.com/RyanZhou416/ClipBridge/commit/09d15fbf3ef48b6aee0206c87ed77baeb97f07e4))
* **shell:** 先挂占位内容再扩展标题栏（实测 -19.7ms） ([61488f4](https://github.com/RyanZhou416/ClipBridge/commit/61488f45b898e87e48c36b41ad67c893c37ad8a7))
* **shell:** 发布默认开启 ReadyToRun（实测 -37.5ms） ([5f78df2](https://github.com/RyanZhou416/ClipBridge/commit/5f78df2c1434249815b212bbafcc1d17247d3d45))
* **shell:** 启动耗时打点 + 把通知注册/启动日志自检移出启动关键路径 ([420eb36](https://github.com/RyanZhou416/ClipBridge/commit/420eb36ec452c9cc562b3b0199e1bf459660ca1b))
* **shell:** 本地化器后台构建不阻塞激活，消费者各自等待（LEVER 1，约 -56ms） ([b4f024f](https://github.com/RyanZhou416/ClipBridge/commit/b4f024fddb90c80b04fc4468c874943ab64e289a))
* **shell:** 用 ServiceCollection 取代通用主机，省掉 Host 的配置加载与生命周期开销（LEVER 3，约 -118ms） ([2abaed7](https://github.com/RyanZhou416/ClipBridge/commit/2abaed7859b7ca8fa0f8342d4dbbd0a988e81800))
* **shell:** 用普通 Window 取代 WinUIEx.WindowEx（基类构造实测 87ms） ([5aa3eb9](https://github.com/RyanZhou416/ClipBridge/commit/5aa3eb9b4d73fe1d3feda54a057c4023e46db269))
* **shell:** 窗口先生效、Shell 后构建（实测冷启动 -108.7ms） ([a5f69e3](https://github.com/RyanZhou416/ClipBridge/commit/a5f69e3f9896727e05de488d3e03828be9c9a331))
* **ui:** 延迟初始化改到线程池（图表/背景图就绪 -70.1ms）+ 首页构造细分打点 ([9ed2bb4](https://github.com/RyanZhou416/ClipBridge/commit/9ed2bb4ae5a0e27db75afcc5bcae028e8e592f83))
* **windows:** 拖拽/缩放期间临时摘掉 Mica 背景 ([7cef5fe](https://github.com/RyanZhou416/ClipBridge/commit/7cef5fef29e976c86466a81083f1aaad069cb38d))
* **windows:** 拖拽期改铺 Mica 回退色，DWM 属性关背景 ([e13c9d9](https://github.com/RyanZhou416/ClipBridge/commit/e13c9d9795530439dcf02b23006725094541758c))
* **windows:** 拖拽期用屏幕快照覆盖层冻结重内容 ([2ceb0d3](https://github.com/RyanZhou416/ClipBridge/commit/2ceb0d3c3b01aca53f6dc219f1e6ddf5c3f21fb1))
* **windows:** 缩放手势检测到尺寸变化后完整恢复外观 ([b174b27](https://github.com/RyanZhou416/ClipBridge/commit/b174b27217606e79f154888d6e481d93dd7aa2d4))


### Refactors

* **shell:** core 桥接层归位、启动顺序与单实例 ([3fc7303](https://github.com/RyanZhou416/ClipBridge/commit/3fc7303fff6a4cc252375662a2b53b2ecd12384c))
* 升级 .NET 10 + WinAppSDK 2.0 + 全面修复编译警告与自启重构 ([c5b1bc3](https://github.com/RyanZhou416/ClipBridge/commit/c5b1bc377ecb8500e69c31b3dedeba23cfb6c4ad))


### Chores

* **shell:** 删除启动优化的 A/B 对照开关与旧路径（-338/+70） ([f8bffb6](https://github.com/RyanZhou416/ClipBridge/commit/f8bffb6763b4a706bd83c71eca58c6288dc1e154))
* **shell:** 删除实测无收益的测量开关并移除 WinUIEx 包 ([569170d](https://github.com/RyanZhou416/ClipBridge/commit/569170d0acf002b0de98dcbe6942c48445260e73))


### Tests

* **shell:** 测量不再在屏幕上闪窗口（CB_STARTUP_OFFSCREEN） ([4ddb07c](https://github.com/RyanZhou416/ClipBridge/commit/4ddb07c41b083840986605d927519d648998820f))
* 隐藏桌面测量工具 + "可见→可交互"细分打点（两条假设已实测否决） ([ea28fa2](https://github.com/RyanZhou416/ClipBridge/commit/ea28fa2234bf31caf92826fe0c511efa38a51a09))
