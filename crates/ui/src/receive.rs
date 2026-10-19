use super::{
    Action, Intent,
    theme::*,
    widgets::{action, text},
};
use bevy::prelude::*;
use portalo_core::receive::{ReceivePhase, ReceiveTask, ReceiveTransfers};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub(super) struct ReceivePlugin;
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ReceiveSystems;
impl Plugin for ReceivePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReceiveTransfers>()
            .init_resource::<ReceiveUi>()
            .add_systems(Update, (poll, render).chain().in_set(ReceiveSystems));
    }
}

#[derive(Component)]
struct ReceivePanel {
    summary: Entity,
    list: Entity,
}
#[derive(Component, Clone, Copy)]
struct ReceiveCard {
    id: u64,
    title: Entity,
    phase: Entity,
    bytes: Entity,
    speed: Entity,
    error: Entity,
    fill: Entity,
    close: Entity,
}

pub(super) fn mount(commands: &mut Commands, parent: Entity) {
    let panel = commands
        .spawn(Node {
            display: Display::None,
            width: percent(100),
            flex_shrink: 0.0,
            flex_direction: FlexDirection::Column,
            row_gap: px(5),
            ..default()
        })
        .id();
    let summary = text(commands, panel, "", 12.0, MUTED);
    let list = commands
        .spawn(Node {
            width: percent(100),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            ..default()
        })
        .id();
    commands
        .entity(panel)
        .add_child(list)
        .insert(ReceivePanel { summary, list });
    commands.entity(parent).add_child(panel);
}

struct RateSample {
    at: Instant,
    bytes: u64,
    speed: f64,
    receiving: bool,
}
impl RateSample {
    fn new(task: &ReceiveTask) -> Self {
        Self {
            at: task.started_at,
            bytes: 0,
            speed: 0.0,
            receiving: task.phase == ReceivePhase::Receiving,
        }
    }
    fn sample(&mut self, task: &ReceiveTask, now: Instant) {
        if task.phase != ReceivePhase::Receiving {
            self.speed = 0.0;
            self.receiving = false;
            self.at = now;
            self.bytes = task.received;
            return;
        }
        if !self.receiving {
            self.receiving = true;
            self.at = now;
            self.bytes = task.received;
            self.speed = 0.0;
            return;
        }
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        if elapsed < 0.5 {
            return;
        }
        let delta = task.received.saturating_sub(self.bytes);
        let instant = delta as f64 / elapsed;
        self.speed = if delta == 0 {
            0.0
        } else if self.speed == 0.0 {
            instant
        } else {
            self.speed * 0.3 + instant * 0.7
        };
        self.bytes = task.received;
        self.at = now;
    }
}

#[derive(Resource)]
struct ReceiveUi {
    revision: Option<u64>,
    tasks: Vec<ReceiveTask>,
    rates: HashMap<u64, RateSample>,
    sampled_at: Instant,
}
impl Default for ReceiveUi {
    fn default() -> Self {
        Self {
            revision: None,
            tasks: Vec::new(),
            rates: HashMap::new(),
            sampled_at: Instant::now(),
        }
    }
}

fn poll(
    transfers: Res<ReceiveTransfers>,
    mut ui: ResMut<ReceiveUi>,
    mut intents: MessageReader<Intent>,
) {
    for Intent(action) in intents.read() {
        if let Action::DismissReceive(id) = action {
            transfers.dismiss(*id);
        }
    }
    let now = Instant::now();
    let sample_due = now.saturating_duration_since(ui.sampled_at) >= Duration::from_millis(500)
        && ui
            .tasks
            .iter()
            .any(|task| task.phase == ReceivePhase::Receiving);
    let snapshot = transfers.snapshot_since(if sample_due { None } else { ui.revision });
    if snapshot.is_none() && !sample_due {
        return;
    }
    if let Some((revision, tasks)) = snapshot {
        ui.revision = Some(revision);
        ui.tasks = tasks;
    }
    // Only real byte/phase changes or a rate sample tick mark this resource changed.
    let ui = &mut *ui;
    ui.rates
        .retain(|id, _| ui.tasks.iter().any(|task| task.id == *id));
    for task in &ui.tasks {
        ui.rates
            .entry(task.id)
            .or_insert_with(|| RateSample::new(task))
            .sample(task, now);
    }
    if sample_due {
        ui.sampled_at = now;
    }
}

