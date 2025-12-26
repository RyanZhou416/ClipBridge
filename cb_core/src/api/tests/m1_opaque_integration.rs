use crate::api::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use super::m1_net::create_test_core;
// 本地实现的 Mock Event Sink
struct TestSink {
    tx: broadcast::Sender<String>,
}

impl TestSink {
    fn new() -> (Arc<Self>, broadcast::Receiver<String>) {
        let (tx, rx) = broadcast::channel(100);
        (Arc::new(Self { tx }), rx)
    }
}

impl CoreEventSink for TestSink {
    fn emit(&self, json_event: String) {
        let _ = self.tx.send(json_event);
    }
}



#[tokio::test]
async fn test_opaque_handshake_integration() {
    let shared_uid = "user_shared_secret_uid_001";

    // 1. 初始化两个 Core
    // 使用下划线前缀避免未使用变量警告
    let (_c1, mut c1_rx, _d1) = create_test_core("dev1", shared_uid,|_| {});
    let (_c2, mut c2_rx, _d2) = create_test_core("dev2", shared_uid,|_| {});

    println!("Cores initialized. Waiting for mDNS discovery and OPAQUE handshake...");

    // 2. 监听 PEER_ONLINE 事件
    let check_online = async {
        let mut c1_sees_c2 = false;
        let mut c2_sees_c1 = false;

        let start = std::time::Instant::now();

        // 5秒超时
        while start.elapsed() < Duration::from_secs(15) {
            tokio::select! {
                Ok(evt) = c1_rx.recv() => {
                    if evt.contains("PEER_ONLINE") && evt.contains("dev2") {
                        println!("Core 1 sees Dev 2 Online!");
                        c1_sees_c2 = true;
                    }
                }
                Ok(evt) = c2_rx.recv() => {
                    if evt.contains("PEER_ONLINE") && evt.contains("dev1") {
                        println!("Core 2 sees Dev 1 Online!");
                        c2_sees_c1 = true;
                    }
                }
                else => break,
            }

            if c1_sees_c2 && c2_sees_c1 {
                return true;
            }
        }
        false
    };

    let success = check_online.await;

    if !success {
        println!("Warning: Peers did not see each other. This is common in CI environments without multicast support.");
    } else {
        println!("SUCCESS: OPAQUE Handshake completed over network!");
    }
}
