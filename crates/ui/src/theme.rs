use bevy::prelude::*;

pub const BACKGROUND: Color = Color::srgb(0.96, 0.96, 0.95);
pub const INK: Color = Color::srgb(0.145, 0.145, 0.145);
pub const MUTED: Color = Color::srgb(0.45, 0.45, 0.45);
pub const LINE: Color = Color::srgb(0.86, 0.86, 0.84);
pub const SUCCESS: Color = Color::srgb(0.12, 0.48, 0.32);
pub const ERROR: Color = Color::srgb(0.75, 0.15, 0.13);

#[derive(Component)]
pub struct AccentText;

#[derive(Resource, Default)]
pub struct Theme {
    pub accent: usize,
}
impl Theme {
    pub fn color(&self) -> Color {
        match self.accent {
            1 => Color::srgb(0.08, 0.40, 0.72),
            2 => Color::srgb(0.10, 0.48, 0.39),
            _ => Color::srgb(0.91, 0.39, 0.14),
        }
    }
}

pub fn install_font(app: &mut App) {
    // Keep typography and rem-based spacing on the same logical-pixel scale.
    app.insert_resource(bevy::text::RemSize(16.0));
    let font = Font::from_bytes(include_bytes!("../assets/wqy-microhei.ttc").to_vec());
    app.world_mut()
        .resource_mut::<Assets<Font>>()
        .insert(AssetId::default(), font)
        .expect("Default font asset");
}

pub fn label(
    text: impl Into<String>,
    size: f32,
    color: Color,
) -> (Text, TextFont, TextColor, TextLayout) {
    (
        Text::new(text),
        TextFont::from_font_size(bevy::text::FontSize::Rem(size / 16.0)),
        TextColor(color),
        TextLayout::linebreak(bevy::text::LineBreak::WordOrCharacter),
    )
}

pub fn accent_labels(theme: Res<Theme>, mut labels: Query<&mut TextColor, With<AccentText>>) {
    for mut color in &mut labels {
        if color.0 != theme.color() {
            color.0 = theme.color();
        }
    }
}