fn visible_tasks(tasks: &[ReceiveTask]) -> Vec<&ReceiveTask> {
    let active: Vec<_> = tasks.iter().filter(|task| task.phase.is_active()).collect();
    if !active.is_empty() {
        active
    } else {
        tasks
            .iter()
            .max_by_key(|task| (task.finished_at, task.id))
            .into_iter()
            .collect()
    }
}

fn card(commands: &mut Commands, parent: Entity, id: u64) -> (Entity, ReceiveCard) {
    let entity = commands
        .spawn((
            Node {
                width: percent(100),
                padding: UiRect::all(px(8)),
                border: UiRect::left(px(3)),
                flex_direction: FlexDirection::Column,
                row_gap: px(3),
                flex_shrink: 0.0,
                ..default()
            },
            BackgroundColor(Color::WHITE),
            BorderColor::all(LINE),
        ))
        .id();
    let header = commands
        .spawn(Node {
            width: percent(100),
            align_items: AlignItems::Center,
            column_gap: px(4),
            ..default()
        })
        .id();
    commands.entity(entity).add_child(header);
    let title = text(commands, header, "", 14.0, INK);
    commands.entity(title).insert((
        Node {
            flex_grow: 1.0,
            min_width: px(0),
            overflow: Overflow::clip(),
            ..default()
        },
        TextLayout::no_wrap(),
    ));
    let close = action(commands, header, "关闭", Action::DismissReceive(id), false);
    commands.entity(close).insert((
        Node {
            display: Display::None,
            width: px(48),
            min_width: px(48),
            min_height: px(48),
            padding: UiRect::all(px(4)),
            border: UiRect::all(px(1)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        bevy::ui::InteractionDisabled,
    ));
    let phase = text(commands, entity, "", 12.0, MUTED);
    let bytes = text(commands, entity, "", 13.0, INK);
    let speed = text(commands, entity, "", 13.0, MUTED);
    for label in [phase, bytes, speed] {
        commands.entity(label).insert((
            Node {
                width: percent(100),
                min_width: px(0),
                overflow: Overflow::clip(),
                ..default()
            },
            TextLayout::no_wrap(),
        ));
    }
    let track = commands
        .spawn((
            Node {
                width: percent(100),
                height: px(4),
                min_height: px(4),
                ..default()
            },
            BackgroundColor(LINE),
        ))
        .id();
    let fill = commands
        .spawn((
            Node {
                width: percent(0),
                height: percent(100),
                ..default()
            },
            BackgroundColor(SUCCESS),
        ))
        .id();
    commands.entity(track).add_child(fill);
    commands.entity(entity).add_child(track);
    let error = text(commands, entity, "", 12.0, ERROR);
    commands.entity(error).insert(Node {
        display: Display::None,
        ..default()
    });
    commands.entity(parent).add_child(entity);
    (
        entity,
        ReceiveCard {
            id,
            title,
            phase,
            bytes,
            speed,
            error,
            fill,
            close,
        },
    )
}

fn clipped(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut text: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

fn format_bytes(value: f64) -> String {
    let mut value = value.max(0.0);
    let mut unit = 0;
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

fn phase_text(phase: &ReceivePhase) -> &'static str {
    match phase {
        ReceivePhase::Preparing => "准备接收",
        ReceivePhase::Receiving => "正在接收",
        ReceivePhase::Saving => "正在保存",
        ReceivePhase::Confirming => "文件已保存，正在确认",
        ReceivePhase::Completed => "接收完成",
        ReceivePhase::SavedWithoutConfirmation(_) => "文件已保存，确认未送达",
        ReceivePhase::Failed(_) => "接收失败",
    }
}

fn speed_text(task: &ReceiveTask, speed: f64, now: Instant) -> String {
    match task.phase {
        ReceivePhase::Preparing => "等待文件信息或准备接收位置…".into(),
        ReceivePhase::Receiving => {
            let mut text = format!("速度 {}/s", format_bytes(speed));
            if speed >= 1.0
                && let Some(total) = task.total
            {
                let seconds = total.saturating_sub(task.received) as f64 / speed;
                text.push_str(&format!(" · 剩余 {seconds:.0} 秒"));
            }
            text
        }
        ReceivePhase::Saving => "正在写入并保存文件，请稍候…".into(),
        ReceivePhase::Confirming => "正在向发送方确认接收结果…".into(),
        ReceivePhase::Completed | ReceivePhase::SavedWithoutConfirmation(_) => format!(
            "耗时 {:.1} 秒 · 平均 {}/s",
            task.elapsed(now).as_secs_f64(),
            format_bytes(task.average_bytes_per_second())
        ),
        ReceivePhase::Failed(_) => format!(
            "接收已停止 · 用时 {:.1} 秒",
            task.elapsed(now).as_secs_f64()
        ),
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn render(
    mut commands: Commands,
    ui: Res<ReceiveUi>,
    theme: Res<Theme>,
    panels: Query<(Entity, &ReceivePanel)>,
    added: Query<Entity, Added<ReceivePanel>>,
    cards: Query<(Entity, &ReceiveCard)>,
    mut labels: Query<(&mut Text, &mut TextColor)>,
    mut nodes: Query<&mut Node>,
    mut backgrounds: Query<&mut BackgroundColor>,
    children: Query<&Children>,
) {
    if !ui.is_changed() && !theme.is_changed() && added.is_empty() {
        return;
    }
    let Ok((panel_entity, panel)) = panels.single() else {
        return;
    };
    if let Ok(mut node) = nodes.get_mut(panel_entity) {
        let display = if ui.tasks.is_empty() {
            Display::None
        } else {
            Display::Flex
        };
        if node.display != display {
            node.display = display;
        }
    }
    let active = ui
        .tasks
        .iter()
        .filter(|task| task.phase.is_active())
        .count();
    let finished = ui.tasks.len() - active;
    if let Ok((mut text, _)) = labels.get_mut(panel.summary) {
        let value = if active > 0 {
            format!("接收通知 · {active} 个进行中 · {finished} 条结果")
        } else if finished > 1 {
            format!("接收结果 · {finished} 条（关闭后显示上一条）")
        } else {
            "接收结果".into()
        };
        if text.0 != value {
            text.0 = value;
        }
    }
    let visible = visible_tasks(&ui.tasks);
    for (entity, row) in &cards {
        if !visible.iter().any(|task| task.id == row.id) {
            commands.entity(entity).despawn();
        }
    }
    let mut order = Vec::new();
    for task in visible {
        let (entity, row) =
            if let Some((entity, row)) = cards.iter().find(|(_, row)| row.id == task.id) {
                (entity, *row)
            } else {
                let (entity, row) = card(&mut commands, panel.list, task.id);
                // Fill newly created texts via deferred commands too; they are not
                // yet in queries, and must appear even if the next frame is idle.
                commands.entity(entity).insert(row);
                (entity, row)
            };
        order.push(entity);
        let color = match task.phase {
            ReceivePhase::Completed => SUCCESS,
            ReceivePhase::Failed(_) => ERROR,
            ReceivePhase::SavedWithoutConfirmation(_) => Color::srgb(0.75, 0.38, 0.08),
            _ => theme.color(),
        };
        let title = if task.file_name.is_empty() {
            "未识别文件".into()
        } else {
            clipped(&task.file_name, 64)
        };
        let bytes = match task.total {
            Some(total) => format!(
                "{:.1}% · {} / {}",
                task.progress() * 100.0,
                format_bytes(task.received as f64),
                format_bytes(total as f64)
            ),
            None => "尚未取得文件大小".into(),
        };
        let error = match &task.phase {
            ReceivePhase::Failed(error) | ReceivePhase::SavedWithoutConfirmation(error) => {
                if error.starts_with("Incomplete file:") {
                    "连接提前断开，文件未接收完整。".into()
                } else if error == "deadline has elapsed" {
                    "接收超时，请检查连接或存储状态。".into()
                } else {
                    clipped(error, 240)
                }
            }
            _ => String::new(),
        };
        let values = [
            (row.title, title, INK),
            (
                row.phase,
                format!("{} · 来自 {}", phase_text(&task.phase), task.source.ip()),
                color,
            ),
            (row.bytes, bytes, INK),
            (
                row.speed,
                speed_text(
                    task,
                    ui.rates.get(&task.id).map_or(0.0, |rate| rate.speed),
                    Instant::now(),
                ),
                MUTED,
            ),
            (row.error, error.clone(), color),
        ];
        for (entity, value, color) in values {
            if let Ok((mut text, mut text_color)) = labels.get_mut(entity) {
                if text.0 != value {
                    text.0 = value;
                }
                if text_color.0 != color {
                    text_color.0 = color;
                }
            } else {
                commands
                    .entity(entity)
                    .insert((Text::new(value), TextColor(color)));
            }
        }
        let width = percent(task.progress() * 100.0);
        if let Ok(mut node) = nodes.get_mut(row.fill) {
            if node.width != width {
                node.width = width;
            }
        } else {
            commands
                .entity(row.fill)
                .entry::<Node>()
                .and_modify(move |mut node| node.width = width);
        }
        if let Ok(mut background) = backgrounds.get_mut(row.fill) {
            if background.0 != color {
                background.0 = color;
            }
        } else {
            commands.entity(row.fill).insert(BackgroundColor(color));
        }
        for (entity, display) in [
            (
                row.error,
                if error.is_empty() {
                    Display::None
                } else {
                    Display::Flex
                },
            ),
            (
                row.close,
                if task.phase.is_active() {
                    Display::None
                } else {
                    Display::Flex
                },
            ),
        ] {
            if let Ok(mut node) = nodes.get_mut(entity) {
                if node.display != display {
                    node.display = display;
                }
            } else {
                commands
                    .entity(entity)
                    .entry::<Node>()
                    .and_modify(move |mut node| node.display = display);
            }
        }
        if task.phase.is_active() {
            commands
                .entity(row.close)
                .insert(bevy::ui::InteractionDisabled);
        } else {
            commands
                .entity(row.close)
                .remove::<bevy::ui::InteractionDisabled>();
        }
    }
    if !children
        .get(panel.list)
        .is_ok_and(|children| children.iter().eq(order.iter().copied()))
    {
        commands.entity(panel.list).replace_children(&order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(ledger: &ReceiveTransfers) -> ReceiveTask {
        ledger.begin("127.0.0.1:1000".parse().unwrap()).unwrap()
    }
    #[test]
    fn rate_sampling_counts_previous_updates_and_stalls_clear_speed() {
        let ledger = ReceiveTransfers::default();
        let mut task = task(&ledger);
        task.phase = ReceivePhase::Receiving;
        let mut rate = RateSample::new(&task);
        task.received = 1024;
        rate.sample(&task, task.started_at + Duration::from_millis(100));
        assert_eq!(rate.speed, 0.0);
        rate.sample(&task, task.started_at + Duration::from_millis(500));
        assert_eq!(rate.speed, 2048.0);
        rate.sample(&task, task.started_at + Duration::from_secs(1));
        assert_eq!(rate.speed, 0.0);
        assert!(!speed_text(&task, rate.speed, task.started_at).contains("剩余"));
        task.phase = ReceivePhase::Preparing;
        rate.sample(&task, task.started_at + Duration::from_secs(2));
        task.phase = ReceivePhase::Receiving;
        rate.sample(&task, task.started_at + Duration::from_secs(30));
        task.received += 1024;
        rate.sample(&task, task.started_at + Duration::from_millis(30_500));
        assert_eq!(rate.speed, 2048.0);
    }

    #[test]
    fn active_tasks_take_priority_and_only_latest_result_is_expanded() {
        let ledger = ReceiveTransfers::default();
        let mut a = task(&ledger);
        let b = task(&ledger);
        a.phase = ReceivePhase::Completed;
        a.finished_at = Some(Instant::now());
        let tasks = vec![a, b];
        assert_eq!(visible_tasks(&tasks).len(), 1);
        assert_eq!(visible_tasks(&tasks)[0].id, tasks[1].id);
        let mut tasks = tasks;
        tasks[1].phase = ReceivePhase::Completed;
        tasks[1].finished_at = Some(Instant::now());
        assert_eq!(visible_tasks(&tasks).len(), 1);
        assert_eq!(visible_tasks(&tasks)[0].id, tasks[1].id);
    }

    #[test]
    fn cards_survive_navigation_without_affecting_send_and_only_terminal_cards_close() {
        use portalo_core::{AppState, FileTransferState, TransferStatus};
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .init_state::<AppState>()
            .init_resource::<Theme>()
            .add_message::<Intent>()
            .insert_resource(FileTransferState {
                status: TransferStatus::Sending,
                ..default()
            })
            .add_plugins(ReceivePlugin)
            .add_systems(OnEnter(AppState::Menu), |mut commands: Commands| {
                super::super::widgets::page(&mut commands, AppState::Menu, "Menu", "");
            })
            .add_systems(OnEnter(AppState::Setting), |mut commands: Commands| {
                super::super::widgets::page(&mut commands, AppState::Setting, "Settings", "");
            });
        let ledger = app.world().resource::<ReceiveTransfers>().clone();
        let mut task = task(&ledger);
        task.file_name = "接收测试.bin".into();
        task.total = Some(100);
        task.received = 50;
        task.phase = ReceivePhase::Receiving;
        ledger.update(task.clone());
        app.update();
        let (entity, row) = app
            .world_mut()
            .query::<(Entity, &ReceiveCard)>()
            .single(app.world())
            .map(|(e, r)| (e, *r))
            .unwrap();
        assert!(
            app.world()
                .get::<Text>(row.bytes)
                .unwrap()
                .0
                .contains("50.0%")
        );
        app.world_mut()
            .write_message(Intent(Action::DismissReceive(task.id)));
        app.update();
        assert!(app.world().get::<ReceiveCard>(entity).is_some());
        task.received = 75;
        ledger.update(task.clone());
        app.update();
        assert!(
            app.world()
                .get::<Text>(row.bytes)
                .unwrap()
                .0
                .contains("75.0%")
        );
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Setting);
        app.update();
        assert!(app.world().get_entity(entity).is_err());
        let row = *app
            .world_mut()
            .query::<&ReceiveCard>()
            .single(app.world())
            .unwrap();
        assert!(
            app.world()
                .get::<Text>(row.bytes)
                .unwrap()
                .0
                .contains("75.0%")
        );
        task.received = 100;
        task.phase = ReceivePhase::Completed;
        task.saved_at = Some(Instant::now());
        task.finished_at = task.saved_at;
        ledger.update(task.clone());
        app.update();
        assert!(
            app.world()
                .get::<bevy::ui::InteractionDisabled>(row.close)
                .is_none()
        );
        assert_eq!(
            app.world().resource::<FileTransferState>().status,
            TransferStatus::Sending
        );
        app.world_mut()
            .write_message(Intent(Action::DismissReceive(task.id)));
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&ReceiveCard>()
                .iter(app.world())
                .count(),
            0
        );
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Menu);
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&ReceiveCard>()
                .iter(app.world())
                .count(),
            0
        );
    }
}
