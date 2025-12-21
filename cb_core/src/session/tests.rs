use super::*;
use crate::api::{CoreConfig, CoreEventSink};
use crate::store::Store;
use crate::util::now_ms;
use crate::transport::{Connection, Transport};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

// --- 1. 测试辅助工具 ---

// 一个简单的 Sink，把收到的事件存进内存列表，方便断言
struct TestSink {
    events: Mutex<Vec<serde_json::Value>>,
    notify: Notify, // 收到消息时通知测试线程
}

impl TestSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            notify: Notify::new(),
        }
    }

    // 等待直到收到某种类型的事件
    async fn wait_for_event(&self, event_type: &str, timeout: Duration) -> Option<serde_json::Value> {
        let start = std::time::Instant::now();
        loop {
            {
                let events = self.events.lock().unwrap();
                if let Some(evt) = events.iter().find(|e| e["type"] == event_type) {
                    return Some(evt.clone());
                }
            }
            if start.elapsed() > timeout {
                return None;
            }
            // 等待新消息通知或超时
            // 使用 timeout 包裹 notified() 以防止永久阻塞
            let _ = tokio::time::timeout(Duration::from_millis(100), self.notify.notified()).await;
        }
    }

    // 检查是否**没有**收到某种事件
    fn assert_no_event(&self, event_type: &str) {
        let events = self.events.lock().unwrap();
        assert!(events.iter().all(|e| e["type"] != event_type), "Should NOT receive {}", event_type);
    }
}

impl CoreEventSink for TestSink {
    fn emit(&self, event_json: String) {
        let v: serde_json::Value = serde_json::from_str(&event_json).unwrap();
        // println!("Sink received: {}", v); // 调试时打开
        self.events.lock().unwrap().push(v);
        self.notify.notify_waiters();
    }
}

// 快速创建测试环境
struct TestContext {
    config: CoreConfig,
    sink: Arc<TestSink>,
    transport: Arc<Transport>, // 保持引用防止 drop
}

async fn setup(name: &str, tag: &str) -> TestContext {
    let mut path = std::env::temp_dir();
    path.push("cb_test_session");
    path.push(name);
    // 清理旧数据，保证测试环境纯净
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();

    let config = CoreConfig {
        device_id: name.to_string(),
        device_name: name.to_string(),
        account_uid: "test_uid".to_string(),
        account_tag: tag.to_string(),
        // 注意：根据 api/mod.rs 的定义，data_dir 是 String 类型
        data_dir: path.to_string_lossy().to_string(),
        cache_dir: path.to_string_lossy().to_string(),
        limits: Default::default(),
        gc_history_max_items: 100,
        gc_cas_max_bytes: 1024,
        global_policy: Default::default()
    };

    // 初始化 DB (为了 TOFU 表)
    let _ = Store::open(&config.data_dir).unwrap();

    let sink = Arc::new(TestSink::new());
    // 端口传 0 让系统自动分配，避免端口冲突
    let transport = Arc::new(Transport::new(0).unwrap());

    TestContext { config, sink, transport }
}

// 建立两个 Transport 之间的真实连接
async fn link_peers(server: &TestContext, client: &TestContext) -> (Connection, Connection) {
    let server_port = server.transport.local_port().unwrap();
    let addr = format!("127.0.0.1:{}", server_port);

    // 使用 tokio::join! 让 connect 和 accept 并发执行，模拟真实网络交互
    let (client_res, server_res) = tokio::join!(
        client.transport.connect(&addr),
        server.transport.accept()
    );

    let client_conn = client_res.expect("Client connect failed");

    // server_res 是 Option<Connection>
    let server_conn = server_res.expect("Server accept failed / No incoming connection");

    (server_conn, client_conn)
}

// --- 2. 单元测试用例 ---

