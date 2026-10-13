use bevy::tasks::{IoTaskPool, Task, block_on, poll_once};
use bevy::{app::AppExit, prelude::*, time::common_conditions::on_real_timer as on_timer};
use local_ip_address::list_afinet_netifas;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const SERVICE: &str = "_portalo._tcp.local.";
type NetworkInterfaces = Vec<(String, std::net::IpAddr)>;
pub struct ServiceDiscoveryPlugin;
impl Plugin for ServiceDiscoveryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PeerList>()
            .init_resource::<Instances>()
            .add_systems(
                Update,
                (
                    refresh_interfaces,
                    listen_for_service,
                    keep_alive_broadcast.run_if(on_timer(Duration::from_secs(30))),
                    cleanup_expired_peers.run_if(on_timer(Duration::from_secs(5))),
                )
                    .chain(),
            )
            .add_systems(Last, shutdown_service);
    }
}

#[derive(Resource)]
pub struct MdnsManager {
    pub daemon: ServiceDaemon,
    pub receiver: Receiver<ServiceEvent>,
    registered: HashMap<String, ServiceInfo>,
    interfaces: NetworkInterfaces,
    hostname: String,
    interface_scan: Option<Task<Option<NetworkInterfaces>>>,
}

#[derive(Resource, Default)]
pub struct PeerList {
    pub peers: HashMap<String, PeerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub name: String,
    pub ips: Vec<String>,
    pub os: String,
    pub port: u16,
}

struct Instance {
    peer: PeerInfo,
    last_seen: Instant,
}
#[derive(Resource, Default)]
struct Instances(HashMap<String, Instance>);

pub fn initialize_mdns_daemon(mut commands: Commands) {
    let hostname: String = portalo_core::device::device_name()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let daemon = ServiceDaemon::new().expect("mDNS daemon");
    let receiver = daemon.browse(SERVICE).expect("mDNS browse");
    commands.insert_resource(MdnsManager {
        daemon,
        receiver,
        hostname,
        registered: HashMap::new(),
        interfaces: Vec::new(),
        interface_scan: None,
    });
}

pub fn auto_publish_service(mut mdns: ResMut<MdnsManager>) {
    start_interface_scan(&mut mdns);
}

fn start_interface_scan(mdns: &mut MdnsManager) {
    if mdns.interface_scan.is_none() {
        mdns.interface_scan = Some(IoTaskPool::get().spawn(async { list_afinet_netifas().ok() }));
    }
}

fn refresh_interfaces(mut mdns: ResMut<MdnsManager>) {
    let Some(task) = mdns.interface_scan.as_mut() else {
        return;
    };
    let Some(result) = block_on(poll_once(task)) else {
        return;
    };
    mdns.interface_scan = None;
    if let Some(mut interfaces) = result {
        interfaces.retain(|(_, ip)| !ip.is_loopback() && ip.is_ipv4());
        interfaces.sort();
        if mdns.interfaces != interfaces {
            mdns.interfaces = interfaces;
            publish(&mut mdns);
        }
    }
}

fn publish(mdns: &mut MdnsManager) {
    // Heartbeats use cached interfaces; OS enumeration runs on the I/O pool.
    let heartbeat = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    let mut registered = HashMap::new();
    for (interface, ip) in &mdns.interfaces {
        let properties = [
            ("os", std::env::consts::OS),
            ("ver", "1"),
            ("hb", heartbeat.as_str()),
        ];
        let service = ServiceInfo::new(
            SERVICE,
            &format!("portalo-{}-{interface}", mdns.hostname),
            &format!("{}.local.", mdns.hostname),
            *ip,
            49527,
            &properties[..],
        );
        match service {
            Ok(service) => {
                let name = service.get_fullname().to_owned();
                if let Err(e) = mdns.daemon.register(service.clone()) {
                    warn!("mDNS registration: {e}");
                }
                registered.insert(name, service);
            }
            Err(e) => warn!("mDNS service: {e}"),
        }
    }
    for name in mdns.registered.keys() {
        if !registered.contains_key(name) {
            let _ = mdns.daemon.unregister(name);
        }
    }
    mdns.registered = registered;
}

