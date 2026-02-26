use bevy::prelude::*;
use mdns_sd::ServiceInfo;

mod discovery;

use discovery::{MdnsManager, ServiceDiscoveryPlugin};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(ServiceDiscoveryPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (publish_service, list_interfaces))
        .run();
}

fn setup() {
    info!("🚀 Portalo (Dukto Remake) Ready");
    println!("Controls: [P] Publish Service | [L] List Interfaces");
}

// 发布服务
fn publish_service(keyboard: Res<ButtonInput<KeyCode>>, mdns_res: Res<MdnsManager>) {
    if keyboard.just_pressed(KeyCode::KeyP) {
        let service_type = "_game._tcp.local.";
        let instance_name = "my_instance";
        let ip = "10.6.31.50";
        let hostname = format!("{}.local.", ip);
        let port = 5353;
        let properties = [("property_1", "test"), ("property_2", "1234")];

        let my_service = ServiceInfo::new(
            service_type,
            instance_name,
            &hostname,
            ip,
            port,
            &properties[..],
        )
        .unwrap();

        mdns_res
            .daemon
            .register(my_service)
            .expect("Failed to register our service");
    }
}

// 列出所有的网卡
fn list_interfaces(keyboard: Res<ButtonInput<KeyCode>>) {
    if keyboard.just_pressed(KeyCode::KeyL) {
        use pnet::datalink;

        println!("\n{}", "=".repeat(70));
        println!("📡 Available Network Interfaces:");
        println!("{}", "=".repeat(70));

        let interfaces = datalink::interfaces();
        if interfaces.is_empty() {
            println!("  No network interfaces found");
        } else {
            for (idx, iface) in interfaces.iter().enumerate() {
                println!("\n{}. {}", idx + 1, iface.name);
                println!(
                    "   Status: {}",
                    if iface.is_up() {
                        "🟢 UP"
                    } else {
                        "🔴 DOWN"
                    }
                );
                println!(
                    "   Loopback: {}",
                    if iface.is_loopback() { "Yes" } else { "No" }
                );

                if !iface.ips.is_empty() {
                    println!("   IP Addresses:");
                    for ip in &iface.ips {
                        println!("     • {} (/{}/)", ip.ip(), ip.prefix());
                    }
                }

                if let Some(mac) = iface.mac {
                    println!("   MAC: {}", mac);
                }
            }
        }
        println!("\n{}\n", "=".repeat(70));
    }
}
