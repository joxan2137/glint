//! The editor window's view: document state, tools, gestures, chrome and app wiring.

mod canvas;
mod commands;
mod staging;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use glint_core::{CaptureMode, HdrImage, HdrStats, Image, PointF, RectF, Settings, SizeF, ToneMapParams};
use glint_ui::widgets::Response;
use glint_ui::{
    Animated, Color, Ctx, Cursor, Event, Gfx, HitArea, Icon, Key, KeyEvent, Modifiers, MouseButton, Painter, TimerId,
    View,
};

use crate::EditorHost;
use crate::chrome::{self, Chrome, ChromeAction, ChromeState, HdrPanel, OcrBar};
use crate::crop;
use crate::history::History;
use crate::model::{Body, Document, InkPoint, Redaction, Shape, StrokeKind};
use crate::ocr_text::RecognizedText;
use crate::render::{GeometryCache, GfxMeasure, Layer};
use crate::model::{Annotation, ShapeKind};
use crate::stroke_panel::{StrokeChange, StrokeSpec, StrokeTarget};
use crate::tools::{self, TEXT_SIZES, Tool, ToolOptions};
use crate::viewport::{self, Mapping, Viewport};
use crate::worker::{Poster, Retoner};

const TIMER_CARET: u64 = 1;
const TIMER_TOAST: u64 = 2;
const TIMER_AUTOCOPY: u64 = 3;
const TIMER_PERSIST: u64 = 4;

/// How an option edit enters the undo history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditPhase {
    /// A slider is still moving: the step stays open.
    Dragging,
    /// The slider was released: the open step closes.
    Released,
    /// A click, key or wheel notch: one step, merged with quick repeats on the same object.
    Discrete,
}

enum Gesture {
    None,
    Ink { raw: Vec<InkPoint>, kind: StrokeKind, pressure: bool, min_distance: f32 },
    Shape { start: PointF, shape: Shape },
    Redact { start: PointF, redaction: Redaction },
    Move { id: u64, last: PointF },
    Resize { id: u64, handle: crate::hit::Handle, start: PointF, original: crate::model::Annotation },
    Erase { last: PointF },
    Pan { last: PointF },
    Crop { handle: crop::CropHandle, start: PointF, start_rect: RectF },
    OcrSelect { anchor: Option<usize>, start: PointF },
}

struct TextEditing {
    id: u64,
    /// Byte index into the note's text.
    caret: usize,
}

struct CropSession {
    rect: RectF,
    shown: Animated<RectF>,
    grid: Animated<f32>,
    previous_tool: Tool,
}

enum OcrState {
    Off,
    Busy,
    Ready { text: RecognizedText, selection: Option<(usize, usize)> },
}

pub(crate) struct EditorView {
    doc: Document,
    base_image: Rc<Image>,
    /// The base as drawn on the canvas (downscaled when beyond the GPU's bitmap limit); built on first paint.
    base_layer: Option<Layer>,
    pixelated_image: Option<Rc<Image>>,
    pixelated_layer: Option<Layer>,
    hdr: Option<Arc<HdrImage>>,
    hdr_stats: Option<HdrStats>,
    retoner: Option<Retoner>,
    retone_requested: u64,
    retone_applied: u64,
    requested_tone_map: ToneMapParams,
    /// Image pixels per DIP at 100 % (the capture monitor's scale).
    image_scale: f32,
    history: History,
    cache: GeometryCache,

    tool: Tool,
    options: [ToolOptions; 8],
    selection: Option<u64>,
    handles: Animated<f32>,
    gesture: Gesture,
    /// The pointer id and button that started the gesture; other pointers and buttons are ignored until it ends.
    gesture_pointer: Option<(u32, Option<MouseButton>)>,
    hover: Option<PointF>,
    erase_target: Option<u64>,
    space_held: bool,
    editing: Option<TextEditing>,
    caret_on: bool,
    crop: Option<CropSession>,
    ocr: OcrState,
    /// Bumped whenever recognized text would no longer match the canvas; older OCR results are dropped.
    ocr_job: u64,

    viewport: Viewport,
    size: SizeF,
    scale: f32,
    chrome: Chrome,

    host: Option<EditorHost>,
    settings: Settings,
    saved_path: Option<PathBuf>,
    mode: CaptureMode,
    delay: u32,
    poster: Option<Poster>,
    timers: [Option<TimerId>; 5],
    unsynced_changes: bool,
    /// (object, frame time) of the last option edit on a selection; rapid edits (a color drag) share one undo step.
    last_option_edit: Option<(u64, f64)>,
}

