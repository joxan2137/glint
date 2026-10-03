//! Everything around the canvas: the unified title bar (New, tool pill, actions), the crop pill, the tool-options
//! pill, the bottom zoom pill, the OCR bar, the HDR and color popovers, the mode/delay menu and toasts.

use glint_core::{CaptureMode, PointF, RectF, SizeF, ToneMapMode};
use glint_ui::anim::{Motion, Spring, Tween};
use glint_ui::widgets::{
    Badge, BadgeStyle, Button, ButtonStyle, ColorSwatch, IconButton, Interaction, Menu, MenuItem, Popover, Presence,
    Response, Segment, Segmented, SegmentedStyle, Slider, Toolbar, ToolbarAction, ToolbarItem,
};
use std::rc::Rc;

use glint_core::Image;
use glint_ui::{Animated, Backdrop, Bitmap, Color, Ctx, Event, Gfx, Icon, Painter, TextAlign, TextStyle, Theme, Weight};

use crate::color_picker::{self, ColorPicker};
use crate::model::{RedactKind, ShapeKind};
use crate::tools::{self, OptionGroups, PALETTE, PALETTE_NAMES, SIZE_DOTS, Tool, ToolOptions};

pub const TITLE_BAR: f32 = 52.0;
const EDGE: f32 = 10.0;
const PILL_TOP: f32 = 4.0;
const OPTIONS_TOP: f32 = 58.0;
const BOTTOM_GAP: f32 = 14.0;
const PILL_H: f32 = 44.0;
const GROUP_GAP: f32 = 12.0;
/// The image fits inside the window minus these margins (DIP).
pub const FIT_TOP: f32 = 116.0;
pub const FIT_BOTTOM: f32 = 72.0;
pub const FIT_SIDE: f32 = 24.0;
const SWATCH_IDS: [&str; 9] = ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "custom"];
const DEFAULT_CAPTION_W: f32 = 138.0;

const MODES: [(CaptureMode, &str, Icon); 4] = [
    (CaptureMode::Rectangle, "Rectangle", Icon::SquareDashed),
    (CaptureMode::Window, "Window", Icon::AppWindow),
    (CaptureMode::FullScreen, "Full screen", Icon::Monitor),
    (CaptureMode::Freeform, "Freeform", Icon::Lasso),
];
const DELAYS: [u32; 4] = [0, 3, 5, 10];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OcrBar {
    Hidden,
    Busy,
    Ready { lines: usize },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrPanel {
    pub mode: ToneMapMode,
    pub exposure: f32,
    /// Brightest content in SDR-white units (99.9th percentile).
    pub peak: f32,
}

/// What the chrome shows; the view rebuilds it every frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ChromeState {
    pub tool: Tool,
    /// Which tool's options the options pill shows (the tool, or the selected object's kind).
    pub options_tool: Option<Tool>,
    pub options: ToolOptions,
    pub can_undo: bool,
    pub can_redo: bool,
    pub cropping: bool,
    pub zoom_percent: u32,
    pub content_size: (u32, u32),
    pub hdr: Option<HdrPanel>,
    pub ocr: OcrBar,
    pub mode: CaptureMode,
    pub delay: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChromeAction {
    Tool(Tool),
    Undo,
    Redo,
    NewSnip,
    PickMode(CaptureMode),
    PickDelay(u32),
    Ocr,
    Copy,
    Save,
    Share,
    CropCancel,
    CropReset,
    CropDone,
    Color { color: Color, custom: bool },
    Size(usize),
    Shape(ShapeKind),
    Fill(bool),
    TextSize(usize),
    Redact(RedactKind),
    ZoomIn,
    ZoomOut,
    ZoomActual,
    Fit,
    ToneMode(ToneMapMode),
    /// `done` = the slider was released (closes the undo step).
    Exposure { value: f32, done: bool },
    OcrCopyAll,
    OcrClose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Done,
    Info,
    Error,
}

struct Toast {
    text: String,
    kind: ToastKind,
}

/// A hover/press area the chrome paints itself (chevron, zoom label, HDR badge).
#[derive(Clone, Debug, Default)]
struct HotSpot {
    rect: RectF,
    interaction: Interaction,
    tooltip: Option<(&'static str, Option<&'static str>)>,
}

impl HotSpot {
    fn tip(text: &'static str, shortcut: Option<&'static str>) -> Self {
        Self { tooltip: Some((text, shortcut)), ..Self::default() }
    }

    fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        let was = self.interaction.hovered;
        let response = self.interaction.update(event, self.rect, true);
        if let Some((text, shortcut)) = self.tooltip {
            match (was, self.interaction.hovered) {
                (false, true) => cx.show_tooltip(self.rect, text, shortcut),
                (true, false) => cx.hide_tooltip_for(self.rect),
                _ => {}
            }
            if matches!(event, Event::PointerDown(e) if self.rect.contains(e.pos)) {
                cx.hide_tooltip_for(self.rect);
            }
        }
        response
    }

    fn highlight(&self, p: &Painter) -> Color {
        let theme = p.theme();
        Color::TRANSPARENT
            .lerp(&theme.hover, self.interaction.hover_amount())
            .lerp(&theme.pressed, self.interaction.press_amount())
    }
}

