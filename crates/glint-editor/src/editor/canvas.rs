//! Canvas input (tools, gestures, zoom and pan) and canvas painting (image, annotations, selection, crop, OCR).

use std::rc::Rc;

use glint_core::{Image, PointF, RectF};
use glint_ui::{
    Color, Ctx, Cursor, Event, Gfx, Interpolation, Matrix3x2, MouseButton, Painter, PathBuilder, PointerEvent,
    PointerKind, Shadow, WheelEvent,
};

use super::{CropSession, EditorView, Gesture, OcrState, TextEditing};
use crate::chrome::{TITLE_BAR, ToastKind};
use crate::crop::{self, CropHandle};
use crate::hit::{self, Handle};
use crate::ink;
use crate::math::{Vec2, outset, snap_angle, square_up};
use crate::model::{Annotation, Body, InkPoint, Redaction, Shape, Stroke, StrokeKind, TextNote};
use crate::render::{self, Layer, Scene};
use crate::text_edit;
use crate::tools::Tool;
use crate::viewport::Mapping;

const ERASER_RADIUS: f32 = 10.0;
const HANDLE_REACH: f32 = 9.0;
const HIT_TOLERANCE: f32 = 4.0;
const MIN_DRAG: f32 = 4.0;
const SELECTION_MARGIN: f32 = 4.0;
const WHEEL_STEP: f32 = 48.0;

impl EditorView {
    /// The current mapping, origin snapped to physical pixels when at rest so 100 % is pixel exact.
    pub(super) fn mapping(&self) -> Mapping {
        let m = self.viewport.mapping();
        if self.viewport.is_animating() {
            return m;
        }
        let snap = |v: f32| (v * self.scale).round() / self.scale;
        Mapping { scale: m.scale, origin: PointF::new(snap(m.origin.x), snap(m.origin.y)) }
    }

    fn to_image(&self, pos: PointF) -> PointF {
        self.mapping().to_image(pos)
    }

    /// One DIP in image pixels.
    fn dip(&self) -> f32 {
        1.0 / self.mapping().scale
    }

    pub(super) fn wheel(&mut self, cx: &mut Ctx, w: &WheelEvent) {
        if w.mods.ctrl {
            self.zoom_by(1.2f32.powf(w.delta.y), w.pos);
        } else {
            let (dx, dy) = if w.mods.shift && w.delta.x == 0.0 { (w.delta.y, 0.0) } else { (w.delta.x, w.delta.y) };
            let before = self.viewport.target();
            self.viewport.pan(PointF::new(-dx * WHEEL_STEP, dy * WHEEL_STEP), self.content(), self.fit_area());
            if self.viewport.target() != before {
                self.viewport.fit = false;
            }
        }
        cx.request_paint();
    }

    pub(super) fn pointer(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        match event {
            Event::PointerDown(e) => {
                if !matches!(self.gesture, Gesture::None) {
                    return true;
                }
                let handled = self.pointer_down(cx, e);
                if !matches!(self.gesture, Gesture::None) {
                    self.gesture_pointer = Some((e.id, e.button));
                }
                handled
            }
            Event::PointerMove(e) => {
                if matches!(self.gesture, Gesture::None) {
                    self.update_hover(cx, e.pos);
                    false
                } else {
                    if self.owns(e) {
                        self.drag(cx, e);
                    }
                    true
                }
            }
            Event::PointerUp(e) => {
                if matches!(self.gesture, Gesture::None) {
                    return false;
                }
                let other_button = self.gesture_pointer.is_some_and(|(_, b)| b.is_some() && e.button.is_some() && b != e.button);
                if !self.owns(e) || other_button {
                    return true;
                }
                self.drag(cx, e);
                self.end_gesture(cx);
                self.update_hover(cx, e.pos);
                true
            }
            Event::PointerLeave => {
                self.hover = None;
                self.erase_target = None;
                cx.request_paint();
                false
            }
            _ => false,
        }
    }

    /// The selection box (ink bounds plus a margin, image px) and the handles on it; edge handles are dropped on
    /// boxes too small to hold them.
    fn selection_handles(&self, gfx: &Gfx, a: &Annotation) -> (RectF, Vec<(Handle, PointF)>) {
        let m = self.mapping();
        let frame = outset(a.bounds(&render::GfxMeasure(gfx)), SELECTION_MARGIN / m.scale);
        let view = m.rect_to_view(frame);
        let handles = hit::handles(a, frame)
            .into_iter()
            .filter(|(h, _)| match h {
                Handle::N | Handle::S => view.w >= 48.0,
                Handle::E | Handle::W => view.h >= 48.0,
                _ => true,
            })
            .collect();
        (frame, handles)
    }

