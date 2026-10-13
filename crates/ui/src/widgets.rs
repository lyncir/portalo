use super::{Action, Intent, theme::*};
use bevy::picking::hover::Hovered;
use bevy::{
    input_focus::{
        InputFocus,
        tab_navigation::{TabGroup, TabIndex},
    },
    prelude::*,
    ui::InteractionDisabled,
    ui_widgets::{Button, observe},
};

#[derive(Component)]
pub struct ActionButton(pub Action, pub bool);
#[derive(Component)]
pub struct ScrollArea;
#[derive(Component)]
pub(super) struct PageColumn;
#[derive(Component)]
pub(super) struct ScrollThumb;
#[derive(Component)]
pub struct Notice;
#[derive(Component)]
pub struct ButtonCaption;
#[derive(Resource, Default)]
pub struct Feedback(pub String);
#[derive(Resource, Default)]
pub struct Dragged(pub bool);

#[derive(Default)]
pub(super) struct TouchGestures(std::collections::HashMap<u64, (Vec2, Option<Entity>, bool)>);

/// Android can deliver touch start/end together. Picking may discard its touch
/// pointer before generating Click, so use raw touches for activation and retain
/// picking for dragging/hover visuals. Coordinates are converted to physical UI
/// space and checked against the scroll clip as well as the button rectangle.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn touch_actions(
    mut events: MessageReader<bevy::input::touch::TouchInput>,
    windows: Query<&Window>,
    buttons: Query<(
        Entity,
        &ActionButton,
        &ComputedNode,
        &UiGlobalTransform,
        Option<&CalculatedClip>,
        Has<InteractionDisabled>,
    )>,
    mut gestures: Local<TouchGestures>,
    mut messages: MessageWriter<Intent>,
    mut focus: ResMut<InputFocus>,
) {
    use bevy::input::touch::TouchPhase;
    for event in events.read() {
        let Ok(window) = windows.get(event.window) else {
            continue;
        };
        let position = event.position * window.scale_factor();
        let target = buttons
            .iter()
            .filter(|(_, _, node, transform, clip, disabled)| {
                if *disabled
                    || node.is_empty()
                    || clip.is_some_and(|clip| !clip.clip.contains(position))
                {
                    return false;
                }
                transform.try_inverse().is_some_and(|inverse| {
                    let local = inverse.transform_point2(position);
                    (local.abs().cmple(node.size() * 0.5)).all()
                })
            })
            .max_by_key(|(_, _, node, _, _, _)| node.stack_index())
            .map(|(entity, _, _, _, _, _)| entity);
        match event.phase {
            TouchPhase::Started => {
                if gestures.0.len() < 8 {
                    gestures.0.insert(event.id, (event.position, target, false));
                }
            }
            TouchPhase::Moved => {
                if let Some((start, _, moved)) = gestures.0.get_mut(&event.id) {
                    *moved |= event.position.distance_squared(*start) > 64.0;
                }
            }
            TouchPhase::Ended => {
                if let Some((start, pressed, moved)) = gestures.0.remove(&event.id)
                    && !moved
                    && event.position.distance_squared(start) <= 64.0
                    && pressed.is_some()
                    && pressed == target
                    && let Ok((entity, button, _, _, _, false)) = buttons.get(pressed.unwrap())
                {
                    focus.0 = Some(entity);
                    messages.write(Intent(button.0.clone()));
                }
            }
            TouchPhase::Canceled => {
                gestures.0.remove(&event.id);
            }
        }
    }
}

