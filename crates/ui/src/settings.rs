#[cfg(not(target_os = "android"))]
use super::dialog::{Dialogs, DirectoryPicked};
use super::{Action, Intent, theme::*, widgets::*};
use bevy::{
    prelude::*,
    tasks::{IoTaskPool, Task, block_on, poll_once},
};
use portalo_core::{
    AppState,
    settings::{Preferences, SharedSettings, default_download_directory, persist},
};

pub struct SettingsPlugin;
#[derive(Resource)]
pub struct LocalDevice {
    pub name: String,
    pub addresses: String,
}
impl Default for LocalDevice {
    fn default() -> Self {
        Self {
            name: "本机".into(),
            addresses: "正在读取网络信息…".into(),
        }
    }
}
#[derive(Component)]
pub struct LocalName;
#[derive(Component)]
pub struct LocalAddresses;
#[derive(Component)]
struct DirectoryLabel;
#[derive(Component)]
struct AccentLabel;
#[derive(Resource, Default)]
struct Jobs {
    scan: Option<Task<LocalDevice>>,
    save: Option<Task<Result<Preferences, String>>>,
    picking_directory: bool,
}
#[cfg(not(target_os = "android"))]
struct ClipboardHolder(Option<arboard::Clipboard>);

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SharedSettings>()
            .init_resource::<LocalDevice>()
            .init_resource::<Jobs>()
            .add_systems(Startup, (initialize, scan))
            .add_systems(OnEnter(AppState::Setting), setup)
            .add_systems(OnEnter(AppState::Licenses), license_page)
            .add_systems(Update, (actions, poll_jobs, refresh_labels).chain())
            .add_systems(Update, receive_feedback)
            .add_systems(
                Update,
                scan.run_if(bevy::time::common_conditions::on_real_timer(
                    std::time::Duration::from_secs(30),
                )),
            );
        #[cfg(not(target_os = "android"))]
        app.insert_non_send(ClipboardHolder(None))
            .add_systems(Update, (directory_picked, copy_addresses));
        #[cfg(target_os = "android")]
        app.add_systems(
            Update,
            (copy_addresses, android_directory_picked.before(poll_jobs)),
        );
    }
}

fn initialize(settings: Res<SharedSettings>, mut theme: ResMut<Theme>) {
    theme.accent = settings.0.read().unwrap().accent;
}
fn scan(mut jobs: ResMut<Jobs>) {
    if jobs.scan.is_some() {
        return;
    }
    jobs.scan = Some(IoTaskPool::get().spawn(async {
        let name = portalo_core::device::device_name();
        let mut addresses: Vec<_> = local_ip_address::list_afinet_netifas()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, ip)| ip.is_ipv4() && !ip.is_loopback())
            .map(|(_, ip)| ip.to_string())
            .collect();
        addresses.sort();
        addresses.dedup();
        LocalDevice {
            name,
            addresses: if addresses.is_empty() {
                "未找到可用的局域网 IPv4 地址".into()
            } else {
                addresses.join("\n")
            },
        }
    }));
}

pub fn connection_page(mut commands: Commands, local: Res<LocalDevice>) {
    let body = page(
        &mut commands,
        AppState::Connection,
        "连接信息",
        "两台设备需要连接到同一局域网",
    );
    action(&mut commands, body, "‹ 返回附近设备", Action::Back, false);
    text(&mut commands, body, "设备名称", 14.0, MUTED);
    let name = text(&mut commands, body, &local.name, 26.0, INK);
    commands.entity(name).insert(LocalName);
    text(&mut commands, body, "本机 IPv4 地址", 14.0, MUTED);
    let addresses = text(&mut commands, body, &local.addresses, 22.0, INK);
    commands.entity(addresses).insert(LocalAddresses);
    action(&mut commands, body, "复制地址", Action::CopyAddresses, true);
    text(
        &mut commands,
        body,
        "TCP 49527 · Portalo 协议 v1\n自动发现使用 mDNS；发送与接收设备均需使用新版 Portalo。",
        14.0,
        MUTED,
    );
}

fn setup(mut commands: Commands, settings: Res<SharedSettings>) {
    let body = page(
        &mut commands,
        AppState::Setting,
        "设置",
        "简单的工具，也可以有自己的颜色",
    );
    action(&mut commands, body, "‹ 返回附近设备", Action::Back, false);
    let label = text(&mut commands, body, "主题色", 24.0, INK);
    commands.entity(label).insert(AccentLabel);
    for (index, name) in ["活力橙", "海洋蓝", "森林绿"].into_iter().enumerate() {
        action(
            &mut commands,
            body,
            name,
            Action::Accent(index),
            index == settings.0.read().unwrap().accent,
        );
    }
    text(&mut commands, body, "接收目录", 24.0, INK);
    let directory = text(
        &mut commands,
        body,
        settings.0.read().unwrap().directory_label(),
        14.0,
        MUTED,
    );
    commands.entity(directory).insert(DirectoryLabel);
    action(
        &mut commands,
        body,
        "选择接收文件夹",
        Action::ChooseDirectory,
        false,
    );
    #[cfg(target_os = "android")]
    {
        action(
            &mut commands,
            body,
            "使用应用外部存储目录",
            Action::UseExternalDirectory,
            false,
        );
        text(
            &mut commands,
            body,
            "系统选择器可选择下载目录中的子文件夹或 SD 卡目录。\nAndroid 11 及以上限制选择存储根目录及 Download 根目录。\n目录变更仅影响之后建立的接收任务。",
            13.0,
            MUTED,
        );
    }
    action(
        &mut commands,
        body,
        "恢复默认接收目录",
        Action::ResetDirectory,
        false,
    );
    text(
        &mut commands,
        body,
        "Portalo 0.0.1\n界面灵感来自 Dukto R6 · 本地传输，无需账号",
        13.0,
        MUTED,
    );
    action(
        &mut commands,
        body,
        "字体许可",
        Action::Open(AppState::Licenses),
        false,
    );
}