pub struct Chrome {
    state: Option<ChromeState>,
    size: SizeF,
    caption: Option<RectF>,
    new_button: IconButton,
    chevron: HotSpot,
    actions: Vec<(&'static str, IconButton)>,
    menu: Menu,
    tools: Toolbar,
    tools_shape: ShapeKind,
    crop_presence: Presence,
    crop_buttons: [Button; 3],
    crop_rect: RectF,
    options: Toolbar,
    /// Mirrors the options pill's appear/disappear so the swatches painted on top move with it.
    options_presence: Presence,
    options_key: Option<(Tool, OptionGroups)>,
    swatches: Vec<ColorSwatch>,
    bottom: Toolbar,
    bottom_presence: Presence,
    bottom_key: (String, bool),
    zoom_label: HotSpot,
    fit: Button,
    hdr_badge: HotSpot,
    ocr_presence: Presence,
    ocr_rect: RectF,
    ocr_copy: Button,
    ocr_close: Button,
    hdr_popover: Popover,
    tone_mode: Segmented,
    exposure: Slider,
    exposure_dragging: bool,
    picker_popover: Popover,
    picker: ColorPicker,
    toast: Option<Toast>,
    toast_progress: Animated<f32>,
    spinner_phase: f32,
    /// Opaque base for glass floating over the canvas, so image content never bleeds through panels.
    solid: Option<([u8; 4], Rc<Bitmap>)>,
}

fn tools_toolbar(shape: ShapeKind, selected: Tool) -> Toolbar {
    let segments = Tool::ALL
        .iter()
        .map(|t| {
            let name = if *t == Tool::Shapes { tools::shape_name(shape) } else { t.name() };
            Segment::icon(t.icon(shape)).tooltip(name, Some(key_label(t.key())))
        })
        .collect();
    Toolbar::new(vec![
        ToolbarItem::segmented("tools", Segmented::new(segments, selected.index())),
        ToolbarItem::separator(),
        ToolbarItem::button("undo", IconButton::new(Icon::Undo).tooltip("Undo", Some("Ctrl+Z"))),
        ToolbarItem::button("redo", IconButton::new(Icon::Redo).tooltip("Redo", Some("Ctrl+Y"))),
    ])
}

fn key_label(key: char) -> &'static str {
    const KEYS: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V",
        "W", "X", "Y", "Z",
    ];
    KEYS.get((key as u8).wrapping_sub(b'A') as usize).copied().unwrap_or("")
}

fn options_toolbar(tool: Tool, groups: OptionGroups, o: &ToolOptions) -> Toolbar {
    let mut items = Vec::new();
    let separate = |items: &mut Vec<ToolbarItem>| {
        if !items.is_empty() {
            items.push(ToolbarItem::separator());
        }
    };
    if groups.colors {
        items.extend(SWATCH_IDS.iter().map(|id| ToolbarItem::custom(id, SizeF::new(30.0, 32.0))));
    }
    if groups.sizes {
        separate(&mut items);
        let names = ["Thin", "Medium", "Thick"];
        let dots = (0..3).map(|i| Segment::dot(SIZE_DOTS[i]).tooltip(names[i], None)).collect();
        items.push(ToolbarItem::segmented("size", Segmented::new(dots, o.size)));
    }
    if groups.text_size {
        separate(&mut items);
        let names = ["Small", "Medium", "Large"];
        let blanks = names.iter().map(|n| Segment::default().tooltip(n, None)).collect();
        items.push(ToolbarItem::segmented("tsize", Segmented::new(blanks, o.text_size)));
    }
    if groups.shape {
        separate(&mut items);
        let kinds = ShapeKind::ALL
            .iter()
            .map(|k| Segment::icon(tools::shape_icon(*k)).tooltip(tools::shape_name(*k), None))
            .collect();
        let index = ShapeKind::ALL.iter().position(|k| *k == o.shape).unwrap_or(0);
        items.push(ToolbarItem::segmented("shape", Segmented::new(kinds, index)));
    }
    if groups.fill {
        separate(&mut items);
        let tip = if tool == Tool::Text { "Background" } else { "Fill" };
        items.push(ToolbarItem::button("fill", IconButton::new(Icon::PaintBucket).tooltip(tip, None).with_selected(o.filled)));
    }
    if groups.redact {
        separate(&mut items);
        let kinds = vec![
            Segment::icon_label(Icon::Blur, "Blur").tooltip("Blur", None),
            Segment::icon_label(Icon::Redact, "Pixelate").tooltip("Pixelate", None),
        ];
        let index = usize::from(o.redact == RedactKind::Pixelate);
        items.push(ToolbarItem::segmented("redact", Segmented::new(kinds, index)));
    }
    Toolbar::new(items)
}

fn bottom_toolbar(size_w: f32, hdr: bool) -> Toolbar {
    let mut items = vec![
        ToolbarItem::button("zoom_out", IconButton::new(Icon::Minus).tooltip("Zoom out", Some("Ctrl+−"))),
        ToolbarItem::custom("zoom", SizeF::new(54.0, 32.0)),
        ToolbarItem::button("zoom_in", IconButton::new(Icon::Plus).tooltip("Zoom in", Some("Ctrl++"))),
        ToolbarItem::separator(),
        ToolbarItem::custom("fit", SizeF::new(44.0, 28.0)),
        ToolbarItem::separator(),
        ToolbarItem::custom("size", SizeF::new(size_w, 32.0)),
    ];
    if hdr {
        items.push(ToolbarItem::separator());
        items.push(ToolbarItem::custom("hdr", SizeF::new(48.0, 24.0)));
    }
    Toolbar::new(items)
}

/// Menu rows: the four modes, a separator, then the delays.
fn menu_action(index: usize) -> Option<ChromeAction> {
    match index.checked_sub(MODES.len() + 1) {
        None => MODES.get(index).map(|m| ChromeAction::PickMode(m.0)),
        Some(delay) => DELAYS.get(delay).map(|d| ChromeAction::PickDelay(*d)),
    }
}

fn size_text(size: (u32, u32)) -> String {
    format!("{} × {}", size.0, size.1)
}

fn size_style() -> TextStyle {
    TextStyle::body().tabular()
}

