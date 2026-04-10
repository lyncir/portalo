use bevy::input_focus::InputDispatchPlugin;
use bevy::prelude::*;
use bevy::ui_widgets::{Activate, Button, UiWidgetsPlugins, observe};
use bevy_file_dialog::prelude::*;
use portalo_discovery::PeerList;
use portalo_network::{TokioRuntime, send_file_fast};

// UI插件
// --------------- PLUGIN --------------- //
pub struct UIPlugin;

impl Plugin for UIPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SelectedPeer>()
            .add_plugins((
                UiWidgetsPlugins,
                InputDispatchPlugin,
                FileDialogPlugin::new().with_pick_file::<SelectedFilePath>(),
            ))
            .add_systems(Startup, setup)
            .add_systems(Update, (update_peer_list_ui, file_picked));
    }
}

// --------------- SETUP --------------- //
fn setup(mut commands: Commands) {
    // 2D摄像机
    commands.spawn(Camera2d);

    // UI
    commands.spawn(ui_root());
}

// --------------- UI --------------- //
fn ui_root() -> impl Bundle {
    (
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
        children![
            // 上部分 - 标题
            (
                Node {
                    width: percent(100),
                    height: px(100),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    padding: UiRect::all(px(20)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.2, 0.3, 0.5)),
                children![(
                    Text::new("Portalo"),
                    TextFont {
                        font_size: 32.0,
                        ..default()
                    },
                    TextColor::WHITE,
                )],
            ),
            // 中部分 - 设备列表（可滑动）
            (
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
            ),
            // 下部分 - 设置
            (
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
                children![
                    // 设置文本
                    (
                        Text::new("Settings"),
                        TextFont {
                            font_size: 32.0,
                            ..default()
                        },
                        TextColor::WHITE,
                    )
                ]
            )
        ],
    )
}

fn button(host_name: String, ips_str: String) -> impl Bundle {
    (
        Node {
            width: Val::Percent(100.0),
            height: Val::Px(70.0),
            padding: UiRect::horizontal(Val::Px(15.0)),
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgb(0.12, 0.12, 0.12)),
        PeerButton,
        PeerEntry(host_name.clone()),
        Button,
        children![
            // 图标
            (
                Node {
                    width: Val::Px(40.0),
                    height: Val::Px(40.0),
                    margin: UiRect::right(Val::Px(15.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.3, 0.3, 0.3)),
            ),
            // 文字信息
            (
                Node {
                    flex_direction: FlexDirection::Column,
                    ..default()
                },
                children![
                    // 名字文本
                    (
                        Text::new(host_name.clone()),
                        TextFont::from_font_size(18.0),
                        TextColor(Color::WHITE),
                    ),
                    // IP地址文本
                    (
                        Text::new(format!("IP: {}", ips_str)),
                        TextFont::from_font_size(12.0),
                        TextColor(Color::srgb(0.6, 0.6, 0.6)),
                    )
                ],
            )
        ],
    )
}

// --------------- RESOURCES --------------- //
// 当前选择的主机
#[derive(Resource, Default)]
struct SelectedPeer(Option<String>);

// 当前选择的文件
struct SelectedFilePath;

// --------------- COMPONENTS --------------- //
// 存储设备的唯一标识（如 hostname）
#[derive(Component)]
struct PeerEntry(String);

// 列表滚动区域容器
#[derive(Component)]
struct PeerListContent;

// 列表中设备按钮
#[derive(Component)]
struct PeerButton;

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
                    button(id.clone(), info.ips.join(", ")),
                    observe(on_button_clicked),
                ))
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

// 点击设备
fn on_button_clicked(
    activate: On<Activate>,
    query: Query<&PeerEntry>,
    mut commands: Commands,
    mut selected_peer: ResMut<SelectedPeer>,
) {
    // 获取目标名字
    if let Ok(peer) = query.get(activate.entity) {
        info!("# 准备发送文件给: {}", peer.0);

        // 记录目标设备
        selected_peer.0 = Some(peer.0.clone());

        // 打开选择文件对话框
        commands.dialog().pick_file_path::<SelectedFilePath>();
    }
}

// 当前选中的文件路径
fn file_picked(
    mut ev_picked: MessageReader<DialogFilePicked<SelectedFilePath>>,
    selected_peer: Res<SelectedPeer>,
    peer_list: Res<PeerList>,
    runtime: Res<TokioRuntime>,
) {
    for ev in ev_picked.read() {
        let path_owned = ev.path.to_path_buf();

        if let Some(peer_id) = &selected_peer.0 {
            // 从 PeerList 中找到对应的 IP
            if let Some(info) = peer_list.peers.get(peer_id) {
                // TODO: 这里只取第一个地址
                let target_ip = &info.ips[0];
                info!(
                    "# 正在发送文件 {:?} 到设备 {} (IP: {})",
                    path_owned, peer_id, target_ip
                );

                // 发送文件
                let dest = format!("{}:49527", target_ip);
                let handle = runtime.0.handle().clone();
                handle.spawn(async move {
                    info!("# Starting transfer to {}...", dest);
                    if let Err(e) = send_file_fast(dest, path_owned).await {
                        error!("# Transfer failed: {}", e);
                    }
                });
            }
        } else {
            warn!("# 选择了文件，但未找到目标设备");
        }
    }
}
