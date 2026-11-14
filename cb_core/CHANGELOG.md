# Changelog

## [0.2.0](https://github.com/RyanZhou416/ClipBridge/compare/cb_core-0.1.0...cb_core-0.2.0) (2026-11-14)


### Features

* **backup:** one-way export of core.db, logs.db, manifest and CAS content ([d44083e](https://github.com/RyanZhou416/ClipBridge/commit/d44083e521a2f516cb5e5e46a97690dfd144278b))
* **core:** add C1 multi-endpoint data links and per-peer connection pool ([76bcada](https://github.com/RyanZhou416/ClipBridge/commit/76bcada75974f6f132d92e235f97515cb8b76b92))
* **core:** add CB_QUIC_WORKERS to cap NetManager runtime workers ([44a5035](https://github.com/RyanZhou416/ClipBridge/commit/44a5035b18d609b3f9c2c1069ce5d494d8de3de9))
* **core:** enable 9000B jumbo QUIC payload via DPLPMTUD and 8MiB socket buffers ([3ee408d](https://github.com/RyanZhou416/ClipBridge/commit/3ee408dc86fe3759aca2fb566af21fe199dd74eb))
* **core:** move transfer data to per-transfer payload streams (ADR-002 B1) ([d8a306f](https://github.com/RyanZhou416/ClipBridge/commit/d8a306f6f7962a9d5c332812a9966fdc225fb883))
* **core:** shard transfers across N payload streams with converging controller (ADR-002 B2) ([b2f5dd8](https://github.com/RyanZhou416/ClipBridge/commit/b2f5dd8dbbaa2f8b633dbc92c3d5e61defef420d))
* **core:** size-based fsync policy with durable tracking ([b3db769](https://github.com/RyanZhou416/ClipBridge/commit/b3db769547806f02f41c56ebb285ad9fffb94db2))
* **core:** v2 DDL 增加 kind CHECK 与 history 外键，并加入启动缓存对账 ([dddde83](https://github.com/RyanZhou416/ClipBridge/commit/dddde8338aee9c9299c93ac0510e2f754154dae8))
* **core:** 引入 schema 版本号与 drop-and-rebuild 重建策略 ([ff6025f](https://github.com/RyanZhou416/ClipBridge/commit/ff6025fd518caae426d9d5c0a4a3ddfbb7d163e5))
* **core:** 移除死 stats.db，三个统计接口改为现算真实数据 ([b5b9008](https://github.com/RyanZhou416/ClipBridge/commit/b5b900814096e3a50058d5965ac1a16ea4fdddd4))
* **crypto:** 启用 OPAQUE 密码拉伸 Argon2id(19MiB,t=2) + 注册记录缓存 ([4a277f4](https://github.com/RyanZhou416/ClipBridge/commit/4a277f419e64db799ac0157ab1726d64ca0c51cd))
* history lock/select sync, language fix, UI improvements ([ed10cf3](https://github.com/RyanZhou416/ClipBridge/commit/ed10cf3acaa364e0256cdf88c5fd0cae360855ea))
* implement missing M3 FFI bindings and Core API updates ([fea2bcd](https://github.com/RyanZhou416/ClipBridge/commit/fea2bcd333397e9f17698a27a889ebfb2b660eb0))
* **logs:** 日志按天数和行数上限自动清理，VACUUM 设阈值门控 ([9f292db](https://github.com/RyanZhou416/ClipBridge/commit/9f292dbdaf6d3c577ab600a46e782dd0f73afb45))
* **proto:** 新增非致命 ContentDenied 变体，协议版本升到 3 ([d741db1](https://github.com/RyanZhou416/ClipBridge/commit/d741db14983a0d0b9c4d69aa64fb3f39f7690acc))
* **store:** read-only query pool + incremental auto-vacuum space reclamation ([a4802ef](https://github.com/RyanZhou416/ClipBridge/commit/a4802ef132d13b66c49bc65194bec77ed3631cf1))
* **store:** versioned schema migrations and SQLite pragma tuning (P1/P4) ([97e089c](https://github.com/RyanZhou416/ClipBridge/commit/97e089cb6a092b5cc7f0164636cdd41071791133))


### Bug Fixes

* **api:** clear_cache 保留 content_cache 元数据，避免清缓存后历史消失 ([a0a9a8b](https://github.com/RyanZhou416/ClipBridge/commit/a0a9a8b50dcc1c1bb5ba34c8936ae40339d92214))
* **cas:** 缓存 GC 按真实磁盘占用核算，并同步清理视图硬链接 ([ed8fcbf](https://github.com/RyanZhou416/ClipBridge/commit/ed8fcbf5da0158bf39c99b349f9881f9f9cbfdb6))
* **core:** bound peer metadata and history/log growth; reject Windows-unsafe paths; zeroize secrets (F12/F14/F15) ([e7dd348](https://github.com/RyanZhou416/ClipBridge/commit/e7dd348bf43168cf83633362449b1b0244f0c163))
* **core:** harden inbound local_path handling and OPAQUE auth handshake ([5e3e084](https://github.com/RyanZhou416/ClipBridge/commit/5e3e084461c098942cedea83c0beac49645f8faa))
* **core:** harden inbound transfer admission, handshake deadline, and session limits (F4/F5/F6/F8) ([422fc3d](https://github.com/RyanZhou416/ClipBridge/commit/422fc3d2afbd3f678218dbebc397e8133001f65a))
* **core:** move logging and remaining blocking IO off async runtime workers ([f8c516a](https://github.com/RyanZhou416/ClipBridge/commit/f8c516a77efc386e4c09325f6a4cf0f197c1e0f4))
* **core:** 安全加固、存储健壮性与测试网络隔离 ([f019b1a](https://github.com/RyanZhou416/ClipBridge/commit/f019b1ae604da244f845e0c4982e71aef7276cc2))
* core性能优化、UI布局修复、设置下拉框初始化修复 ([218fd0c](https://github.com/RyanZhou416/ClipBridge/commit/218fd0cf67fe564a120afe54c4e477f7d3814e6a))
* **net:** default multi-endpoint data links to off after C1 A/B showed no gain ([22e7761](https://github.com/RyanZhou416/ClipBridge/commit/22e77618c2b98f6f60feda3e85a8faeb22370d73))
* **net:** multi-thread runtime, timed non-blocking dials, non-blocking broadcasts ([26649bf](https://github.com/RyanZhou416/ClipBridge/commit/26649bf4df87f10a250aff49a404e0f7865d4206))
* **session:** non-blocking inbound backpressure, async ContentEnd finalize, outbound sender cap ([394b300](https://github.com/RyanZhou416/ClipBridge/commit/394b300f86ae14df1c3a275cb8221ab9245fa72d))
* **session:** run OPAQUE Argon2id KSF off tokio workers; align client QUIC idle timeout ([f5a1a18](https://github.com/RyanZhou416/ClipBridge/commit/f5a1a1806cbf775d120c8bab2e4728a29976eb7e))
* **session:** scale data-chunk send timeout with chunk size ([3be2c3a](https://github.com/RyanZhou416/ClipBridge/commit/3be2c3a4fc7a6ad249968b97faea3f97459a75fb))
* **session:** 为入站传输加上硬上限、并发上限与空闲超时 ([08d68bf](https://github.com/RyanZhou416/ClipBridge/commit/08d68bfd057f5f0edd7e6333f94becf5ee7ad187))
* **session:** 入站传输按 item 自身的 content.sha256 标记缓存 ([09d2a7e](https://github.com/RyanZhou416/ClipBridge/commit/09d2a7eb00e6cf95f45a4fbe14f9a97f49a5c7ff))
* **session:** 未认证会话统一拒绝业务消息 ([ce960c4](https://github.com/RyanZhou416/ClipBridge/commit/ce960c4e48f4f88944c4ce75f7a17e9aa83d9c1b))
* **session:** 校验协议版本并拒绝重复 Hello ([c856634](https://github.com/RyanZhou416/ClipBridge/commit/c8566345a24df6b2c0ae089a5fd0ba498fed5fc7))
* **session:** 用 Drop 守护保证会话状态必然落到 Terminated ([97a1b60](https://github.com/RyanZhou416/ClipBridge/commit/97a1b60341144ad965f94382c142897320ae61fc))
* **session:** 真正执行对等设备共享策略 ([d776a52](https://github.com/RyanZhou416/ClipBridge/commit/d776a5202b0bc4ea9de7c6dd687e9438adb84b67))
* **session:** 策略拒绝改用非致命 ContentDenied，避免共享开关引发断连抖动 ([c8e08ea](https://github.com/RyanZhou416/ClipBridge/commit/c8e08ea1b17c03ca4142229d48c36b77d853bc3f))


### Performance

* **core:** avoid per-ingest full CAS walk in run_gc ([6f63703](https://github.com/RyanZhou416/ClipBridge/commit/6f63703d52591aa1d7be2a16a36c359d4c484988))
* **net:** raise tokio worker cap to 8; drop dead upload hash; 256 KiB chunks ([450ab5b](https://github.com/RyanZhou416/ClipBridge/commit/450ab5b54ebff5ba5bb1e5129bb631f14cef46e1))
* **net:** set explicit 8/64/32 MiB QUIC flow-control windows ([6395950](https://github.com/RyanZhou416/ClipBridge/commit/63959505b6a4fc4d1fb2241ce7211e9b33aaf63b))
* **session:** 去掉传输热路径上的逐块日志，改为每传输一条汇总 ([0ab47c1](https://github.com/RyanZhou416/ClipBridge/commit/0ab47c11a5ebd0abc4210c15d11858b48b6f5f1d))
* **store:** move WAL checkpoint off the write lock; pool non-authorization reads ([1c9bd6f](https://github.com/RyanZhou416/ClipBridge/commit/1c9bd6f517ef890b4b284dfe0f1f5028b2b0ccb0))


### Refactors

* **crypto:** bincode 1.3 换成 postcard，清掉 RUSTSEC-2025-0141 ([9de21b9](https://github.com/RyanZhou416/ClipBridge/commit/9de21b9815ef700740c8b5c7d56ac2e5303c31a9))
* 升级 .NET 10 + WinAppSDK 2.0 + 全面修复编译警告与自启重构 ([c5b1bc3](https://github.com/RyanZhou416/ClipBridge/commit/c5b1bc377ecb8500e69c31b3dedeba23cfb6c4ad))


### Documentation

* **deps:** 在清单里写明三个被上游卡住的依赖与原因 ([d11788a](https://github.com/RyanZhou416/ClipBridge/commit/d11788a9281e2740135f054e9806bf7e845b8a28))


### Chores

* **deps:** opaque-ke 从预发布 4.1.0-pre.2 退回稳定版 4.0.1 ([2ce326c](https://github.com/RyanZhou416/ClipBridge/commit/2ce326c6a1d6ddee8e1f7e57ef5e270bfa7e8934))
* **deps:** opaque-ke 升到最新 4.1.0-pre.2（实测无需适配，但未解开密码学栈卡点） ([0bdf56d](https://github.com/RyanZhou416/ClipBridge/commit/0bdf56dd7f12269f1558c21ce8a2afe97c3394ae))
* **deps:** 更新 Rust 依赖并整理 manifest ([aae0c33](https://github.com/RyanZhou416/ClipBridge/commit/aae0c33490597c7121c6bc03904993a359f9184e))


### Tests

* **core:** add ignored quinn tuning probe (payload, endpoints, streams, GSO bound) ([56db9bc](https://github.com/RyanZhou416/ClipBridge/commit/56db9bc1b42d176209dac49a0178c0262124fd5d))
* **core:** add ignored Windows UDP GSO/GRO probe ([5b6fa70](https://github.com/RyanZhou416/ClipBridge/commit/5b6fa701d4d3c6f1076211bb3c9400a2f7e96351))
* **core:** add jumbo socket-buffer control to quinn tuning probe ([92d78aa](https://github.com/RyanZhou416/ClipBridge/commit/92d78aa7c5d50a4925e6cae35f9582300422512a))
* **net:** add ignored WAN RTT/window throughput probe ([e4526d2](https://github.com/RyanZhou416/ClipBridge/commit/e4526d2a1948b9f15ab4f7d6554f065a2bd06c05))
* **perf:** add ignored cb_core runtime perf probe ([e05e6e7](https://github.com/RyanZhou416/ClipBridge/commit/e05e6e798498676a995abe3fed9be8d47789642d))
* **perf:** add ignored hub concurrency probe ([01068db](https://github.com/RyanZhou416/ClipBridge/commit/01068dba665a35ef9b7c6e1375963333779e37b8))
* **session:** 修正新增测试的 clippy 告警 ([645792b](https://github.com/RyanZhou416/ClipBridge/commit/645792b787a7922d2e1a930c36a6087ffffc7e8d))
* **session:** 覆盖入站限额、Terminated 守护与 FileList 缓存标记 ([0a699c8](https://github.com/RyanZhou416/ClipBridge/commit/0a699c89b225086b4dc24bbab20ba3cc24d3c296))
