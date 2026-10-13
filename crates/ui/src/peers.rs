use super::{Action, settings::LocalDevice, theme::*, widgets::*};
use bevy::prelude::*;
use portalo_core::AppState;
use portalo_discovery::PeerList;

#[derive(Component)]
pub(super) struct DeviceList;
#[derive(Component)]
pub(super) struct DeviceRow {
    id: String,
    caption: String,
}
#[derive(Component)]
pub(super) struct DeviceCount;
#[derive(Component)]
pub(super) struct EmptyState;

pub fn setup(mut commands: Commands, local: Res<LocalDevice>) {
    let body = page(
        &mut commands,
        AppState::Menu,
        "Portalo",
        "简单、直接的局域网文件传输",
    );
    text(&mut commands, body, "本机", 13.0, MUTED);
    let name = text(&mut commands, body, &local.name, 23.0, INK);
    commands.entity(name).insert(super::settings::LocalName);
    let addresses = text(&mut commands, body, &local.addresses, 14.0, MUTED);
    commands
        .entity(addresses)
        .insert(super::settings::LocalAddresses);
    let count = text(&mut commands, body, "附近设备", 24.0, INK);
    commands.entity(count).insert(DeviceCount);
    let empty = text(
        &mut commands,
        body,
        "正在寻找附近设备…\n请在同一局域网的另一台设备上打开 Portalo。",
        16.0,
        MUTED,
    );
    commands.entity(empty).insert(EmptyState);
    let list = commands
        .spawn((
            DeviceList,
            Node {
                width: percent(100),
                flex_direction: FlexDirection::Column,
                row_gap: px(8),
                flex_shrink: 0.0,
                ..default()
            },
        ))
        .id();
    commands.entity(body).add_child(list);
    text(&mut commands, body, "选择设备，然后发送文件", 13.0, MUTED);
    action(
        &mut commands,
        body,
        "连接信息",
        Action::Open(AppState::Connection),
        false,
    );
    action(
        &mut commands,
        body,
        "设置",
        Action::Open(AppState::Setting),
        false,
    );
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update(
    mut commands: Commands,
    peers: Res<PeerList>,
    list: Query<Entity, With<DeviceList>>,
    added: Query<Entity, Added<DeviceList>>,
    rows: Query<(Entity, &DeviceRow, &Children)>,
    mut labels: Query<&mut Text>,
    mut empty: Query<&mut Node, With<EmptyState>>,
    count: Query<Entity, With<DeviceCount>>,
) {
    if !peers.is_changed() && added.is_empty() {
        return;
    }
    let Ok(list) = list.single() else {
        return;
    };
    if let Ok(entity) = count.single()
        && let Ok(mut label) = labels.get_mut(entity)
    {
        label.0 = format!("附近设备  {}", peers.peers.len());
    }
    if let Ok(mut node) = empty.single_mut() {
        node.display = if peers.peers.is_empty() {
            Display::Flex
        } else {
            Display::None
        };
    }
    let mut existing: std::collections::HashMap<_, _> = rows
        .iter()
        .map(|(entity, row, children)| (row.id.as_str(), (entity, row, children)))
        .collect();
    let mut sorted: Vec<_> = peers.peers.iter().collect();
    sorted.sort_by(|(a, pa), (b, pb)| pa.name.cmp(&pb.name).then(a.cmp(b)));
    let mut order = Vec::new();
    for (id, peer) in sorted {
        let caption = format!(
            "{}\n{}  ·  {}",
            peer.name.trim_end_matches(".local."),
            peer.os,
            peer.ips.first().map(String::as_str).unwrap_or("无可用地址")
        );
        let entity = if let Some((entity, row, children)) = existing.remove(id.as_str()) {
            if row.caption != caption {
                if let Ok(mut label) = labels.get_mut(children[0]) {
                    label.0.clone_from(&caption);
                }
                commands.entity(entity).insert(DeviceRow {
                    id: id.clone(),
                    caption,
                });
            }
            entity
        } else {
            let entity = action(
                &mut commands,
                list,
                &caption,
                Action::Select(id.clone()),
                true,
            );
            commands.entity(entity).insert((
                DeviceRow {
                    id: id.clone(),
                    caption,
                },
                Node {
                    width: percent(100),
                    min_height: px(90),
                    flex_shrink: 0.0,
                    padding: UiRect::all(px(16)),
                    border: UiRect::all(px(2)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: px(16),
                    ..default()
                },
            ));
            // Local vector-like outline icon; no platform-dependent emoji or image loading.
            let mobile = peer.os == "android" || peer.os == "ios";
            let icon = commands
                .spawn((
                    Node {
                        width: px(if mobile { 22 } else { 36 }),
                        height: px(if mobile { 36 } else { 26 }),
                        min_width: px(if mobile { 22 } else { 36 }),
                        border: UiRect::all(px(2)),
                        padding: UiRect::all(px(3)),
                        align_items: AlignItems::End,
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BorderColor::all(Color::WHITE),
                    children![(
                        Node {
                            width: px(8),
                            height: px(2),
                            ..default()
                        },
                        BackgroundColor(Color::WHITE)
                    )],
                ))
                .id();
            commands.entity(entity).add_child(icon);
            entity
        };
        order.push(entity);
    }
    for (_, (entity, _, _)) in existing {
        commands.entity(entity).despawn();
    }
    commands.entity(list).replace_children(&order);
}
