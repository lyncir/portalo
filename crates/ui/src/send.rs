use super::{Action, Intent, PickedFile, SelectedPeer, TransferDetails, theme::*, widgets::*};
use bevy::prelude::*;
#[cfg(not(target_os = "android"))]
use bevy_file_dialog::prelude::*;
use portalo_core::{AppState, FileTransferState, ProgressSender, TransferStatus};
use portalo_network::{TokioRuntime, send_file_fast};

pub struct SendPlugin;
#[cfg(not(target_os = "android"))]
struct FileSelection;
#[derive(Component)]
struct PickerStatus;

impl Plugin for SendPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Send), setup).add_systems(
            Update,
            (
                pick_action,
                picked,
                picker_status.run_if(in_state(AppState::Send)),
                dropped_files,
            )
                .chain(),
        );
        #[cfg(not(target_os = "android"))]
        app.add_plugins(
            FileDialogPlugin::new()
                .with_pick_file::<FileSelection>()
                .with_pick_directory::<super::settings::DirectorySelection>(),
        )
        .add_systems(Update, desktop_picked.before(picked));
        #[cfg(target_os = "android")]
        app.add_systems(
            Update,
            (
                super::android::poll_file.before(picked),
                super::android::update_insets,
            ),
        );
    }
}

pub(super) fn setup(mut commands: Commands, selected: Res<SelectedPeer>) {
    let body = page(
        &mut commands,
        AppState::Send,
        "发送文件",
        "选择目标 · 选择文件 · 开始传输",
    );
    action(&mut commands, body, "‹ 返回附近设备", Action::Back, false);
    if let Some((_, peer)) = &selected.peer {
        text(
            &mut commands,
            body,
            peer.name.trim_end_matches(".local."),
            30.0,
            INK,
        );
        text(
            &mut commands,
            body,
            format!("{}\n{}", peer.os, peer.ips.join("\n")),
            15.0,
            MUTED,
        );
    }
    action(
        &mut commands,
        body,
        "选择文件并发送",
        Action::PickFile,
        true,
    );
    let status = text(&mut commands, body, "", 15.0, MUTED);
    commands.entity(status).insert(PickerStatus);
    #[cfg(not(target_os = "android"))]
    text(
        &mut commands,
        body,
        "也可以将一个文件拖到此窗口\n当前支持单个文件，暂不支持文件夹。",
        14.0,
        MUTED,
    );
    #[cfg(target_os = "android")]
    text(
        &mut commands,
        body,
        "选择后会先在后台准备文件。\n较大的文件需要一些时间和可用缓存空间。",
        14.0,
        MUTED,
    );
}

fn pick_action(
    mut intents: MessageReader<Intent>,
    mut selected: ResMut<SelectedPeer>,
    mut commands: Commands,
    state: Res<State<AppState>>,
) {
    for Intent(intent) in intents.read() {
        if !matches!(intent, Action::PickFile) || selected.picking || *state.get() != AppState::Send
        {
            continue;
        }
        selected.picking = true;
        #[cfg(not(target_os = "android"))]
        commands.dialog().pick_file_path::<FileSelection>();
        #[cfg(target_os = "android")]
        {
            let _ = &mut commands;
            super::android::pick_file();
        }
    }
}