fn license_page(mut commands: Commands) {
    let body = page(
        &mut commands,
        AppState::Licenses,
        "字体许可",
        "文泉驿微米黑 · 随应用内嵌，离线可用",
    );
    action(&mut commands, body, "‹ 返回设置", Action::Back, false);
    text(
        &mut commands,
        body,
        include_str!("../assets/FONT-NOTICE.txt"),
        14.0,
        INK,
    );
    text(
        &mut commands,
        body,
        include_str!("../assets/LICENSE-Apache-2.0.txt"),
        13.0,
        MUTED,
    );
}

fn save(jobs: &mut Jobs, preferences: Preferences, feedback: &mut Feedback) {
    feedback.0 = "正在保存设置…".into();
    jobs.save = Some(IoTaskPool::get().spawn(async move {
        persist(&preferences)
            .map(|_| preferences)
            .map_err(|e| e.to_string())
    }));
}

fn actions(
    mut messages: MessageReader<Intent>,
    settings: Res<SharedSettings>,
    mut jobs: ResMut<Jobs>,
    mut feedback: ResMut<Feedback>,
    #[cfg(not(target_os = "android"))] mut dialogs: ResMut<Dialogs>,
) {
    for Intent(action) in messages.read() {
        if jobs.save.is_some() || jobs.picking_directory {
            continue;
        }
        let mut preferences = settings.0.read().unwrap().clone();
        match action {
            Action::Accent(index) => {
                preferences.accent = *index;
                save(&mut jobs, preferences, &mut feedback);
            }
            Action::ResetDirectory => {
                preferences.use_directory(default_download_directory());
                save(&mut jobs, preferences, &mut feedback);
            }
            Action::ChooseDirectory => {
                #[cfg(not(target_os = "android"))]
                {
                    jobs.picking_directory = true;
                    dialogs.directory();
                }
                #[cfg(target_os = "android")]
                {
                    jobs.picking_directory = true;
                    feedback.0 = "请选择接收文件夹，并允许长期读写访问。".into();
                    if let Err(error) = portalo_core::android_storage::pick_directory() {
                        jobs.picking_directory = false;
                        feedback.0 = format!("无法打开文件夹选择器：{error}");
                    }
                }
            }
            #[cfg(target_os = "android")]
            Action::UseExternalDirectory => {
                if let Some(path) = bevy::android::ANDROID_APP
                    .get()
                    .and_then(|app| app.external_data_path())
                {
                    preferences.use_directory(path.join("downloads"));
                    save(&mut jobs, preferences, &mut feedback);
                } else {
                    feedback.0 = "此设备的应用外部存储不可用。".into();
                }
            }
            _ => {}
        }
    }
}

