use bevy::prelude::*;

use portalo_core::{AppState, FileTransferState, ProgressReceiver};

// 进度条插件
// --------------- PLUGIN --------------- //
pub struct ProgressBarPlugin;

impl Plugin for ProgressBarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Transfer), setup)
            //.insert_resource(FileTransferState::new(100_000_000))
            .add_systems(
                Update,
                (
                    (update_progress_bar, update_progress_text)
                        .chain()
                        .run_if(in_state(AppState::Transfer)),
                    update_transfer_state_system,
                ),
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
                        ProgressBarFill { last_progress: 0.0 },
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
struct ProgressBarFill {
    last_progress: f32,
}

#[derive(Component)]
struct ProgressText;

#[derive(Component)]
struct SpeedText;

// --------------- SYSTEMS --------------- //
fn update_transfer_state_system(
    mut receiver: ResMut<ProgressReceiver>,
    mut state: ResMut<FileTransferState>,
    time: Res<Time<Real>>,
) {
    let mut updated = false;

    // 非阻塞地读取所有最新进度
    while let Ok(msg) = receiver.0.try_recv() {
        // 假设 FileTransferState 有这两个字段
        state.bytes_transferred = msg.current;
        state.total_bytes = msg.total;
        updated = true;
    }

    // 计算速度（每 0.5 秒计算一次，避免 UI 抖动）
    let now = time.elapsed_secs_f64();
    let delta_t = now - state.last_update_time;

    if delta_t >= 0.5 {
        // 判定是否已经完成
        if state.bytes_transferred >= state.total_bytes && state.total_bytes > 0 {
            state.current_speed = 0.0; // 传输完成，立即归零
        } else if updated {
            let delta_bytes = state.bytes_transferred.saturating_sub(state.last_bytes);
            let instant_speed = (delta_bytes as f32 / 1_048_576.0) / delta_t as f32;

            // 平滑滤波
            state.current_speed = state.current_speed * 0.3 + instant_speed * 0.7;
        } else {
            // 没数据时衰减
            state.current_speed *= 0.5;
            if state.current_speed < 0.01 {
                state.current_speed = 0.0;
            }
        }

        // 更新记录点
        state.last_bytes = state.bytes_transferred;
        state.last_update_time = now;
    }
}

fn update_progress_bar(
    state: Res<FileTransferState>,
    mut progress_query: Query<&mut Node, With<ProgressBarFill>>,
    mut fill_query: Query<&mut ProgressBarFill>,
) {
    let progress = state.get_progress();

    if let Ok(mut fill) = fill_query.single_mut() {
        if (fill.last_progress - progress).abs() > 0.001 {
            // 0.1% 变化才更新
            fill.last_progress = progress;
            if let Ok(mut node) = progress_query.single_mut() {
                node.width = Val::Percent(progress * 100.0);
            }
        }
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
        text.0 = format!(
            "{:.1}% ({} / {})",
            progress * 100.0,
            format_bytes(transferred),
            format_bytes(total),
        );
    }

    // 更新速度
    if let Ok(mut text) = speed_text_query.single_mut() {
        let speed = state.current_speed;
        let eta = if speed > 0.01 {
            let remaining = (total as f32 - transferred as f32) / 1_000_000.0 / speed;
            format!(" ETA: {:.1}s", remaining)
        } else {
            String::new()
        };
        text.0 = format!("Speed: {:.1} MB/s{}", speed, eta);
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1000.0 && unit_idx < UNITS.len() - 1 {
        size /= 1000.0;
        unit_idx += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_idx])
}