impl Chrome {
    pub fn new(tool: Tool, options: &ToolOptions) -> Self {
        let actions = vec![
            ("ocr", IconButton::new(Icon::ScanText).label("Text").tooltip("Recognize text", None)),
            ("copy", IconButton::new(Icon::Copy).label("Copy").tooltip("Copy", Some("Ctrl+C"))),
            ("save", IconButton::new(Icon::Download).label("Save").tooltip("Save", Some("Ctrl+S"))),
            ("share", IconButton::new(Icon::Share).tooltip("Share", None)),
        ];
        let mut crop_presence = Presence::new(false);
        crop_presence.snap(false);
        let mut ocr_presence = Presence::new(false);
        ocr_presence.snap(false);
        let mut options_bar = Toolbar::new(Vec::new());
        options_bar.snap_visible(false);
        Self {
            state: None,
            size: SizeF::default(),
            caption: None,
            new_button: IconButton::new(Icon::Scissors).label("New").tooltip("New snip", Some("Ctrl+N")),
            chevron: HotSpot::tip("Snip mode and delay", None),
            actions,
            menu: Menu::new(Vec::new()),
            tools: tools_toolbar(options.shape, tool),
            tools_shape: options.shape,
            crop_presence,
            crop_buttons: [
                Button::new("Cancel", ButtonStyle::Secondary),
                Button::new("Reset", ButtonStyle::Plain),
                Button::new("Done", ButtonStyle::Primary).default_action(),
            ],
            crop_rect: RectF::default(),
            options: options_bar,
            options_presence: Presence::new(false),
            options_key: None,
            swatches: Vec::new(),
            bottom: bottom_toolbar(0.0, false),
            bottom_presence: Presence::new(true),
            bottom_key: (String::from("\0"), false),
            zoom_label: HotSpot::tip("Actual size", Some("Ctrl+1")),
            fit: Button::new("Fit", ButtonStyle::Plain),
            hdr_badge: HotSpot::tip("HDR tone mapping", None),
            ocr_presence,
            ocr_rect: RectF::default(),
            ocr_copy: Button::new("Copy all text", ButtonStyle::Plain).icon(Icon::Copy),
            ocr_close: Button::new("Close", ButtonStyle::Plain),
            hdr_popover: Popover::new(SizeF::new(236.0, 134.0)),
            tone_mode: Segmented::new(vec![Segment::label("Auto"), Segment::label("Clip")], 0).style(SegmentedStyle::Track),
            exposure: Slider::new(0.0, -2.0, 2.0).origin(0.0),
            exposure_dragging: false,
            picker_popover: Popover::new(color_picker::content_size()),
            picker: ColorPicker::new(Color::hex("#5AC8FA").unwrap_or(Color::WHITE)),
            toast: None,
            toast_progress: Animated::new(0.0),
            spinner_phase: 0.0,
            solid: None,
        }
    }

    /// Applies the view's state: selected tool, option values, undo availability, crop mode, zoom, HDR.
    pub fn sync(&mut self, gfx: &Gfx, state: ChromeState) {
        if self.state.as_ref() == Some(&state) {
            return;
        }
        let previous = self.state.replace(state.clone());
        if state.options.shape != self.tools_shape {
            self.tools_shape = state.options.shape;
            self.tools = tools_toolbar(state.options.shape, state.tool);
        }
        if let Some(segmented) = self.tools.segmented_mut("tools") {
            segmented.set_selected(state.tool.index());
        }
        if let Some(undo) = self.tools.button_mut("undo") {
            undo.enabled = state.can_undo;
        }
        if let Some(redo) = self.tools.button_mut("redo") {
            redo.enabled = state.can_redo;
        }
        let was_cropping = previous.as_ref().is_some_and(|p| p.cropping);
        if state.cropping != was_cropping || previous.is_none() {
            self.tools.set_visible(!state.cropping);
            self.crop_presence.set_shown(state.cropping);
            if previous.is_none() {
                self.tools.snap_visible(!state.cropping);
                self.crop_presence.snap(state.cropping);
            }
        }
        self.sync_options(&state, previous.is_none());
        let text = size_text(state.content_size);
        let key = (text.clone(), state.hdr.is_some());
        if key != self.bottom_key {
            let w = (gfx.measure_text(&text, &size_style()).w + 16.0).ceil();
            let visible = self.bottom.is_visible();
            self.bottom = bottom_toolbar(w, state.hdr.is_some());
            self.bottom.snap_visible(visible);
            self.bottom_key = key;
        }
        let ocr_shown = state.ocr != OcrBar::Hidden;
        if previous.is_none() {
            self.ocr_presence.snap(ocr_shown);
            self.bottom.snap_visible(!ocr_shown);
            self.bottom_presence.snap(!ocr_shown);
        } else {
            self.ocr_presence.set_shown(ocr_shown);
            self.bottom.set_visible(!ocr_shown);
            self.bottom_presence.set_shown(!ocr_shown);
        }
        if let Some(hdr) = state.hdr {
            self.tone_mode.set_selected(usize::from(hdr.mode == ToneMapMode::Clip));
            if !self.exposure_dragging {
                self.exposure.set_value(hdr.exposure);
            }
        } else if self.hdr_popover.is_open() {
            self.hdr_popover.close();
        }
        if let Some(custom) = state.options.custom {
            self.picker.set_color(custom);
        }
    }

