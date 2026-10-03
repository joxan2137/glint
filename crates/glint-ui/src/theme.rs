//! Palettes from DESIGN §5, system dark-mode/accent detection.

use glint_core::ThemeMode;
use windows::UI::ViewManagement::{UIColorType, UISettings};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

use crate::color::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeKind {
    Dark,
    Light,
}

/// CSS-style box shadow: offset, blur radius (Gaussian σ = blur / 2) and color, in DIP.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub color: Color,
}

impl Shadow {
    pub const fn new(dy: f32, blur: f32, color: Color) -> Self {
        Self { dx: 0.0, dy, blur, color }
    }

    pub fn scaled_alpha(&self, factor: f32) -> Self {
        Self { color: self.color.multiply_alpha(factor), ..*self }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub kind: ThemeKind,
    /// Glass panel tint over the blurred backdrop.
    pub glass_fill: Color,
    /// Glass fill when no backdrop bitmap is available (slightly more opaque for legibility).
    pub glass_fill_solid: Color,
    /// 1 physical px inner border of glass.
    pub hairline: Color,
    /// 1 physical px outer border of glass.
    pub outer_border: Color,
    /// Far and near glass shadows.
    pub shadows: [Shadow; 2],
    pub text: Color,
    pub text_secondary: Color,
    pub text_tertiary: Color,
    pub hover: Color,
    pub pressed: Color,
    pub selected: Color,
    pub accent: Color,
    pub destructive: Color,
    pub success: Color,
    pub overlay_dim: Color,
    /// Text and glyphs drawn on accent or destructive fills.
    pub on_accent: Color,
    pub separator: Color,
    /// Toggle-off track, slider track, swatch ring backgrounds.
    pub control_track: Color,
    /// Toggle/slider knob.
    pub knob: Color,
    /// Plain window background for windows without Mica.
    pub window_background: Color,
    /// Backdrop blur σ for glass, in DIP.
    pub backdrop_blur: f32,
    pub backdrop_saturation: f32,
}

pub const APPLE_BLUE_DARK: Color = Color::rgb8(0x0A, 0x84, 0xFF);
pub const APPLE_BLUE_LIGHT: Color = Color::rgb8(0x00, 0x7A, 0xFF);

impl Theme {
    pub fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            glass_fill: Color::rgba8(30, 30, 32, 0.70),
            glass_fill_solid: Color::rgba8(36, 36, 38, 0.86),
            hairline: Color::rgba(1.0, 1.0, 1.0, 0.14),
            outer_border: Color::rgba(0.0, 0.0, 0.0, 0.45),
            shadows: [
                Shadow::new(12.0, 40.0, Color::rgba(0.0, 0.0, 0.0, 0.40)),
                Shadow::new(2.0, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.25)),
            ],
            text: Color::rgba(1.0, 1.0, 1.0, 0.92),
            text_secondary: Color::rgba(1.0, 1.0, 1.0, 0.60),
            text_tertiary: Color::rgba(1.0, 1.0, 1.0, 0.35),
            hover: Color::rgba(1.0, 1.0, 1.0, 0.08),
            pressed: Color::rgba(1.0, 1.0, 1.0, 0.14),
            selected: Color::rgba(1.0, 1.0, 1.0, 0.20),
            accent: APPLE_BLUE_DARK,
            destructive: Color::rgb8(0xFF, 0x45, 0x3A),
            success: Color::rgb8(0x30, 0xD1, 0x58),
            overlay_dim: Color::rgba(0.0, 0.0, 0.0, 0.40),
            on_accent: Color::WHITE,
            separator: Color::rgba(1.0, 1.0, 1.0, 0.14),
            control_track: Color::rgba(1.0, 1.0, 1.0, 0.18),
            knob: Color::WHITE,
            window_background: Color::rgb8(0x1E, 0x1E, 0x20),
            backdrop_blur: 24.0,
            backdrop_saturation: 1.5,
        }
    }

    pub fn light() -> Self {
        let dark = Self::dark();
        Self {
            kind: ThemeKind::Light,
            glass_fill: Color::rgba8(246, 246, 248, 0.78),
            glass_fill_solid: Color::rgba8(250, 250, 252, 0.90),
            hairline: Color::rgba(0.0, 0.0, 0.0, 0.08),
            outer_border: Color::rgba(0.0, 0.0, 0.0, 0.12),
            shadows: [dark.shadows[0].scaled_alpha(0.5), dark.shadows[1].scaled_alpha(0.5)],
            text: Color::rgba(0.0, 0.0, 0.0, 0.88),
            text_secondary: Color::rgba(0.0, 0.0, 0.0, 0.55),
            text_tertiary: Color::rgba(0.0, 0.0, 0.0, 0.30),
            hover: Color::rgba(0.0, 0.0, 0.0, 0.05),
            pressed: Color::rgba(0.0, 0.0, 0.0, 0.09),
            selected: Color::rgba(0.0, 0.0, 0.0, 0.12),
            accent: APPLE_BLUE_LIGHT,
            destructive: Color::rgb8(0xFF, 0x3B, 0x30),
            success: Color::rgb8(0x34, 0xC7, 0x59),
            overlay_dim: dark.overlay_dim,
            on_accent: Color::WHITE,
            separator: Color::rgba(0.0, 0.0, 0.0, 0.10),
            control_track: Color::rgba(0.0, 0.0, 0.0, 0.10),
            knob: Color::WHITE,
            window_background: Color::rgb8(0xF5, 0xF5, 0xF7),
            backdrop_blur: dark.backdrop_blur,
            backdrop_saturation: dark.backdrop_saturation,
        }
    }

    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }

    /// Resolves a settings `ThemeMode` against the current system preference.
    pub fn resolve(mode: ThemeMode, system_dark: bool) -> Self {
        let dark = match mode {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => system_dark,
        };
        Self::for_kind(if dark { ThemeKind::Dark } else { ThemeKind::Light })
    }

    pub fn with_accent(mut self, accent: Color) -> Self {
        self.accent = accent;
        self
    }

    pub fn is_dark(&self) -> bool {
        self.kind == ThemeKind::Dark
    }

    /// Accent tinted for focus rings and selection outlines.
    pub fn accent_soft(&self) -> Color {
        self.accent.with_alpha(if self.is_dark() { 0.32 } else { 0.24 })
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// True when Windows apps use the dark theme (UISettings foreground is light).
pub fn system_prefers_dark() -> bool {
    let foreground = UISettings::new().and_then(|s| s.GetColorValue(UIColorType::Foreground));
    match foreground {
        Ok(c) => (c.R as u32 + c.G as u32 + c.B as u32) > 3 * 128,
        Err(_) => true,
    }
}

/// The Windows accent color, if available.
pub fn system_accent() -> Option<Color> {
    let c = UISettings::new().and_then(|s| s.GetColorValue(UIColorType::Accent)).ok()?;
    Some(Color::rgb8(c.R, c.G, c.B))
}

/// False when "Show animations in Windows" is off.
pub fn system_animations_enabled() -> bool {
    let mut enabled = windows::core::BOOL(1);
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL to the provided pointer.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut windows::core::BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    !ok || enabled.as_bool()
}
