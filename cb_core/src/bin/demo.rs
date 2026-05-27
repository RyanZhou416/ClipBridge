use cb_core::prelude::*;
use std::collections::HashSet;
use std::env;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// 用于在 Demo 内部追踪在线设备状态（因为 CLI 和 Core 是分离的）
struct DemoState {
	online_peers: HashSet<String>,
}

struct ConsoleSink {
	device_id: String,
	state: Arc<Mutex<DemoState>>,
}

impl CoreEventSink for ConsoleSink {
	fn emit(&self, event_json: String) {
		let v: serde_json::Value = match serde_json::from_str(&event_json) {
			Ok(v) => v,
			Err(_) => {
				println!("\n[RAW] {}", event_json);
				return;
			}
		};

		let type_field = v.get("type").and_then(|t| t.as_str()).unwrap_or("UNKNOWN");

		// 拦截 PEER_ONLINE / PEER_OFFLINE 更新本地状态
		if type_field == "PEER_ONLINE" {
			if let Some(did) = v
				.get("payload")
				.and_then(|p| p.get("device_id"))
				.and_then(|s| s.as_str())
			{
				let mut state = self.state.lock().unwrap();
				state.online_peers.insert(did.to_string());
			}
		} else if type_field == "PEER_OFFLINE" {
			if let Some(did) = v
				.get("payload")
				.and_then(|p| p.get("device_id"))
				.and_then(|s| s.as_str())
			{
				let mut state = self.state.lock().unwrap();
				state.online_peers.remove(did);
			}
		}

		// 格式化输出
		println!("\n---------------------------------------------------");
		println!("⚡ [{}] EVENT: {}", self.device_id, type_field);
		if let Ok(pretty) = serde_json::to_string_pretty(&v["payload"]) {
			println!("{}", pretty);
		} else {
			println!("{:?}", v);
		}
		println!("---------------------------------------------------");
		print!("(demo) > ");
		let _ = io::stdout().flush();
	}
}

fn get_temp_dirs(suffix: &str) -> (String, String) {
	let mut path = env::temp_dir();
	path.push("clipbridge_demo_v2");
	path.push(suffix);
	let data = path.join("data");
	let cache = path.join("cache");
	std::fs::create_dir_all(&data).unwrap();
	std::fs::create_dir_all(&cache).unwrap();
	(
		data.to_string_lossy().to_string(),
		cache.to_string_lossy().to_string(),
	)
}

#[tokio::main]
async fn main() {
	let args: Vec<String> = env::args().collect();
	if args.len() < 3 {
		println!("用法: cargo run --bin demo <DeviceID> <AccountPassword>");
		return;
	}

	let device_id = args[1].clone();
	let account_password = args[2].clone();
	let device_name = format!("{}_Console", device_id);

	// 状态追踪
	let state = Arc::new(Mutex::new(DemoState {
		online_peers: HashSet::new(),
	}));
	let sink = Arc::new(ConsoleSink {
		device_id: device_id.clone(),
		state: state.clone(),
	});

	let (data_dir, cache_dir) = get_temp_dirs(&device_id);
	let config = CoreConfig {
		device_id: device_id.clone(),
		device_name,
		account_uid: "demo_uid".to_string(),
		account_password,
		data_dir,
		cache_dir,
		app_config: Default::default(),
	};

	let core = Core::init(config, sink);

	println!("======================================================");
	println!("ClipBridge M1 Interactive Shell");
	println!("Device: {}", device_id);
	println!("Commands:");
	println!("  status         - 显示在线 Peer 和自身信息");
	println!("  copy <text>    - 模拟复制文本 (发送元数据)");
	println!("  spam <n>       - 快速发送 N 条数据 (压力测试)");
	println!("  quit           - 退出");
	println!("======================================================");
	print!("(demo) > ");
	io::stdout().flush().unwrap();

	let stdin = io::stdin();
	let mut line = String::new();

	loop {
		line.clear();
		if stdin.read_line(&mut line).is_err() {
			break;
		}
		let input = line.trim();
		let parts: Vec<&str> = input.splitn(2, ' ').collect();

		match parts[0] {
			"status" => {
				let s = state.lock().unwrap();
				println!("--- STATUS ---");
				println!("Online Peers Count: {}", s.online_peers.len());
				for p in &s.online_peers {
					println!(" -> 🟢 {}", p);
				}
				println!("--------------");
			}
			"copy" => {
				if parts.len() < 2 {
					println!("Usage: copy <text content>");
				} else {
					let text = parts[1].to_string();
					let snap = cb_core::clipboard::ClipboardSnapshot::Text {
						text_utf8: text,
						ts_ms: cb_core::util::now_ms(),
					};
					// 调用 Core API
					match core.ingest_local_copy(snap) {
						Ok(meta) => println!("✅ Copied. ItemID: {}", meta.item_id),
						Err(e) => println!("❌ Error: {}", e),
					}
				}
			}
			"spam" => {
				let n: usize = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
				println!("🚀 Starting SPAM test ({} items)...", n);
				for i in 0..n {
					let snap = cb_core::clipboard::ClipboardSnapshot::Text {
						text_utf8: format!("Spam Message #{}", i),
						ts_ms: cb_core::util::now_ms(),
					};
					if let Err(e) = core.ingest_local_copy(snap) {
						println!("❌ Failed at #{}: {}", i, e);
					}
					// 稍微休眠 10ms 防止瞬间把 sqlite 写锁占满 (真实 Shell 也会有间隔)
					thread::sleep(Duration::from_millis(10));
				}
				println!("✅ Spam done.");
			}
			"quit" | "exit" => {
				core.shutdown();
				break;
			}
			"" => {}
			_ => println!("Unknown command."),
		}

		print!("(demo) > ");
		io::stdout().flush().unwrap();
	}
}