    /// True for the pointer that started the current gesture.
    fn owns(&self, e: &PointerEvent) -> bool {
        self.gesture_pointer.is_none_or(|(id, _)| id == e.id)
    }

    fn handle_under(&self, cx: &Ctx, pos: PointF) -> Option<(u64, Handle)> {
        if self.editing.is_some() {
            return None;
        }
        let id = self.selection?;
        let a = self.doc.get(id)?;
        let (_, list) = self.selection_handles(cx.gfx(), a);
        hit::handle_at(&list, self.to_image(pos), HANDLE_REACH * self.dip()).map(|h| (id, h))
    }

    fn pointer_down(&mut self, cx: &mut Ctx, e: &PointerEvent) -> bool {
        let pos = e.pos;
        if self.chrome.covers(pos) {
            return false;
        }
        let img = self.to_image(pos);
        if self.space_held || e.button == Some(MouseButton::Middle) {
            self.gesture = Gesture::Pan { last: pos };
            cx.set_cursor(Cursor::Move);
            return true;
        }
        if e.button != Some(MouseButton::Left) {
            return false;
        }
        if self.editing.is_some() {
            if !self.place_caret(cx, img) {
                self.finish_text(cx);
            }
            return true;
        }
        if let Some(session) = &self.crop {
            let frame = self.mapping().rect_to_view(session.rect);
            if let Some(handle) = crop::hit_handle(frame, pos, 18.0, 10.0) {
                self.gesture = Gesture::Crop { handle, start: img, start_rect: session.rect };
            }
            return true;
        }
        let dip = self.dip();
        if !matches!(self.ocr, OcrState::Off) {
            if let OcrState::Ready { text, selection } = &mut self.ocr {
                let anchor = text.word_at(img, 6.0 * dip);
                *selection = anchor.map(|a| (a, a));
                self.gesture = Gesture::OcrSelect { anchor, start: img };
            }
            cx.request_paint();
            return true;
        }
        let tolerance = HIT_TOLERANCE * self.dip();
        let pen_eraser = e.kind == PointerKind::Pen && e.buttons.eraser;
        let measure = self.measure(cx.gfx());
        let tool = if pen_eraser { Tool::Eraser } else { self.tool };
        match tool {
            Tool::Eraser => {
                self.history.begin(&self.doc);
                self.gesture = Gesture::Erase { last: img };
                self.erase_along(cx, img, img);
            }
            Tool::Select => {
                if let Some((id, handle)) = self.handle_under(cx, pos) {
                    self.begin_resize(id, handle, img);
                } else if let Some(id) = hit::topmost(&self.doc, img, tolerance, &measure) {
                    self.select(Some(id));
                    let is_text = matches!(self.doc.get(id).map(|a| &a.body), Some(Body::Text(_)));
                    if e.click_count >= 2 && is_text {
                        self.start_editing(cx, id, None);
                        self.place_caret(cx, img);
                    } else {
                        self.history.begin(&self.doc);
                        self.gesture = Gesture::Move { id, last: img };
                    }
                } else {
                    self.select(None);
                }
            }
            Tool::Pen | Tool::Highlighter => {
                self.select(None);
                self.history.begin(&self.doc);
                let pressure = self.tool == Tool::Pen && e.kind == PointerKind::Pen;
                let kind = if self.tool == Tool::Pen { StrokeKind::Pen } else { StrokeKind::Highlighter };
                self.gesture = Gesture::Ink {
                    raw: vec![InkPoint { pos: img, pressure: e.pressure }],
                    kind,
                    pressure,
                    min_distance: 0.6 * self.dip(),
                };
            }
            Tool::Shapes | Tool::Redact => {
                if let Some((id, handle)) = self.handle_under(cx, pos) {
                    self.begin_resize(id, handle, img);
                } else {
                    self.select(None);
                    self.history.begin(&self.doc);
                    let o = self.options[self.tool.index()];
                    self.gesture = if self.tool == Tool::Shapes {
                        Gesture::Shape {
                            start: img,
                            shape: Shape {
                                kind: o.shape,
                                start: img,
                                end: img,
                                color: o.color,
                                width: o.width_dip(Tool::Shapes) * self.image_scale,
                                filled: o.filled,
                            },
                        }
                    } else {
                        Gesture::Redact { start: img, redaction: Redaction { rect: RectF::new(img.x, img.y, 0.0, 0.0), kind: o.redact } }
                    };
                }
            }
            Tool::Text => {
                let existing = hit::topmost(&self.doc, img, tolerance, &measure)
                    .filter(|id| matches!(self.doc.get(*id).map(|a| &a.body), Some(Body::Text(_))));
                match existing {
                    Some(id) => {
                        self.select(Some(id));
                        self.start_editing(cx, id, None);
                        self.place_caret(cx, img);
                    }
                    None => self.create_text(cx, img),
                }
            }
            Tool::Crop => {}
        }
        self.update_cursor(cx, pos);
        cx.request_paint();
        true
    }