    fn sync_options(&mut self, state: &ChromeState, first: bool) {
        let target = state.options_tool.map(|t| (t, t.option_groups())).filter(|(_, g)| g.any());
        if target != self.options_key {
            match target {
                Some((tool, groups)) => {
                    self.options = options_toolbar(tool, groups, &state.options);
                    self.options.snap_visible(first);
                    self.options.set_visible(true);
                    self.options_presence.snap(first);
                    self.options_presence.set_shown(true);
                    self.swatches = if groups.colors {
                        (0..8)
                            .map(|i| ColorSwatch::new(tools::palette(i)).tooltip(PALETTE_NAMES[i]))
                            .chain(std::iter::once(ColorSwatch::well(state.options.custom).tooltip("Custom color")))
                            .collect()
                    } else {
                        Vec::new()
                    };
                }
                None => {
                    self.options.set_visible(false);
                    self.options_presence.set_shown(false);
                }
            }
            self.options_key = target;
            if self.picker_popover.is_open() && !target.is_some_and(|(_, g)| g.colors) {
                self.picker_popover.close();
            }
        }
        let o = &state.options;
        if let Some(s) = self.options.segmented_mut("size") {
            s.set_selected(o.size);
        }
        if let Some(s) = self.options.segmented_mut("tsize") {
            s.set_selected(o.text_size);
        }
        if let Some(s) = self.options.segmented_mut("shape") {
            s.set_selected(ShapeKind::ALL.iter().position(|k| *k == o.shape).unwrap_or(0));
        }
        if let Some(s) = self.options.segmented_mut("redact") {
            s.set_selected(usize::from(o.redact == RedactKind::Pixelate));
        }
        if let Some(b) = self.options.button_mut("fill")
            && b.is_selected() != o.filled
        {
            b.set_selected(o.filled);
        }
        let current = o.color.to_rgba8();
        let palette_index = PALETTE.iter().position(|hex| Color::hex(hex).is_some_and(|c| c.to_rgba8() == current));
        for (i, swatch) in self.swatches.iter_mut().enumerate() {
            let selected = if i < 8 { palette_index == Some(i) } else { palette_index.is_none() };
            if i == 8 {
                swatch.color = o.custom;
            }
            if swatch.is_selected() != selected {
                swatch.set_selected(selected);
            }
        }
    }

    pub fn layout(&mut self, gfx: &Gfx, size: SizeF, caption: Option<RectF>) {
        self.size = size;
        let caption = caption.unwrap_or(RectF::new(size.w - DEFAULT_CAPTION_W, 0.0, DEFAULT_CAPTION_W, 32.0));
        self.caption = Some(caption);
        let center_w = self.tools.preferred_size(gfx).w.max(self.crop_pill_width(gfx));
        let collapse_steps = [(true, true), (false, true), (false, false)];
        for (action_labels, new_label) in collapse_steps {
            self.set_labels(action_labels, new_label);
            let left = self.left_width(gfx);
            let right = self.right_width(gfx);
            if left + right + center_w + 4.0 * GROUP_GAP + EDGE + (size.w - caption.x) <= size.w || !new_label {
                break;
            }
        }
        let new_size = self.new_button.preferred_size(gfx);
        self.new_button.set_rect(RectF::new(EDGE, 10.0, new_size.w, 32.0));
        self.chevron.rect = RectF::new(EDGE + new_size.w + 1.0, 10.0, 22.0, 32.0);
        let left_end = self.chevron.rect.right();
        let mut x = caption.x - 8.0;
        for (_, button) in self.actions.iter_mut().rev() {
            let w = button.preferred_size(gfx).w;
            x -= w;
            button.set_rect(RectF::new(x, 10.0, w, 32.0));
            x -= 4.0;
        }
        let right_start = x + 4.0;
        let tools_w = self.tools.preferred_size(gfx).w;
        let center = |w: f32| (size.w / 2.0 - w / 2.0).min(right_start - GROUP_GAP - w).max(left_end + GROUP_GAP);
        self.tools.layout_at(gfx, PointF::new(center(tools_w), PILL_TOP));
        let crop_w = self.crop_pill_width(gfx);
        self.crop_rect = RectF::new(center(crop_w).round(), PILL_TOP, crop_w, PILL_H);
        let mut bx = self.crop_rect.x + 8.0;
        for button in &mut self.crop_buttons {
            let w = button.preferred_size(gfx).w.max(72.0);
            button.set_rect(RectF::new(bx, PILL_TOP + 8.0, w, 28.0));
            bx += w + 8.0;
        }

        self.options.layout_centered(gfx, size.w / 2.0, OPTIONS_TOP);
        for (i, id) in SWATCH_IDS.iter().enumerate() {
            if let (Some(item), Some(swatch)) = (self.options.item(id), self.swatches.get_mut(i)) {
                swatch.set_center(item.rect().center());
            }
        }

        let bottom_y = size.h - BOTTOM_GAP - PILL_H;
        self.bottom.layout_centered(gfx, size.w / 2.0, bottom_y);
        let slot = |bar: &Toolbar, id: &str| bar.item(id).map(|i| i.rect()).unwrap_or_default();
        self.zoom_label.rect = slot(&self.bottom, "zoom");
        self.fit.set_rect(slot(&self.bottom, "fit"));
        self.hdr_badge.rect = slot(&self.bottom, "hdr");

        let copy_w = self.ocr_copy.preferred_size(gfx).w;
        let close_w = self.ocr_close.preferred_size(gfx).w;
        let label_w = 128.0;
        let ocr_w = 8.0 + label_w + copy_w + 4.0 + close_w + 8.0;
        self.ocr_rect = RectF::new((size.w / 2.0 - ocr_w / 2.0).round(), bottom_y, ocr_w, PILL_H);
        let ox = self.ocr_rect.x + 8.0 + label_w;
        self.ocr_copy.set_rect(RectF::new(ox, bottom_y + 8.0, copy_w, 28.0));
        self.ocr_close.set_rect(RectF::new(ox + copy_w + 4.0, bottom_y + 8.0, close_w, 28.0));

        if let Some(anchor) = self.hdr_anchor()
            && self.hdr_popover.is_open()
        {
            self.hdr_popover.open(anchor, size);
        }
        if let Some(well) = self.options.item("custom").map(|i| i.rect())
            && self.picker_popover.is_open()
        {
            self.picker_popover.open(well, size);
        }
    }

