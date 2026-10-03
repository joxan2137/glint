//! The Stroke popover: a live preview drawn by the real renderer, then only the rows that apply to the target
//! (width, opacity, style, pen pressure and smoothing, line caps and arrowhead size, corner radius, fill opacity).

use glint_core::{PointF, RectF, SizeF};
use glint_ui::widgets::{Response, Segment, Segmented, SegmentedStyle, Slider, Toggle};
use glint_ui::{Color, Ctx, Event, LineCap, LineJoin, Painter, TextAlign, TextStyle};

use crate::math::clamp_safe;
use crate::model::{Cap, Dash, InkPoint, Shape, ShapeKind, Stroke, StrokeKind};
use crate::render::{self, GeometryCache};
use crate::tools::{CORNER_RANGE, HEAD_SCALE_RANGE, OPACITY_RANGE, WIDTH_RANGE};

pub const WIDTH: f32 = 264.0;
const PREVIEW_H: f32 = 64.0;
const PREVIEW_GAP: f32 = 14.0;
const ROW_H: f32 = 28.0;
const ROW_GAP: f32 = 8.0;
const LABEL_W: f32 = 84.0;
const VALUE_W: f32 = 46.0;
/// Preview strokes wider than this are drawn scaled down so the sample stays legible.
const PREVIEW_MAX_WIDTH: f32 = 18.0;

/// What the stroke settings apply to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeTarget {
    Pen,
    Highlighter,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
}

impl StrokeTarget {
    pub fn for_shape(kind: ShapeKind) -> Self {
        match kind {
            ShapeKind::Rectangle => StrokeTarget::Rectangle,
            ShapeKind::Ellipse => StrokeTarget::Ellipse,
            ShapeKind::Line => StrokeTarget::Line,
            ShapeKind::Arrow => StrokeTarget::Arrow,
        }
    }
}

/// The stroke as the panel shows it; lengths in image pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeSpec {
    pub target: StrokeTarget,
    pub color: Color,
    pub width: f32,
    pub opacity: f32,
    pub dash: Dash,
    pub pressure: bool,
    pub smoothing: f32,
    pub caps: [Cap; 2],
    pub head_scale: f32,
    pub corner_radius: f32,
    pub filled: bool,
    pub fill_opacity: f32,
    /// Image pixels per DIP at 100 % zoom (the capture's scale): previews show the stroke at its on-screen size.
    pub px_per_dip: f32,
}

