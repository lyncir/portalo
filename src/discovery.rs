use bevy::prelude::*;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent};

// 服务发现插件
pub struct ServiceDiscoveryPlugin;

impl Plugin for ServiceDiscoveryPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, initialize_mdns_daemon)
            .add_systems(Update, listen_for_services);
    }
}

// 初始化mdns守护进程
fn initialize_mdns_daemon(mut commands: Commands) {
    let daemon = ServiceDaemon::new().expect("Failed to create mDNS daemon");

    // 在启动时仅调用一次 browse
    let service_type = "_game._tcp.local.";
    let receiver = daemon
        .browse(service_type)
        .expect("Failed to start browsing");

    commands.insert_resource(MdnsManager { daemon, receiver });

    info!("✅ mDNS Manager & Browser started");
}

// 监听服务
fn listen_for_services(mdns: Res<MdnsManager>) {
    while let Ok(event) = mdns.receiver.try_recv() {
        match event {
            // 解析
            ServiceEvent::ServiceResolved(info) => {
                info!("❌ Service resolved: {} ", info.fullname);
            }
            // 断开
            ServiceEvent::ServiceRemoved(service_type, fullname) => {
                info!("❌ Service removed: {} (type: {})", fullname, service_type);
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
