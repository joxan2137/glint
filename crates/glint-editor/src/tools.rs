//! Tools, their keys and icons, and per-tool options (remembered across editor windows on the UI thread and, in
//! the app, across sessions in `editor.json`).

use std::cell::{Cell, RefCell};

use glint_ui::{Color, Icon};

use crate::math::clamp_safe;
use crate::model::{Cap, DEFAULT_SMOOTHING, Dash, RedactKind, ShapeKind};
use crate::prefs;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Select,
    Pen,
    Highlighter,
    Eraser,
    Shapes,
    Text,
    Redact,
    Crop,
}

impl Tool {
    pub const ALL: [Tool; 8] =
        [Tool::Select, Tool::Pen, Tool::Highlighter, Tool::Eraser, Tool::Shapes, Tool::Text, Tool::Redact, Tool::Crop];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Pen => "Pen",
            Tool::Highlighter => "Highlighter",
            Tool::Eraser => "Eraser",
            Tool::Shapes => "Shapes",
            Tool::Text => "Text",
            Tool::Redact => "Redact",
            Tool::Crop => "Crop",
        }
    }

    pub fn key(self) -> char {
        match self {
            Tool::Select => 'V',
            Tool::Pen => 'P',
            Tool::Highlighter => 'H',
            Tool::Eraser => 'E',
            Tool::Shapes => 'S',
            Tool::Text => 'T',
            Tool::Redact => 'B',
            Tool::Crop => 'C',
        }
    }

    pub fn from_key(key: char) -> Option<Tool> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    pub fn from_name(name: &str) -> Option<Tool> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    pub fn icon(self, shape: ShapeKind) -> Icon {
        match self {
            Tool::Select => Icon::MousePointer,
            Tool::Pen => Icon::Pen,
            Tool::Highlighter => Icon::Highlighter,
            Tool::Eraser => Icon::Eraser,
            Tool::Shapes => shape_icon(shape),
            Tool::Text => Icon::Type,
            Tool::Redact => Icon::Redact,
            Tool::Crop => Icon::Crop,
        }
    }

    /// Which option groups the options pill shows.
    pub fn option_groups(self) -> OptionGroups {
        let colors = OptionGroups { colors: true, ..OptionGroups::default() };
        match self {
            Tool::Pen | Tool::Highlighter => OptionGroups { sizes: true, ..colors },
            Tool::Shapes => OptionGroups { sizes: true, shape: true, fill: true, ..colors },
            Tool::Text => OptionGroups { text_size: true, fill: true, ..colors },
            Tool::Redact => OptionGroups { redact: true, ..OptionGroups::default() },
            Tool::Select | Tool::Eraser | Tool::Crop => OptionGroups::default(),
        }
    }
}

pub fn shape_icon(kind: ShapeKind) -> Icon {
    match kind {
        ShapeKind::Rectangle => Icon::Square,
        ShapeKind::Ellipse => Icon::Circle,
        ShapeKind::Line => Icon::Line,
        ShapeKind::Arrow => Icon::Arrow,
    }
}

