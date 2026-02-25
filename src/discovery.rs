use bevy::prelude::*;
use mdns_sd::ServiceDaemon;
use std::sync::{Arc, Mutex};

pub struct ServiceDiscoveryPlugin;

impl Plugin for ServiceDiscoveryPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, initialize_mdns_daemon)
            .add_systems(Update, listen_for_services);
    }
}

#[derive(Resource)]
pub struct MdnsDaemon {
    pub daemon: Arc<Mutex<Option<ServiceDaemon>>>,
}

#[derive(Resource, Default)]
pub struct DiscoveredServices {
    pub services: Vec<DiscoveredService>,
}

#[derive(Clone, Debug)]
pub struct DiscoveredService {
    pub name: String,
    pub service_type: String,
    pub addresses: Vec<String>,
    pub port: u16,
    pub properties: Vec<(String, String)>,
}

fn initialize_mdns_daemon(mut commands: Commands) {
    match ServiceDaemon::new() {
        Ok(daemon) => {
            commands.insert_resource(MdnsDaemon {
                daemon: Arc::new(Mutex::new(Some(daemon))),
            });
            info!("mDNS daemon initialized successfully");
        }
        Err(e) => {
            error!("Failed to initialize mDNS daemon: {}", e);
            commands.insert_resource(MdnsDaemon {
                daemon: Arc::new(Mutex::new(None)),
            });
        }
    }

    commands.insert_resource(DiscoveredServices::default());
}

fn listen_for_services(mdns: Res<MdnsDaemon>, mut discovered: ResMut<DiscoveredServices>) {
    if let Ok(daemon_guard) = mdns.daemon.lock() {
        if let Some(ref daemon) = *daemon_guard {
            // 搜索特定的服务类型
            if let Ok(receiver) = daemon.browse("_game._tcp.local.") {
                while let Ok(event) = receiver.try_recv() {
                    match event {
                        mdns_sd::ServiceEvent::ServiceResolved(info) => {
                            let service = DiscoveredService {
                                name: info.get_fullname().to_string(),
                                service_type: "_game._tcp".to_string(),
                                addresses: info
                                    .get_addresses()
                                    .iter()
                                    .map(|ip| ip.to_string())
                                    .collect(),
                                port: info.get_port(),
                                properties: info
                                    .get_properties()
                                    .iter()
                                    .map(|prop| {
                                        (prop.key().to_string(), prop.val_str().to_string())
                                    })
                                    .collect(),
                            };

                            info!(
                                "Discovered service: {} at {}:{}",
                                service.name,
                                service.addresses.join(", "),
                                service.port
                            );

                            discovered.services.push(service);
                        }
                        mdns_sd::ServiceEvent::ServiceRemoved(service_type, fullname) => {
                            info!("Service removed: {} (type: {})", fullname, service_type);

                            // 从发现列表中移除该服务
                            discovered.services.retain(|s| s.name != fullname);
                        }
                        mdns_sd::ServiceEvent::ServiceFound(service_type, fullname) => {
                            info!("Service found: {} (type: {})", fullname, service_type);
                        }
                        mdns_sd::ServiceEvent::SearchStarted(service_type) => {
                            info!("Started searching for service type: {}", service_type);
                        }
                        mdns_sd::ServiceEvent::SearchStopped(service_type) => {
                            info!("Stopped searching for service type: {}", service_type);
                        }
                        // ✅ 添加 catch-all 模式来处理未来可能添加的新变体
                        _ => {
                            debug!("Received unknown ServiceEvent");
                        }
                    }
                }
            }
        }
    }
}