impl EditorView {
    pub fn new(doc: crate::EditorDoc, settings: &Settings, host: Option<EditorHost>) -> Self {
        let (tool, options) = tools::remembered();
        let image_scale = doc.monitor.as_ref().map_or(1.0, |m| m.scale()).max(0.5);
        let base_image = Rc::new(doc.image);
        let hdr = doc.hdr.filter(|h| {
            let valid = h.width == base_image.width
                && h.height == base_image.height
                && h.data.len() == h.width as usize * h.height as usize * 4;
            if !valid {
                log::warn!("ignoring an HDR capture whose size does not match the image");
            }
            valid
        });
        let mode = match settings.capture.last_mode {
            CaptureMode::Text | CaptureMode::ColorPicker => CaptureMode::Rectangle,
            m => m,
        };
        Self {
            doc: Document::new(doc.tone_map),
            base_image,
            base_layer: None,
            pixelated_image: None,
            pixelated_layer: None,
            hdr: hdr.map(Arc::new),
            hdr_stats: None,
            retoner: None,
            retone_requested: 0,
            retone_applied: 0,
            requested_tone_map: doc.tone_map,
            image_scale,
            history: History::new(),
            cache: GeometryCache::default(),
            tool,
            chrome: Chrome::new(tool, &options[tool.index()]),
            options,
            selection: None,
            handles: Animated::fade(0.0),
            gesture: Gesture::None,
            gesture_pointer: None,
            hover: None,
            erase_target: None,
            space_held: false,
            editing: None,
            caret_on: true,
            crop: None,
            ocr: OcrState::Off,
            ocr_job: 0,
            viewport: Viewport::default(),
            size: SizeF::default(),
            scale: 1.0,
            host,
            settings: settings.clone(),
            saved_path: doc.saved_path,
            mode,
            delay: settings.capture.delay_secs,
            poster: None,
            timers: [None; 5],
            unsynced_changes: false,
            last_option_edit: None,
        }
    }

    pub fn image_size(&self) -> SizeF {
        SizeF::new(self.base_image.width as f32, self.base_image.height as f32)
    }