pub fn page(
    commands: &mut Commands,
    state: portalo_core::AppState,
    title: &str,
    subtitle: &str,
) -> Entity {
    let root = commands
        .spawn((
            DespawnOnExit(state),
            TabGroup::default(),
            Node {
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(BACKGROUND),
        ))
        .id();
    let column = commands
        .spawn((
            PageColumn,
            Node {
                width: percent(100),
                max_width: px(780),
                height: percent(100),
                padding: UiRect::all(px(20)),
                flex_direction: FlexDirection::Column,
                row_gap: px(12),
                ..default()
            },
        ))
        .id();
    commands.entity(root).add_child(column);
    let header = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                flex_shrink: 0.0,
                row_gap: px(4),
                ..default()
            },
            children![
                (label(title, 40.0, INK), AccentText),
                label(subtitle, 14.0, MUTED)
            ],
        ))
        .id();
    commands.entity(column).add_child(header);
    let viewport = commands
        .spawn(Node {
            width: percent(100),
            min_height: px(0),
            flex_grow: 1.0,
            ..default()
        })
        .id();
    commands.entity(column).add_child(viewport);
    let body = commands
        .spawn((
            ScrollArea,
            ScrollPosition::default(),
            Node {
                width: percent(100),
                height: percent(100),
                padding: UiRect::right(px(8)),
                min_height: px(0),
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                row_gap: px(14),
                overflow: Overflow::scroll_y(),
                ..default()
            },
        ))
        .observe(
            |drag: On<Pointer<Drag>>,
             scale: Res<UiScale>,
             mut dragged: ResMut<Dragged>,
             mut areas: Query<(&mut ScrollPosition, &ComputedNode), With<ScrollArea>>| {
                if drag.distance.length() < 8.0 {
                    return;
                }
                dragged.0 = true;
                if let Ok((mut pos, computed)) = areas.single_mut() {
                    let max = ((computed.content_size().y - computed.size().y)
                        * computed.inverse_scale_factor())
                    .max(0.0);
                    pos.y = (pos.y - drag.delta.y / scale.0).clamp(0.0, max);
                }
            },
        )
        .id();
    commands.entity(viewport).add_child(body);
    let thumb = commands
        .spawn((
            ScrollThumb,
            Node {
                position_type: PositionType::Absolute,
                right: px(0),
                width: px(3),
                display: Display::None,
                ..default()
            },
            BackgroundColor(LINE),
        ))
        .id();
    commands.entity(viewport).add_child(thumb);
    let notice = commands
        .spawn((
            label("", 13.0, MUTED),
            Notice,
            Node {
                flex_shrink: 0.0,
                min_height: px(20),
                ..default()
            },
        ))
        .id();
    commands.entity(column).add_child(notice);
    body
}

pub fn normalize_focus(mut focus: ResMut<InputFocus>, buttons: Query<(), With<ActionButton>>) {
    if let Some(entity) = focus.0
        && !buttons.contains(entity)
    {
        focus.0 = None;
    }
}

pub fn scroll_focus(
    focus: Res<InputFocus>,
    buttons: Query<(&ComputedNode, &UiGlobalTransform), With<ActionButton>>,
    mut areas: Query<(&ComputedNode, &UiGlobalTransform, &mut ScrollPosition), With<ScrollArea>>,
) {
    if !focus.is_changed() {
        return;
    }
    let Some(entity) = focus.0 else { return };
    let Ok((button, location)) = buttons.get(entity) else {
        return;
    };
    let Ok((area, origin, mut scroll)) = areas.single_mut() else {
        return;
    };
    let top = location.translation.y - button.size().y * 0.5;
    let bottom = location.translation.y + button.size().y * 0.5;
    let viewport_top = origin.translation.y - area.size().y * 0.5;
    let viewport_bottom = origin.translation.y + area.size().y * 0.5;
    let delta = if top < viewport_top {
        top - viewport_top
    } else if bottom > viewport_bottom {
        bottom - viewport_bottom
    } else {
        0.0
    };
    if delta != 0.0 {
        let max = ((area.content_size().y - area.size().y) * area.inverse_scale_factor()).max(0.0);
        scroll.y = (scroll.y + delta * area.inverse_scale_factor()).clamp(0.0, max);
    }
}

pub fn scrollbar(
    areas: Query<(&ComputedNode, &ScrollPosition), With<ScrollArea>>,
    mut thumbs: Query<&mut Node, With<ScrollThumb>>,
) {
    let Ok((area, scroll)) = areas.single() else {
        return;
    };
    let Ok(mut node) = thumbs.single_mut() else {
        return;
    };
    let ratio = (area.size().y / area.content_size().y.max(1.0)).clamp(0.0, 1.0);
    let display = if ratio < 0.999 {
        Display::Flex
    } else {
        Display::None
    };
    if node.display != display {
        node.display = display;
    }
    let max = ((area.content_size().y - area.size().y) * area.inverse_scale_factor()).max(1.0);
    let height = percent(ratio * 100.0);
    let top = percent((scroll.y / max).clamp(0.0, 1.0) * (1.0 - ratio) * 100.0);
    if node.height != height {
        node.height = height;
    }
    if node.top != top {
        node.top = top;
    }
}