    fn begin_resize(&mut self, id: u64, handle: Handle, img: PointF) {
        if let Some(original) = self.doc.get(id).cloned() {
            self.history.begin(&self.doc);
            self.gesture = Gesture::Resize { id, handle, start: img, original };
        }
    }

    fn create_text(&mut self, cx: &mut Ctx, img: PointF) {
        let o = self.options[Tool::Text.index()];
        let size = o.text_size_dip() * self.image_scale;
        self.history.begin(&self.doc);
        let note = TextNote {
            text: String::new(),
            origin: PointF::new(img.x, img.y - size * 0.66),
            size,
            color: o.color,
            background: o.filled,
        };
        let id = self.doc.add(Body::Text(note));
        self.select(Some(id));
        self.start_editing(cx, id, Some(0));
    }

    /// Starts typing into note `id` (caret at the end unless given).
    pub(crate) fn start_editing(&mut self, cx: &mut Ctx, id: u64, caret: Option<usize>) {
        let len = match self.doc.get(id).map(|a| &a.body) {
            Some(Body::Text(t)) => t.text.len(),
            _ => return,
        };
        self.history.begin(&self.doc);
        self.editing = Some(TextEditing { id, caret: caret.unwrap_or(len).min(len) });
        self.restart_caret(cx);
    }

    /// Moves the caret to `img` when it is inside the note being edited.
    fn place_caret(&mut self, cx: &Ctx, img: PointF) -> bool {
        let Some(editing) = &self.editing else { return false };
        let Some(Body::Text(note)) = self.doc.get(editing.id).map(|a| &a.body) else { return false };
        let measure = self.measure(cx.gfx());
        if !outset(crate::model::text_bounds(note, &measure), 4.0 * self.dip()).contains(img) {
            return false;
        }
        let shown = if note.text.is_empty() { " " } else { note.text.as_str() };
        let Ok(layout) = cx.gfx().text_layout(shown, &render::note_style(note.size), None) else { return true };
        let index = layout.hit_test(img.minus(note.origin));
        let caret = if note.text.is_empty() { 0 } else { text_edit::byte_index(&note.text, index) };
        if let Some(editing) = &mut self.editing {
            editing.caret = caret;
        }
        self.caret_on = true;
        true
    }