impl StrokeSpec {
    /// The width as it appears at 100 % zoom, in DIP.
    pub fn width_dip(&self) -> f32 {
        self.width / self.px_per_dip.max(1e-3)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrokeChange {
    /// Image pixels.
    Width(f32),
    Opacity(f32),
    Dash(Dash),
    Pressure(bool),
    Smoothing(f32),
    /// End 0 = start, 1 = end.
    Cap(usize, Cap),
    HeadScale(f32),
    /// Image pixels.
    CornerRadius(f32),
    FillOpacity(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Width,
    Opacity,
    Style,
    Pressure,
    Smoothing,
    StartCap,
    EndCap,
    HeadSize,
    Corner,
    Fill,
}

impl Row {
    fn label(self) -> &'static str {
        match self {
            Row::Width => "Width",
            Row::Opacity => "Opacity",
            Row::Style => "Style",
            Row::Pressure => "Pressure",
            Row::Smoothing => "Smoothing",
            Row::StartCap => "Start",
            Row::EndCap => "End",
            Row::HeadSize => "Arrowhead",
            Row::Corner => "Corners",
            Row::Fill => "Fill",
        }
    }
}

fn rows_for(spec: &StrokeSpec) -> Vec<Row> {
    let mut rows = vec![Row::Width, Row::Opacity];
    match spec.target {
        StrokeTarget::Highlighter => {}
        StrokeTarget::Pen => rows.extend([Row::Style, Row::Pressure, Row::Smoothing]),
        StrokeTarget::Rectangle => rows.extend([Row::Style, Row::Corner]),
        StrokeTarget::Ellipse => rows.push(Row::Style),
        StrokeTarget::Line | StrokeTarget::Arrow => {
            rows.extend([Row::Style, Row::StartCap, Row::EndCap]);
            if spec.caps.iter().any(|c| c.is_arrow()) {
                rows.push(Row::HeadSize);
            }
        }
    }
    if spec.filled && matches!(spec.target, StrokeTarget::Rectangle | StrokeTarget::Ellipse) {
        rows.push(Row::Fill);
    }
    rows
}

/// Width slider position (0..=1) for a width: logarithmic, so thin strokes get most of the travel.
pub fn width_to_slider(px: f32) -> f32 {
    (clamp_safe(px, WIDTH_RANGE.0, WIDTH_RANGE.1) / WIDTH_RANGE.0).ln() / (WIDTH_RANGE.1 / WIDTH_RANGE.0).ln()
}

/// Width for a slider position: half pixels below 10 px, whole pixels above.
pub fn slider_to_width(t: f32) -> f32 {
    let px = WIDTH_RANGE.0 * (WIDTH_RANGE.1 / WIDTH_RANGE.0).powf(clamp_safe(t, 0.0, 1.0));
    let rounded = if px < 10.0 { (px * 2.0).round() / 2.0 } else { px.round() };
    clamp_safe(rounded, WIDTH_RANGE.0, WIDTH_RANGE.1)
}

/// "6 px", "2.5 px".
pub fn format_px(px: f32) -> String {
    if (px - px.round()).abs() < 0.05 { format!("{:.0} px", px.round()) } else { format!("{px:.1} px") }
}

fn percent(v: f32) -> String {
    format!("{:.0} %", v * 100.0)
}

pub fn content_height(row_count: usize) -> f32 {
    PREVIEW_H + PREVIEW_GAP + row_count as f32 * (ROW_H + ROW_GAP) - ROW_GAP
}

fn blank_segments(names: &[&str]) -> Vec<Segment> {
    names.iter().map(|n| Segment::default().tooltip(n, None)).collect()
}

pub struct StrokePanel {
    spec: Option<StrokeSpec>,
    rows: Vec<Row>,
    width: Slider,
    opacity: Slider,
    smoothing: Slider,
    head: Slider,
    corner: Slider,
    fill: Slider,
    style: Segmented,
    start_cap: Segmented,
    end_cap: Segmented,
    pressure: Toggle,
    dragging: Option<Row>,
    rect: RectF,
    cache: GeometryCache,
}

impl Default for StrokePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl StrokePanel {
    pub fn new() -> Self {
        let caps = ["None", "Arrow", "Filled arrow", "Dot"];
        Self {
            spec: None,
            rows: Vec::new(),
            width: Slider::new(0.5, 0.0, 1.0),
            opacity: Slider::new(100.0, OPACITY_RANGE.0 * 100.0, 100.0).step(1.0),
            smoothing: Slider::new(30.0, 0.0, 100.0).step(1.0),
            head: Slider::new(100.0, HEAD_SCALE_RANGE.0 * 100.0, HEAD_SCALE_RANGE.1 * 100.0).step(5.0),
            corner: Slider::new(0.0, CORNER_RANGE.0, CORNER_RANGE.1).step(1.0),
            fill: Slider::new(100.0, OPACITY_RANGE.0 * 100.0, 100.0).step(1.0),
            style: Segmented::new(blank_segments(&["Solid", "Dashed", "Dotted"]), 0).style(SegmentedStyle::Track),
            start_cap: Segmented::new(blank_segments(&caps), 0).style(SegmentedStyle::Track),
            end_cap: Segmented::new(blank_segments(&caps), 0).style(SegmentedStyle::Track),
            pressure: Toggle::new(true),
            dragging: None,
            rect: RectF::default(),
            cache: GeometryCache::default(),
        }
    }

    pub fn spec(&self) -> Option<StrokeSpec> {
        self.spec
    }

    pub fn content_size(&self) -> SizeF {
        SizeF::new(WIDTH, content_height(self.rows.len()))
    }

    /// Shows `spec`; returns true when the set of rows changed (the popover must be resized).
    pub fn sync(&mut self, spec: StrokeSpec) -> bool {
        let rows = rows_for(&spec);
        let changed = rows != self.rows;
        self.rows = rows;
        self.spec = Some(spec);
        let dragging = self.dragging;
        let set = |row: Row, slider: &mut Slider, value: f32| {
            if dragging != Some(row) {
                slider.set_value(value);
            }
        };
        set(Row::Width, &mut self.width, width_to_slider(spec.width));
        set(Row::Opacity, &mut self.opacity, spec.opacity * 100.0);
        set(Row::Smoothing, &mut self.smoothing, spec.smoothing);
        set(Row::HeadSize, &mut self.head, spec.head_scale * 100.0);
        set(Row::Corner, &mut self.corner, spec.corner_radius);
        set(Row::Fill, &mut self.fill, spec.fill_opacity * 100.0);
        let index = |dash: Dash| Dash::ALL.iter().position(|d| *d == dash).unwrap_or(0);
        self.style.set_selected(index(spec.dash));
        let cap_index = |cap: Cap| Cap::ALL.iter().position(|c| *c == cap).unwrap_or(0);
        self.start_cap.set_selected(cap_index(spec.caps[0]));
        self.end_cap.set_selected(cap_index(spec.caps[1]));
        if self.pressure.is_on() != spec.pressure {
            self.pressure.set_on(spec.pressure);
        }
        changed
    }

    fn row_rect(&self, index: usize) -> RectF {
        let y = self.rect.y + PREVIEW_H + PREVIEW_GAP + index as f32 * (ROW_H + ROW_GAP);
        RectF::new(self.rect.x, y, self.rect.w, ROW_H)
    }

    fn slider_rect(row: RectF) -> RectF {
        RectF::new(row.x + LABEL_W - 4.0, row.center().y - Slider::HEIGHT / 2.0, row.w - LABEL_W - VALUE_W + 4.0, Slider::HEIGHT)
    }

    fn control_rect(row: RectF) -> RectF {
        RectF::new(row.x + LABEL_W, row.y, row.w - LABEL_W, row.h)
    }

    /// Places every row inside the popover's content rect.
    pub fn layout(&mut self, ctx_gfx: &glint_ui::Gfx, content: RectF) {
        self.rect = content;
        for (i, row) in self.rows.clone().into_iter().enumerate() {
            let r = self.row_rect(i);
            match row {
                Row::Width => self.width.set_rect(Self::slider_rect(r)),
                Row::Opacity => self.opacity.set_rect(Self::slider_rect(r)),
                Row::Smoothing => self.smoothing.set_rect(Self::slider_rect(r)),
                Row::HeadSize => self.head.set_rect(Self::slider_rect(r)),
                Row::Corner => self.corner.set_rect(Self::slider_rect(r)),
                Row::Fill => self.fill.set_rect(Self::slider_rect(r)),
                Row::Style => self.style.layout(ctx_gfx, Self::control_rect(r)),
                Row::StartCap => self.start_cap.layout(ctx_gfx, Self::control_rect(r)),
                Row::EndCap => self.end_cap.layout(ctx_gfx, Self::control_rect(r)),
                Row::Pressure => self.pressure.set_origin(PointF::new(r.right() - Toggle::SIZE.w, r.center().y - Toggle::SIZE.h / 2.0)),
            }
        }
    }

    fn slider(&mut self, row: Row) -> Option<&mut Slider> {
        match row {
            Row::Width => Some(&mut self.width),
            Row::Opacity => Some(&mut self.opacity),
            Row::Smoothing => Some(&mut self.smoothing),
            Row::HeadSize => Some(&mut self.head),
            Row::Corner => Some(&mut self.corner),
            Row::Fill => Some(&mut self.fill),
            _ => None,
        }
    }

    fn change(row: Row, value: f32) -> Option<StrokeChange> {
        Some(match row {
            Row::Width => StrokeChange::Width(slider_to_width(value)),
            Row::Opacity => StrokeChange::Opacity(value / 100.0),
            Row::Smoothing => StrokeChange::Smoothing(value),
            Row::HeadSize => StrokeChange::HeadScale(value / 100.0),
            Row::Corner => StrokeChange::CornerRadius(value),
            Row::Fill => StrokeChange::FillOpacity(value / 100.0),
            _ => return None,
        })
    }

    /// `Action((change, done))`: `done` is false while a slider is being dragged (the edit continues).
    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<(StrokeChange, bool)> {
        if matches!(event, Event::PointerCancel | Event::Focus(false))
            && let Some(row) = self.dragging.take()
            && let Some(slider) = self.slider(row)
        {
            let _ = slider.event(cx, event);
            let value = slider.value();
            return Self::change(row, value).map_or(Response::Ignored, |c| Response::Action((c, true)));
        }
        let mut result = Response::Ignored;
        for row in self.rows.clone() {
            let response = match row {
                Row::Style => self.style.event(cx, event).map(|i| (StrokeChange::Dash(Dash::ALL[i.min(2)]), true)),
                Row::StartCap => self.start_cap.event(cx, event).map(|i| (StrokeChange::Cap(0, Cap::ALL[i.min(3)]), true)),
                Row::EndCap => self.end_cap.event(cx, event).map(|i| (StrokeChange::Cap(1, Cap::ALL[i.min(3)]), true)),
                Row::Pressure => self.pressure.event(cx, event).map(|on| (StrokeChange::Pressure(on), true)),
                _ => self.slider_event(row, cx, event),
            };
            match response {
                Response::Action(a) => return Response::Action(a),
                Response::Consumed => result = Response::Consumed,
                Response::Ignored => {}
            }
        }
        result
    }

    fn slider_event(&mut self, row: Row, cx: &mut Ctx, event: &Event) -> Response<(StrokeChange, bool)> {
        let dragging = self.dragging;
        let Some(slider) = self.slider(row) else { return Response::Ignored };
        match slider.event(cx, event) {
            Response::Action(value) => {
                let moving = matches!(event, Event::PointerDown(_) | Event::PointerMove(_));
                self.dragging = moving.then_some(row);
                Self::change(row, value).map_or(Response::Consumed, |c| Response::Action((c, !moving)))
            }
            Response::Consumed => {
                let value = slider.value();
                match event {
                    Event::PointerDown(_) => {
                        self.dragging = Some(row);
                        Response::Consumed
                    }
                    Event::PointerUp(_) if dragging == Some(row) => {
                        self.dragging = None;
                        Self::change(row, value).map_or(Response::Consumed, |c| Response::Action((c, true)))
                    }
                    _ => Response::Consumed,
                }
            }
            Response::Ignored => Response::Ignored,
        }
    }

    pub fn paint(&mut self, p: &mut Painter, content: RectF) {
        let Some(spec) = self.spec else { return };
        self.layout(p.gfx(), content);
        let theme = p.theme().clone();
        self.paint_preview(p, &spec, RectF::new(content.x, content.y, content.w, PREVIEW_H));
        let label = TextStyle::body();
        let value_style = TextStyle::body().tabular().align(TextAlign::Trailing);
        for (i, row) in self.rows.clone().into_iter().enumerate() {
            let r = self.row_rect(i);
            p.text(row.label(), &label, theme.text, RectF::new(r.x, r.y, LABEL_W - 8.0, r.h));
            let value_rect = RectF::new(r.right() - VALUE_W, r.y, VALUE_W, r.h);
            let value = match row {
                Row::Width => Some(format_px(slider_to_width(self.width.value()))),
                Row::Opacity => Some(percent(self.opacity.value() / 100.0)),
                Row::Smoothing => Some(format!("{:.0}", self.smoothing.value())),
                Row::HeadSize => Some(percent(self.head.value() / 100.0)),
                Row::Corner => Some(format_px(self.corner.value())),
                Row::Fill => Some(percent(self.fill.value() / 100.0)),
                _ => None,
            };
            if let Some(value) = value {
                p.text(&value, &value_style, theme.text_secondary, value_rect);
            }
            match row {
                Row::Style => {
                    self.style.paint(p);
                    self.paint_dash_glyphs(p);
                }
                Row::StartCap | Row::EndCap => {
                    let end = usize::from(row == Row::EndCap);
                    let control = if end == 0 { &mut self.start_cap } else { &mut self.end_cap };
                    control.paint(p);
                    let selected = control.selected();
                    let cells: Vec<RectF> = (0..4).filter_map(|i| control.item_rect(i)).collect();
                    for (i, cell) in cells.into_iter().enumerate() {
                        let color = if i == selected { theme.text } else { theme.text_secondary };
                        paint_cap_glyph(p, cell, Cap::ALL[i], end, color);
                    }
                }
                Row::Pressure => self.pressure.paint(p),
                _ => {
                    if let Some(slider) = self.slider(row) {
                        slider.paint(p);
                    }
                }
            }
        }
    }

    fn paint_dash_glyphs(&self, p: &mut Painter) {
        let theme = p.theme().clone();
        for (i, dash) in Dash::ALL.iter().enumerate() {
            let Some(cell) = self.style.item_rect(i) else { continue };
            let color = if i == self.style.selected() { theme.text } else { theme.text_secondary };
            let y = p.snap(cell.center().y) + 0.5 * p.px();
            let style = render::line_style(*dash, LineCap::Round, LineJoin::Round);
            p.line(PointF::new(cell.x + 16.0, y), PointF::new(cell.right() - 16.0, y), color, 2.0, &style);
        }
    }

    fn paint_preview(&self, p: &mut Painter, spec: &StrokeSpec, rect: RectF) {
        let theme = p.theme().clone();
        let light_ink = spec.color.luminance() > 0.7;
        let paper = if light_ink { Color::rgb8(0x3A, 0x3A, 0x3C) } else { Color::WHITE };
        p.fill_round_rect(rect, 8.0, paper);
        let edge = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.10) } else { Color::rgba(0.0, 0.0, 0.0, 0.08) };
        p.hairline_round_rect(rect, 8.0, edge, true);
        let inner = RectF::new(rect.x + 24.0, rect.y + 14.0, rect.w - 48.0, rect.h - 28.0);
        let width = preview_width(spec, inner.h * 0.5 + 6.0);
        let corner_radius = spec.corner_radius / spec.px_per_dip.max(1e-3);
        let cache = &self.cache;
        p.clip_round_rect(rect, 8.0, |p| {
            if spec.target == StrokeTarget::Highlighter {
                let bar = if light_ink { Color::rgba(1.0, 1.0, 1.0, 0.35) } else { Color::rgba(0.0, 0.0, 0.0, 0.22) };
                p.fill_round_rect(RectF::new(inner.x, rect.center().y - 3.0, inner.w * 0.85, 6.0), 3.0, bar);
                p.fill_round_rect(RectF::new(inner.x, rect.center().y + 9.0, inner.w * 0.55, 6.0), 3.0, bar);
                p.fill_round_rect(RectF::new(inner.x, rect.center().y - 15.0, inner.w * 0.7, 6.0), 3.0, bar);
            }
            p.layer(spec.opacity, |p| match spec.target {
                StrokeTarget::Pen | StrokeTarget::Highlighter => {
                    let pen = spec.target == StrokeTarget::Pen;
                    let samples = 48;
                    let points: Vec<InkPoint> = (0..=samples)
                        .map(|i| {
                            let t = i as f32 / samples as f32;
                            let y = if pen { rect.center().y + (t * std::f32::consts::TAU).sin() * inner.h * 0.32 } else { rect.center().y };
                            let pressure = if spec.pressure { 0.2 + 0.8 * (std::f32::consts::PI * t).sin().max(0.0) } else { 0.5 };
                            InkPoint { pos: PointF::new(inner.x + inner.w * t, y), pressure }
                        })
                        .collect();
                    let kind = if pen { StrokeKind::Pen } else { StrokeKind::Highlighter };
                    let mut stroke = Stroke::new(kind, points, spec.color, width);
                    stroke.pressure = pen && spec.pressure;
                    stroke.dash = if pen { spec.dash } else { Dash::Solid };
                    stroke.smoothing = spec.smoothing;
                    render::paint_stroke(p, u64::MAX, &stroke, cache);
                }
                StrokeTarget::Rectangle | StrokeTarget::Ellipse => {
                    let kind = if spec.target == StrokeTarget::Rectangle { ShapeKind::Rectangle } else { ShapeKind::Ellipse };
                    let box_w = inner.w * 0.62;
                    let r = RectF::new(rect.center().x - box_w / 2.0, inner.y, box_w, inner.h);
                    let mut shape = Shape::new(kind, PointF::new(r.x, r.y), PointF::new(r.right(), r.bottom()), spec.color, width);
                    shape.dash = spec.dash;
                    shape.filled = spec.filled;
                    shape.fill_opacity = spec.fill_opacity;
                    shape.corner_radius = corner_radius;
                    render::paint_shape(p, &shape);
                }
                StrokeTarget::Line | StrokeTarget::Arrow => {
                    let y = rect.center().y;
                    let kind = if spec.target == StrokeTarget::Line { ShapeKind::Line } else { ShapeKind::Arrow };
                    let mut shape = Shape::new(kind, PointF::new(inner.x, y), PointF::new(inner.right(), y), spec.color, width);
                    shape.dash = spec.dash;
                    shape.caps = spec.caps;
                    shape.head_scale = spec.head_scale;
                    render::paint_shape(p, &shape);
                }
            });
        });
    }
}

