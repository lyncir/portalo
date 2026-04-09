use bevy::prelude::*;

use portalo_discovery::{
    PeerList, ServiceDiscoveryPlugin, auto_publish_service, initialize_mdns_daemon,
};
use portalo_network::{NetworkPlugin, TokioRuntime, send_file_fast};
use portalo_ui::UIPlugin;

#[bevy_main] //安卓应用入口点
pub fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(ServiceDiscoveryPlugin)
        .add_plugins(NetworkPlugin)
        .add_plugins(UIPlugin)
        .add_systems(
            Startup,
            (initialize_mdns_daemon, auto_publish_service).chain(),
        )
        .add_systems(Update, list_interfaces)
        .run();
}

// 列出已发现的服务
fn list_interfaces(
    keyboard: Res<ButtonInput<KeyCode>>,
    peer_list: Res<PeerList>,
    runtime: Res<TokioRuntime>,
) {
    if keyboard.just_pressed(KeyCode::KeyL) {
        println!("\n{}", "=".repeat(70));
        println!("📋 Discovered Services (Total: {})", peer_list.peers.len());
        println!("{}", "=".repeat(70));

        if peer_list.peers.is_empty() {
            println!("  No services discovered yet");
        } else {
            for (idx, (name, info)) in peer_list.peers.iter().enumerate() {
                println!("\n{}. {}", idx + 1, name);
                println!("   Addresses: {}", info.ips.join(", "));
                println!("   OS: {}", info.os);
                println!("   Last: {}", info.last_seen);
            }
        }
        println!("\n{}\n", "=".repeat(70));

        // 发送文件
        let dest = format!("{}:49527", "192.168.56.133");
        let path = "/tmp/1.txt";

        let handle = runtime.0.handle().clone();
        handle.spawn(async move {
            info!("🚀 Starting transfer to {}...", dest);
            if let Err(e) = send_file_fast(dest, path.to_string()).await {
                error!("❌ Transfer failed: {}", e);
            }
        });
    }
}
