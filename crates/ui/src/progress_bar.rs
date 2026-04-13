use bevy::prelude::*;
use portalo_core::{AppState, FileTransferState};
use tokio::sync::mpsc;

// 进度条插件
// --------------- PLUGIN --------------- //
pub struct ProgressBarPlugin;

impl Plugin for ProgressBarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Transfer), setup)
            //.insert_resource(FileTransferState::new(100_000_000))
            .add_systems(
                Update,
                (update_progress_bar, update_progress_text)
                    .chain()
                    .run_if(in_state(AppState::Transfer)),
            );
    }
}

// --------------- SETUP --------------- //
pub fn setup(mut commands: Commands) {
    // 进度条UI
    commands.spawn(ui_root());
}

// --------------- UI --------------- //
fn ui_root() -> impl Bundle {
    (
        // 容器
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(20.0),
            ..default()
        },
        BackgroundColor(Color::srgb(0.08, 0.09, 0.11)), // 背景颜色
        children![
            // 标题
            (
                Text::new("File Transfer Progress"),
                TextFont {
                    font_size: 32.0,
                    ..default()
                },
                TextColor(Color::WHITE),
            ),
            // 进度条背景
            (
                // 容器
                Node {
                    width: Val::Px(400.0),
                    height: Val::Px(30.0),
                    border: UiRect::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.15, 0.15, 0.15)),
                children![
                    // 进度条填充部分
                    (
                        Node {
                            width: Val::Percent(0.0),
                            height: Val::Percent(100.0),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.2, 0.8, 0.3)), // 绿色
                        ProgressBarFill,
                    ),
                ],
            ),
            // 进度文本（百分比和字节数）
            (
                Text::new("0%"),
                TextFont {
                    font_size: 20.0,
                    ..default()
                },
                TextColor(Color::WHITE),
                ProgressText,
            ),
            // 速度文本
            (
                Text::new("Speed: 0.0 MB/s"),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(0.7, 0.7, 0.7)),
                SpeedText,
            ),
        ],
    )
}

#[derive(Resource)]
pub struct ProgressReceiver(pub mpsc::Receiver<u64>); // 接收已传输字节数

// --------------- COMPONENTS --------------- //
#[derive(Component)]
struct ProgressBar {
    current: f32,
    total: f32,
}

impl ProgressBar {
    fn new(total: f32) -> Self {
        Self {
            current: 0.0,
            total,
        }
    }

    fn progress(&self) -> f32 {
        (self.current / self.total).clamp(0.0, 1.0)
    }

    fn is_complete(&self) -> bool {
        self.current >= self.total
    }
}

#[derive(Component)]
struct ProgressBarFill;

#[derive(Component)]
struct ProgressText;

#[derive(Component)]
struct SpeedText;

// --------------- SYSTEMS --------------- //
fn simulate_transfer(mut state: ResMut<FileTransferState>, time: Res<Time>) {
    // 模拟传输速度（每秒增加字节数）
    let transfer_speed = 50_000_000.0; // 50 MB/s
    let delta = time.delta_secs();

    state.bytes_transferred =
        ((state.bytes_transferred as f32 + transfer_speed * delta) as u64).min(state.total_bytes);
}

fn update_progress_bar(
    state: Res<FileTransferState>,
    mut progress_query: Query<&mut Node, With<ProgressBarFill>>,
) {
    let progress = state.get_progress();

    if let Ok(mut node) = progress_query.single_mut() {
        node.width = Val::Percent(progress * 100.0);
    }
}

fn update_progress_text(
    state: Res<FileTransferState>,
    mut progress_text_query: Query<&mut Text, (With<ProgressText>, Without<SpeedText>)>,
    mut speed_text_query: Query<&mut Text, (With<SpeedText>, Without<ProgressText>)>,
) {
    let progress = state.get_progress();
    let transferred = state.bytes_transferred;
    let total = state.total_bytes;

    // 更新百分比和字节数
    if let Ok(mut text) = progress_text_query.single_mut() {
        let transferred_mb = transferred as f32 / 1_000_000.0;
        let total_mb = total as f32 / 1_000_000.0;
        text.0 = format!(
            "{:.1}% ({:.1} MB / {:.1} MB)",
            progress * 100.0,
            transferred_mb,
            total_mb
        );
    }

    // 更新速度
    if let Ok(mut text) = speed_text_query.single_mut() {
        let speed = state.get_speed_mbps();
        let eta = if speed > 0.0 {
            let remaining = (total as f32 - transferred as f32) / 1_000_000.0 / speed;
            format!(" ETA: {:.1}s", remaining)
        } else {
            String::new()
        };
        text.0 = format!("Speed: {:.1} MB/s{}", speed, eta);
    }
}