fn poll_jobs(
    mut jobs: ResMut<Jobs>,
    mut local: ResMut<LocalDevice>,
    settings: Res<SharedSettings>,
    mut theme: ResMut<Theme>,
    mut feedback: ResMut<Feedback>,
) {
    if let Some(task) = jobs.scan.as_mut()
        && let Some(value) = block_on(poll_once(task))
    {
        *local = value;
        jobs.scan = None;
    }
    if let Some(task) = jobs.save.as_mut()
        && let Some(result) = block_on(poll_once(task))
    {
        jobs.save = None;
        match result {
            Ok(value) => {
                theme.accent = value.accent;
                *settings.0.write().unwrap() = value;
                feedback.0 = "设置已保存，新的接收任务将使用此目录。".into();
            }
            Err(error) => feedback.0 = format!("设置未保存：{error}"),
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn refresh_labels(
    settings: Res<SharedSettings>,
    local: Res<LocalDevice>,
    theme: Res<Theme>,
    mut labels: Query<
        (
            &mut Text,
            Has<LocalName>,
            Has<LocalAddresses>,
            Has<DirectoryLabel>,
            Has<AccentLabel>,
        ),
        Or<(
            With<LocalName>,
            With<LocalAddresses>,
            With<DirectoryLabel>,
            With<AccentLabel>,
        )>,
    >,
    mut buttons: Query<(
        Entity,
        &mut ActionButton,
        Has<bevy::ui::InteractionDisabled>,
    )>,
    jobs: Res<Jobs>,
    mut commands: Commands,
    added: Query<Entity, Added<AccentLabel>>,
) {
    let config = settings.0.read().unwrap();
    if local.is_changed() || theme.is_changed() || !added.is_empty() {
        for (mut label, name, addresses, directory, accent) in &mut labels {
            let value = if name {
                Some(local.name.clone())
            } else if addresses {
                Some(local.addresses.clone())
            } else if directory {
                Some(config.directory_label())
            } else if accent {
                Some(format!(
                    "主题色 · {}",
                    ["活力橙", "海洋蓝", "森林绿"][theme.accent.min(2)]
                ))
            } else {
                None
            };
            if let Some(value) = value
                && label.0 != value
            {
                label.0 = value;
            }
        }
    }
    for (entity, mut button, disabled) in &mut buttons {
        if let Action::Accent(index) = button.0 {
            let primary = index == theme.accent;
            if button.1 != primary {
                button.1 = primary;
            }
        }
        let settings_control = match button.0 {
            Action::Accent(_) | Action::ChooseDirectory | Action::ResetDirectory => true,
            #[cfg(target_os = "android")]
            Action::UseExternalDirectory => true,
            _ => false,
        };
        if settings_control {
            let busy = jobs.save.is_some() || jobs.picking_directory;
            if busy != disabled {
                if busy {
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
}

#[cfg(not(target_os = "android"))]
fn directory_picked(
    mut input: MessageReader<DirectoryPicked>,
    settings: Res<SharedSettings>,
    mut jobs: ResMut<Jobs>,
    mut feedback: ResMut<Feedback>,
) {
    for event in input.read() {
        jobs.picking_directory = false;
        let Some(path) = &event.0 else {
            feedback.0 = "已取消更改接收目录。".into();
            continue;
        };
        let mut preferences = settings.0.read().unwrap().clone();
        preferences.use_directory(path.clone());
        save(&mut jobs, preferences, &mut feedback);
    }
}

#[cfg(target_os = "android")]
fn android_directory_picked(
    settings: Res<SharedSettings>,
    mut jobs: ResMut<Jobs>,
    mut feedback: ResMut<Feedback>,
) {
    let Some(result) = super::android::take_directory_result() else {
        return;
    };
    jobs.picking_directory = false;
    match result {
        Ok(Some(tree)) => {
            let mut preferences = settings.0.read().unwrap().clone();
            preferences.document_tree = Some(tree);
            save(&mut jobs, preferences, &mut feedback);
        }
        Ok(None) => feedback.0 = "已取消选择，原接收目录保持不变。".into(),
        Err(error) => feedback.0 = error,
    }
}

fn receive_feedback(
    notices: Option<ResMut<portalo_core::ReceiveFeedback>>,
    mut feedback: ResMut<Feedback>,
) {
    if let Some(mut notices) = notices
        && notices.0.has_changed().unwrap_or(false)
    {
        feedback.0 = notices.0.borrow_and_update().clone();
    }
}

#[cfg(not(target_os = "android"))]
fn copy_addresses(
    mut messages: MessageReader<Intent>,
    local: Res<LocalDevice>,
    mut clipboard: NonSendMut<ClipboardHolder>,
    mut feedback: ResMut<Feedback>,
) {
    for Intent(action) in messages.read() {
        if !matches!(action, Action::CopyAddresses) {
            continue;
        }
        let result = (|| {
            if clipboard.0.is_none() {
                clipboard.0 = Some(arboard::Clipboard::new()?);
            }
            clipboard
                .0
                .as_mut()
                .unwrap()
                .set_text(local.addresses.clone())
        })();
        feedback.0 = match result {
            Ok(()) => "地址已复制到剪贴板。".into(),
            Err(e) => format!("复制失败：{e}"),
        };
    }
}

#[cfg(target_os = "android")]
fn copy_addresses(
    mut messages: MessageReader<Intent>,
    local: Res<LocalDevice>,
    mut feedback: ResMut<Feedback>,
) {
    for Intent(action) in messages.read() {
        if matches!(action, Action::CopyAddresses) {
            feedback.0 = match super::android::copy_text(&local.addresses) {
                Ok(()) => "地址已复制到剪贴板。".into(),
                Err(e) => format!("复制失败：{e}"),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receiver_error_is_visible_without_replaying_after_user_action() {
        let (tx, rx) = tokio::sync::watch::channel(String::new());
        let mut app = App::new();
        app.init_resource::<Feedback>()
            .insert_resource(portalo_core::ReceiveFeedback(rx))
            .add_systems(Update, receive_feedback);
        tx.send_replace("接收目录授权已失效，请重新选择".into());
        app.update();
        assert!(app.world().resource::<Feedback>().0.contains("重新选择"));
        app.world_mut().resource_mut::<Feedback>().0 = "新目录已保存".into();
        app.update();
        assert_eq!(app.world().resource::<Feedback>().0, "新目录已保存");
    }
}
