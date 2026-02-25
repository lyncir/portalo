use bevy::prelude::*;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

mod discovery;
mod service;

use discovery::ServiceDiscoveryPlugin;
use service::ServicePublisher;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        // ✅ 添加服务发现插件
        .add_plugins(ServiceDiscoveryPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (publish_service, discover_services))
        .run();
}

#[derive(Resource)]
struct ServiceState {
    published_services: Vec<String>,
}

fn setup(mut commands: Commands) {
    commands.insert_resource(ServiceState {
        published_services: Vec::new(),
    });

    info!("🚀 Service Discovery initialized");
}

fn publish_service(mut state: ResMut<ServiceState>, keyboard: Res<ButtonInput<KeyCode>>) {
    if keyboard.just_pressed(KeyCode::KeyP) {
        let publisher = ServicePublisher::new("MyGameService", "_game._tcp.local.", 5353)
            .add_property("version".to_string(), "1.0".to_string())
            .add_property("platform".to_string(), "bevy".to_string());

        match publisher.publish() {
            Ok(service_name) => {
                state.published_services.push(service_name.clone());
                info!("✅ Published service: {}", service_name);
            }
            Err(e) => {
                error!("❌ Failed to publish service: {}", e);
            }
        }
    }
}

fn discover_services(
    keyboard: Res<ButtonInput<KeyCode>>,
    discovered: Res<discovery::DiscoveredServices>,
) {
    if keyboard.just_pressed(KeyCode::KeyD) {
        info!("📋 Current discovered services:");
        for service in &discovered.services {
            info!(
                "  - {} at {}:{}",
                service.name,
                service.addresses.join(", "),
                service.port
            );

            if !service.properties.is_empty() {
                for (key, value) in &service.properties {
                    info!("    - {}: {}", key, value);
                }
            }
        }

        if discovered.services.is_empty() {
            info!("  No services discovered yet");
        }
    }
}