    fn measure<'a>(&self, gfx: &'a Gfx) -> GfxMeasure<'a> {
        GfxMeasure(gfx)
    }

    /// The image region the canvas shows: everything while cropping, else the crop.
    fn content(&self) -> RectF {
        if self.crop.is_some() {
            RectF::new(0.0, 0.0, self.image_size().w, self.image_size().h)
        } else {
            self.doc.content_rect(self.image_size()).to_f()
        }
    }

    fn fit_area(&self) -> RectF {
        RectF::from_ltrb(
            chrome::FIT_SIDE,
            chrome::FIT_TOP,
            (self.size.w - chrome::FIT_SIDE).max(chrome::FIT_SIDE + 1.0),
            (self.size.h - chrome::FIT_BOTTOM).max(chrome::FIT_TOP + 1.0),
        )
    }

    fn fit_mapping(&self) -> Mapping {
        let content = self.content();
        let area = self.fit_area();
        let s = viewport::fit_scale(content.size(), area, 1.0 / self.scale);
        viewport::centered(content, s, area)
    }

    fn refit(&mut self, animate: bool) {
        self.viewport.fit = true;
        let m = self.fit_mapping();
        if animate { self.viewport.animate_to(m) } else { self.viewport.snap_to(m) }
    }

    /// Lays out chrome and keeps the canvas fitted on resize. Safe to call every event and frame.
    fn prepare(&mut self, gfx: &Gfx, size: SizeF, scale: f32, caption: Option<RectF>) {
        let resized = size != self.size || scale != self.scale;
        self.size = size;
        self.scale = scale;
        let state = self.chrome_state();
        self.chrome.sync(gfx, state);
        self.chrome.layout(gfx, size, caption);
        if resized && size.w >= 1.0 && size.h >= 1.0 {
            if self.viewport.fit {
                self.refit(false);
            } else {
                let m = viewport::clamp(self.viewport.target(), self.content(), self.fit_area());
                self.viewport.snap_to(m);
            }
        }
    }

    fn prepare_cx(&mut self, cx: &Ctx) {
        let caption = cx.caption_buttons_rect();
        let gfx = cx.gfx().clone();
        self.prepare(&gfx, cx.size(), cx.scale(), caption);
    }

    fn selection_tool(&self) -> Option<Tool> {
        let a = self.doc.get(self.selection?)?;
        Some(match &a.body {
            Body::Stroke(s) if s.kind == StrokeKind::Highlighter => Tool::Highlighter,
            Body::Stroke(_) => Tool::Pen,
            Body::Shape(_) => Tool::Shapes,
            Body::Text(_) => Tool::Text,
            Body::Redact(_) => Tool::Redact,
        })
    }

    /// Options as the selected object has them (Select tool), so the pill edits that object.
    fn selection_options(&self, tool: Tool) -> Option<ToolOptions> {
        let a = self.doc.get(self.selection?)?;
        let mut o = self.options[tool.index()];
        let px = self.image_scale;
        match &a.body {
            Body::Stroke(s) => {
                o.color = s.color;
                o.width = s.width / px;
                o.opacity = s.opacity;
                o.dash = s.dash;
                o.pressure = s.pressure;
                o.smoothing = s.smoothing;
            }
            Body::Shape(s) => {
                o.color = s.color;
                o.width = s.width / px;
                o.shape = s.kind;
                o.filled = s.filled;
                o.opacity = s.opacity;
                o.dash = s.dash;
                o.fill_opacity = s.fill_opacity;
                o.corner_radius = s.corner_radius / px;
                o.head_scale = s.head_scale;
                o.set_caps(s.kind, s.caps);
            }
            Body::Text(t) => {
                o.color = t.color;
                o.text_size = tools::nearest(&TEXT_SIZES, t.size / px);
                o.filled = t.background;
            }
            Body::Redact(r) => o.redact = r.kind,
        }
        Some(o)
    }

    /// The stroke button and popover's view of `o` for `tool`, in image pixels.
    fn stroke_spec(&self, tool: Tool, o: &ToolOptions) -> Option<StrokeSpec> {
        let target = match tool {
            Tool::Pen => StrokeTarget::Pen,
            Tool::Highlighter => StrokeTarget::Highlighter,
            Tool::Shapes => StrokeTarget::for_shape(o.shape),
            _ => return None,
        };
        let px = self.image_scale;
        Some(StrokeSpec {
            target,
            color: o.color,
            width: o.width * px,
            opacity: o.opacity,
            dash: if target == StrokeTarget::Highlighter { crate::model::Dash::Solid } else { o.dash },
            pressure: o.pressure,
            smoothing: o.smoothing,
            caps: o.caps(o.shape),
            head_scale: o.head_scale,
            corner_radius: o.corner_radius * px,
            filled: o.filled,
            fill_opacity: o.fill_opacity,
            px_per_dip: px,
        })
    }

    fn chrome_state(&self) -> ChromeState {
        let selected_tool = self.selection_tool();
        let options_tool = match self.tool {
            Tool::Select => selected_tool,
            Tool::Crop => None,
            t => Some(t),
        };
        let options = match (self.tool, selected_tool) {
            (Tool::Select, Some(t)) => self.selection_options(t).unwrap_or(self.options[t.index()]),
            (t, Some(s)) if s == t => self.selection_options(t).unwrap_or(self.options[t.index()]),
            _ => self.options[self.tool.index()],
        };
        let content = self.doc.content_rect(self.image_size());
        let hdr = self.hdr_stats.filter(|s| s.has_hdr_content()).map(|s| HdrPanel {
            mode: self.doc.tone_map.mode,
            exposure: self.doc.tone_map.exposure_stops,
            peak: s.peak,
        });
        let ocr = match &self.ocr {
            OcrState::Off => OcrBar::Hidden,
            OcrState::Busy => OcrBar::Busy,
            OcrState::Ready { text, .. } => OcrBar::Ready { lines: text.line_count },
        };
        ChromeState {
            tool: self.tool,
            options_tool,
            options,
            can_undo: self.history.can_undo(),
            can_redo: self.history.can_redo(),
            cropping: self.crop.is_some(),
            zoom_percent: (self.viewport.target().scale * self.scale * 100.0).round() as u32,
            content_size: (content.w.max(0) as u32, content.h.max(0) as u32),
            hdr,
            ocr,
            mode: self.mode,
            delay: self.delay,
            stroke: options_tool.and_then(|t| self.stroke_spec(t, &options)),
            preset: options_tool.and_then(|t| options.preset(t)),
        }
    }

    fn select(&mut self, id: Option<u64>) {
        if self.selection != id {
            self.selection = id;
            self.handles.snap(0.0);
            self.handles.set(if id.is_some() { 1.0 } else { 0.0 });
        }
    }

    pub(crate) fn set_tool(&mut self, cx: &mut Ctx, tool: Tool) {
        self.finish_text(cx);
        self.exit_ocr();
        if self.crop.is_some() {
            return;
        }
        if tool == Tool::Crop {
            self.enter_crop();
        } else {
            self.tool = tool;
            if tool != Tool::Select && self.selection_tool() != Some(tool) {
                self.select(None);
            }
        }
        tools::remember(self.tool, &self.options);
        cx.request_paint();
    }

    fn after_change(&mut self, cx: &mut Ctx) {
        self.unsynced_changes = true;
        self.retone_if_needed();
        self.schedule_autocopy(cx);
        cx.request_paint();
    }

    /// Undo and redo wait for drags and slider moves to finish (typing is committed first).
    fn history_busy(&self) -> bool {
        !matches!(self.gesture, Gesture::None) || self.history.is_open()
    }

    fn undo(&mut self, cx: &mut Ctx) {
        self.finish_text(cx);
        if self.history_busy() {
            return;
        }
        if let Some(previous) = self.history.undo(&self.doc) {
            self.restore(previous);
            self.after_change(cx);
        }
    }

    fn redo(&mut self, cx: &mut Ctx) {
        self.finish_text(cx);
        if self.history_busy() {
            return;
        }
        if let Some(next) = self.history.redo(&self.doc) {
            self.restore(next);
            self.after_change(cx);
        }
    }

    fn restore(&mut self, doc: Document) {
        let crop_changed = doc.crop != self.doc.crop;
        self.doc = doc;
        if self.selection.is_some_and(|id| self.doc.get(id).is_none()) {
            self.select(None);
        }
        if crop_changed && self.crop.is_none() {
            self.exit_ocr();
            self.refit(true);
        }
    }

    fn delete_selection(&mut self, cx: &mut Ctx) {
        let Some(id) = self.selection else { return };
        self.history.record(&self.doc);
        self.doc.remove(id);
        self.select(None);
        self.after_change(cx);
    }

    /// Moves the selection by whole pixels; a held key (auto-repeat) is one undo step.
    fn nudge(&mut self, cx: &mut Ctx, dx: f32, dy: f32, repeat: bool) {
        let Some(id) = self.selection else { return };
        if !repeat {
            self.history.record(&self.doc);
        }
        if let Some(a) = self.doc.get_mut(id) {
            a.translate(PointF::new(dx, dy));
        }
        self.after_change(cx);
    }

    fn apply_chrome_action(&mut self, cx: &mut Ctx, action: ChromeAction) {
        match action {
            ChromeAction::Tool(tool) => self.set_tool(cx, tool),
            ChromeAction::Undo => self.undo(cx),
            ChromeAction::Redo => self.redo(cx),
            ChromeAction::NewSnip => self.new_snip(cx, self.mode),
            ChromeAction::PickMode(mode) => {
                self.mode = mode;
                self.new_snip(cx, mode);
            }
            ChromeAction::PickDelay(delay) => self.delay = delay,
            ChromeAction::Ocr => self.start_ocr(cx),
            ChromeAction::Copy => self.copy(cx),
            ChromeAction::Save => self.save(cx),
            ChromeAction::Share => self.share(cx),
            ChromeAction::CropCancel => self.cancel_crop(),
            ChromeAction::CropReset => self.reset_crop(),
            ChromeAction::CropDone => self.commit_crop(cx),
            ChromeAction::Color { color, custom } => {
                self.change_options(cx, |o| {
                    o.color = color;
                    if custom {
                        o.custom = Some(color);
                    }
                });
            }
            ChromeAction::Size(size) => {
                let tool = self.chrome_state().options_tool.unwrap_or(self.tool);
                let width = ToolOptions::presets(tool)[size.min(2)];
                self.change_options(cx, |o| o.width = width);
            }
            ChromeAction::Shape(shape) => self.change_options(cx, |o| o.shape = shape),
            ChromeAction::Fill(filled) => self.change_options(cx, |o| o.filled = filled),
            ChromeAction::TextSize(size) => self.change_options(cx, |o| o.text_size = size),
            ChromeAction::Redact(kind) => self.change_options(cx, |o| o.redact = kind),
            ChromeAction::ZoomIn => self.zoom_step(true, None),
            ChromeAction::ZoomOut => self.zoom_step(false, None),
            ChromeAction::ZoomActual => self.zoom_to(1.0, None),
            ChromeAction::Fit => self.refit(true),
            ChromeAction::ToneMode(mode) => {
                self.history.record(&self.doc);
                self.doc.tone_map.mode = mode;
                self.after_change(cx);
            }
            ChromeAction::Exposure { value, done } => {
                self.history.begin(&self.doc);
                self.doc.tone_map.exposure_stops = value;
                self.retone_if_needed();
                if done {
                    self.history.commit(&self.doc);
                    self.after_change(cx);
                }
            }
            ChromeAction::OcrCopyAll => self.copy_ocr_text(cx, true),
            ChromeAction::OcrClose => self.exit_ocr(),
            ChromeAction::Stroke { change, done } => {
                let discrete = !matches!(
                    change,
                    StrokeChange::Width(_)
                        | StrokeChange::Opacity(_)
                        | StrokeChange::Smoothing(_)
                        | StrokeChange::HeadScale(_)
                        | StrokeChange::CornerRadius(_)
                        | StrokeChange::FillOpacity(_)
                );
                let phase = match (discrete, done) {
                    (true, _) => EditPhase::Discrete,
                    (false, false) => EditPhase::Dragging,
                    (false, true) => EditPhase::Released,
                };
                self.change_stroke(cx, change, phase);
            }
            ChromeAction::WidthStep(steps) => {
                self.step_width(cx, steps);
            }
        }
        cx.request_paint();
    }

    /// Applies a stroke setting to the tool's defaults and, when an object of that kind is selected, to the object.
    fn change_stroke(&mut self, cx: &mut Ctx, change: StrokeChange, phase: EditPhase) {
        let state = self.chrome_state();
        let (Some(tool), Some(spec)) = (state.options_tool, state.stroke) else { return };
        let kind = match spec.target {
            StrokeTarget::Line => ShapeKind::Line,
            StrokeTarget::Arrow => ShapeKind::Arrow,
            StrokeTarget::Ellipse => ShapeKind::Ellipse,
            _ => ShapeKind::Rectangle,
        };
        apply_stroke_to_options(&mut self.options[tool.index()], change, kind, self.image_scale);
        if self.selection_tool() == Some(tool) {
            self.edit_selection(cx, phase, |a| apply_stroke_to_annotation(a, change));
        }
        tools::remember(self.tool, &self.options);
        self.schedule_persist(cx);
    }

    /// `[` / `]` and the wheel over the stroke button.
    fn step_width(&mut self, cx: &mut Ctx, steps: i32) -> bool {
        let Some(spec) = self.chrome_state().stroke else { return false };
        let width = tools::step_width(spec.width, steps);
        if (width - spec.width).abs() > 1e-3 {
            self.change_stroke(cx, StrokeChange::Width(width), EditPhase::Discrete);
        }
        true
    }

    /// Edits the selected object; `phase` decides how the edit joins the undo history.
    fn edit_selection(&mut self, cx: &mut Ctx, phase: EditPhase, edit: impl Fn(&mut Annotation)) {
        let Some(id) = self.selection else { return };
        match phase {
            EditPhase::Dragging | EditPhase::Released => {
                self.history.begin(&self.doc);
                if let Some(a) = self.doc.get_mut(id) {
                    edit(a);
                }
                if phase == EditPhase::Released && self.history.commit(&self.doc) {
                    self.after_change(cx);
                }
            }
            EditPhase::Discrete => {
                let before = self.doc.clone();
                if let Some(a) = self.doc.get_mut(id) {
                    edit(a);
                }
                if self.doc != before {
                    let now = glint_ui::anim::now();
                    let continuing = self.last_option_edit.is_some_and(|(last, at)| last == id && now - at < 1.0);
                    if !continuing {
                        self.history.record(&before);
                    }
                    self.last_option_edit = Some((id, now));
                    self.after_change(cx);
                }
            }
        }
        cx.request_paint();
    }

    fn schedule_persist(&mut self, cx: &mut Ctx) {
        if cx.app().is_some() {
            self.set_view_timer(cx, TIMER_PERSIST, 1200);
        }
    }

    /// Applies an option change to the tool and, when an object of that kind is selected, to the object.
    fn change_options(&mut self, cx: &mut Ctx, change: impl Fn(&mut ToolOptions)) {
        let target_tool = self.chrome_state().options_tool.unwrap_or(self.tool);
        change(&mut self.options[target_tool.index()]);
        if self.selection_tool() == Some(target_tool)
            && let Some(mut o) = self.selection_options(target_tool)
        {
            change(&mut o);
            let scale = self.image_scale;
            self.edit_selection(cx, EditPhase::Discrete, |a| apply_options(a, &o, scale));
        }
        tools::remember(self.tool, &self.options);
        self.schedule_persist(cx);
    }

    fn zoom_to(&mut self, zoom_physical: f32, anchor: Option<PointF>) {
        let zoom = zoom_physical.clamp(viewport::MIN_ZOOM, viewport::MAX_ZOOM);
        let anchor = anchor.unwrap_or(self.fit_area().center());
        let current = self.viewport.mapping();
        let target = viewport::zoom_about(current, anchor, zoom / self.scale);
        self.viewport.fit = false;
        self.viewport.animate_to(viewport::clamp(target, self.content(), self.fit_area()));
    }

    fn zoom_step(&mut self, up: bool, anchor: Option<PointF>) {
        let zoom = self.viewport.target().scale * self.scale;
        self.zoom_to(viewport::step_zoom(zoom, up), anchor);
    }

    /// Multiplies the zoom target (so pinches accumulate) while the pixel under `anchor` stays under it.
    fn zoom_by(&mut self, factor: f32, anchor: PointF) {
        let zoom = (self.viewport.target().scale * self.scale * factor).clamp(viewport::MIN_ZOOM, viewport::MAX_ZOOM);
        let target = viewport::zoom_about(self.viewport.mapping(), anchor, zoom / self.scale);
        self.viewport.fit = false;
        self.viewport.animate_to(viewport::clamp(target, self.content(), self.fit_area()));
    }

    fn key_down(&mut self, cx: &mut Ctx, k: &KeyEvent) -> bool {
        if self.editing.is_some() && self.text_key(cx, k) {
            return true;
        }
        let ctrl = Modifiers::CTRL;
        let ctrl_shift = Modifiers::CTRL_SHIFT;
        match (k.key, k.mods) {
            (Key::Char('Z'), m) if m == ctrl => self.undo(cx),
            (Key::Char('Y'), m) if m == ctrl => self.redo(cx),
            (Key::Char('Z'), m) if m == ctrl_shift => self.redo(cx),
            (Key::Char('C' | 'S' | 'N'), m) if k.repeat && (m == ctrl || m == ctrl_shift) => {}
            (Key::Char('C'), m) if m == ctrl => {
                if matches!(self.ocr, OcrState::Ready { selection: Some(_), .. }) {
                    self.copy_ocr_text(cx, false);
                } else {
                    self.copy(cx);
                }
            }
            (Key::Char('S'), m) if m == ctrl => self.save(cx),
            (Key::Char('S'), m) if m == ctrl_shift => self.save_as(cx),
            (Key::Char('N'), m) if m == ctrl => self.new_snip(cx, self.mode),
            (Key::Char('W'), m) if m == ctrl => cx.close(),
            (Key::Char('0'), m) if m == ctrl => self.refit(true),
            (Key::Char('1'), m) if m == ctrl => self.zoom_to(1.0, self.hover),
            (Key::Plus, m) if m == ctrl => self.zoom_step(true, None),
            (Key::Minus, m) if m == ctrl => self.zoom_step(false, None),
            (Key::Enter, m) if m.is_empty() && self.crop.is_some() => self.commit_crop(cx),
            (Key::Escape, _) => self.escape(cx),
            (Key::Delete | Key::Backspace, m) if m.is_empty() => self.delete_selection(cx),
            (Key::Left, m) if self.selection.is_some() => self.nudge(cx, -nudge_step(m), 0.0, k.repeat),
            (Key::Right, m) if self.selection.is_some() => self.nudge(cx, nudge_step(m), 0.0, k.repeat),
            (Key::Up, m) if self.selection.is_some() => self.nudge(cx, 0.0, -nudge_step(m), k.repeat),
            (Key::Down, m) if self.selection.is_some() => self.nudge(cx, 0.0, nudge_step(m), k.repeat),
            (Key::Other(0xDB | 0xDD), m) if m.is_empty() || m == Modifiers::SHIFT => {
                let notches = if m.shift { 2 } else { 1 };
                let steps = if k.vk == 0xDB { -notches } else { notches };
                if !self.step_width(cx, steps) {
                    return false;
                }
            }
            (Key::Space, m) if m.is_empty() => {
                if !self.space_held {
                    self.space_held = true;
                    cx.set_cursor(Cursor::Move);
                }
            }
            (Key::Char(c), m) if m.is_empty() && !k.repeat => match Tool::from_key(c) {
                Some(tool) if self.crop.is_none() => self.set_tool(cx, tool),
                _ => return false,
            },
            _ => return false,
        }
        cx.request_paint();
        true
    }

    fn escape(&mut self, cx: &mut Ctx) {
        if !matches!(self.gesture, Gesture::None) {
            self.cancel_gesture(cx);
        } else if self.crop.is_some() {
            self.cancel_crop();
        } else if !matches!(self.ocr, OcrState::Off) {
            self.exit_ocr();
        } else {
            self.select(None);
        }
    }

    fn cancel_gesture(&mut self, cx: &mut Ctx) {
        if let Some(before) = self.history.cancel() {
            self.doc = before;
        }
        self.gesture = Gesture::None;
        self.gesture_pointer = None;
        cx.request_paint();
    }

    fn text_key(&mut self, cx: &mut Ctx, k: &KeyEvent) -> bool {
        use crate::text_edit as te;
        let Some(editing) = &mut self.editing else { return false };
        let Some(Body::Text(note)) = self.doc.get_mut(editing.id).map(|a| &mut a.body) else { return false };
        let caret = &mut editing.caret;
        match k.key {
            Key::Escape => {
                self.finish_text(cx);
                return true;
            }
            Key::Enter => te::insert(&mut note.text, caret, "\n"),
            Key::Backspace => te::backspace(&mut note.text, caret),
            Key::Delete => te::delete(&mut note.text, *caret),
            Key::Left => *caret = te::prev_boundary(&note.text, *caret),
            Key::Right => *caret = te::next_boundary(&note.text, *caret),
            Key::Home => *caret = te::line_start(&note.text, *caret),
            Key::End => *caret = te::line_end(&note.text, *caret),
            Key::Up => *caret = te::vertical(&note.text, *caret, false),
            Key::Down => *caret = te::vertical(&note.text, *caret, true),
            _ if k.mods.alt => return false,
            Key::Char(_) | Key::Space | Key::Plus | Key::Minus | Key::Tab if !k.mods.ctrl => {}
            _ if k.mods.ctrl => {
                self.finish_text(cx);
                return false;
            }
            _ => {}
        }
        self.restart_caret(cx);
        cx.request_paint();
        true
    }

    fn type_text(&mut self, cx: &mut Ctx, text: &str) {
        let Some(editing) = &mut self.editing else { return };
        if let Some(Body::Text(note)) = self.doc.get_mut(editing.id).map(|a| &mut a.body) {
            crate::text_edit::insert(&mut note.text, &mut editing.caret, text);
        }
        self.restart_caret(cx);
        cx.request_paint();
    }

    fn restart_caret(&mut self, cx: &mut Ctx) {
        self.caret_on = true;
        self.set_view_timer(cx, TIMER_CARET, 530);
    }

    pub(crate) fn finish_text(&mut self, cx: &mut Ctx) {
        let Some(editing) = self.editing.take() else { return };
        let empty = matches!(self.doc.get(editing.id).map(|a| &a.body), Some(Body::Text(t)) if t.text.trim().is_empty());
        if empty {
            self.doc.remove(editing.id);
            self.select(None);
        }
        self.cancel_view_timer(cx, TIMER_CARET);
        if self.history.commit(&self.doc) {
            self.after_change(cx);
        }
        cx.request_paint();
    }

    fn set_view_timer(&mut self, cx: &mut Ctx, token: u64, millis: u64) {
        self.cancel_view_timer(cx, token);
        let id = cx.set_timer(std::time::Duration::from_millis(millis), token);
        if let Some(slot) = self.timers.get_mut(token as usize) {
            *slot = Some(id);
        }
    }

    /// Cancels the timer; returns whether one was pending.
    fn cancel_view_timer(&mut self, cx: &mut Ctx, token: u64) -> bool {
        match self.timers.get_mut(token as usize).and_then(Option::take) {
            Some(id) => {
                cx.cancel_timer(id);
                true
            }
            None => false,
        }
    }

    fn on_timer(&mut self, cx: &mut Ctx, token: u64) {
        if let Some(slot) = self.timers.get_mut(token as usize) {
            *slot = None;
        }
        match token {
            TIMER_CARET if self.editing.is_some() => {
                self.caret_on = !self.caret_on;
                self.set_view_timer(cx, TIMER_CARET, 530);
                cx.request_paint();
            }
            TIMER_TOAST => {
                self.chrome.hide_toast();
                cx.request_paint();
            }
            TIMER_AUTOCOPY => self.autocopy(cx),
            TIMER_PERSIST => tools::persist(),
            _ => {}
        }
    }

    pub(crate) fn toast(&mut self, cx: &mut Ctx, text: &str, kind: chrome::ToastKind) {
        self.chrome.show_toast(text, kind);
        let millis = if kind == chrome::ToastKind::Error { 4000 } else { 2200 };
        self.set_view_timer(cx, TIMER_TOAST, millis);
        cx.request_paint();
    }
}