    fn drag(&mut self, cx: &mut Ctx, e: &PointerEvent) {
        let pos = e.pos;
        let img = self.to_image(pos);
        let shift = e.mods.shift;
        let gfx = cx.gfx().clone();
        let full = RectF::new(0.0, 0.0, self.image_size().w, self.image_size().h);
        let mapping = self.mapping();
        let (content, area) = (self.content(), self.fit_area());
        let mut erase: Option<(PointF, PointF)> = None;
        match &mut self.gesture {
            Gesture::None => {}
            Gesture::Ink { raw, .. } => {
                let pen = e.kind == PointerKind::Pen;
                for sample in &e.history {
                    raw.push(InkPoint { pos: mapping.to_image(sample.pos), pressure: if pen { sample.pressure } else { 1.0 } });
                }
                raw.push(InkPoint { pos: img, pressure: if pen { e.pressure } else { 1.0 } });
            }
            Gesture::Shape { start, shape } => {
                shape.end = match (shift, shape.kind.is_linear()) {
                    (true, true) => snap_angle(*start, img),
                    (true, false) => square_up(*start, img),
                    _ => img,
                };
            }
            Gesture::Redact { start, redaction } => redaction.rect = RectF::from_points(*start, img),
            Gesture::Move { id, last } => {
                let delta = img.minus(*last);
                *last = img;
                if let Some(a) = self.doc.get_mut(*id) {
                    a.translate(delta);
                }
            }
            Gesture::Resize { id, handle, start, original } => {
                let mut delta = img.minus(*start);
                if shift
                    && let Body::Shape(s) = &original.body
                    && s.kind.is_linear()
                {
                    let (fixed, moving) = if *handle == Handle::Start { (s.end, s.start) } else { (s.start, s.end) };
                    delta = snap_angle(fixed, moving.plus(delta)).minus(moving);
                }
                let resized = hit::resize(original, *handle, delta, &render::GfxMeasure(&gfx));
                if let Some(a) = self.doc.get_mut(*id) {
                    *a = resized;
                }
            }
            Gesture::Erase { last } => {
                erase = Some((*last, img));
                *last = img;
            }
            Gesture::Pan { last } => {
                let delta = pos.minus(*last);
                *last = pos;
                self.viewport.fit = false;
                self.viewport.pan(delta, content, area);
            }
            Gesture::Crop { handle, start, start_rect } => {
                let rect = crop::drag(*start_rect, *handle, img.minus(*start), full);
                if let Some(session) = &mut self.crop {
                    session.rect = rect;
                    session.shown.snap(rect);
                    session.grid.set(1.0);
                }
            }
            Gesture::OcrSelect { anchor, start } => {
                if let OcrState::Ready { text, selection } = &mut self.ocr {
                    *selection = match anchor {
                        Some(a) => text.word_at(img, 12.0 / mapping.scale).map(|b| (*a, b)).or(*selection),
                        None => text.words_in(RectF::from_points(*start, img)),
                    };
                }
            }
        }
        if let Some((from, to)) = erase {
            self.erase_along(cx, from, to);
        }
        cx.request_paint();
    }

    fn erase_along(&mut self, cx: &mut Ctx, from: PointF, to: PointF) {
        let measure = self.measure(cx.gfx());
        let hits = hit::swept(&self.doc, from, to, ERASER_RADIUS * self.dip(), &measure);
        for id in hits {
            self.doc.remove(id);
            if self.selection == Some(id) {
                self.select(None);
            }
        }
        self.erase_target = None;
    }

    /// Finishes the gesture, keeping what was drawn (also when the pointer was lost mid-drag).
    pub(super) fn end_gesture(&mut self, cx: &mut Ctx) {
        let gesture = std::mem::replace(&mut self.gesture, Gesture::None);
        self.gesture_pointer = None;
        let min_drag = MIN_DRAG * self.dip();
        match gesture {
            Gesture::Ink { raw, kind, pressure, min_distance } => {
                let points = ink::finalize(&raw, min_distance);
                if !points.is_empty() {
                    let tool = if kind == StrokeKind::Pen { Tool::Pen } else { Tool::Highlighter };
                    let o = self.options[tool.index()];
                    self.doc.add(Body::Stroke(Stroke {
                        kind,
                        points,
                        color: o.color,
                        width: o.width_dip(tool) * self.image_scale,
                        pressure,
                    }));
                }
            }
            Gesture::Shape { shape, .. } => {
                if shape.start.distance(shape.end) >= min_drag {
                    let id = self.doc.add(Body::Shape(shape));
                    self.select(Some(id));
                }
            }
            Gesture::Redact { redaction, .. } => {
                if redaction.rect.w >= min_drag && redaction.rect.h >= min_drag {
                    let id = self.doc.add(Body::Redact(redaction));
                    self.select(Some(id));
                }
            }
            Gesture::Crop { .. } => {
                if let Some(session) = &mut self.crop {
                    session.grid.set(0.0);
                }
            }
            Gesture::Pan { .. } | Gesture::OcrSelect { .. } | Gesture::None => {}
            Gesture::Move { .. } | Gesture::Resize { .. } | Gesture::Erase { .. } => {}
        }
        if self.history.commit(&self.doc) {
            self.after_change(cx);
        }
        cx.request_paint();
    }

    pub(super) fn update_hover(&mut self, cx: &mut Ctx, pos: PointF) {
        if self.chrome.covers(pos) {
            self.hover = None;
            if self.erase_target.take().is_some() {
                cx.request_paint();
            }
            cx.set_cursor(Cursor::Arrow);
            return;
        }
        self.hover = Some(pos);
        if self.tool == Tool::Eraser && self.crop.is_none() {
            let measure = self.measure(cx.gfx());
            let target = hit::topmost(&self.doc, self.to_image(pos), ERASER_RADIUS * self.dip(), &measure);
            self.erase_target = target;
            cx.request_paint();
        }
        self.update_cursor(cx, pos);
    }

