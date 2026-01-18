// cb_core/src/discovery/mod.rs

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task;

/// 文档 3.4.2.1 定义的服务类型 (QUIC)
const SERVICE_TYPE: &str = "_clipbridge._udp.local.";

/// 发现层输出的事件，通知 Supervisor 更新候选列表
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// 发现新设备或地址更新 (仅线索，未验证)
    CandidateFound(PeerCandidate),
    /// 设备可能已离线 (mDNS 宣告 TTL 过期)
    CandidateLost(String), // device_id
}

/// 文档 3.3.8.1: 来自 Discovery 的候选信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCandidate {
    pub device_id: String,
    pub addrs: Vec<String>, // "ip:port"
    pub capabilities: Vec<String>,
    // last_seen 不需要在此传输，接收方收到即视为当前在线
}

pub struct DiscoveryService {
    /// 用于停止 mDNS 任务
    shutdown_tx: mpsc::Sender<()>,
}


/// 纯函数：尝试从 mDNS ServiceInfo 中解析出 PeerCandidate
/// 如果 account_uid 不匹配或缺少必要字段，返回 None
fn parse_peer_candidate(info: &Box<ResolvedService>, local_account_uid: &str, local_device_id: &str) -> Option<PeerCandidate> {
    // 排除自己
    if info.get_fullname().contains(local_device_id) {
        return None;
    }

    // 检查 account_uid
    let peer_acct = info.get_property_val_str("acct").unwrap_or("");
    if peer_acct != local_account_uid {
        return None;
    }

    let peer_did = info.get_property_val_str("did").unwrap_or("");
    if peer_did.is_empty() {
        return None;
    }

    let port = info.get_port();
    let addrs: Vec<String> = info
        .get_addresses()
        .iter()
        .map(|ip| {
            // 1. 获取原始字符串
            // mdns-sd 的 to_string() 对于 IPv6 Link-Local 会自动带上 %scope_id
            // 例如: "fe80::1234:5678%en0" 或 "192.168.1.5"
            let ip_str = ip.to_string();

            // 2. 根据是否包含冒号判断 IPv6
            if ip_str.contains(':') {
                // IPv6: 必须加上方括号 [ ]
                // 结果示例: "[fe80::1234:5678%en0]:8080"
                format!("[{}]:{}", ip_str, port)
            } else {
                // IPv4: 直接拼接
                // 结果示例: "192.168.1.5:8080"
                format!("{}:{}", ip_str, port)
            }
        })
        .collect();

    if addrs.is_empty() {
        return None;
    }

    let caps_str = info.get_property_val_str("cap").unwrap_or("");
    let capabilities: Vec<String> = caps_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Some(PeerCandidate {
        device_id: peer_did.to_string(),
        addrs,
        capabilities,
    })
}

impl DiscoveryService {
    /// 启动发现服务 (发布 + 监听)
    ///
    /// - `cfg`: 本机配置 (用于发布自己的 info)
    /// - `event_tx`: 发送发现结果的通道
    pub fn spawn(
        cfg: crate::api::CoreConfig,
        port: u16, // <--- 新增
        event_tx: mpsc::Sender<DiscoveryEvent>,
    ) -> anyhow::Result<Self> {
        let mdns = ServiceDaemon::new()?;

        let hostname = format!("{}.local.", cfg.device_id);


        let properties = [
            ("acct", cfg.account_uid.as_str()),
            ("did", cfg.device_id.as_str()),
            ("proto", "1"),
            ("cap", "txt,img,file"),
        ];

        let my_service = ServiceInfo::new(
            SERVICE_TYPE,
            &cfg.device_id,
            &hostname,
            "",
            port,
            &properties[..],
        )?.enable_addr_auto();

        mdns.register(my_service)?;

        // 4. 启动监听任务
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let receiver = mdns.browse(SERVICE_TYPE)?;
        let local_device_id = cfg.device_id.clone();
        let local_account_uid = cfg.account_uid.clone();

        task::spawn(async move {
            // println!("[Discovery] Started browsing: {}", SERVICE_TYPE);

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        let _ = mdns.shutdown();
                        break;
                    }

                    event = async { receiver.recv_async().await } => {
                        match event {
                            Ok(ServiceEvent::ServiceResolved(info)) => {
                                if let Some(candidate) = parse_peer_candidate(&info, &local_account_uid, &local_device_id) {
                                     if event_tx.send(DiscoveryEvent::CandidateFound(candidate)).await.is_err() {
                                         break;
                                     }
                                }
                            }
                            Ok(ServiceEvent::ServiceRemoved(_srv_type, fullname)) => {

                                let parts: Vec<&str> = fullname.split('.').collect();
                                if let Some(instance) = parts.first() {
                                    if *instance != local_device_id {
                                         let _ = event_tx.send(DiscoveryEvent::CandidateLost(instance.to_string())).await;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        Ok(Self { shutdown_tx })
    }

    /// 停止发现服务
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(()).await;
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_discovery_parsing_match() {
        let mut properties = std::collections::HashMap::new();
        properties.insert("acct".to_string(), "tag_ok".to_string());
        properties.insert("did".to_string(), "dev_b".to_string());
        properties.insert("cap".to_string(), "txt,img".to_string());


    }

    #[test]
    fn test_cap_parsing() {
        let caps_str = "txt,img,,file";
        let capabilities: Vec<String> = caps_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(capabilities, vec!["txt", "img", "file"]);
    }
}