/// A stroke setting as a tool default (`kind` picks which caps a cap change edits).
fn apply_stroke_to_options(o: &mut ToolOptions, change: StrokeChange, kind: ShapeKind, image_scale: f32) {
    match change {
        StrokeChange::Width(px) => o.width = px / image_scale,
        StrokeChange::Opacity(v) => o.opacity = v,
        StrokeChange::Dash(d) => o.dash = d,
        StrokeChange::Pressure(on) => o.pressure = on,
        StrokeChange::Smoothing(v) => o.smoothing = v,
        StrokeChange::Cap(end, cap) => {
            let mut caps = o.caps(kind);
            caps[end.min(1)] = cap;
            o.set_caps(kind, caps);
        }
        StrokeChange::HeadScale(v) => o.head_scale = v,
        StrokeChange::CornerRadius(px) => o.corner_radius = px / image_scale,
        StrokeChange::FillOpacity(v) => o.fill_opacity = v,
    }
    *o = o.sanitized();
}

/// A stroke setting applied to an object (settings that do not apply to it are ignored).
fn apply_stroke_to_annotation(a: &mut Annotation, change: StrokeChange) {
    match (&mut a.body, change) {
        (Body::Stroke(s), StrokeChange::Width(px)) => s.width = px,
        (Body::Shape(s), StrokeChange::Width(px)) => s.width = px,
        (Body::Stroke(s), StrokeChange::Opacity(v)) => s.opacity = v,
        (Body::Shape(s), StrokeChange::Opacity(v)) => s.opacity = v,
        (Body::Stroke(s), StrokeChange::Dash(d)) if s.kind == StrokeKind::Pen => s.dash = d,
        (Body::Shape(s), StrokeChange::Dash(d)) => s.dash = d,
        (Body::Stroke(s), StrokeChange::Pressure(on)) => s.pressure = on,
        (Body::Stroke(s), StrokeChange::Smoothing(v)) => s.smoothing = v,
        (Body::Shape(s), StrokeChange::Cap(end, cap)) => s.caps[end.min(1)] = cap,
        (Body::Shape(s), StrokeChange::HeadScale(v)) => s.head_scale = v,
        (Body::Shape(s), StrokeChange::CornerRadius(px)) => s.corner_radius = px,
        (Body::Shape(s), StrokeChange::FillOpacity(v)) => s.fill_opacity = v,
        _ => {}
    }
}

