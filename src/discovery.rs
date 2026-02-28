use bevy::prelude::*;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent};
use std::collections::HashMap;

// 服务发现插件
pub struct ServiceDiscoveryPlugin;

impl Plugin for ServiceDiscoveryPlugin {
    fn build(&self, app: &mut App) {
        // 初始化空的设备列表
        app.init_resource::<PeerList>()
            // 仅仅添加监听系统，初始化系统由 main 控制顺序
            .add_systems(Update, (listen_for_services,));
    }
}

// 初始化mdns守护进程
pub fn initialize_mdns_daemon(mut commands: Commands) {
    let daemon = ServiceDaemon::new().expect("Failed to create mDNS daemon");

    // 在启动时仅调用一次 browse
    let service_type = "_portalo._tcp.local.";
    let receiver = daemon
        .browse(service_type)
        .expect("Failed to start browsing");

    commands.insert_resource(MdnsManager { daemon, receiver });

    info!("✅ mDNS Manager & Browser started");
}

// 监听服务
fn listen_for_services(mdns: Res<MdnsManager>, mut peer_list: ResMut<PeerList>, time: Res<Time>) {
    while let Ok(event) = mdns.receiver.try_recv() {
        match event {
            // 解析
            ServiceEvent::ServiceResolved(info) => {
                info!("❌ Service resolved: {} ", info.fullname);
                let hostname = info.get_hostname().to_string();
                // 过滤出所有 IPv4 地址并转为字符串
                let ips: Vec<String> = info
                    .get_addresses()
                    .iter()
                    .filter(|ip| ip.is_ipv4())
                    .map(|ip| ip.to_string())
                    .collect();

                // 如果没搜到有效 IP 则跳过
                if ips.is_empty() {
                    continue;
                }

                // 提取 OS 信息（Dukto 风格图标显示的关键）
                let os = info
                    .get_properties()
                    .iter()
                    .find(|p| p.key() == "os")
                    .map(|p| p.val_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                // 这里将其插入 PeerList 资源，供 UI 系统遍历
                let peer = PeerInfo {
                    name: hostname.clone(),
                    ips,
                    os,
                    last_seen: time.elapsed_secs_f64(),
                };
                let name = hostname.replace(".local.", "");
                peer_list.peers.insert(name, peer);
            }
            // 断开
            ServiceEvent::ServiceRemoved(service_type, fullname) => {
                info!("❌ Service removed: {} (type: {})", fullname, service_type);

                // 逻辑：fullname 通常是 "portalo-hostname-eth0._portalo._tcp.local."
                // 我们需要提取出 hostname 部分，使其与插入时的 Key 匹配
                if let Some(instance_part) = fullname.split('.').next() {
                    // 去掉 "portalo-" 前缀
                    let raw_name = instance_part
                        .strip_prefix("portalo-")
                        .unwrap_or(instance_part);

                    // 进一步去掉后缀网卡名（如果有的话，比如 "-eth0"）
                    // 这里的逻辑要和你插入时的 Key 生成逻辑对齐
                    if let Some((name, _interface)) = raw_name.rsplit_once('-') {
                        peer_list.peers.remove(name);
                        info!("🗑️  Removed peer from list: {}", name);
                    } else {
                        // 如果没有中划线，说明 instance_name 就是 hostname
                        peer_list.peers.remove(raw_name);
                    }
                }
            }
            // 发现
            ServiceEvent::ServiceFound(service_type, fullname) => {
                debug!("🔍 Service found: {} (type: {})", fullname, service_type);
            }
            // 启动
            ServiceEvent::SearchStarted(service_type) => {
                info!("▶️  Started searching for: {}", service_type);
            }
            // 停止
            ServiceEvent::SearchStopped(service_type) => {
                info!("⏹️  Stopped searching for: {}", service_type);
            }
            // 其它
            _ => {
                trace!("Received other ServiceEvent");
            }
        }
    }
}

// 守护进程资源
#[derive(Resource)]
pub struct MdnsManager {
    pub daemon: ServiceDaemon,
    pub receiver: Receiver<ServiceEvent>,
}

// 发现的设备
#[derive(Resource, Default)]
pub struct PeerList {
    // Key 为 fullname, Value 为设备信息
    pub peers: HashMap<String, PeerInfo>,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub name: String,
    pub ips: Vec<String>,
    pub os: String,
    pub last_seen: f64,
}
