use super::{Action, Intent, TransferDetails, theme::*, widgets::*};
use bevy::prelude::*;
use portalo_core::{AppState, FileTransferState, ProgressReceiver, TransferStatus};

pub struct ProgressBarPlugin;
impl Plugin for ProgressBarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Transfer), setup)
            .add_systems(
                Update,
                (
                    update_transfer_state_system,
                    toggle_details,
                    render.run_if(in_state(AppState::Transfer)),
                )
                    .chain(),
            );
    }
}
#[derive(Component)]
struct Fill;
#[derive(Component)]
enum Field {
    Status,
    Percent,
    Bytes,
    Speed,
    Error,
    Detail,
}

pub(super) fn setup(mut commands: Commands, details: Res<TransferDetails>) {
    let body = page(
        &mut commands,
        AppState::Transfer,
        "文件传输",
        "点对点发送 · 完成状态以接收方确认为准",
    );
    text(&mut commands, body, &details.file_name, 24.0, INK);
    text(
        &mut commands,
        body,
        format!("发送到：{}", details.peer_name.trim_end_matches(".local.")),
        15.0,
        MUTED,
    );
    for (field, value, size) in [
        (Field::Status, "正在连接设备…", 22.0),
        (Field::Percent, "0%", 60.0),
    ] {
        let entity = text(&mut commands, body, value, size, INK);
        commands.entity(entity).insert(field);
    }
    let track = commands
        .spawn((
            Node {
                width: percent(100),
                height: px(10),
                min_height: px(10),
                ..default()
            },
            BackgroundColor(LINE),
        ))
        .id();
    let fill = commands
        .spawn((
            Fill,
            Node {
                width: percent(0),
                height: percent(100),
                ..default()
            },
            BackgroundColor(SUCCESS),
        ))
        .id();
    commands.entity(track).add_child(fill);
    commands.entity(body).add_child(track);
    for (field, size) in [
        (Field::Bytes, 16.0),
        (Field::Speed, 15.0),
        (Field::Error, 14.0),
        (Field::Detail, 13.0),
    ] {
        let entity = text(&mut commands, body, "", size, MUTED);
        commands.entity(entity).insert(field);
    }
    action(
        &mut commands,
        body,
        "查看 / 收起错误详情",
        Action::ToggleDetails,
        false,
    );
    action(&mut commands, body, "‹ 返回发送页面", Action::Back, true);
    action(
        &mut commands,
        body,
        "暂停（保留进度）",
        Action::PauseTransfer,
        false,
    );
    action(
        &mut commands,
        body,
        "取消任务并清理",
        Action::CancelTransfer,
        false,
    );
}

fn toggle_details(mut messages: MessageReader<Intent>, mut details: ResMut<TransferDetails>) {
    for Intent(action) in messages.read() {
        if matches!(action, Action::ToggleDetails) {
            details.show_error = !details.show_error;
        }
    }
}