pub fn shape_name(kind: ShapeKind) -> &'static str {
    match kind {
        ShapeKind::Rectangle => "Rectangle",
        ShapeKind::Ellipse => "Ellipse",
        ShapeKind::Line => "Line",
        ShapeKind::Arrow => "Arrow",
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OptionGroups {
    pub colors: bool,
    pub sizes: bool,
    pub shape: bool,
    pub fill: bool,
    pub text_size: bool,
    pub redact: bool,
}

impl OptionGroups {
    pub fn any(&self) -> bool {
        self.colors || self.sizes || self.shape || self.fill || self.text_size || self.redact
    }
}

pub const PALETTE: [&str; 8] = ["#FF3B30", "#FF9500", "#FFCC00", "#34C759", "#007AFF", "#AF52DE", "#000000", "#FFFFFF"];
pub const PALETTE_NAMES: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Black", "White"];

pub fn palette(i: usize) -> Color {
    Color::hex(PALETTE[i % PALETTE.len()]).unwrap_or(Color::BLACK)
}

/// Stroke width presets in DIP at 100 % (multiplied by the capture's DPI scale to get image pixels).
pub const PEN_SIZES: [f32; 3] = [2.0, 4.0, 8.0];
pub const HIGHLIGHTER_SIZES: [f32; 3] = [12.0, 20.0, 32.0];
pub const SHAPE_SIZES: [f32; 3] = [2.5, 4.0, 7.0];
pub const TEXT_SIZES: [f32; 3] = [16.0, 24.0, 36.0];
/// Visual dot diameters for the three size segments.
pub const SIZE_DOTS: [f32; 3] = [4.0, 7.0, 11.0];

/// Stroke width range in image pixels.
pub const WIDTH_RANGE: (f32, f32) = (1.0, 64.0);
pub const OPACITY_RANGE: (f32, f32) = (0.1, 1.0);
pub const HEAD_SCALE_RANGE: (f32, f32) = (0.5, 2.0);
/// Corner radius range in image pixels.
pub const CORNER_RANGE: (f32, f32) = (0.0, 40.0);
/// Widths `[` / `]` and the wheel over the stroke button step through (image pixels).
const WIDTH_STEPS: [f32; 20] =
    [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 20.0, 24.0, 28.0, 32.0, 40.0, 48.0, 64.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolOptions {
    pub color: Color,
    /// The custom color well's color, once picked.
    pub custom: Option<Color>,
    /// Stroke width in DIP at 100 % (× the capture's scale = image pixels).
    pub width: f32,
    /// Group opacity of new ink and shapes.
    pub opacity: f32,
    pub dash: Dash,
    /// Pen: width follows pen pressure.
    pub pressure: bool,
    /// Pen: 0..=100.
    pub smoothing: f32,
    pub shape: ShapeKind,
    pub filled: bool,
    pub fill_opacity: f32,
    /// Rectangle corner radius in DIP.
    pub corner_radius: f32,
    pub line_caps: [Cap; 2],
    pub arrow_caps: [Cap; 2],
    pub head_scale: f32,
    pub text_size: usize,
    pub redact: RedactKind,
}

impl ToolOptions {
    pub fn defaults(tool: Tool) -> Self {
        let base = Self {
            color: palette(0),
            custom: None,
            width: Self::presets(tool)[1],
            opacity: 1.0,
            dash: Dash::Solid,
            pressure: true,
            smoothing: DEFAULT_SMOOTHING,
            shape: ShapeKind::Rectangle,
            filled: false,
            fill_opacity: 1.0,
            corner_radius: 0.0,
            line_caps: ShapeKind::Line.default_caps(),
            arrow_caps: ShapeKind::Arrow.default_caps(),
            head_scale: 1.0,
            text_size: 1,
            redact: RedactKind::Blur,
        };
        match tool {
            Tool::Highlighter => Self { color: palette(2), opacity: 0.4, ..base },
            _ => base,
        }
    }

    pub fn presets(tool: Tool) -> &'static [f32; 3] {
        match tool {
            Tool::Highlighter => &HIGHLIGHTER_SIZES,
            Tool::Shapes => &SHAPE_SIZES,
            _ => &PEN_SIZES,
        }
    }

    /// The size preset the width matches, if any (a custom width selects none).
    pub fn preset(&self, tool: Tool) -> Option<usize> {
        Self::presets(tool).iter().position(|p| (p - self.width).abs() < 0.01)
    }

    pub fn text_size_dip(&self) -> f32 {
        TEXT_SIZES[self.text_size.min(2)]
    }

    pub fn caps(&self, kind: ShapeKind) -> [Cap; 2] {
        match kind {
            ShapeKind::Arrow => self.arrow_caps,
            ShapeKind::Line => self.line_caps,
            _ => [Cap::None, Cap::None],
        }
    }

    pub fn set_caps(&mut self, kind: ShapeKind, caps: [Cap; 2]) {
        match kind {
            ShapeKind::Arrow => self.arrow_caps = caps,
            ShapeKind::Line => self.line_caps = caps,
            _ => {}
        }
    }

    /// Every value inside its range (files on disk can say anything).
    pub fn sanitized(mut self) -> Self {
        let finite = |v: f32, fallback: f32| if v.is_finite() { v } else { fallback };
        self.width = clamp_safe(finite(self.width, 4.0), 0.25, WIDTH_RANGE.1);
        self.opacity = clamp_safe(finite(self.opacity, 1.0), OPACITY_RANGE.0, OPACITY_RANGE.1);
        self.smoothing = clamp_safe(finite(self.smoothing, DEFAULT_SMOOTHING), 0.0, 100.0);
        self.fill_opacity = clamp_safe(finite(self.fill_opacity, 1.0), OPACITY_RANGE.0, OPACITY_RANGE.1);
        self.corner_radius = clamp_safe(finite(self.corner_radius, 0.0), CORNER_RANGE.0, CORNER_RANGE.1);
        self.head_scale = clamp_safe(finite(self.head_scale, 1.0), HEAD_SCALE_RANGE.0, HEAD_SCALE_RANGE.1);
        self.text_size = self.text_size.min(2);
        self
    }
}

/// `width` (image px) moved `steps` notches along the width ladder, staying inside `WIDTH_RANGE`.
pub fn step_width(width: f32, steps: i32) -> f32 {
    let mut index = if steps >= 0 {
        WIDTH_STEPS.iter().rposition(|w| *w <= width + 0.01).unwrap_or(0) as i32
    } else {
        WIDTH_STEPS.iter().position(|w| *w >= width - 0.01).unwrap_or(WIDTH_STEPS.len() - 1) as i32
    };
    index = (index + steps).clamp(0, WIDTH_STEPS.len() as i32 - 1);
    WIDTH_STEPS[index as usize]
}

/// Index of the preset nearest to `value` (sizes of existing annotations).
pub fn nearest(presets: &[f32; 3], value: f32) -> usize {
    (0..3).min_by(|a, b| (presets[*a] - value).abs().total_cmp(&(presets[*b] - value).abs())).unwrap_or(1)
}

thread_local! {
    static REMEMBERED: RefCell<Option<(Tool, [ToolOptions; 8])>> = const { RefCell::new(None) };
    static PERSISTENT: Cell<bool> = const { Cell::new(false) };
    static UNSAVED: Cell<bool> = const { Cell::new(false) };
}

/// Loads and saves tool options in `editor.json` from now on (the app; previews and tests stay in memory).
pub fn enable_persistence() {
    PERSISTENT.with(|p| p.set(true));
}

/// The last tool and per-tool options used in any editor window on this thread (loaded from disk once).
pub fn remembered() -> (Tool, [ToolOptions; 8]) {
    if let Some(known) = REMEMBERED.with(|r| *r.borrow()) {
        return known;
    }
    let loaded = PERSISTENT.with(Cell::get).then(|| prefs::load(&prefs::path())).flatten();
    let known = loaded.unwrap_or_else(|| (Tool::Pen, Tool::ALL.map(ToolOptions::defaults)));
    REMEMBERED.with(|r| *r.borrow_mut() = Some(known));
    known
}

pub fn remember(tool: Tool, options: &[ToolOptions; 8]) {
    let tool = if tool == Tool::Crop { Tool::Select } else { tool };
    REMEMBERED.with(|r| *r.borrow_mut() = Some((tool, *options)));
    UNSAVED.with(|u| u.set(true));
}

/// Writes remembered options to disk (on a worker thread) if persistence is on and something changed.
pub fn persist() {
    if !PERSISTENT.with(Cell::get) || !UNSAVED.with(|u| u.replace(false)) {
        return;
    }
    let Some((tool, options)) = REMEMBERED.with(|r| *r.borrow()) else { return };
    let bytes = prefs::encode(tool, &options);
    let spawned = std::thread::Builder::new().name("glint-editor-prefs".into()).spawn(move || {
        if let Err(e) = prefs::write(&prefs::path(), &bytes) {
            log::warn!("saving editor preferences: {e:#}");
        }
    });
    if let Err(e) = spawned {
        log::warn!("spawning the preferences writer: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for tool in Tool::ALL {
            assert_eq!(Tool::from_key(tool.key()), Some(tool));
            assert_eq!(Tool::ALL[tool.index()], tool);
        }
        assert_eq!(Tool::from_key('Q'), None);
    }

    #[test]
    fn options_are_remembered_per_tool() {
        let (_, mut options) = remembered();
        options[Tool::Pen.index()].width = 8.0;
        options[Tool::Highlighter.index()].color = palette(4);
        remember(Tool::Highlighter, &options);
        let (tool, again) = remembered();
        assert_eq!(tool, Tool::Highlighter);
        assert_eq!(again[Tool::Pen.index()].preset(Tool::Pen), Some(2));
        assert_eq!(again[Tool::Highlighter.index()].color, palette(4));
        assert_eq!(again[Tool::Shapes.index()].preset(Tool::Shapes), Some(1));
    }

    #[test]
    fn custom_widths_select_no_preset() {
        let mut o = ToolOptions::defaults(Tool::Pen);
        assert_eq!(o.preset(Tool::Pen), Some(1));
        o.width = 5.0;
        assert_eq!(o.preset(Tool::Pen), None);
        assert_eq!(ToolOptions::defaults(Tool::Highlighter).opacity, 0.4);
    }

    #[test]
    fn width_steps_walk_the_ladder_and_stop_at_the_ends() {
        assert_eq!(step_width(4.0, 1), 5.0);
        assert_eq!(step_width(4.0, -1), 3.0);
        assert_eq!(step_width(4.5, 1), 5.0, "between rungs, up goes to the next rung");
        assert_eq!(step_width(4.5, -1), 4.0);
        assert_eq!(step_width(4.0, 2), 6.0);
        assert_eq!(step_width(64.0, 3), 64.0);
        assert_eq!(step_width(1.0, -2), 1.0);
    }

    #[test]
    fn sanitizing_clamps_everything() {
        let mut o = ToolOptions::defaults(Tool::Shapes);
        o.width = f32::NAN;
        o.opacity = 7.0;
        o.smoothing = -3.0;
        o.head_scale = 0.0;
        o.corner_radius = 1e9;
        o.text_size = 9;
        let o = o.sanitized();
        assert_eq!((o.width, o.opacity, o.smoothing, o.head_scale), (4.0, 1.0, 0.0, 0.5));
        assert_eq!((o.corner_radius, o.text_size), (40.0, 2));
    }

    #[test]
    fn caps_follow_the_shape_kind() {
        let mut o = ToolOptions::defaults(Tool::Shapes);
        assert_eq!(o.caps(ShapeKind::Arrow), [Cap::None, Cap::FilledArrow]);
        assert_eq!(o.caps(ShapeKind::Line), [Cap::None, Cap::None]);
        o.set_caps(ShapeKind::Line, [Cap::Dot, Cap::Arrow]);
        assert_eq!(o.caps(ShapeKind::Line), [Cap::Dot, Cap::Arrow]);
        assert_eq!(o.caps(ShapeKind::Rectangle), [Cap::None, Cap::None]);
    }

    #[test]
    fn nearest_preset() {
        assert_eq!(nearest(&PEN_SIZES, 7.0), 2);
        assert_eq!(nearest(&PEN_SIZES, 2.2), 0);
    }
}