    fn update_cursor(&mut self, cx: &mut Ctx, pos: PointF) {
        let img = self.to_image(pos);
        let cursor = if self.space_held || matches!(self.gesture, Gesture::Pan { .. }) {
            Cursor::Move
        } else if let Some(session) = &self.crop {
            let frame = self.mapping().rect_to_view(session.rect);
            crop::hit_handle(frame, pos, 18.0, 10.0).map_or(Cursor::Arrow, CropHandle::cursor)
        } else if let OcrState::Ready { text, .. } = &self.ocr {
            if text.word_at(img, 2.0 * self.dip()).is_some() { Cursor::IBeam } else { Cursor::Arrow }
        } else if let Some((_, handle)) = self.handle_under(cx, pos) {
            handle.cursor()
        } else {
            match self.tool {
                Tool::Select => {
                    let measure = self.measure(cx.gfx());
                    if hit::topmost(&self.doc, img, HIT_TOLERANCE * self.dip(), &measure).is_some() {
                        Cursor::Move
                    } else {
                        Cursor::Arrow
                    }
                }
                Tool::Text => Cursor::IBeam,
                Tool::Crop => Cursor::Arrow,
                _ => Cursor::Crosshair,
            }
        };
        cx.set_cursor(cursor);
    }

    pub(super) fn enter_crop(&mut self) {
        self.select(None);
        let full = RectF::new(0.0, 0.0, self.image_size().w, self.image_size().h);
        let rect = self.doc.crop.map_or(full, |c| c.to_f());
        self.crop = Some(CropSession {
            rect,
            shown: glint_ui::Animated::new(rect),
            grid: glint_ui::Animated::fade(0.0),
            previous_tool: self.tool,
        });
        self.tool = Tool::Crop;
        self.refit(true);
    }

    pub(super) fn cancel_crop(&mut self) {
        if let Some(session) = self.crop.take() {
            self.tool = session.previous_tool;
            self.refit(true);
        }
    }

    pub(super) fn reset_crop(&mut self) {
        let full = RectF::new(0.0, 0.0, self.image_size().w, self.image_size().h);
        if let Some(session) = &mut self.crop {
            session.rect = full;
            session.shown.set(full);
        }
    }

    pub(super) fn commit_crop(&mut self, cx: &mut Ctx) {
        let Some(session) = self.crop.take() else { return };
        self.tool = session.previous_tool;
        let crop = crop::snap(session.rect, self.image_size());
        if crop != self.doc.crop {
            self.exit_ocr();
            self.history.record(&self.doc);
            self.doc.crop = crop;
            self.after_change(cx);
        }
        self.refit(true);
    }

    pub(super) fn exit_ocr(&mut self) {
        self.ocr = OcrState::Off;
        self.ocr_job += 1;
    }

