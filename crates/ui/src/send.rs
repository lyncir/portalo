#[cfg(not(target_os = "android"))]
use super::dialog::{self, Dialogs};
use super::{Action, Intent, PickedFile, SelectedPeer, TransferDetails, theme::*, widgets::*};
use bevy::prelude::*;
use portalo_core::{AppState, FileTransferState, ProgressSender, TransferStatus};
use portalo_network::{SendSource, TokioRuntime, transfer};

pub struct SendPlugin;
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
        app.init_resource::<Dialogs>()
            .add_message::<dialog::DirectoryPicked>()
            .add_systems(Update, dialog::poll.before(picked));
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
    action(
        &mut commands,
        body,
        "选择文件夹并发送",
        Action::PickFolder,
        true,
    );
    super::transfers::mount(&mut commands, body);
    let status = text(&mut commands, body, "", 15.0, MUTED);
    commands.entity(status).insert(PickerStatus);
    #[cfg(not(target_os = "android"))]
    text(
        &mut commands,
        body,
        "支持拖入多个文件或文件夹。\n按块暂存并发送，需预留缓存空间并保留原文件。",
        14.0,
        MUTED,
    );
    #[cfg(target_os = "android")]
    text(
        &mut commands,
        body,
        "支持重开的单文件按块暂存发送。\n目录及其他来源需先缓存；请保留原文件。",
        14.0,
        MUTED,
    );
}

fn pick_action(
    mut intents: MessageReader<Intent>,
    mut selected: ResMut<SelectedPeer>,
    #[cfg(not(target_os = "android"))] mut dialogs: ResMut<Dialogs>,
    state: Res<State<AppState>>,
) {
    for Intent(intent) in intents.read() {
        if !matches!(intent, Action::PickFile | Action::PickFolder)
            || selected.picking
            || *state.get() != AppState::Send
        {
            continue;
        }
        selected.picking = true;
        #[cfg(not(target_os = "android"))]
        if matches!(intent, Action::PickFolder) {
            dialogs.send_directory();
        } else {
            dialogs.file();
        }
        #[cfg(target_os = "android")]
        {
            if matches!(intent, Action::PickFolder) {
                super::android::pick_folder();
            } else {
                super::android::pick_file();
            }
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
        if matches!(
            button.0,
            Action::PickFile | Action::PickFolder | Action::Back
        ) && disabled != selected.picking
        {
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

#[allow(clippy::too_many_arguments)]
fn picked(
    mut messages: MessageReader<PickedFile>,
    mut selected: ResMut<SelectedPeer>,
    peers: Res<portalo_discovery::PeerList>,
    runtime: Res<TokioRuntime>,
    mut next: ResMut<NextState<AppState>>,
    progress: Res<ProgressSender>,
    control: Res<transfer::TransferControl>,
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
        let Some(source) = event.source.clone() else {
            feedback.0 = "已取消选择文件，可以重新选择。".into();
            continue;
        };
        let Some((peer_id, peer)) = &selected.peer else {
            continue;
        };
        // Discovery expiry is not proof that TCP is unreachable. Prefer fresh
        // addresses, but retain the selected snapshot when multicast is absent.
        let peer = peers.peers.get(peer_id).unwrap_or(peer);
        let id = transfer.id.wrapping_add(1);
        *transfer = FileTransferState {
            id,
            status: TransferStatus::Preparing,
            last_update_time: time.elapsed_secs_f64(),
            ..default()
        };
        *details = TransferDetails {
            file_name: source.name().unwrap_or("未命名文件").to_owned(),
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
        tx.send_replace(portalo_core::TransferProgressMsg {
            id,
            status: TransferStatus::Preparing,
            detail: "正在建立文件清单；可重开的文件将按块暂存并发送。".into(),
            ..default()
        });
        control.0.send_replace(transfer::Command::Run);
        let control = control.0.subscribe();
        runtime.0.spawn(async move {
            let preparation_control = control.clone();
            let preparation_progress = tx.clone();
            let result = async {
                let task = tokio::task::spawn_blocking(move || {
                    let prepared = std::cell::Cell::new(0u64);
                    transfer::prepare_streaming_with_progress(
                        vec![source],
                        addresses,
                        &preparation_control,
                        &|bytes, path| {
                            prepared.set(prepared.get() + bytes);
                            preparation_progress.send_modify(|msg| {
                                msg.detail = format!(
                                    "正在扫描／准备来源 · 已缓存 {} MiB\n{path}",
                                    prepared.get() / 1024 / 1024
                                )
                            });
                        },
                    )
                })
                .await??;
                transfer::send(task, tx.clone(), id, control).await
            }
            .await;
            match result {
                Ok(address) => {
                    preferred.lock().unwrap().insert(peer_id, address);
                }
                Err(e) => {
                    error!("Send: {e:#}");
                    tx.send_modify(|msg| {
                        msg.id = id;
                        msg.status = TransferStatus::Failed(format!("{e:#}"));
                    });
                }
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
            if batch.paths.len() < 100_000 {
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
    } else {
        files.write(PickedFile {
            source: Some(SendSource::Batch(
                dropped.into_iter().map(SendSource::Path).collect(),
            )),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multi_file_drop_across_frames_is_collected_into_one_task() {
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
        let messages = app.world().resource::<Messages<PickedFile>>();
        let mut cursor = messages.get_cursor();
        let source = cursor
            .read(messages)
            .next()
            .unwrap()
            .source
            .as_ref()
            .unwrap();
        assert!(matches!(source,SendSource::Batch(paths) if paths.len()==2));
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
