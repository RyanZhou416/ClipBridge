// cb_core/src/net/mdns.rs
//! mDNS/Bonjour 发现模块（v1 占位实现）
//!
//! 设计目标：
//! - 启动/停止一个“发现”后台任务；
//! - 未来可替换为真正的 mDNS/Bonjour（例如通过 `libmdns`/`bonjour` 绑定）；
//! - 向 Core 报告设备上线/下线（on_device_online/offline）；
//!
//! v1 当前实现：
//! - 不进行真实网络发现，仅提供一个稳定的句柄与线程生命周期管理；
//! - 预留了向回调发送事件的管道与示例（代码注释中给出），便于后续无缝接线；
//!
//! 公共 API：
//! - [`MdnsHandle::start`]：启动发现任务，返回可 Drop 的句柄；
//! - [`MdnsHandle::stop`]：显式停止（也可依赖 Drop 自动停止）；
//!
//! 注意：本模块引用了 `crate::api::CbCallbacks` 与 `crate::proto::DeviceInfo`，
//!       仅用于向宿主回调事件；真实发现逻辑接入后保持这个回调路径不变即可。

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::api::CbCallbacks;
use crate::proto::DeviceInfo;

/// mDNS 发现任务的可取消句柄。
///
/// - `running`：跨线程的运行标记；
/// - `join`：后台线程句柄（Drop 时自动 join）；
/// - `self_device_id`：本机设备 ID（避免把自己当作对端上报）。
pub struct MdnsHandle {
    running: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    self_device_id: String,
}

impl MdnsHandle {
    /// 启动 mDNS 发现（v1：占位线程，仅维持生命周期）。
    ///
    /// 参数：
    /// - `self_device`：本机设备信息（避免回环上报自身）；
    /// - `callbacks`：Core 中转的回调对象，用于上报发现事件。
    ///
    /// 返回：可 Drop 的句柄；调用 [`stop`](Self::stop) 或让其 Drop 即可结束线程。
    #[must_use]
    pub fn start(self_device: DeviceInfo, callbacks: Arc<dyn CbCallbacks>) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // 为了演示与未来接线，这里启动一个"心跳线程"：
        // - 每 5s 醒来一次，检查 running；
        // - 真实实现中，这里会执行 mDNS 查询、解析记录、维护上线/下线差分；
        // - 通过 callbacks.on_device_online/offline 上报变更。
        let self_id = self_device.device_id.clone();
        let _cb_for_thread = callbacks; // Move 到线程闭包里（避免未使用警告）
        let join = thread::spawn(move || {
            // 示例：可在 DEBUG 模式下定期打印心跳（这里省略日志依赖）
            while running_clone.load(Ordering::SeqCst) {
                // 占位：未来在此发起 mDNS browse/resolve，解析 TXT 列表到 DeviceInfo
                // 注意：发现到与 `self_id` 相同的设备应忽略（本机）。
                //
                // 伪代码（未来接入时可作为参考）：
                // for event in mdns_browser.poll() {
                //     match event {
                //         Discovered(peer) => {
                //             if peer.device_id != self_id {
                //                 _cb_for_thread.on_device_online(&peer);
                //             }
                //         }
                //         Expired(device_id) => {
                //             if device_id != self_id {
                //                 _cb_for_thread.on_device_offline(&device_id);
                //             }
                //         }
                //     }
                // }

                thread::sleep(Duration::from_secs(5));
            }
        });

        Self {
            running,
            join: Some(join),
            self_device_id: self_id,
        }
    }

    /// 显式停止后台任务（可选；Drop 时也会自动停止）。
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }

    /// 返回本机设备 ID（用于诊断/日志）。
    #[must_use]
    pub fn self_device_id(&self) -> &str {
        &self.self_device_id
    }
}

impl Drop for MdnsHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

// ------------------------------ 单元测试（轻量） ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{DeviceInfo, Platform, PROTOCOL_VERSION};

    struct NopCb;
    impl CbCallbacks for NopCb {}

    #[test]
    fn start_and_stop() {
        let dev = DeviceInfo {
            device_id: "dev-123".into(),
            device_name: "My-PC".into(),
            platform: Platform::Other,
            app_version: None,
            protocol_version: PROTOCOL_VERSION,
        };
        let cb = Arc::new(NopCb);
        let mut h = MdnsHandle::start(dev, cb);
        assert_eq!(h.self_device_id(), "dev-123");
        // 运行一小会儿
        std::thread::sleep(std::time::Duration::from_millis(50));
        h.stop(); // 显式关闭
    }
}