    /// The canvas scene; None until `prepare_layers` ran.
    pub(super) fn scene(&self) -> Option<Scene<'_>> {
        Some(Scene {
            base: self.base_layer.as_ref()?,
            pixelated: self.pixelated_layer.as_ref(),
            image: (self.base_image.width, self.base_image.height),
            cache: &self.cache,
        })
    }

    pub(super) fn pixelated_image(&mut self) -> Rc<Image> {
        let base = &self.base_image;
        self.pixelated_image.get_or_insert_with(|| Rc::new(render::pixelated_image(base))).clone()
    }

    /// Builds the canvas bitmaps (downscaled for images beyond the GPU limit) and, when needed, the pixelation.
    pub(super) fn prepare_layers(&mut self, gfx: &Gfx, pixelation: bool) {
        let max_side = gfx.max_bitmap_size();
        if self.base_layer.is_none() {
            self.base_layer = Some(Layer::fitted(&self.base_image, max_side));
        }
        if (pixelation || self.doc.has_pixelation()) && self.pixelated_layer.is_none() {
            let pixelated = self.pixelated_image();
            self.pixelated_layer = Some(Layer::fitted(&pixelated, max_side));
        }
    }

    pub(super) fn paint_canvas(&mut self, cx: &mut Ctx, p: &mut Painter) {
        let live_pixelate =
            matches!(&self.gesture, Gesture::Redact { redaction, .. } if redaction.kind == crate::model::RedactKind::Pixelate);
        let gfx = cx.gfx().clone();
        self.prepare_layers(&gfx, live_pixelate);
        let theme = p.theme().clone();
        let m = self.mapping();
        let image_rect = m.rect_to_view(self.content());
        let zoom = m.scale * self.scale;
        let interpolation = if (zoom - 1.0).abs() < 1e-3 || zoom >= 2.0 {
            Interpolation::Nearest
        } else if zoom > 1.0 {
            Interpolation::Linear
        } else {
            Interpolation::Cubic
        };
        let canvas = RectF::from_ltrb(0.0, TITLE_BAR, self.size.w, self.size.h);
        let matrix = Matrix3x2 { M11: m.scale, M12: 0.0, M21: 0.0, M22: m.scale, M31: m.origin.x, M32: m.origin.y };
        let dark = theme.is_dark();
        p.clip_rect(canvas, |p| {
            p.shadow(image_rect, 6.0, &Shadow::new(10.0, 32.0, Color::rgba(0.0, 0.0, 0.0, if dark { 0.42 } else { 0.16 })));
            p.shadow(image_rect, 6.0, &Shadow::new(1.0, 3.0, Color::rgba(0.0, 0.0, 0.0, if dark { 0.30 } else { 0.10 })));
            p.clip_round_rect(image_rect, 6.0, |p| {
                p.with_transform(matrix, |p| {
                    let Some(scene) = self.scene() else { return };
                    render::paint_base(p, &scene, interpolation);
                    render::paint_annotations(p, &self.doc, &scene, self.erase_target);
                    self.paint_live_gesture(p, &scene);
                });
                self.paint_ocr(p, m, cx.time());
            });
            let edge = if dark { Color::rgba(1.0, 1.0, 1.0, 0.10) } else { Color::rgba(0.0, 0.0, 0.0, 0.10) };
            p.hairline_round_rect(image_rect, 6.0, edge, false);
            self.paint_crop(p, m, image_rect);
            self.paint_selection(cx, p, m);
            self.paint_eraser(p);
        });
    }

    fn paint_live_gesture(&self, p: &mut Painter, scene: &Scene) {
        match &self.gesture {
            Gesture::Ink { raw, kind, pressure, min_distance } => {
                let tool = if *kind == StrokeKind::Pen { Tool::Pen } else { Tool::Highlighter };
                let o = self.options[tool.index()];
                let stroke = Stroke {
                    kind: *kind,
                    points: ink::finalize(raw, *min_distance),
                    color: o.color,
                    width: o.width_dip(tool) * self.image_scale,
                    pressure: *pressure,
                };
                render::paint_stroke(p, 0, &stroke, scene);
            }
            Gesture::Shape { shape, .. } => render::paint_shape(p, shape),
            Gesture::Redact { redaction, .. } => render::paint_redaction(p, redaction, scene),
            _ => {}
        }
    }

    fn paint_selection(&self, cx: &mut Ctx, p: &mut Painter, m: Mapping) {
        let theme = p.theme().clone();
        let editing_id = self.editing.as_ref().map(|e| e.id);
        let Some(id) = editing_id.or(self.selection) else { return };
        let Some(a) = self.doc.get(id) else { return };
        let editing = editing_id == Some(id);
        let alpha = if editing { 1.0 } else { self.handles.get().clamp(0.0, 1.0) };
        let accent = theme.accent.multiply_alpha(alpha);
        let (frame, handles) = self.selection_handles(cx.gfx(), a);
        let px = p.px();
        if !matches!(&a.body, Body::Shape(s) if s.kind.is_linear()) {
            let r = p.snap_rect(m.rect_to_view(frame));
            let width = self.scale.round().max(1.0) * px;
            let line = RectF::new(r.x + width * 0.5, r.y + width * 0.5, r.w - width, r.h - width);
            p.stroke_round_rect(line, 3.0, accent, width);
        }
        if !editing {
            for (_, at) in handles {
                let c = p.snap_point(m.to_view(at));
                let r = 4.5;
                let disc = RectF::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r);
                p.shadow(disc, r, &Shadow::new(0.5, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.32 * alpha)));
                p.fill_circle(c, r, Color::WHITE.multiply_alpha(alpha));
                p.stroke_ellipse(c, r - 0.625, r - 0.625, accent, 1.25);
            }
        }
        if let (true, Body::Text(note)) = (editing, &a.body) {
            let shown = if note.text.is_empty() { " " } else { note.text.as_str() };
            let Ok(layout) = cx.gfx().text_layout(shown, &render::note_style(note.size), None) else { return };
            let caret_index = self.editing.as_ref().map_or(0, |e| text_edit::utf16_index(&note.text, e.caret));
            let caret = layout.caret_rect(caret_index);
            let top = m.to_view(PointF::new(note.origin.x + caret.x, note.origin.y + caret.y));
            let height = caret.h * m.scale;
            let width = (note.size * m.scale * 0.07).clamp(1.5, 3.0);
            let caret_rect = RectF::new(p.snap(top.x - width * 0.5), top.y, width, height);
            cx.set_ime_caret(caret_rect);
            if self.caret_on {
                p.fill_round_rect(caret_rect, width * 0.5, theme.accent);
            }
        }
    }

    fn paint_crop(&self, p: &mut Painter, m: Mapping, image_rect: RectF) {
        let Some(session) = &self.crop else { return };
        let frame = p.snap_rect(m.rect_to_view(session.shown.get()));
        let dim = Color::rgba(0.0, 0.0, 0.0, 0.6);
        let i = image_rect;
        p.fill_rect(RectF::from_ltrb(i.x, i.y, i.right(), frame.y), dim);
        p.fill_rect(RectF::from_ltrb(i.x, frame.bottom(), i.right(), i.bottom()), dim);
        p.fill_rect(RectF::from_ltrb(i.x, frame.y, frame.x, frame.bottom()), dim);
        p.fill_rect(RectF::from_ltrb(frame.right(), frame.y, i.right(), frame.bottom()), dim);
        let px = p.px();
        let grid = session.grid.get();
        if grid > 0.001 {
            for (a, b) in crop::thirds(frame) {
                let (a, b) = (p.snap_point(a), p.snap_point(b));
                let line = RectF::from_ltrb(a.x, a.y, b.x.max(a.x + px), b.y.max(a.y + px));
                let shadow = if a.x == b.x { line.offset(px, 0.0) } else { line.offset(0.0, px) };
                p.fill_rect(shadow, Color::rgba(0.0, 0.0, 0.0, 0.22 * grid));
                p.fill_rect(line, Color::rgba(1.0, 1.0, 1.0, 0.75 * grid));
            }
        }
        p.stroke_rect(frame.inset(-px * 0.5), Color::rgba(0.0, 0.0, 0.0, 0.3), px);
        p.stroke_rect(frame.inset(px * 0.5), Color::rgba(1.0, 1.0, 1.0, 0.92), px);
        let thickness = (3.0 * self.scale).round() * px;
        let arm = (frame.w.min(frame.h) * 0.5).clamp(8.0, 24.0);
        let bars = crop_bracket_bars(frame, thickness, arm);
        for bar in &bars {
            p.fill_rect(outset(*bar, px), Color::rgba(0.0, 0.0, 0.0, 0.3));
        }
        for bar in &bars {
            p.fill_rect(*bar, Color::WHITE);
        }
        if grid > 0.001 {
            let size = session.rect;
            let label = format!("{} × {}", size.w.round() as i32, size.h.round() as i32);
            let badge = glint_ui::widgets::Badge::new(&label);
            let below = PointF::new(frame.center().x, frame.bottom() + 26.0);
            let center = if below.y + 14.0 < self.size.h - 70.0 { below } else { PointF::new(frame.center().x, frame.bottom() - 26.0) };
            p.layer(grid, |p| {
                badge.paint_centered(p, center);
            });
        }
    }

    fn paint_ocr(&self, p: &mut Painter, m: Mapping, time: f64) {
        let theme = p.theme().clone();
        let content = m.rect_to_view(self.content());
        match &self.ocr {
            OcrState::Off => {}
            OcrState::Busy => {
                let phase = (time * 0.8).fract() as f32;
                let y = content.y + (content.h + 240.0) * phase - 120.0;
                let upper = RectF::new(content.x, y - 120.0, content.w, 120.0);
                let lower = RectF::new(content.x, y, content.w, 120.0);
                p.fill_rect(upper, glint_ui::Brush::vertical(upper, theme.accent.with_alpha(0.0), theme.accent.with_alpha(0.16)));
                p.fill_rect(lower, glint_ui::Brush::vertical(lower, theme.accent.with_alpha(0.16), theme.accent.with_alpha(0.0)));
            }
            OcrState::Ready { text, selection } => {
                let lines = line_boxes(text, m);
                let mut dim = PathBuilder::new().even_odd();
                dim.polyline(
                    &[
                        PointF::new(content.x, content.y),
                        PointF::new(content.right(), content.y),
                        PointF::new(content.right(), content.bottom()),
                        PointF::new(content.x, content.bottom()),
                    ],
                    true,
                );
                for (_, r) in &lines {
                    dim.round_rect(*r, (r.h * 0.25).min(6.0));
                }
                if let Ok(path) = dim.build(p.gfx()) {
                    p.fill_path(&path, Color::rgba(0.0, 0.0, 0.0, 0.32));
                }
                if let Some((a, b)) = selection {
                    let (a, b) = ((*a).min(*b), (*a).max(*b));
                    for (line, _) in &lines {
                        let span: Vec<RectF> = text
                            .words
                            .iter()
                            .enumerate()
                            .filter(|(i, w)| w.line == *line && *i >= a && *i <= b)
                            .map(|(_, w)| m.rect_to_view(w.rect))
                            .collect();
                        if let Some(r) = span.into_iter().reduce(crate::math::union) {
                            let r = outset(r, 2.0);
                            p.fill_round_rect(r, (r.h * 0.2).min(5.0), theme.accent.with_alpha(0.34));
                        }
                    }
                }
            }
        }
    }

    fn paint_eraser(&self, p: &mut Painter) {
        if self.tool != Tool::Eraser || self.crop.is_some() || !matches!(self.ocr, OcrState::Off) {
            return;
        }
        let Some(pos) = self.hover else { return };
        if pos.y < TITLE_BAR {
            return;
        }
        let c = p.snap_point(pos);
        p.stroke_ellipse(c, ERASER_RADIUS + 0.75, ERASER_RADIUS + 0.75, Color::rgba(0.0, 0.0, 0.0, 0.35), 1.5);
        p.stroke_ellipse(c, ERASER_RADIUS - 0.5, ERASER_RADIUS - 0.5, Color::rgba(1.0, 1.0, 1.0, 0.95), 1.5);
    }

    pub(super) fn notify_copied_text(&mut self, cx: &mut Ctx, lines: usize) {
        let text = if lines == 1 { "Copied 1 line".to_string() } else { format!("Copied {lines} lines") };
        self.toast(cx, &text, ToastKind::Done);
    }
}