fn picker_status(
    mut commands: Commands,
    selected: Res<SelectedPeer>,
    mut labels: Query<&mut Text, With<PickerStatus>>,
    buttons: Query<(Entity, &ActionButton, Has<bevy::ui::InteractionDisabled>)>,
) {
    for mut text in &mut labels {
        let value = if selected.picking {
            "正在选择或准备文件，请稍候…"
        } else {
            "选择文件后立即发送到上方设备"
        };
        if text.0 != value {
            text.0 = value.into();
        }
    }
    for (entity, button, disabled) in &buttons {
        if matches!(button.0, Action::PickFile | Action::Back) && disabled != selected.picking {
            if selected.picking {
                commands
                    .entity(entity)
                    .insert(bevy::ui::InteractionDisabled);
            } else {
                commands
                    .entity(entity)
                    .remove::<bevy::ui::InteractionDisabled>();
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
fn desktop_picked(
    mut input: MessageReader<DialogFilePicked<FileSelection>>,
    mut cancelled: MessageReader<DialogFilePickCanceled<FileSelection>>,
    mut output: MessageWriter<PickedFile>,
) {
    for event in input.read() {
        output.write(PickedFile {
            path: Some(event.path.clone()),
        });
    }
    for _ in cancelled.read() {
        output.write(PickedFile { path: None });
    }
}

#[allow(clippy::too_many_arguments)]
fn picked(
    mut messages: MessageReader<PickedFile>,
    mut selected: ResMut<SelectedPeer>,
    runtime: Res<TokioRuntime>,
    mut next: ResMut<NextState<AppState>>,
    progress: Res<ProgressSender>,
    mut transfer: ResMut<FileTransferState>,
    time: Res<Time<Real>>,
    mut details: ResMut<TransferDetails>,
    mut feedback: ResMut<Feedback>,
    preferred: Local<
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, std::net::SocketAddr>>>,
    >,
) {
    for event in messages.read() {
        selected.picking = false;
        if transfer.is_active() {
            continue;
        }
        let Some(path) = event.path.clone() else {
            feedback.0 = "已取消选择文件，可以重新选择。".into();
            continue;
        };
        let Some((peer_id, peer)) = &selected.peer else {
            continue;
        };
        let id = transfer.id.wrapping_add(1);
        *transfer = FileTransferState {
            id,
            status: TransferStatus::Connecting,
            last_update_time: time.elapsed_secs_f64(),
            ..default()
        };
        *details = TransferDetails {
            file_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            peer_name: peer.name.clone(),
            show_error: false,
        };
        feedback.0.clear();
        next.set(AppState::Transfer);
        let mut addresses: Vec<_> = peer
            .ips
            .iter()
            .filter_map(|s| s.parse().ok())
            .map(|ip| std::net::SocketAddr::new(ip, peer.port))
            .collect();
        let previous = preferred.lock().unwrap().get(peer_id).copied();
        addresses.sort_by_key(|addr| Some(*addr) != previous);
        let preferred = preferred.clone();
        let peer_id = peer_id.clone();
        let tx = progress.0.clone();
        runtime.0.spawn(async move {
            match send_file_fast(addresses, &path, tx, id).await {
                Ok(address) => {
                    preferred.lock().unwrap().insert(peer_id, address);
                }
                Err(e) => error!("Send: {e:#}"),
            }
            #[cfg(target_os = "android")]
            {
                let _ = tokio::fs::remove_file(&path).await;
            }
        });
    }
}

#[derive(Default)]
struct DropBatch {
    paths: Vec<std::path::PathBuf>,
    last_event: f64,
}

#[allow(clippy::too_many_arguments)]
fn dropped_files(
    mut events: MessageReader<bevy::window::FileDragAndDrop>,
    state: Res<State<AppState>>,
    selected: Res<SelectedPeer>,
    transfer: Res<FileTransferState>,
    mut files: MessageWriter<PickedFile>,
    mut feedback: ResMut<Feedback>,
    time: Res<Time<Real>>,
    mut batch: Local<DropBatch>,
) {
    for event in events.read() {
        if let bevy::window::FileDragAndDrop::DroppedFile { path_buf, .. } = event {
            // OS drop notifications may span several frames. Keep only enough
            // paths to distinguish a single file from an unsupported batch.
            if batch.paths.len() < 2 {
                batch.paths.push(path_buf.clone());
            }
            batch.last_event = time.elapsed_secs_f64();
        }
    }
    if batch.paths.is_empty() || time.elapsed_secs_f64() - batch.last_event < 0.15 {
        return;
    }
    let dropped = std::mem::take(&mut batch.paths);
    if *state.get() != AppState::Send {
        feedback.0 = "请先选择目标设备，再拖入一个文件。".into();
    } else if selected.picking || transfer.is_active() {
        feedback.0 = "请等待当前操作完成。".into();
    } else if dropped.len() != 1 {
        feedback.0 = "一次只支持一个文件，请逐个发送。".into();
    } else {
        files.write(PickedFile {
            path: dropped.into_iter().next(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multi_file_drop_across_frames_is_rejected_without_sending_first_file() {
        let mut app = App::new();
        app.add_plugins(bevy::state::app::StatesPlugin)
            .insert_state(AppState::Send)
            .init_resource::<SelectedPeer>()
            .init_resource::<FileTransferState>()
            .init_resource::<Feedback>()
            .init_resource::<Time<Real>>()
            .add_message::<bevy::window::FileDragAndDrop>()
            .add_message::<PickedFile>()
            .add_systems(Update, dropped_files);
        let window = app.world_mut().spawn_empty().id();
        app.world_mut()
            .write_message(bevy::window::FileDragAndDrop::DroppedFile {
                window,
                path_buf: "first.txt".into(),
            });
        app.update();
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_millis(50));
        app.world_mut()
            .write_message(bevy::window::FileDragAndDrop::DroppedFile {
                window,
                path_buf: "second.txt".into(),
            });
        app.update();
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_millis(200));
        app.update();
        assert_eq!(app.world().resource::<Messages<PickedFile>>().len(), 0);
        assert!(
            app.world()
                .resource::<Feedback>()
                .0
                .contains("一次只支持一个文件")
        );
        app.world_mut()
            .write_message(bevy::window::FileDragAndDrop::DroppedFile {
                window,
                path_buf: "single.txt".into(),
            });
        app.update();
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_millis(200));
        app.update();
        assert_eq!(app.world().resource::<Messages<PickedFile>>().len(), 1);
    }
}
