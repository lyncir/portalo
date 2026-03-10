use bevy::prelude::*;

use portalo_discovery::{
    auto_publish_service, initialize_mdns_daemon, keep_alive_broadcast, DeviceMetadata, PeerList,
    ServiceDiscoveryPlugin,
};
use portalo_network::{send_file_fast, NetworkPlugin, TokioRuntime};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(ServiceDiscoveryPlugin)
        .add_plugins(NetworkPlugin)
        .add_systems(
            Startup,
            (setup, initialize_mdns_daemon, auto_publish_service).chain(),
        )
        .add_systems(Update, (list_interfaces, update_peer_list_ui))
        .run();
}

fn setup(mut commands: Commands) {
    info!("🚀 Portalo (Dukto Remake) Ready");
    println!("Controls: [P] Publish Service | [L] List Interfaces");

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
        //let dest = format!("{}:49527", "192.168.56.133");
        //let path = "/tmp/1.txt";

        //let handle = runtime.0.handle().clone();
        //handle.spawn(async move {
        //    info!("🚀 Starting transfer to {}...", dest);
        //    if let Err(e) = send_file_fast(dest, path.to_string()).await {
        //        error!("❌ Transfer failed: {}", e);
        //    }
        //});
    }
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