/// The preview's stroke width (DIP): the on-screen width, scaled down only as far as needed for the stroke and
/// its caps to fit `half_height` either side of the center line.
fn preview_width(spec: &StrokeSpec, half_height: f32) -> f32 {
    let mut width = spec.width_dip().min(PREVIEW_MAX_WIDTH);
    let scale = spec.head_scale.max(0.1);
    if matches!(spec.target, StrokeTarget::Line | StrokeTarget::Arrow) {
        if spec.caps.iter().any(|c| c.is_arrow()) {
            width = width.min(((half_height / (0.62 * scale)) - 6.0) / 3.4);
        }
        if spec.caps.contains(&Cap::Dot) {
            width = width.min((half_height / scale - 1.5) / 1.4);
        }
    }
    width.max(1.0)
}

/// A short line with `cap` at its start (`end` 0) or end (`end` 1), drawn by the real line renderer.
fn paint_cap_glyph(p: &mut Painter, cell: RectF, cap: Cap, end: usize, color: Color) {
    let y = cell.center().y;
    let (left, right) = (PointF::new(cell.x + 10.0, y), PointF::new(cell.right() - 10.0, y));
    let mut shape = Shape::new(ShapeKind::Line, left, right, color, 1.75);
    shape.caps = if end == 0 { [cap, Cap::None] } else { [Cap::None, cap] };
    shape.head_scale = 0.8;
    if cap == Cap::None && end == 0 {
        shape.start = PointF::new(left.x + 2.0, y);
    }
    render::paint_shape(p, &shape);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(target: StrokeTarget) -> StrokeSpec {
        StrokeSpec {
            target,
            color: Color::BLACK,
            width: 4.0,
            opacity: 1.0,
            dash: Dash::Solid,
            pressure: true,
            smoothing: 30.0,
            caps: [Cap::None, Cap::None],
            head_scale: 1.0,
            corner_radius: 0.0,
            filled: false,
            fill_opacity: 1.0,
            px_per_dip: 1.0,
        }
    }

    #[test]
    fn previews_show_on_screen_widths_and_keep_caps_inside() {
        let mut pen = spec(StrokeTarget::Pen);
        pen.width = 8.0;
        pen.px_per_dip = 2.0;
        assert_eq!(preview_width(&pen, 24.0), 4.0, "a 2x capture's 8 px stroke shows 4 DIP wide");
        pen.width = 120.0;
        assert_eq!(preview_width(&pen, 24.0), PREVIEW_MAX_WIDTH);
        let mut arrow = spec(StrokeTarget::Arrow);
        arrow.caps = [Cap::Dot, Cap::FilledArrow];
        arrow.width = 40.0;
        arrow.head_scale = 2.0;
        let w = preview_width(&arrow, 24.0);
        let g = crate::ink::line_geometry(PointF::new(0.0, 0.0), PointF::new(200.0, 0.0), w, arrow.caps, arrow.head_scale);
        assert!(g.points().iter().all(|p| p.y.abs() <= 24.0 + 1e-3), "caps fit the preview at width {w}");
    }

    #[test]
    fn rows_follow_the_target() {
        assert_eq!(rows_for(&spec(StrokeTarget::Highlighter)), vec![Row::Width, Row::Opacity]);
        assert!(rows_for(&spec(StrokeTarget::Pen)).contains(&Row::Smoothing));
        let mut line = spec(StrokeTarget::Line);
        assert!(!rows_for(&line).contains(&Row::HeadSize), "no arrowhead size without an arrow cap");
        line.caps[1] = Cap::Arrow;
        assert!(rows_for(&line).contains(&Row::HeadSize));
        let mut rect = spec(StrokeTarget::Rectangle);
        assert!(rows_for(&rect).contains(&Row::Corner) && !rows_for(&rect).contains(&Row::Fill));
        rect.filled = true;
        assert!(rows_for(&rect).contains(&Row::Fill));
    }

    #[test]
    fn width_slider_is_logarithmic_and_round_trips() {
        assert_eq!(width_to_slider(1.0), 0.0);
        assert!((width_to_slider(64.0) - 1.0).abs() < 1e-6);
        assert!((width_to_slider(8.0) - 0.5).abs() < 1e-6, "8 px sits mid-travel");
        for px in [1.0, 1.5, 2.5, 4.0, 6.0, 12.0, 33.0, 64.0] {
            assert_eq!(slider_to_width(width_to_slider(px)), px);
        }
        assert_eq!(slider_to_width(-3.0), 1.0);
        assert_eq!(slider_to_width(9.0), 64.0);
    }

    #[test]
    fn values_read_naturally() {
        assert_eq!(format_px(6.0), "6 px");
        assert_eq!(format_px(2.5), "2.5 px");
        assert_eq!(percent(0.4), "40 %");
        assert_eq!(content_height(2), PREVIEW_H + PREVIEW_GAP + 2.0 * ROW_H + ROW_GAP);
    }
}
