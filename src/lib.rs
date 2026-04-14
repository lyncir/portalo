use bevy::prelude::*;
use tokio::sync::mpsc;

use portalo_core::{AppState, FileTransferState, ProgressReceiver, ProgressSender};
use portalo_discovery::{ServiceDiscoveryPlugin, auto_publish_service, initialize_mdns_daemon};
use portalo_network::NetworkPlugin;
use portalo_ui::UIPlugin;

#[bevy_main] //安卓应用入口点
pub fn main() {
    let (tx, rx) = mpsc::unbounded_channel();

    App::new()
        .add_plugins(DefaultPlugins)
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
