use bevy::prelude::*;
use bevy::{
    input::mouse::{MouseScrollUnit, MouseWheel},
    picking::hover::HoverMap,
};
use bevy_file_dialog::prelude::*;
use portalo_discovery::{PeerInfo, PeerList};

// UI插件
// --------------- PLUGIN --------------- //
pub struct UIPlugin;

impl Plugin for UIPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FileDialogPlugin::new().with_pick_file::<PrintFilePath>())
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (update_peer_list_ui, send_scroll_events, file_picked),
            )
            .add_observer(on_scroll_handler);
    }
}

// --------------- SETUP --------------- //
fn setup(mut commands: Commands, mut peer_list: ResMut<PeerList>) {
    // 2D摄像机
    commands.spawn(Camera2d);

    // UI
    commands
        .spawn(
            // 容器: 宽度自适应
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                margin: UiRect {
                    left: auto(),
                    right: auto(),
                    top: Val::ZERO,
                    bottom: Val::ZERO,
                },
                ..default()
            },
        )
        .with_children(|parent| {
            // 上部分 - 标题
            parent
                .spawn((
                    // 靠左
                    Node {
                        width: percent(100),
                        height: px(100),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::Center,
                        padding: UiRect::all(px(20)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.2, 0.3, 0.5)),
                ))
                .with_children(|parent| {
                    parent.spawn((
                        Text::new("Portalo"),
                        TextFont {
                            font_size: 32.0,
                            ..default()
                        },
                        TextColor::WHITE,
                    ));
                });

            // 中部分 - 设备列表（可滑动）
            parent.spawn((
                Node {
                    width: percent(100),
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::scroll_y(), // 滑动
                    row_gap: Val::Px(10.0),         // 节点间距
                    ..default()
                },
                BackgroundColor(Color::srgb(0.1, 0.1, 0.1)),
                PeerListContent,
            ));

            // 下部分 - 设置
            parent
                .spawn((
                    // 居中
                    Node {
                        width: percent(100),
                        height: px(100),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        padding: UiRect::all(px(20)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.3, 0.2, 0.2)),
                ))
                .with_children(|parent| {
                    parent.spawn((
                        Text::new("Settings"),
                        TextFont {
                            font_size: 32.0,
                            ..default()
                        },
                        TextColor::WHITE,
                    ));
                });
        });

    // TODO: 测试数据
    let fake_data = [
        ("Redmi-K60", "android", "192.168.1.5"),
        ("ThinkPad-X1", "linux", "192.168.1.12"),
        ("iPad-Air", "ios", "192.168.1.15"),
        ("Gaming-PC", "windows", "192.168.1.20"),
        ("Mac-Studio1", "macos", "192.168.1.31"),
        ("Mac-Studio2", "macos", "192.168.1.32"),
        ("Mac-Studio3", "macos", "192.168.1.33"),
        ("Mac-Studio4", "macos", "192.168.1.34"),
        ("Mac-Studio5", "macos", "192.168.1.35"),
        ("Mac-Studio6", "macos", "192.168.1.36"),
        ("Mac-Studio7", "macos", "192.168.1.37"),
        ("Mac-Studio8", "macos", "192.168.1.38"),
        ("Mac-Studio9", "macos", "192.168.1.39"),
    ];
    for (name, os, ip) in fake_data {
        peer_list.peers.insert(
            name.to_string(),
            PeerInfo {
                name: format!("{}.local.", name),
                ips: vec![ip.to_string()],
                os: os.to_string(),
                // 给一个极大的时间戳，防止被 45s 的清理系统回收
                last_seen: 999999.0,
            },
        );
    }
}

// --------------- RESOURCES --------------- //

// --------------- COMPONENTS --------------- //
// 存储设备的唯一标识（如 hostname）
#[derive(Component)]
struct PeerEntry(String);

// 列表滚动区域容器
#[derive(Component)]
struct PeerListContent;

// --------------- SYSTEMS --------------- //
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

const LINE_HEIGHT: f32 = 21.;

/// UI 滚动事件
#[derive(EntityEvent, Debug)]
#[entity_event(propagate, auto_propagate)]
struct Scroll {
    entity: Entity,
    delta: Vec2,
}

/// 发送滚动事件
// TODO: 跨平台 TouchInput
fn send_scroll_events(
    mut mouse_wheel_reader: MessageReader<MouseWheel>,
    hover_map: Res<HoverMap>,
    mut commands: Commands,
) {
    for mouse_wheel in mouse_wheel_reader.read() {
        let mut delta = -Vec2::new(mouse_wheel.x, mouse_wheel.y);

        if mouse_wheel.unit == MouseScrollUnit::Line {
            delta *= LINE_HEIGHT;
        }

        for pointer_map in hover_map.values() {
            for entity in pointer_map.keys().copied() {
                commands.trigger(Scroll { entity, delta });
            }
        }
    }
}

/// 处理滚动事件
fn on_scroll_handler(
    mut scroll: On<Scroll>,
    mut query: Query<(&mut ScrollPosition, &Node, &ComputedNode)>,
) {
    let Ok((mut scroll_position, node, computed)) = query.get_mut(scroll.entity) else {
        return;
    };

    let max_offset = (computed.content_size() - computed.size()) * computed.inverse_scale_factor();

    let delta = &mut scroll.delta;
    if node.overflow.y == OverflowAxis::Scroll && delta.y != 0. {
        let max = if delta.y > 0. {
            scroll_position.y >= max_offset.y
        } else {
            scroll_position.y <= 0.
        };

        if !max {
            scroll_position.y += delta.y;
            delta.y = 0.;
        }
    }

    if *delta == Vec2::ZERO {
        scroll.propagate(false);
    }
}

// 文件选择
struct PrintFilePath;

fn file_picked(mut ev_picked: MessageReader<DialogFilePicked<PrintFilePath>>) {
    for ev in ev_picked.read() {
        eprintln!("File picked, path {:?}", ev.path);
    }
}
