use bevy::prelude::*;

use portalo_core::{AppState, FileTransferState};
use portalo_discovery::{ServiceDiscoveryPlugin, auto_publish_service, initialize_mdns_daemon};
use portalo_network::NetworkPlugin;
use portalo_ui::UIPlugin;

#[bevy_main] //安卓应用入口点
pub fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .init_state::<AppState>()
        .insert_resource(FileTransferState::new(100_000_000))
        .add_plugins(ServiceDiscoveryPlugin)
        .add_plugins(NetworkPlugin)
        .add_plugins(UIPlugin)
        .add_systems(
            Startup,
            (initialize_mdns_daemon, auto_publish_service).chain(),
        )
        .run();
}
