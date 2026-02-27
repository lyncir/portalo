use bevy::prelude::*;
use gethostname::gethostname;
use local_ip_address::list_afinet_netifas;
use mdns_sd::ServiceInfo;

mod discovery;

use discovery::{MdnsManager, PeerList, ServiceDiscoveryPlugin};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(ServiceDiscoveryPlugin)
        .add_systems(
            Startup,
            (
                setup,
                discovery::initialize_mdns_daemon,
                auto_publish_service,
            )
                .chain(),
        )
        .add_systems(Update, list_interfaces)
        .run();
}

fn setup(mut commands: Commands) {
    info!("🚀 Portalo (Dukto Remake) Ready");
    println!("Controls: [P] Publish Service | [L] List Interfaces");

    // 获取主机名字
    let hostname = get_cross_platform_hostname();
    commands.insert_resource(DeviceMetadata {
        hostname: hostname,
        os: std::env::consts::OS.to_string(),
    });
}

// 发布服务
fn auto_publish_service(mdns_res: Res<MdnsManager>, device_metadata: Res<DeviceMetadata>) {
    let service_type = "_portalo._tcp.local.";
    let port = 5353;
    let hostname_suffix = get_hostname_with_suffix(&device_metadata.hostname);

    if let Ok(network_interfaces) = list_afinet_netifas() {
        for (name, ip) in network_interfaces {
            // 过滤：非回环且IPv4接口
            if !ip.is_loopback() && ip.is_ipv4() {
                // 为每个网卡创建一个唯一的实例名称（Dukto 识别需要）
                let instance_name =
                    format!("portalo-{}-{}", device_metadata.hostname.clone(), name);
                // 额外信息
                let properties = [
                    ("os", device_metadata.os.clone()),
                    ("ver", "0.1.0".to_string()),
                ];

                let my_service = ServiceInfo::new(
                    service_type,
                    &instance_name,
                    &hostname_suffix,
                    ip,
                    port,
                    &properties[..],
                )
                .unwrap();

                // 注册
                if let Err(e) = mdns_res.daemon.register(my_service) {
                    error!("❌ Failed to register on {}: {}", name, e);
                } else {
                    info!("✨ Published Portalo on {} ({})", name, ip);
                }
            }
        }
    }
}

// 列出已发现的服务
fn list_interfaces(keyboard: Res<ButtonInput<KeyCode>>, peer_list: Res<PeerList>) {
    if keyboard.just_pressed(KeyCode::KeyL) {
        println!("\n{}", "=".repeat(70));
        println!("📋 Discovered Services (Total: {})", peer_list.peers.len());
        println!("{}", "=".repeat(70));

        if peer_list.peers.is_empty() {
            println!("  No services discovered yet");
        } else {
            for (idx, (name, info)) in peer_list.peers.iter().enumerate() {
                println!("\n{}. {}", idx + 1, info.name);
                println!("   Addresses: {}", info.ips.join(", "));
                println!("   OS: {}", info.os);
            }
        }
        println!("\n{}\n", "=".repeat(70));
    }
}

// 设备信息
#[derive(Resource)]
struct DeviceMetadata {
    // 主机名字
    pub hostname: String,
    pub os: String,
}

// 获取主机名字 eg: devuan
fn get_cross_platform_hostname() -> String {
    let raw_name = gethostname()
        .into_string()
        .unwrap_or_else(|_| "PortaloDevice".into());

    // 移动端处理：转义空格和特殊字符，确保 mDNS 兼容
    raw_name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
}

// 获取带后缀的主机名(用于mdns) eg: devuan.local.
fn get_hostname_with_suffix(hostname: &str) -> String {
    format!("{}.local.", hostname.trim_end_matches('.'))
}
