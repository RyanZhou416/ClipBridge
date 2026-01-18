use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde_json::json;

use crate::testsupport::core::TestCore;
use crate::util::now_ms;

/// inproc "假网络"
///
/// 目标：只为 M1 测试提供闭环：
/// - account_uid 相同：双方立刻收到 PEER_ONLINE；disconnect 后双方收到 PEER_OFFLINE
/// - account_uid 不同：A 侧立刻收到 CORE_ERROR(AUTH_ACCOUNT_UID_MISMATCH, affects_session=true)
pub struct FakeTransport;

impl FakeTransport {
    pub fn new() -> Self {
        Self
    }

    /// 连接两个 TestCore，返回可断开的句柄
    pub fn connect_pair(&self, a: &TestCore, b: &TestCore) -> LinkHandle {
        let a_id = a.core.inner.core_config.device_id.clone();
        let b_id = b.core.inner.core_config.device_id.clone();
        let a_name = a.core.inner.core_config.device_name.clone();
        let b_name = b.core.inner.core_config.device_name.clone();

        let a_tag = a.core.inner.core_config.account_uid.clone();
        let b_tag = b.core.inner.core_config.account_uid.clone();

        // account_uid 不一致：按文档要求发 AUTH_ACCOUNT_UID_MISMATCH（只对 A 侧）
        if a_tag != b_tag {
            a.core.inner.emit_json(json!({
                "type": "CORE_ERROR",
                "ts_ms": now_ms(),
                "payload": {
                    "code": "AUTH_ACCOUNT_UID_MISMATCH",
                    "affects_session": true
                }
            }));

            return LinkHandle {
                a_inner: a.core.inner.clone(),
                b_inner: b.core.inner.clone(),
                a_device_id: a_id,
                b_device_id: b_id,
                disconnected: AtomicBool::new(true), // 不允许再 disconnect
            };
        }

        // account_uid 一致：双方上线
        a.core.inner.emit_json(json!({
            "type": "PEER_ONLINE",
            "ts_ms": now_ms(),
            "payload": {
                "device_id": b_id.clone(),
                "name": b_name
            }
        }));

        b.core.inner.emit_json(json!({
            "type": "PEER_ONLINE",
            "ts_ms": now_ms(),
            "payload": {
                "device_id": a_id.clone(),
                "name": a_name
            }
        }));

        LinkHandle {
            a_inner: a.core.inner.clone(),
            b_inner: b.core.inner.clone(),
            a_device_id: a_id,
            b_device_id: b_id,
            disconnected: AtomicBool::new(false),
        }
    }
}

pub struct LinkHandle {
    a_inner: Arc<crate::api::Inner>,
    b_inner: Arc<crate::api::Inner>,
    a_device_id: String,
    b_device_id: String,
    disconnected: AtomicBool,
}

impl LinkHandle {
    pub fn disconnect(&self) {
        let already = self.disconnected.swap(true, Ordering::AcqRel);
        if already {
            return;
        }

        // 双方下线：A 收到 B 下线、B 收到 A 下线
        self.a_inner.emit_json(json!({
            "type": "PEER_OFFLINE",
            "ts_ms": now_ms(),
            "payload": {
                "device_id": self.b_device_id.clone(),
                "reason": "Disconnected"
            }
        }));

        self.b_inner.emit_json(json!({
            "type": "PEER_OFFLINE",
            "ts_ms": now_ms(),
            "payload": {
                "device_id": self.a_device_id.clone(),
                "reason": "Disconnected"
            }
        }));
    }
}