/// One rounded box per recognized line (view DIP), keyed by line index.
fn line_boxes(text: &crate::ocr_text::RecognizedText, m: Mapping) -> Vec<(usize, RectF)> {
    let mut boxes: Vec<(usize, RectF)> = Vec::new();
    for word in &text.words {
        let r = m.rect_to_view(word.rect);
        match boxes.last_mut() {
            Some((line, b)) if *line == word.line => *b = crate::math::union(*b, r),
            _ => boxes.push((word.line, r)),
        }
    }
    boxes.into_iter().map(|(line, r)| (line, RectF::new(r.x - 4.0, r.y - 2.0, r.w + 8.0, r.h + 4.0))).collect()
}

/// Apple Photos-style corner brackets (two bars per corner) and edge handles (one bar per edge midpoint), just
/// outside `frame`.
fn crop_bracket_bars(frame: RectF, t: f32, arm: f32) -> [RectF; 12] {
    let (l, top, r, b) = (frame.x, frame.y, frame.right(), frame.bottom());
    let c = frame.center();
    let edge = arm * 0.8;
    [
        RectF::new(c.x - edge / 2.0, top - t, edge, t),
        RectF::new(c.x - edge / 2.0, b, edge, t),
        RectF::new(l - t, c.y - edge / 2.0, t, edge),
        RectF::new(r, c.y - edge / 2.0, t, edge),
        RectF::new(l - t, top - t, arm + t, t),
        RectF::new(l - t, top - t, t, arm + t),
        RectF::new(r - arm, top - t, arm + t, t),
        RectF::new(r, top - t, t, arm + t),
        RectF::new(l - t, b, arm + t, t),
        RectF::new(l - t, b - arm, t, arm + t),
        RectF::new(r - arm, b, arm + t, t),
        RectF::new(r, b - arm, t, arm + t),
    ]
}
