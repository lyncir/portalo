use bevy::prelude::*;
use local_ip_address::list_afinet_netifas;
use mdns_sd::ServiceInfo;
use std::net::IpAddr;

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
        let ip = "192.168.56.1";
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

// 列出可用的ip
fn list_interfaces(keyboard: Res<ButtonInput<KeyCode>>) {
    if keyboard.just_pressed(KeyCode::KeyL) {
        println!("\n{}", "=".repeat(70));
        println!("📡 Available Network Interfaces:");
        println!("{}", "=".repeat(70));

        let network_interfaces = list_afinet_netifas().unwrap();

        for (name, ipaddr) in network_interfaces.iter() {
            if matches!(ipaddr, IpAddr::V4(_)) {
                println!("{}\t{}", name, ipaddr);
            }
        }
    }
}
