use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use gethostname::gethostname;
use local_ip_address::list_afinet_netifas;
use mdns_sd::ServiceInfo;

mod discovery;
mod file_receiver;
mod file_send;
mod network;

use discovery::{MdnsManager, PeerList, ServiceDiscoveryPlugin};
use network::{NetworkPlugin, TokioRuntime};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(ServiceDiscoveryPlugin)
        .add_plugins(NetworkPlugin)
        .add_systems(
            Startup,
            (
                setup,
                discovery::initialize_mdns_daemon,
                auto_publish_service,
            )
                .chain(),
        )
        .add_systems(Update, (list_interfaces, update_peer_list_ui))
        .add_systems(Last, shutdown_service)
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

    // 2D摄像机
    commands.spawn(Camera2d);

    // ui
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(30.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.10, 0.10, 0.10)),
        ))
        .with_children(|parent| {
            // 顶部标题
            parent.spawn((
                Text::new("PORTALO"),
                TextFont::from_font_size(32.0),
                TextColor(Color::WHITE),
                Node {
                    margin: UiRect::bottom(Val::Px(20.0)),
                    ..default()
                },
            ));

            // 设备列表滚动/显示区域
            parent.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    flex_grow: 1.0,
                    width: Val::Percent(100.0),
                    row_gap: Val::Px(10.0), // 节点间距
                    ..default()
                },
                PeerListContent,
            ));
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
            }
        }
        println!("\n{}\n", "=".repeat(70));

        // 发送文件
        let dest = format!("{}:49527", "192.168.56.133");
        let path = "/tmp/1.txt";

        let handle = runtime.0.handle().clone();
        handle.spawn(async move {
            info!("🚀 Starting transfer to {}...", dest);
            if let Err(e) = file_send::send_file_fast(dest, path.to_string()).await {
                error!("❌ Transfer failed: {}", e);
            }
        });
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

// 存储设备的唯一标识（如 hostname）
#[derive(Component)]
struct PeerEntry(String);

// 列表滚动区域容器
#[derive(Component)]
struct PeerListContent;

fn update_peer_list_ui(
    mut commands: Commands,
    peer_list: Res<PeerList>,
    container_q: Query<Entity, With<PeerListContent>>,
    entry_q: Query<(Entity, &PeerEntry)>,
) {
    let Ok(container_entity) = container_q.single() else {
        return;
    };

    // 1. 获取当前 UI 中已有的设备 ID
    let mut ui_peers: std::collections::HashSet<String> =
        entry_q.iter().map(|(_, e)| e.0.clone()).collect();

    // 2. 遍历资源中的 Peers
    for (id, info) in peer_list.peers.iter() {
        if !ui_peers.remove(id) {
            // 如果 UI 里没有，创建它
            let new_entry = commands
                .spawn((
                    PeerEntry(id.clone()),
                    Button,
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(70.0),
                        padding: UiRect::horizontal(Val::Px(15.0)),
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.12, 0.12, 0.12)),
                ))
                //.observe(
                //    |trigger: Trigger<Pointer<Over>>, mut q: Query<&mut BackgroundColor>| {
                //        if let Ok(mut bg) = q.get_mut(trigger.entity()) {
                //            bg.0 = Color::srgb(0.2, 0.6, 0.2); // 悬停变绿
                //        }
                //    },
                //)
                //.observe(
                //    |trigger: Trigger<Pointer<Out>>, mut q: Query<&mut BackgroundColor>| {
                //        if let Ok(mut bg) = q.get_mut(trigger.entity()) {
                //            bg.0 = Color::srgb(0.12, 0.12, 0.12); // 恢复原色
                //        }
                //    },
                //)
                .with_children(|p| {
                    // 图标
                    p.spawn((
                        Node {
                            width: Val::Px(40.0),
                            height: Val::Px(40.0),
                            margin: UiRect::right(Val::Px(15.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.3, 0.3, 0.3)),
                    ));
                    // 文字信息
                    p.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        ..default()
                    })
                    .with_children(|inner| {
                        inner.spawn((
                            Text::new(id.clone()),
                            TextFont::from_font_size(18.0),
                            TextColor(Color::WHITE),
                        ));
                        inner.spawn((
                            Text::new(format!("IP: {}", info.ips.join(", "))),
                            TextFont::from_font_size(12.0),
                            TextColor(Color::srgb(0.6, 0.6, 0.6)),
                        ));
                    });
                })
                .id();

            commands.entity(container_entity).add_child(new_entry);
        }
    }

    // 3. 剩下的 ui_peers 说明已经在 mDNS 中消失，需要从 UI 删除
    for (entity, entry) in entry_q.iter() {
        if ui_peers.contains(&entry.0) {
            commands.entity(entity).despawn();
        }
    }
}

fn shutdown_service(
    exit: MessageReader<AppExit>,
    mdns: Res<MdnsManager>,
    device_metadata: Res<DeviceMetadata>,
) {
    if !exit.is_empty() {
        let service_type = "_portalo._tcp.local.";
        // 必须与注册时的 instance_name 完全一致
        if let Ok(network_interfaces) = list_afinet_netifas() {
            for (name, ip) in network_interfaces {
                // 过滤：非回环且IPv4接口
                if !ip.is_loopback() && ip.is_ipv4() {
                    // 为每个网卡创建一个唯一的实例名称（Dukto 识别需要）
                    let instance_name =
                        format!("portalo-{}-{}", device_metadata.hostname.clone(), name);

                    let full_name = format!("{}.{}", instance_name, service_type);
                    // 注销
                    if let Err(e) = mdns.daemon.unregister(&full_name) {
                        error!("❌ 无法注销服务: {}", e);
                    } else {
                        info!("👋 服务已主动下线: {}", full_name);
                    }
                }
            }
        }
    }
}

// 清理异常的peer
fn cleanup_stale_peers(
    mut peer_list: ResMut<PeerList>,
    time: Res<Time>,
    device_metadata: Res<DeviceMetadata>,
) {
    let now = time.elapsed_secs_f64();
    let timeout = 30.0; // 30秒超时

    // retain 会保留返回 true 的项，移除返回 false 的项
    peer_list.peers.retain(|name, info| {
        // 自己或有效期内
        let is_alive =
            (name.to_string() == device_metadata.hostname) || ((now - info.last_seen) < timeout);

        if !is_alive {
            info!("⏳ Peer timed out: {}", name);
        }
        is_alive
    });
}