fn status_text(status: &TransferStatus) -> &'static str {
    match status {
        TransferStatus::Idle => "准备发送",
        TransferStatus::Connecting => "正在连接设备…",
        TransferStatus::Sending => "正在发送，请保持设备连接",
        TransferStatus::AwaitingConfirmation => "文件已发送，等待对方确认…",
        TransferStatus::Complete => "传输完成，对方已确认接收",
        TransferStatus::Paused => "任务已暂停，可从未完成任务继续",
        TransferStatus::Preparing => "正在扫描并准备文件来源…",
        TransferStatus::Cancelled => "任务已取消并清理",
        TransferStatus::Failed(_) => "发送失败，原因见下方",
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    mut commands: Commands,
    state: Res<FileTransferState>,
    details: Res<TransferDetails>,
    theme: Res<Theme>,
    added: Query<Entity, Added<Fill>>,
    mut labels: Query<(&Field, &mut Text, &mut TextColor)>,
    mut bars: Query<(&mut Node, &mut BackgroundColor), With<Fill>>,
    buttons: Query<(Entity, &ActionButton)>,
) {
    if !state.is_changed() && !details.is_changed() && !theme.is_changed() && added.is_empty() {
        return;
    }
    let color = match state.status {
        TransferStatus::Complete => SUCCESS,
        TransferStatus::Failed(_) => ERROR,
        _ => theme.color(),
    };
    for (field, mut text, mut text_color) in &mut labels {
        let value = match field {
            Field::Detail => match &state.status {
                TransferStatus::Failed(error) => {
                    format!("失败原因：{}", error.lines().next().unwrap_or(error))
                }
                _ => state.detail.clone(),
            },
            Field::Status => {
                if text_color.0 != color {
                    text_color.0 = color;
                }
                status_text(&state.status).into()
            }
            Field::Percent => format!("{:.0}%", state.get_progress() * 100.0),
            Field::Bytes => format!(
                "{} / {}",
                format_bytes(state.bytes_transferred),
                format_bytes(state.total_bytes)
            ),
            Field::Speed => {
                if state.status == TransferStatus::Sending && state.current_speed > 0.01 {
                    let eta = state.total_bytes.saturating_sub(state.bytes_transferred) as f64
                        / 1_048_576.0
                        / state.current_speed as f64;
                    format!(
                        "速度 {:.1} MiB/s · 预计剩余 {:.0} 秒",
                        state.current_speed, eta
                    )
                } else if state.is_active() {
                    "传输过程中暂不可返回，请等待完成或超时。".into()
                } else {
                    "可以返回继续发送其他文件。".into()
                }
            }
            Field::Error => {
                if let TransferStatus::Failed(error) = &state.status {
                    if details.show_error {
                        error.clone()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            }
        };
        if text.0 != value {
            text.0 = value;
        }
    }
    for (mut node, mut background) in &mut bars {
        let width = percent(state.get_progress() * 100.0);
        if node.width != width {
            node.width = width;
        }
        if background.0 != color {
            background.0 = color;
        }
    }
    for (entity, button) in &buttons {
        if matches!(button.0, Action::PauseTransfer | Action::CancelTransfer) {
            let active = state.is_active();
            commands
                .entity(entity)
                .entry::<Node>()
                .and_modify(move |mut node| {
                    node.display = if active { Display::Flex } else { Display::None };
                });
        }
        if matches!(button.0, Action::Back) {
            if state.is_active() {
                commands
                    .entity(entity)
                    .insert(bevy::ui::InteractionDisabled);
            } else {
                commands
                    .entity(entity)
                    .remove::<bevy::ui::InteractionDisabled>();
            }
        }
        if matches!(button.0, Action::ToggleDetails) {
            let failed = matches!(state.status, TransferStatus::Failed(_));
            commands
                .entity(entity)
                .entry::<Node>()
                .and_modify(move |mut node| {
                    node.display = if failed { Display::Flex } else { Display::None };
                });
        }
    }
}

fn update_transfer_state_system(
    mut receiver: ResMut<ProgressReceiver>,
    mut state: ResMut<FileTransferState>,
    time: Res<Time<Real>>,
) {
    if receiver.0.has_changed().unwrap_or(false) {
        let msg = receiver.0.borrow_and_update().clone();
        if msg.id == state.id {
            state.bytes_transferred = msg.current;
            state.total_bytes = msg.total;
            state.status = msg.status;
            state.detail = msg.detail;
            if !state.is_active() {
                state.current_speed = 0.0;
            }
        }
    }
    if !state.is_active() {
        return;
    }
    let now = time.elapsed_secs_f64();
    let elapsed = now - state.last_update_time;
    if elapsed >= 0.5 {
        let bytes = state.bytes_transferred.saturating_sub(state.last_bytes);
        if bytes > 0 {
            state.current_speed =
                state.current_speed * 0.3 + (bytes as f64 / 1_048_576.0 / elapsed) as f32 * 0.7;
        } else {
            state.current_speed *= 0.5;
            if state.current_speed < 0.01 {
                state.current_speed = 0.0;
            }
        }
        state.last_bytes = state.bytes_transferred;
        state.last_update_time = now;
    }
}

fn format_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", units[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use portalo_core::TransferProgressMsg;
    #[test]
    fn speed_counts_earlier_frames_and_ignores_old_tasks() {
        let (tx, rx) = tokio::sync::watch::channel(TransferProgressMsg::default());
        let mut app = App::new();
        app.insert_resource(ProgressReceiver(rx))
            .insert_resource(FileTransferState {
                id: 2,
                status: TransferStatus::Sending,
                ..default()
            })
            .init_resource::<Time<Real>>()
            .add_systems(Update, update_transfer_state_system);
        tx.send_replace(TransferProgressMsg {
            id: 2,
            current: 1_048_576,
            total: 10_485_760,
            status: TransferStatus::Sending,
            detail: String::new(),
        });
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_millis(100));
        app.update();
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_millis(400));
        app.update();
        assert!(app.world().resource::<FileTransferState>().current_speed > 1.0);
        tx.send_replace(TransferProgressMsg {
            id: 1,
            status: TransferStatus::Complete,
            ..default()
        });
        app.update();
        assert!(!app.world().resource::<FileTransferState>().is_complete());
        assert_eq!(
            app.world()
                .resource::<FileTransferState>()
                .bytes_transferred,
            1_048_576
        );
    }
    #[test]
    fn confirmation_has_distinct_user_status() {
        assert_ne!(
            status_text(&TransferStatus::AwaitingConfirmation),
            status_text(&TransferStatus::Complete)
        );
        assert_eq!(format_bytes(1_048_576), "1.0 MiB");
    }

    #[test]
    fn back_button_remains_disabled_until_confirmation() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .init_state::<AppState>()
            .init_resource::<TransferDetails>()
            .init_resource::<Theme>()
            .insert_resource(FileTransferState {
                bytes_transferred: 100,
                total_bytes: 100,
                status: TransferStatus::AwaitingConfirmation,
                ..default()
            })
            .add_systems(OnEnter(AppState::Transfer), setup)
            .add_systems(Update, render.run_if(in_state(AppState::Transfer)));
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Transfer);
        app.update();
        let back = app
            .world_mut()
            .query::<(Entity, &ActionButton)>()
            .iter(app.world())
            .find(|(_, button)| matches!(button.0, Action::Back))
            .unwrap()
            .0;
        assert!(
            app.world()
                .get::<bevy::ui::InteractionDisabled>(back)
                .is_some()
        );
        app.world_mut().resource_mut::<FileTransferState>().status = TransferStatus::Complete;
        app.update();
        assert!(
            app.world()
                .get::<bevy::ui::InteractionDisabled>(back)
                .is_none()
        );
        assert!(
            app.world_mut()
                .query::<&Text>()
                .iter(app.world())
                .any(|text| text.0 == "传输完成，对方已确认接收")
        );
    }
}