#[tokio::test]
async fn test_handshake_success() {
    let srv_ctx = setup("srv_ok", "tag_secret").await;
    let cli_ctx = setup("cli_ok", "tag_secret").await;

    let (srv_conn, cli_conn) = link_peers(&srv_ctx, &cli_ctx).await;

    // 启动 Actors
    // Server 端不知道谁会连进来，所以 expected_peer_id 为 None
    let srv_handle = SessionActor::spawn(
        SessionRole::Server,
        srv_conn,
        srv_ctx.config.clone(),
        srv_ctx.sink.clone(),
        None
    );

    // Client 端知道自己要连 srv_ok
    let cli_handle = SessionActor::spawn(
        SessionRole::Client,
        cli_conn,
        cli_ctx.config.clone(),
        cli_ctx.sink.clone(),
        Some("srv_ok".to_string())
    );

    // 断言：双方都应该收到 PEER_ONLINE
    // 握手包含多次交互，给它 2 秒钟
    let srv_evt = srv_ctx.sink.wait_for_event("PEER_ONLINE", Duration::from_secs(2)).await;
    assert!(srv_evt.is_some(), "Server should emit PEER_ONLINE");
    assert_eq!(srv_evt.unwrap()["payload"]["device_id"], "cli_ok");

    let cli_evt = cli_ctx.sink.wait_for_event("PEER_ONLINE", Duration::from_secs(2)).await;
    assert!(cli_evt.is_some(), "Client should emit PEER_ONLINE");
    assert_eq!(cli_evt.unwrap()["payload"]["device_id"], "srv_ok");

    // 验证状态句柄
    assert!(srv_handle.is_online());
    assert!(cli_handle.is_online());

    // 验证 TOFU 已经写入 DB
    // 注意：Store::open 可能会阻塞，但在测试中文件 IO 压力小，这里暂不使用 spawn_blocking
    let store = Store::open(&cli_ctx.config.data_dir).unwrap();
    let fp = store.get_peer_fingerprint("test_uid", "srv_ok").unwrap();
    assert!(fp.is_some(), "Client should have pinned Server fingerprint");
}

#[tokio::test]
async fn test_auth_fail_tag_mismatch() {
    // 设置不同的 Account Tag
    let srv_ctx = setup("srv_diff", "tag_A").await;
    let cli_ctx = setup("cli_diff", "tag_B").await;

    let (srv_conn, cli_conn) = link_peers(&srv_ctx, &cli_ctx).await;

    let _srv_handle = SessionActor::spawn(
        SessionRole::Server,
        srv_conn,
        srv_ctx.config.clone(),
        srv_ctx.sink.clone(),
        None
    );

    let _cli_handle = SessionActor::spawn(
        SessionRole::Client,
        cli_conn,
        cli_ctx.config.clone(),
        cli_ctx.sink.clone(),
        Some("srv_diff".to_string())
    );

    // 等待一会
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 断言：Server 不应该发出 ONLINE 事件
    srv_ctx.sink.assert_no_event("PEER_ONLINE");

    // 断言：Client 也不应该 ONLINE
    cli_ctx.sink.assert_no_event("PEER_ONLINE");
}

#[tokio::test]
async fn test_tofu_reject_changed_fingerprint() {
    let srv_ctx = setup("srv_hack", "tag_same").await;
    let cli_ctx = setup("cli_victim", "tag_same").await;

    // --- 关键步骤：在 Client 的 DB 里预埋一个【错误】的指纹 ---
    {
        let mut store = Store::open(&cli_ctx.config.data_dir).unwrap();
        // 插入一个假的指纹，模拟 Server 以前是另一个人
        store.save_peer_fingerprint("test_uid", "srv_hack", "deadbeefdeadbeefdeadbeefdeadbeef", now_ms()).unwrap();
    }

    let (srv_conn, cli_conn) = link_peers(&srv_ctx, &cli_ctx).await;

    let _srv_handle = SessionActor::spawn(
        SessionRole::Server,
        srv_conn,
        srv_ctx.config.clone(),
        srv_ctx.sink.clone(),
        None
    );

    let _cli_handle = SessionActor::spawn(
        SessionRole::Client,
        cli_conn,
        cli_ctx.config.clone(),
        cli_ctx.sink.clone(),
        Some("srv_hack".to_string())
    );

    // Client 会完成握手（因为 Tag 是对的），但在最后一步 AuthOk 处理时，
    // 触发 perform_tofu_check -> 发现 DB 里的 deadbeef 和 srv_hack 的真实指纹不一致 -> 报错退出

    // 给它足够的时间跑完流程
    tokio::time::sleep(Duration::from_secs(1)).await;

    // 断言：Client 绝对不能 Online
    cli_ctx.sink.assert_no_event("PEER_ONLINE");
}