use bevy::{
    input_focus::{InputDispatchPlugin, tab_navigation::TabNavigationPlugin},
    prelude::*,
};
use portalo_core::{AppState, FileTransferState};
use portalo_discovery::PeerInfo;

#[cfg(target_os = "android")]
pub mod android;
mod peers;
pub mod progress_bar;
mod receive;
mod send;
mod settings;
mod theme;
mod widgets;

#[derive(Clone)]
enum Action {
    Open(AppState),
    Select(String),
    PickFile,
    Back,
    CopyAddresses,
    Accent(usize),
    ChooseDirectory,
    #[cfg(target_os = "android")]
    UseExternalDirectory,
    ResetDirectory,
    ToggleDetails,
    DismissReceive(u64),
}
#[derive(Message)]
struct Intent(Action);
#[derive(Message)]
pub struct PickedFile {
    pub path: Option<std::path::PathBuf>,
}
#[derive(Resource, Default)]
struct SelectedPeer {
    peer: Option<(String, PeerInfo)>,
    picking: bool,
}
#[derive(Resource, Default)]
struct TransferDetails {
    file_name: String,
    peer_name: String,
    show_error: bool,
}

pub struct UIPlugin;
impl Plugin for UIPlugin {
    fn build(&self, app: &mut App) {
        theme::install_font(app);
        app.init_resource::<SelectedPeer>()
            .init_resource::<TransferDetails>()
            .init_resource::<theme::Theme>()
            .init_resource::<widgets::Feedback>()
            .init_resource::<widgets::Dragged>()
            .add_message::<Intent>()
            .add_message::<PickedFile>()
            .add_plugins((
                InputDispatchPlugin,
                TabNavigationPlugin,
                settings::SettingsPlugin,
                send::SendPlugin,
                progress_bar::ProgressBarPlugin,
                receive::ReceivePlugin,
            ))
            .add_systems(Startup, |mut commands: Commands| {
                commands.spawn(Camera2d);
            })
            .add_systems(OnEnter(AppState::Menu), peers::setup)
            .add_systems(OnEnter(AppState::Connection), settings::connection_page)
            .add_systems(
                Update,
                (
                    navigate,
                    peers::update,
                    widgets::styles.after(receive::ReceiveSystems),
                    theme::accent_labels,
                    widgets::feedback,
                    widgets::scroll,
                    widgets::scroll_focus.after(widgets::scroll),
                    keyboard_back,
                ),
            )
            .add_systems(
                PreUpdate,
                widgets::normalize_focus.before(bevy::input_focus::InputFocusSystems::Dispatch),
            )
            .add_systems(Update, widgets::scrollbar)
            .add_systems(Update, widgets::touch_actions)
            .add_observer(
                |_: On<Pointer<Press>>, mut drag: ResMut<widgets::Dragged>| {
                    drag.0 = false;
                },
            );
    }
}

fn navigate(
    mut intents: MessageReader<Intent>,
    mut next: ResMut<NextState<AppState>>,
    state: Res<State<AppState>>,
    peers: Res<portalo_discovery::PeerList>,
    transfer: Res<FileTransferState>,
    mut selected: ResMut<SelectedPeer>,
    mut feedback: ResMut<widgets::Feedback>,
) {
    for Intent(action) in intents.read() {
        if transfer.is_active() || selected.picking {
            if matches!(action, Action::Back) {
                feedback.0 = "正在处理文件，请等待当前操作完成。".into();
            }
            continue;
        }
        match action {
            Action::Open(page) => {
                feedback.0.clear();
                next.set(*page);
            }
            Action::Select(id) => {
                if let Some(peer) = peers.peers.get(id) {
                    selected.peer = Some((id.clone(), peer.clone()));
                    feedback.0.clear();
                    next.set(AppState::Send);
                }
            }
            Action::Back => {
                feedback.0.clear();
                next.set(match state.get() {
                    AppState::Transfer => AppState::Send,
                    AppState::Licenses => AppState::Setting,
                    _ => AppState::Menu,
                });
            }
            _ => {}
        }
    }
}