    fn set_labels(&mut self, actions: bool, new: bool) {
        for (id, button) in &mut self.actions {
            button.label = match (*id, actions) {
                ("ocr", true) => Some("Text".into()),
                ("copy", true) => Some("Copy".into()),
                ("save", true) => Some("Save".into()),
                _ => None,
            };
        }
        self.new_button.label = new.then(|| "New".to_string());
    }

    fn left_width(&self, gfx: &Gfx) -> f32 {
        EDGE + self.new_button.preferred_size(gfx).w + 23.0
    }

    fn right_width(&self, gfx: &Gfx) -> f32 {
        self.actions.iter().map(|(_, b)| b.preferred_size(gfx).w + 4.0).sum::<f32>() + 8.0
    }

    fn crop_pill_width(&self, gfx: &Gfx) -> f32 {
        8.0 + self.crop_buttons.iter().map(|b| b.preferred_size(gfx).w.max(72.0) + 8.0).sum::<f32>()
    }

    fn hdr_anchor(&self) -> Option<RectF> {
        self.bottom.item("hdr").map(|i| i.rect())
    }

    /// True over title-bar background (window drag area).
    pub fn is_caption(&self, pos: PointF) -> bool {
        if pos.y >= TITLE_BAR || self.caption.is_some_and(|c| c.contains(pos)) {
            return false;
        }
        let over_control = self.new_button.rect().contains(pos)
            || self.chevron.rect.contains(pos)
            || self.actions.iter().any(|(_, b)| b.rect().contains(pos))
            || (self.tools.is_visible() && self.tools.rect().contains(pos))
            || (self.crop_presence.is_shown() && self.crop_rect.contains(pos))
            || (self.menu.is_open() && self.menu.frame().contains(pos));
        !over_control
    }

    /// True where the chrome owns the pointer (title bar, pills, popovers, menus).
    pub fn covers(&self, pos: PointF) -> bool {
        pos.y < TITLE_BAR
            || (self.options.is_visible() && self.options.rect().contains(pos))
            || (self.bottom.is_visible() && self.bottom.rect().contains(pos))
            || (self.ocr_presence.is_shown() && self.ocr_rect.contains(pos))
            || (self.hdr_popover.is_open() && self.hdr_popover.frame().contains(pos))
            || (self.picker_popover.is_open() && self.picker_popover.frame().contains(pos))
            || (self.menu.is_open() && self.menu.frame().contains(pos))
    }

    /// Something modal (menu, popover) is open and should see keys first.
    pub fn has_popup(&self) -> bool {
        self.menu.is_open() || self.hdr_popover.is_open() || self.picker_popover.is_open()
    }

    pub fn bottom_rect(&self) -> RectF {
        if self.ocr_presence.is_shown() { self.ocr_rect } else { self.bottom.rect() }
    }

    fn open_menu(&mut self, gfx: &Gfx) {
        let Some(state) = &self.state else { return };
        let mut items: Vec<MenuItem> =
            MODES.iter().map(|(mode, name, icon)| MenuItem::new(name).icon(*icon).checked(*mode == state.mode)).collect();
        items.push(MenuItem::separator());
        items.extend(DELAYS.iter().map(|d| {
            let label = if *d == 0 { "No delay".to_string() } else { format!("{d} seconds") };
            MenuItem::new(&label).icon(Icon::Timer).checked(*d == state.delay)
        }));
        *self.menu.items_mut() = items;
        let anchor = RectF::new(self.new_button.rect().x, self.new_button.rect().y, 32.0, 32.0);
        self.menu.open(gfx, anchor, self.size);
    }

    /// Opens the mode menu instantly (previews).
    pub fn force_menu(&mut self, gfx: &Gfx, highlighted: Option<usize>) {
        self.open_menu(gfx);
        let anchor = RectF::new(self.new_button.rect().x, self.new_button.rect().y, 32.0, 32.0);
        self.menu.force_open(gfx, anchor, self.size, highlighted);
    }

    pub fn force_hdr_popover(&mut self) {
        if let Some(anchor) = self.hdr_anchor() {
            self.hdr_popover.force_open(anchor, self.size);
        }
    }

    pub fn force_picker(&mut self) {
        if let Some(anchor) = self.options.item("custom").map(|i| i.rect()) {
            self.picker_popover.force_open(anchor, self.size);
        }
    }

    pub fn show_toast(&mut self, text: &str, kind: ToastKind) {
        self.toast = Some(Toast { text: text.to_string(), kind });
        self.toast_progress.snap(0.0);
        self.toast_progress.set_with(1.0, Motion::Spring(Spring::DEFAULT));
    }

    pub fn snap_toast(&mut self) {
        self.toast_progress.snap(1.0);
    }