fn nudge_step(mods: Modifiers) -> f32 {
    if mods.shift { 10.0 } else { 1.0 }
}

/// Writes the options pill's settings into an annotation (color, width, kind, fill, text size, redaction kind).
fn apply_options(a: &mut Annotation, o: &ToolOptions, image_scale: f32) {
    match &mut a.body {
        Body::Stroke(s) => {
            s.color = o.color;
            s.width = o.width * image_scale;
        }
        Body::Shape(s) => {
            if s.kind != o.shape {
                s.caps = o.caps(o.shape);
            }
            s.color = o.color;
            s.width = o.width * image_scale;
            s.kind = o.shape;
            s.filled = o.filled;
        }
        Body::Text(t) => {
            t.color = o.color;
            t.size = o.text_size_dip() * image_scale;
            t.background = o.filled;
        }
        Body::Redact(r) => r.kind = o.redact,
    }
}

impl View for EditorView {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        self.prepare_cx(cx);
        match event {
            Event::Timer(token) => {
                self.on_timer(cx, *token);
                return true;
            }
            Event::CloseRequested => {
                cx.close();
                return true;
            }
            Event::Closed => {
                self.on_closed(cx);
                return true;
            }
            Event::Shown => {
                self.on_shown(cx);
                return true;
            }
            Event::Resized | Event::ScaleChanged | Event::ThemeChanged => {
                cx.request_paint();
                return false;
            }
            Event::Focus(false) => {
                self.space_held = false;
                if matches!(self.gesture, Gesture::Pan { .. }) {
                    self.gesture = Gesture::None;
                    self.gesture_pointer = None;
                }
            }
            Event::PointerCancel => {
                if !matches!(self.gesture, Gesture::None) {
                    self.end_gesture(cx);
                }
            }
            Event::KeyUp(k) if k.key == Key::Space => {
                self.space_held = false;
                cx.set_cursor(Cursor::Arrow);
                return true;
            }
            _ => {}
        }
        let keyboard_first = matches!(event, Event::KeyDown(_)) && !self.chrome.has_popup();
        if keyboard_first && let Event::KeyDown(k) = event && self.key_down(cx, k) {
            return true;
        }
        if matches!(self.gesture, Gesture::None) && !keyboard_first {
            match self.chrome.event(cx, event) {
                Response::Action(action) => {
                    self.apply_chrome_action(cx, action);
                    return true;
                }
                Response::Consumed => {
                    if matches!(event, Event::PointerMove(_)) {
                        cx.set_cursor(Cursor::Arrow);
                    }
                    cx.request_paint();
                    return true;
                }
                Response::Ignored => {
                    if let Event::KeyDown(k) = event
                        && self.key_down(cx, k)
                    {
                        return true;
                    }
                }
            }
        }
        match event {
            Event::Text(text) => {
                if self.editing.is_some() {
                    self.type_text(cx, text);
                    return true;
                }
                false
            }
            Event::Wheel(w) => {
                self.wheel(cx, w);
                true
            }
            Event::PointerDown(_) | Event::PointerMove(_) | Event::PointerUp(_) | Event::PointerLeave => {
                self.pointer(cx, event)
            }
            _ => false,
        }
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.prepare_cx(cx);
        if cx.is_offscreen() {
            paint_mica_stand_in(p, cx.size());
        }
        self.paint_canvas(cx, p);
        self.chrome.paint(p, cx.time());
        if matches!(self.ocr, OcrState::Busy) || self.viewport.is_animating() {
            cx.animate();
        }
    }

    fn hit_test(&self, pos: PointF) -> HitArea {
        if self.chrome.is_caption(pos) { HitArea::Caption } else { HitArea::Client }
    }
}

/// Offscreen previews have no Mica behind them: a soft wallpaper-tinted gradient stands in.
fn paint_mica_stand_in(p: &mut Painter, size: SizeF) {
    let theme = p.theme().clone();
    let (a, b) = if theme.is_dark() { ("#202128", "#1B1D24") } else { ("#EEF0F5", "#F4F1F4") };
    let brush = glint_ui::Brush::linear(
        PointF::new(0.0, 0.0),
        PointF::new(size.w, size.h),
        &[(0.0, Color::hex(a).unwrap_or(Color::BLACK)), (1.0, Color::hex(b).unwrap_or(Color::BLACK))],
    );
    p.fill_rect(RectF::new(0.0, 0.0, size.w, size.h), brush);
    for (i, icon) in [Icon::Minus, Icon::Square, Icon::X].into_iter().enumerate() {
        let center = PointF::new(size.w - 138.0 + 23.0 + i as f32 * 46.0, 16.0);
        let glyph = if icon == Icon::Square { 10.0 } else { 12.0 };
        p.icon_with_stroke(icon, center, glyph, theme.text_secondary, 1.4);
    }
}
