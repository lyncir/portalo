use bevy::prelude::*;
use tokio::sync::watch;

use portalo_core::{AppState, FileTransferState, ProgressReceiver, ProgressSender};
use portalo_discovery::{ServiceDiscoveryPlugin, auto_publish_service, initialize_mdns_daemon};
use portalo_network::NetworkPlugin;
use portalo_ui::UIPlugin;

#[bevy_main] //安卓应用入口点
pub fn main() {
    let (tx, rx) = watch::channel(Default::default());

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Portalo · 局域网文件传输".into(),
                resolution: (760, 640).into(),
                resize_constraints: bevy::window::WindowResizeConstraints {
                    min_width: 320.0,
                    min_height: 420.0,
                    ..default()
                },
                ..default()
            }),
            ..default()
        }))
        .insert_resource(bevy::winit::WinitSettings {
            focused_mode: bevy::winit::UpdateMode::reactive_low_power(
                std::time::Duration::from_millis(100),
            ),
            unfocused_mode: bevy::winit::UpdateMode::reactive_low_power(
                std::time::Duration::from_secs(1),
            ),
        })
        .init_state::<AppState>()
        .insert_resource(FileTransferState::new(0))
        .insert_resource(ProgressSender(tx))
        .insert_resource(ProgressReceiver(rx))
        .add_plugins(ServiceDiscoveryPlugin)
        .add_plugins(NetworkPlugin)
        .add_plugins(UIPlugin)
        .add_systems(
            Startup,
            (initialize_mdns_daemon, auto_publish_service).chain(),
        )
        .run();
}