    pub fn hide_toast(&mut self) {
        self.toast_progress.set_with(0.0, Motion::Tween(Tween::FADE_OUT));
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<ChromeAction> {
        if self.menu.is_open() {
            return match self.menu.event(cx, event) {
                Response::Action(i) => menu_action(i).map_or(Response::Consumed, Response::Action),
                Response::Consumed => Response::Consumed,
                Response::Ignored => Response::Ignored,
            };
        }
        if self.hdr_popover.is_open()
            && let Some(r) = self.hdr_popover_event(cx, event)
        {
            return r;
        }
        if self.picker_popover.is_open() {
            match self.picker.event(cx, event) {
                Response::Action(color) => return Response::Action(ChromeAction::Color { color, custom: true }),
                Response::Consumed => return Response::Consumed,
                Response::Ignored => {}
            }
            if self.picker_popover.event(cx, event).consumed() {
                return Response::Consumed;
            }
        }
        let routed = self.title_event(cx, event);
        if routed.consumed() {
            return routed;
        }
        let routed = self.options_event(cx, event);
        if routed.consumed() {
            return routed;
        }
        self.bottom_event(cx, event)
    }

    fn hdr_popover_event(&mut self, cx: &mut Ctx, event: &Event) -> Option<Response<ChromeAction>> {
        if matches!(event, Event::PointerCancel | Event::Focus(false)) && self.exposure_dragging {
            let _ = self.exposure.event(cx, event);
            self.exposure_dragging = false;
            return Some(Response::Action(ChromeAction::Exposure { value: self.exposure.value(), done: true }));
        }
        let content = self.hdr_popover.content_rect();
        self.tone_mode.layout(cx.gfx(), RectF::new(content.x, content.y + 28.0, content.w, 28.0));
        self.exposure.set_rect(RectF::new(content.x - 2.0, content.y + 94.0, content.w + 4.0, Slider::HEIGHT));
        if let Response::Action(i) = self.tone_mode.event(cx, event) {
            let mode = if i == 1 { ToneMapMode::Clip } else { ToneMapMode::Auto };
            return Some(Response::Action(ChromeAction::ToneMode(mode)));
        }
        match self.exposure.event(cx, event) {
            Response::Action(value) => {
                self.exposure_dragging = matches!(event, Event::PointerDown(_) | Event::PointerMove(_));
                return Some(Response::Action(ChromeAction::Exposure { value, done: !self.exposure_dragging }));
            }
            Response::Consumed => {
                if matches!(event, Event::PointerDown(_)) {
                    self.exposure_dragging = true;
                }
                if matches!(event, Event::PointerUp(_)) && self.exposure_dragging {
                    self.exposure_dragging = false;
                    return Some(Response::Action(ChromeAction::Exposure { value: self.exposure.value(), done: true }));
                }
                return Some(Response::Consumed);
            }
            Response::Ignored => {}
        }
        self.hdr_popover.event(cx, event).consumed().then_some(Response::Consumed)
    }

    fn title_event(&mut self, cx: &mut Ctx, event: &Event) -> Response<ChromeAction> {
        if self.new_button.event(cx, event).action().is_some() {
            return Response::Action(ChromeAction::NewSnip);
        }
        if self.chevron.event(cx, event).action().is_some() {
            self.open_menu(cx.gfx());
            return Response::Consumed;
        }
        let mut result = Response::Ignored;
        for (id, button) in &mut self.actions {
            match button.event(cx, event) {
                Response::Action(()) => {
                    result = Response::Action(match *id {
                        "ocr" => ChromeAction::Ocr,
                        "copy" => ChromeAction::Copy,
                        "save" => ChromeAction::Save,
                        _ => ChromeAction::Share,
                    })
                }
                Response::Consumed if !matches!(result, Response::Action(_)) => result = Response::Consumed,
                _ => {}
            }
        }
        if result.consumed() {
            return result;
        }
        if self.crop_presence.is_shown() {
            let actions = [ChromeAction::CropCancel, ChromeAction::CropReset, ChromeAction::CropDone];
            for (button, action) in self.crop_buttons.iter_mut().zip(actions) {
                if matches!(event, Event::KeyDown(_)) {
                    continue;
                }
                match button.event(cx, event) {
                    Response::Action(()) => return Response::Action(action),
                    Response::Consumed => result = Response::Consumed,
                    Response::Ignored => {}
                }
            }
            if matches!(event, Event::PointerDown(e) if self.crop_rect.contains(e.pos)) {
                return Response::Consumed;
            }
            return result;
        }
        match self.tools.event(cx, event) {
            Response::Action(ToolbarAction::Selected("tools", i)) => Response::Action(ChromeAction::Tool(Tool::ALL[i])),
            Response::Action(ToolbarAction::Clicked("undo")) => Response::Action(ChromeAction::Undo),
            Response::Action(ToolbarAction::Clicked("redo")) => Response::Action(ChromeAction::Redo),
            other => {
                if other.consumed() {
                    Response::Consumed
                } else {
                    Response::Ignored
                }
            }
        }
    }

    fn options_event(&mut self, cx: &mut Ctx, event: &Event) -> Response<ChromeAction> {
        if !self.options.is_visible() {
            return Response::Ignored;
        }
        for (i, swatch) in self.swatches.iter_mut().enumerate() {
            if swatch.event(cx, event).action().is_some() {
                if i < 8 {
                    return Response::Action(ChromeAction::Color { color: tools::palette(i), custom: false });
                }
                if let Some(anchor) = self.options.item("custom").map(|it| it.rect()) {
                    if self.picker_popover.is_open() {
                        self.picker_popover.close();
                    } else {
                        self.picker_popover.open(anchor, self.size);
                    }
                }
                let custom = swatch.color.unwrap_or_else(|| self.picker.hsv.to_color());
                return Response::Action(ChromeAction::Color { color: custom, custom: true });
            }
        }
        match self.options.event(cx, event) {
            Response::Action(ToolbarAction::Selected("size", i)) => Response::Action(ChromeAction::Size(i)),
            Response::Action(ToolbarAction::Selected("tsize", i)) => Response::Action(ChromeAction::TextSize(i)),
            Response::Action(ToolbarAction::Selected("shape", i)) => Response::Action(ChromeAction::Shape(ShapeKind::ALL[i])),
            Response::Action(ToolbarAction::Selected("redact", i)) => {
                Response::Action(ChromeAction::Redact(if i == 1 { RedactKind::Pixelate } else { RedactKind::Blur }))
            }
            Response::Action(ToolbarAction::Clicked("fill")) => {
                let filled = self.options.button_mut("fill").is_some_and(|b| !b.is_selected());
                Response::Action(ChromeAction::Fill(filled))
            }
            other => {
                if other.consumed() {
                    Response::Consumed
                } else {
                    Response::Ignored
                }
            }
        }
    }

    fn bottom_event(&mut self, cx: &mut Ctx, event: &Event) -> Response<ChromeAction> {
        if self.ocr_presence.is_shown() {
            if self.ocr_copy.event(cx, event).action().is_some() {
                return Response::Action(ChromeAction::OcrCopyAll);
            }
            if self.ocr_close.event(cx, event).action().is_some() {
                return Response::Action(ChromeAction::OcrClose);
            }
            if matches!(event, Event::PointerDown(e) if self.ocr_rect.contains(e.pos)) {
                return Response::Consumed;
            }
            return Response::Ignored;
        }
        if !self.bottom.is_visible() {
            return Response::Ignored;
        }
        if self.zoom_label.event(cx, event).action().is_some() {
            return Response::Action(ChromeAction::ZoomActual);
        }
        if !matches!(event, Event::KeyDown(_)) && self.fit.event(cx, event).action().is_some() {
            return Response::Action(ChromeAction::Fit);
        }
        if self.bottom.item("hdr").is_some() && self.hdr_badge.event(cx, event).action().is_some() {
            if let Some(anchor) = self.hdr_anchor() {
                self.hdr_popover.open(anchor, self.size);
            }
            return Response::Consumed;
        }
        match self.bottom.event(cx, event) {
            Response::Action(ToolbarAction::Clicked("zoom_in")) => Response::Action(ChromeAction::ZoomIn),
            Response::Action(ToolbarAction::Clicked("zoom_out")) => Response::Action(ChromeAction::ZoomOut),
            other => {
                if other.consumed() {
                    Response::Consumed
                } else {
                    Response::Ignored
                }
            }
        }
    }

    pub fn paint(&mut self, p: &mut Painter, time: f64) {
        let Some(state) = self.state.clone() else { return };
        let theme = p.theme().clone();
        self.new_button.icon = if state.delay > 0 { Icon::Timer } else { Icon::Scissors };
        self.new_button.paint(p);
        let chevron_bg = self.chevron.highlight(p);
        if chevron_bg.a > 0.0 {
            p.fill_round_rect(p.snap_rect(self.chevron.rect), 8.0, chevron_bg);
        }
        p.icon_with_stroke(Icon::ChevronDown, self.chevron.rect.center(), 12.0, theme.text_secondary, 2.0);
        for (_, button) in &mut self.actions {
            button.paint(p);
        }

        let solid = self.solid_base(&theme);
        let backdrop = Backdrop::new(&solid, RectF::new(0.0, 0.0, self.size.w, self.size.h));
        self.tools.paint(p, None);
        let crop_rect = self.crop_rect;
        let buttons = &mut self.crop_buttons;
        self.crop_presence.paint(p, PointF::new(crop_rect.center().x, crop_rect.y), |p| {
            p.glass(crop_rect, 14.0, None);
            for b in buttons.iter_mut() {
                b.paint(p);
            }
        });

        self.paint_options(p, &state, &backdrop);
        self.paint_bottom(p, &state, time, &backdrop);
        self.paint_toast(p);

        let hdr = state.hdr;
        let tone_mode = &mut self.tone_mode;
        let exposure = &mut self.exposure;
        self.hdr_popover.paint(p, Some(&backdrop), |p, r| {
            let Some(hdr) = hdr else { return };
            let theme = p.theme().clone();
            p.text("Tone mapping", &TextStyle::emphasized(), theme.text, RectF::new(r.x, r.y, r.w, 20.0));
            tone_mode.layout(p.gfx(), RectF::new(r.x, r.y + 28.0, r.w, 28.0));
            tone_mode.paint(p);
            p.text("Exposure", &TextStyle::body(), theme.text_secondary, RectF::new(r.x, r.y + 68.0, r.w, 20.0));
            let value = format!("{:+.1} EV", hdr.exposure);
            let value_style = TextStyle::body().tabular().align(TextAlign::Trailing);
            p.text(&value, &value_style, theme.text_secondary, RectF::new(r.x, r.y + 68.0, r.w, 20.0));
            exposure.set_rect(RectF::new(r.x - 2.0, r.y + 94.0, r.w + 4.0, glint_ui::widgets::Slider::HEIGHT));
            exposure.paint(p);
            let caption = format!("Highlights up to {:.1}× SDR white", hdr.peak.max(1.0));
            p.text(&caption, &TextStyle::caption(), theme.text_tertiary, RectF::new(r.x, r.y + 120.0, r.w, 14.0));
        });
        let picker = &mut self.picker;
        self.picker_popover.paint(p, Some(&backdrop), |p, r| {
            picker.set_rect(r);
            picker.paint(p);
        });
        self.menu.paint(p, Some(&backdrop));
    }

    fn solid_base(&mut self, theme: &Theme) -> Rc<Bitmap> {
        let key = theme.window_background.to_rgba8();
        if let Some((k, bitmap)) = &self.solid
            && *k == key
        {
            return bitmap.clone();
        }
        let [r, g, b, _] = key;
        let bitmap = Bitmap::new(Image::from_bgra(2, 2, [b, g, r, 255].repeat(4)));
        self.solid = Some((key, bitmap.clone()));
        bitmap
    }

    fn paint_options(&mut self, p: &mut Painter, state: &ChromeState, backdrop: &Backdrop) {
        self.options.paint(p, Some(backdrop));
        let rect = self.options.rect();
        let swatches = &mut self.swatches;
        let cells: Vec<RectF> =
            self.options.segmented_mut("tsize").map(|s| (0..3).filter_map(|i| s.item_rect(i)).collect()).unwrap_or_default();
        let text_size = state.options.text_size;
        self.options_presence.paint(p, PointF::new(rect.center().x, rect.y), |p| {
            for swatch in swatches.iter_mut() {
                swatch.paint(p);
            }
            let theme = p.theme().clone();
            for (i, cell) in cells.iter().enumerate() {
                let style = TextStyle::new([11.0, 14.0, 18.0][i]).weight(Weight::Semibold).centered();
                let color = if i == text_size { theme.text } else { theme.text_secondary };
                p.text("A", &style, color, *cell);
            }
        });
    }

    fn paint_bottom(&mut self, p: &mut Painter, state: &ChromeState, time: f64, backdrop: &Backdrop) {
        let theme = p.theme().clone();
        self.bottom.paint(p, Some(backdrop));
        let bottom_rect = self.bottom.rect();
        let has_hdr = self.bottom.item("hdr").is_some();
        let size_slot = self.bottom.item("size").map(|i| i.rect());
        let popover_open = self.hdr_popover.is_open();
        let (zoom_label, fit, hdr_badge) = (&self.zoom_label, &mut self.fit, &self.hdr_badge);
        self.bottom_presence.paint(p, PointF::new(bottom_rect.center().x, bottom_rect.y), |p| {
            let zoom_bg = zoom_label.highlight(p);
            if zoom_bg.a > 0.0 {
                p.fill_round_rect(p.snap_rect(zoom_label.rect), 8.0, zoom_bg);
            }
            let zoom = format!("{} %", state.zoom_percent);
            let style = TextStyle::body().weight(Weight::Medium).tabular().centered();
            p.text(&zoom, &style, theme.text, zoom_label.rect);
            fit.paint(p);
            if let Some(size) = size_slot {
                p.text(&size_text(state.content_size), &size_style().centered(), theme.text_secondary, size);
            }
            if has_hdr {
                let r = hdr_badge.rect;
                Badge::new("HDR").style(BadgeStyle::Accent).paint(p, r);
                let lift = hdr_badge.interaction.hover_amount() * 0.12 + if popover_open { 0.12 } else { 0.0 };
                if lift > 0.0 {
                    p.fill_round_rect(r, r.h / 2.0, Color::WHITE.with_alpha(lift));
                }
            }
        });
        let rect = self.ocr_rect;
        let copy = &mut self.ocr_copy;
        let close = &mut self.ocr_close;
        let ocr = state.ocr;
        self.spinner_phase = (time * 1.4).fract() as f32;
        let phase = self.spinner_phase;
        self.ocr_presence.paint(p, PointF::new(rect.center().x, rect.bottom()), |p| {
            p.glass(rect, 14.0, Some(backdrop));
            let label = RectF::new(rect.x + 14.0, rect.y, 120.0, rect.h);
            match ocr {
                OcrBar::Busy => {
                    paint_spinner(p, PointF::new(label.x + 8.0, label.center().y), phase);
                    p.text("Recognizing…", &TextStyle::body(), theme.text_secondary, label.offset(22.0, 0.0));
                }
                OcrBar::Ready { lines } => {
                    p.icon(Icon::ScanText, PointF::new(label.x + 8.0, label.center().y), 16.0, theme.accent);
                    let text = match lines {
                        0 => "No text found".to_string(),
                        1 => "1 line".to_string(),
                        n => format!("{n} lines"),
                    };
                    p.text(&text, &TextStyle::body().weight(Weight::Medium), theme.text, label.offset(22.0, 0.0));
                }
                OcrBar::Hidden => {}
            }
            copy.enabled = matches!(ocr, OcrBar::Ready { lines } if lines > 0);
            copy.paint(p);
            close.paint(p);
        });
    }

    fn paint_toast(&mut self, p: &mut Painter) {
        let Some(toast) = &self.toast else { return };
        let progress = self.toast_progress.get();
        if progress <= 0.001 {
            return;
        }
        let badge = match toast.kind {
            ToastKind::Done => Badge::new(&toast.text).icon(Icon::Check),
            ToastKind::Info => Badge::new(&toast.text),
            ToastKind::Error => Badge::new(&toast.text).style(BadgeStyle::Destructive).icon(Icon::Info),
        };
        let anchor = self.bottom_rect();
        let size = badge.size(p.gfx());
        let center = PointF::new(self.size.w / 2.0, anchor.y - 12.0 - size.h / 2.0 + (1.0 - progress) * 14.0);
        let opacity = progress.clamp(0.0, 1.0);
        p.layer(opacity, |p| {
            p.scale_around(0.96 + 0.04 * opacity, center, |p| {
                badge.paint_centered(p, center);
            });
        });
    }
}

fn paint_spinner(p: &mut Painter, center: PointF, phase: f32) {
    let theme = p.theme().clone();
    for i in 0..8 {
        let angle = std::f32::consts::TAU * (i as f32 / 8.0 + phase);
        let fade = ((i as f32 / 8.0) + 0.15).min(1.0);
        let dir = PointF::new(angle.cos(), angle.sin());
        let a = PointF::new(center.x + dir.x * 3.5, center.y + dir.y * 3.5);
        let b = PointF::new(center.x + dir.x * 7.0, center.y + dir.y * 7.0);
        p.line(a, b, theme.text_secondary.multiply_alpha(fade), 1.8, &glint_ui::StrokeStyle::round());
    }
}