fn rebuild(instances: &Instances, peers: &mut PeerList) {
    let mut next: HashMap<String, PeerInfo> = HashMap::new();
    for instance in instances.0.values() {
        let peer = &instance.peer;
        let key = format!("{}:{}", peer.name, peer.port);
        let entry = next.entry(key).or_insert_with(|| PeerInfo {
            ips: Vec::new(),
            ..peer.clone()
        });
        entry.ips.extend(peer.ips.iter().cloned());
    }
    for peer in next.values_mut() {
        peer.ips.sort();
        peer.ips.dedup();
    }
    if next != peers.peers {
        peers.peers = next;
    }
}

fn listen_for_service(
    mdns: Res<MdnsManager>,
    mut instances: ResMut<Instances>,
    mut peers: ResMut<PeerList>,
) {
    let mut changed = false;
    // Bound work per frame even when several interfaces announce simultaneously.
    for _ in 0..64 {
        let Ok(event) = mdns.receiver.try_recv() else {
            break;
        };
        match event {
            ServiceEvent::ServiceResolved(info) => {
                if mdns.registered.contains_key(&info.fullname) {
                    continue;
                }
                let mut ips: Vec<_> = info
                    .get_addresses()
                    .iter()
                    .filter(|ip| ip.is_ipv4())
                    .map(|ip| ip.to_string())
                    .collect();
                ips.sort();
                ips.dedup();
                if ips.is_empty() {
                    continue;
                }
                let peer = PeerInfo {
                    name: info.get_hostname().to_owned(),
                    ips,
                    port: info.get_port(),
                    os: info
                        .get_properties()
                        .iter()
                        .find(|p| p.key() == "os")
                        .map(|p| p.val_str().to_owned())
                        .unwrap_or_default(),
                };
                changed |= instances
                    .0
                    .get(&info.fullname)
                    .is_none_or(|old| old.peer != peer);
                instances.0.insert(
                    info.fullname,
                    Instance {
                        peer,
                        last_seen: Instant::now(),
                    },
                );
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                changed |= instances.0.remove(&fullname).is_some();
            }
            _ => {}
        }
    }
    if changed {
        // Avoid marking the public resource changed for heartbeat-only updates.
        let mut candidate = PeerList::default();
        rebuild(&instances, &mut candidate);
        if candidate.peers != peers.peers {
            peers.peers = candidate.peers;
        }
    }
}

fn cleanup_expired_peers(mut instances: ResMut<Instances>, mut peers: ResMut<PeerList>) {
    let before = instances.0.len();
    instances
        .0
        .retain(|_, instance| instance.last_seen.elapsed() < Duration::from_secs(100));
    if instances.0.len() != before {
        let mut candidate = PeerList::default();
        rebuild(&instances, &mut candidate);
        if candidate.peers != peers.peers {
            peers.peers = candidate.peers;
        }
    }
}

fn keep_alive_broadcast(mut mdns: ResMut<MdnsManager>) {
    publish(&mut mdns);
    start_interface_scan(&mut mdns);
}

fn shutdown_service(exit: MessageReader<AppExit>, mdns: Res<MdnsManager>) {
    if !exit.is_empty() {
        for name in mdns.registered.keys() {
            let _ = mdns.daemon.unregister(name);
        }
        let _ = mdns.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_one_interface_keeps_other_addresses() {
        let mut instances = Instances::default();
        for (interface, ip) in [("eth0", "192.168.1.2"), ("wlan0", "192.168.2.2")] {
            instances.0.insert(
                interface.into(),
                Instance {
                    peer: PeerInfo {
                        name: "laptop.local.".into(),
                        ips: vec![ip.into()],
                        os: "linux".into(),
                        port: 1234,
                    },
                    last_seen: Instant::now(),
                },
            );
        }
        let mut peers = PeerList::default();
        rebuild(&instances, &mut peers);
        assert_eq!(peers.peers.len(), 1);
        assert_eq!(peers.peers.values().next().unwrap().ips.len(), 2);
        instances.0.remove("eth0");
        rebuild(&instances, &mut peers);
        assert_eq!(peers.peers.values().next().unwrap().ips, ["192.168.2.2"]);
        instances.0.clear();
        rebuild(&instances, &mut peers);
        assert!(peers.peers.is_empty());
    }
}
