use super::{Action, Intent, SelectedPeer, TransferDetails, theme::*, widgets::*};
use bevy::{
    prelude::*,
    tasks::{IoTaskPool, Task, block_on, poll_once},
};
use portalo_core::{AppState, FileTransferState, ProgressSender, TransferStatus};
use portalo_network::{
    TokioRuntime,
    transfer::{self, Command, SavedTask, TransferControl},
};

pub(super) struct TransfersPlugin;
#[derive(Resource, Default)]
struct Tasks {
    scan: Option<Task<anyhow::Result<Vec<SavedTask>>>>,
    entries: Vec<SavedTask>,
}
#[derive(Component)]
struct TaskList;
impl Plugin for TransfersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TransferControl>()
            .init_resource::<Tasks>()
            .add_systems(OnEnter(AppState::Send), reload)
            .add_systems(Update, (poll, actions).chain());
    }
}
pub(super) fn mount(commands: &mut Commands, parent: Entity) {
    let list = commands
        .spawn((
            TaskList,
            Node {
                width: percent(100),
                min_width: px(0),
                flex_direction: FlexDirection::Column,
                row_gap: px(6),
                ..default()
            },
        ))
        .id();
    commands.entity(parent).add_child(list);
}
fn reload(mut tasks: ResMut<Tasks>) {
    tasks.scan = Some(IoTaskPool::get().spawn(async { transfer::tasks() }));
}
fn poll(
    mut commands: Commands,
    mut tasks: ResMut<Tasks>,
    list: Query<(Entity, Option<&Children>), With<TaskList>>,
    mut feedback: ResMut<Feedback>,
) {
    let Some(task) = tasks.scan.as_mut() else {
        return;
    };
    let Some(result) = block_on(poll_once(task)) else {
        return;
    };
    tasks.scan = None;
    match result {
        Ok(entries) => tasks.entries = entries,
        Err(error) => {
            feedback.0 = format!("读取恢复任务失败：{error}");
            return;
        }
    }
    let Ok((list, children)) = list.single() else {
        return;
    };
    if let Some(children) = children {
        for child in children {
            commands.entity(*child).despawn();
        }
    }
    if !tasks.entries.is_empty() {
        text(
            &mut commands,
            list,
            "未完成任务（选择原接收设备后继续）",
            15.0,
            MUTED,
        );
    }
    for task in &tasks.entries {
        text(&mut commands, list, &task.manifest.name, 16.0, INK);
        action(
            &mut commands,
            list,
            "继续传输",
            Action::ResumeTask(task.manifest.id.clone()),
            true,
        );
        action(
            &mut commands,
            list,
            "取消并清理两端",
            Action::DiscardTask(task.manifest.id.clone()),
            false,
        );
    }
}
#[allow(clippy::too_many_arguments)]
fn actions(
    mut input: MessageReader<Intent>,
    tasks: Res<Tasks>,
    control: Res<TransferControl>,
    runtime: Res<TokioRuntime>,
    progress: Res<ProgressSender>,
    mut state: ResMut<FileTransferState>,
    mut details: ResMut<TransferDetails>,
    selected: Res<SelectedPeer>,
    peers: Res<portalo_discovery::PeerList>,
    mut next: ResMut<NextState<AppState>>,
    mut feedback: ResMut<Feedback>,
) {
    for Intent(action) in input.read() {
        match action {
            Action::PauseTransfer if state.is_active() => {
                control.0.send_replace(Command::Pause);
            }
            Action::CancelTransfer if state.is_active() => {
                control.0.send_replace(Command::Cancel);
            }
            Action::ResumeTask(id) | Action::DiscardTask(id)
                if !state.is_active() && !selected.picking =>
            {
                let Some(mut task) = tasks
                    .entries
                    .iter()
                    .find(|task| task.manifest.id == *id)
                    .cloned()
                else {
                    continue;
                };
                if let Some((peer_id, old)) = &selected.peer {
                    let peer = peers.peers.get(peer_id).unwrap_or(old);
                    details.peer_name = peer.name.clone();
                    task.addresses = peer
                        .ips
                        .iter()
                        .filter_map(|ip| ip.parse().ok())
                        .map(|ip| std::net::SocketAddr::new(ip, peer.port))
                        .collect();
                }
                if task.addresses.is_empty() {
                    feedback.0 = "请先选择原接收设备。".into();
                    continue;
                }
                let id = state.id.wrapping_add(1);
                *state = FileTransferState {
                    id,
                    status: TransferStatus::Connecting,
                    ..default()
                };
                details.file_name = task.manifest.name.clone();
                details.show_error = false;
                control
                    .0
                    .send_replace(if matches!(action, Action::DiscardTask(_)) {
                        Command::Cancel
                    } else {
                        Command::Run
                    });
                let receiver = control.0.subscribe();
                let tx = progress.0.clone();
                runtime.0.spawn(async move {
                    if let Err(error) = transfer::send(task, tx, id, receiver).await {
                        error!("Resume: {error:#}");
                    }
                });
                next.set(AppState::Transfer);
            }
            _ => {}
        }
    }
}