pub fn text(
    commands: &mut Commands,
    parent: Entity,
    value: impl Into<String>,
    size: f32,
    color: Color,
) -> Entity {
    let entity = commands.spawn(label(value, size, color)).id();
    commands.entity(parent).add_child(entity);
    entity
}

pub fn action(
    commands: &mut Commands,
    parent: Entity,
    caption: &str,
    intent: Action,
    primary: bool,
) -> Entity {
    // Keep the accessible Button role, but dispatch clicks directly rather than
    // depending on the experimental widget's deferred Pressed component. A fast
    // press/release can otherwise be processed together by the low-power loop.
    let entity = commands
        .spawn((
            Button,
            Interaction::None,
            Hovered::default(),
            TabIndex(0),
            ActionButton(intent, primary),
            Node {
                min_height: px(52),
                width: percent(100),
                flex_shrink: 0.0,
                padding: UiRect::all(px(12)),
                border: UiRect::all(px(2)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::WHITE),
            BorderColor::all(LINE),
            children![(
                label(caption, 17.0, if primary { Color::WHITE } else { INK }),
                ButtonCaption,
                Pickable::IGNORE,
                Node {
                    min_width: px(0),
                    max_width: percent(100),
                    flex_shrink: 1.0,
                    ..default()
                }
            )],
            observe(
                |mut event: On<Pointer<Click>>,
                 query: Query<(&ActionButton, Has<InteractionDisabled>)>,
                 dragged: Res<Dragged>,
                 mut messages: MessageWriter<Intent>| {
                    if let Ok((button, false)) = query.get(event.entity)
                        && !dragged.0
                        && event.button == PointerButton::Primary
                        && !matches!(
                            event.pointer_id,
                            bevy::picking::pointer::PointerId::Touch(_)
                        )
                    {
                        event.propagate(false);
                        messages.write(Intent(button.0.clone()));
                    }
                },
            ),
            observe(
                |event: On<Pointer<Press>>,
                 mut focus: ResMut<InputFocus>,
                 query: Query<Has<InteractionDisabled>, With<ActionButton>>| {
                    if query.get(event.entity) == Ok(false) {
                        focus.0 = Some(event.entity);
                    }
                },
            ),
            observe(
                |mut event: On<
                    bevy::input_focus::FocusedInput<bevy::input::keyboard::KeyboardInput>,
                >,
                 query: Query<(&ActionButton, Has<InteractionDisabled>)>,
                 mut messages: MessageWriter<Intent>| {
                    if event.input.state == bevy::input::ButtonState::Pressed
                        && !event.input.repeat
                        && matches!(event.input.key_code, KeyCode::Enter | KeyCode::Space)
                        && let Ok((button, false)) = query.get(event.focused_entity)
                    {
                        event.propagate(false);
                        messages.write(Intent(button.0.clone()));
                    }
                },
            ),
        ))
        .id();
    commands.entity(parent).add_child(entity);
    entity
}

#[allow(clippy::type_complexity)]
pub fn styles(
    theme: Res<Theme>,
    focus: Res<InputFocus>,
    mut query: Query<(
        Entity,
        &ActionButton,
        &Hovered,
        &Interaction,
        Has<InteractionDisabled>,
        &mut BackgroundColor,
        &mut BorderColor,
        &Children,
    )>,
    mut captions: Query<&mut TextColor, With<ButtonCaption>>,
) {
    for (entity, button, hover, pressed, disabled, mut background, mut border, children) in
        &mut query
    {
        let base = if button.1 {
            theme.color()
        } else {
            Color::WHITE
        };
        let color = if disabled {
            Color::srgb(0.65, 0.65, 0.63)
        } else if *pressed == Interaction::Pressed {
            base.mix(&INK, 0.22)
        } else if hover.0 {
            if button.1 {
                base.mix(&INK, 0.08)
            } else {
                base.mix(&theme.color(), 0.10)
            }
        } else {
            base
        };
        if background.0 != color {
            background.0 = color;
        }
        let outline = BorderColor::all(if focus.0 == Some(entity) && !disabled {
            INK
        } else if button.1 {
            color
        } else {
            LINE
        });
        if *border != outline {
            *border = outline;
        }
        for child in children.iter() {
            if let Ok(mut color) = captions.get_mut(child) {
                let target = if button.1 || disabled {
                    Color::WHITE
                } else {
                    INK
                };
                if color.0 != target {
                    color.0 = target;
                }
            }
        }
    }
}

pub fn feedback(feedback: Res<Feedback>, mut texts: Query<&mut Text, With<Notice>>) {
    for mut text in &mut texts {
        if text.0 != feedback.0 {
            text.0.clone_from(&feedback.0);
        }
    }
}

pub fn scroll(
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    keys: Res<ButtonInput<KeyCode>>,
    mut dragged: ResMut<Dragged>,
    mut areas: Query<(&mut ScrollPosition, &ComputedNode), With<ScrollArea>>,
) {
    let mut delta = 0.0;
    for event in wheel.read() {
        delta -= event.y
            * if event.unit == bevy::input::mouse::MouseScrollUnit::Line {
                32.0
            } else {
                1.0
            };
    }
    if keys.just_pressed(KeyCode::Tab)
        || keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::Space)
    {
        dragged.0 = false;
    }
    if keys.just_pressed(KeyCode::PageDown) {
        delta += 250.0;
    }
    if keys.just_pressed(KeyCode::PageUp) {
        delta -= 250.0;
    }
    if delta != 0.0
        && let Ok((mut pos, computed)) = areas.single_mut()
    {
        let max = ((computed.content_size().y - computed.size().y)
            * computed.inverse_scale_factor())
        .max(0.0);
        pos.y = (pos.y + delta).clamp(0.0, max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::touch::{TouchInput, TouchPhase};

    #[test]
    fn same_frame_touch_activates_once_and_respects_movement_clips_and_disabled_state() {
        let mut app = App::new();
        app.init_resource::<InputFocus>()
            .add_message::<TouchInput>()
            .add_message::<Intent>()
            .add_systems(Update, touch_actions);
        let window = app
            .world_mut()
            .spawn(Window {
                resolution: bevy::window::WindowResolution::new(400, 400)
                    .with_scale_factor_override(2.0),
                ..default()
            })
            .id();
        let button = app
            .world_mut()
            .spawn((
                ActionButton(Action::Back, false),
                ComputedNode {
                    size: Vec2::new(100.0, 60.0),
                    ..default()
                },
                UiGlobalTransform::from_xy(100.0, 100.0),
            ))
            .id();
        let input = |phase, position| TouchInput {
            phase,
            position,
            window,
            id: 1,
            force: None,
        };
        let center = Vec2::new(50.0, 50.0);
        app.world_mut()
            .write_message(input(TouchPhase::Started, center));
        app.world_mut()
            .write_message(input(TouchPhase::Ended, center));
        app.update();
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<Intent>>()
                .drain()
                .count(),
            1
        );
        app.world_mut()
            .write_message(input(TouchPhase::Started, center));
        app.world_mut()
            .write_message(input(TouchPhase::Moved, center + Vec2::new(0.0, 20.0)));
        app.world_mut()
            .write_message(input(TouchPhase::Ended, center));
        app.update();
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<Intent>>()
                .drain()
                .count(),
            0
        );
        app.world_mut()
            .entity_mut(button)
            .insert(InteractionDisabled);
        app.world_mut()
            .write_message(input(TouchPhase::Started, center));
        app.world_mut()
            .write_message(input(TouchPhase::Ended, center));
        app.update();
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<Intent>>()
                .drain()
                .count(),
            0
        );
        app.world_mut()
            .entity_mut(button)
            .remove::<InteractionDisabled>()
            .insert(CalculatedClip {
                clip: Rect::from_corners(Vec2::ZERO, Vec2::splat(10.0)),
            });
        app.world_mut()
            .write_message(input(TouchPhase::Started, center));
        app.world_mut()
            .write_message(input(TouchPhase::Ended, center));
        app.update();
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<Intent>>()
                .drain()
                .count(),
            0
        );
    }
}
