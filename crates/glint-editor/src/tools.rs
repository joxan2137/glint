//! Tools, their keys and icons, and per-tool options (remembered across editor windows on the UI thread).

use std::cell::RefCell;

use glint_ui::{Color, Icon};

use crate::model::{RedactKind, ShapeKind};

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

/// Stroke widths in DIP at 100 % (multiplied by the capture's DPI scale to get image pixels).
pub const PEN_SIZES: [f32; 3] = [2.0, 4.0, 8.0];
pub const HIGHLIGHTER_SIZES: [f32; 3] = [12.0, 20.0, 32.0];
pub const SHAPE_SIZES: [f32; 3] = [2.5, 4.0, 7.0];
pub const TEXT_SIZES: [f32; 3] = [16.0, 24.0, 36.0];
/// Visual dot diameters for the three size segments.
pub const SIZE_DOTS: [f32; 3] = [4.0, 7.0, 11.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolOptions {
    pub color: Color,
    /// The custom color well's color, once picked.
    pub custom: Option<Color>,
    pub size: usize,
    pub shape: ShapeKind,
    pub filled: bool,
    pub text_size: usize,
    pub redact: RedactKind,
}

impl ToolOptions {
    pub fn defaults(tool: Tool) -> Self {
        let base = Self {
            color: palette(0),
            custom: None,
            size: 1,
            shape: ShapeKind::Rectangle,
            filled: false,
            text_size: 1,
            redact: RedactKind::Blur,
        };
        match tool {
            Tool::Highlighter => Self { color: palette(2), ..base },
            _ => base,
        }
    }

    /// Nominal width in DIP for the tool.
    pub fn width_dip(&self, tool: Tool) -> f32 {
        match tool {
            Tool::Highlighter => HIGHLIGHTER_SIZES[self.size.min(2)],
            Tool::Shapes => SHAPE_SIZES[self.size.min(2)],
            _ => PEN_SIZES[self.size.min(2)],
        }
    }

    pub fn text_size_dip(&self) -> f32 {
        TEXT_SIZES[self.text_size.min(2)]
    }
}

/// Index of the preset nearest to `value` (sizes of existing annotations).
pub fn nearest(presets: &[f32; 3], value: f32) -> usize {
    (0..3).min_by(|a, b| (presets[*a] - value).abs().total_cmp(&(presets[*b] - value).abs())).unwrap_or(1)
}

thread_local! {
    static REMEMBERED: RefCell<Option<(Tool, [ToolOptions; 8])>> = const { RefCell::new(None) };
}

/// The last tool and per-tool options used in any editor window on this thread.
pub fn remembered() -> (Tool, [ToolOptions; 8]) {
    REMEMBERED.with(|r| *r.borrow()).unwrap_or_else(|| (Tool::Pen, Tool::ALL.map(ToolOptions::defaults)))
}

pub fn remember(tool: Tool, options: &[ToolOptions; 8]) {
    let tool = if tool == Tool::Crop { Tool::Select } else { tool };
    REMEMBERED.with(|r| *r.borrow_mut() = Some((tool, *options)));
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
        options[Tool::Pen.index()].size = 2;
        options[Tool::Highlighter.index()].color = palette(4);
        remember(Tool::Highlighter, &options);
        let (tool, again) = remembered();
        assert_eq!(tool, Tool::Highlighter);
        assert_eq!(again[Tool::Pen.index()].size, 2);
        assert_eq!(again[Tool::Highlighter.index()].color, palette(4));
        assert_eq!(again[Tool::Shapes.index()].size, 1);
    }

    #[test]
    fn nearest_preset() {
        assert_eq!(nearest(&PEN_SIZES, 7.0), 2);
        assert_eq!(nearest(&PEN_SIZES, 2.2), 0);
    }
}