fn keyboard_back(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<AppState>>,
    mut messages: MessageWriter<Intent>,
) {
    if keys.just_pressed(KeyCode::Escape) && *state.get() != AppState::Menu {
        messages.write(Intent(Action::Back));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portalo_discovery::PeerList;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .init_state::<AppState>()
            .init_resource::<SelectedPeer>()
            .init_resource::<TransferDetails>()
            .init_resource::<FileTransferState>()
            .init_resource::<PeerList>()
            .init_resource::<settings::LocalDevice>()
            .init_resource::<widgets::Feedback>()
            .init_resource::<widgets::Dragged>()
            .add_message::<Intent>()
            .add_systems(OnEnter(AppState::Menu), peers::setup)
            .add_systems(OnEnter(AppState::Send), send::setup)
            .add_systems(OnEnter(AppState::Transfer), progress_bar::setup)
            .add_systems(Update, (navigate, peers::update));
        app.world_mut().resource_mut::<PeerList>().peers.insert(
            "phone".into(),
            PeerInfo {
                name: "测试手机.local.".into(),
                ips: vec!["192.168.1.2".into()],
                os: "android".into(),
                port: 49527,
            },
        );
        app.update();
        app
    }

    #[test]
    fn changed_address_preserves_device_entity_and_reentry_does_not_leak() {
        let mut app = app();
        let first = app
            .world_mut()
            .query_filtered::<Entity, With<peers::DeviceRow>>()
            .single(app.world())
            .unwrap();
        app.world_mut()
            .resource_mut::<PeerList>()
            .peers
            .get_mut("phone")
            .unwrap()
            .ips = vec!["192.168.1.3".into()];
        app.update();
        let second = app
            .world_mut()
            .query_filtered::<Entity, With<peers::DeviceRow>>()
            .single(app.world())
            .unwrap();
        assert_eq!(first, second);
        assert!(
            app.world_mut()
                .query::<&Text>()
                .iter(app.world())
                .any(|text| text.0.contains("192.168.1.3"))
        );
        let entities = app.world().entities().len();
        for _ in 0..3 {
            app.world_mut()
                .write_message(Intent(Action::Select("phone".into())));
            app.update();
            app.update();
            assert_eq!(
                *app.world().resource::<State<AppState>>().get(),
                AppState::Send
            );
            assert_eq!(
                app.world_mut()
                    .query_filtered::<Entity, With<peers::DeviceRow>>()
                    .iter(app.world())
                    .count(),
                0
            );
            app.world_mut().write_message(Intent(Action::Back));
            app.update();
            app.update();
            assert_eq!(
                *app.world().resource::<State<AppState>>().get(),
                AppState::Menu
            );
            assert_eq!(app.world().entities().len(), entities);
        }
    }

    #[test]
    fn active_transfer_and_file_preparation_prevent_navigation() {
        let mut app = app();
        app.world_mut().resource_mut::<SelectedPeer>().picking = true;
        app.world_mut()
            .write_message(Intent(Action::Select("phone".into())));
        app.update();
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Menu
        );
        app.world_mut().resource_mut::<SelectedPeer>().picking = false;
        app.world_mut().resource_mut::<FileTransferState>().status =
            portalo_core::TransferStatus::Sending;
        app.world_mut()
            .write_message(Intent(Action::Open(AppState::Setting)));
        app.update();
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Menu
        );
    }

    #[test]
    fn rapid_click_activates_once_without_deferred_pressed_state() {
        use bevy::picking::{
            backend::HitData,
            pointer::{Location, PointerId},
        };
        let mut app = app();
        let button = app
            .world_mut()
            .query::<(Entity, &widgets::ActionButton)>()
            .iter(app.world())
            .find(|(_, button)| matches!(button.0, Action::Open(AppState::Connection)))
            .unwrap()
            .0;
        let window = app.world_mut().spawn_empty().id();
        let location = Location {
            target: bevy::camera::RenderTarget::Window(bevy::window::WindowRef::Entity(window))
                .normalize(Some(window))
                .unwrap(),
            position: Vec2::ZERO,
        };
        let click = || {
            Pointer::new(
                PointerId::Mouse,
                location.clone(),
                Click {
                    button: PointerButton::Primary,
                    hit: HitData::new(window, 0.0, None, None),
                    duration: std::time::Duration::ZERO,
                },
                button,
            )
        };
        app.world_mut().trigger(click());
        assert_eq!(app.world().resource::<Messages<Intent>>().len(), 1);
        app.world_mut()
            .entity_mut(button)
            .insert(bevy::ui::InteractionDisabled);
        app.world_mut().trigger(click());
        assert_eq!(app.world().resource::<Messages<Intent>>().len(), 1);
        app.world_mut()
            .entity_mut(button)
            .remove::<bevy::ui::InteractionDisabled>();
        app.world_mut().resource_mut::<widgets::Dragged>().0 = true;
        app.world_mut().trigger(click());
        assert_eq!(app.world().resource::<Messages<Intent>>().len(), 1);
    }
}
