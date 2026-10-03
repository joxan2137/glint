//! Immediate-mode drawing in DIPs over an `ID2D1DeviceContext`.

use std::rc::Rc;

use anyhow::Result;
use glint_core::{PointF, RectF, SizeF};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_FILL_MODE_ALTERNATE, D2D1_GRADIENT_STOP,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1Shadow, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BRUSH_PROPERTIES,
    D2D1_CAP_STYLE, D2D1_CAP_STYLE_FLAT, D2D1_CAP_STYLE_ROUND, D2D1_CAP_STYLE_SQUARE, D2D1_DASH_STYLE_CUSTOM,
    D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_INTERPOLATION_MODE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_LINE_JOIN,
    D2D1_LINE_JOIN_BEVEL, D2D1_LINE_JOIN_MITER, D2D1_LINE_JOIN_ROUND, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_PROPERTY_TYPE_VECTOR4, D2D1_ROUNDED_RECT, D2D1_SHADOW_OPTIMIZATION_QUALITY, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION,
    D2D1_SHADOW_PROP_COLOR, D2D1_SHADOW_PROP_OPTIMIZATION, D2D1_STROKE_STYLE_PROPERTIES1, D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
    ID2D1Bitmap1, ID2D1Brush, ID2D1DeviceContext, ID2D1Geometry, ID2D1Image, ID2D1StrokeStyle, ID2D1StrokeStyle1,
};
use windows::core::Interface;
use windows_numerics::{Matrix3x2, Vector2};

use crate::bitmap::{Bitmap, render_to_bitmap, set_enum, set_float, set_property};
use crate::color::Color;
use crate::gfx::Gfx;
use crate::icons::{ICON_GRID, ICON_STROKE, Icon, build_geometry, icon_geometry};
use crate::svg::{self, Segment};
use crate::text::{TextLayout, TextStyle};
use crate::theme::{Shadow, Theme};

#[derive(Clone, Debug, PartialEq)]
pub enum Brush {
    Solid(Color),
    /// Linear gradient between two points with `(offset, color)` stops.
    Linear { start: PointF, end: PointF, stops: Vec<(f32, Color)> },
}

impl Brush {
    pub fn linear(start: PointF, end: PointF, stops: &[(f32, Color)]) -> Self {
        Brush::Linear { start, end, stops: stops.to_vec() }
    }

    pub fn vertical(rect: RectF, top: Color, bottom: Color) -> Self {
        Self::linear(PointF::new(rect.x, rect.y), PointF::new(rect.x, rect.bottom()), &[(0.0, top), (1.0, bottom)])
    }
}

impl From<Color> for Brush {
    fn from(color: Color) -> Self {
        Brush::Solid(color)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineCap {
    #[default]
    Flat,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrokeStyle {
    pub cap: LineCap,
    pub join: LineJoin,
    /// Dash and gap lengths in multiples of the stroke width; empty = solid.
    pub dashes: Vec<f32>,
    pub dash_offset: f32,
}

impl StrokeStyle {
    pub fn round() -> Self {
        Self { cap: LineCap::Round, join: LineJoin::Round, ..Self::default() }
    }

    pub fn dashed(dashes: &[f32]) -> Self {
        Self { dashes: dashes.to_vec(), ..Self::default() }
    }

    fn key(&self) -> StrokeKey {
        StrokeKey {
            cap: self.cap,
            join: self.join,
            dashes: self.dashes.iter().map(|d| d.to_bits()).collect(),
            offset: self.dash_offset.to_bits(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StrokeKey {
    cap: LineCap,
    join: LineJoin,
    dashes: Vec<u32>,
    offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ShadowKey {
    width: u32,
    height: u32,
    radius_quarter_px: u32,
    sigma_quarter_px: u32,
    color: [u8; 4],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interpolation {
    /// Exact pixels (magnifier, 1:1 screenshots).
    Nearest,
    #[default]
    Linear,
    /// High-quality cubic for downscaled photos and thumbnails.
    Cubic,
}

impl Interpolation {
    fn d2d(self) -> D2D1_INTERPOLATION_MODE {
        match self {
            Interpolation::Nearest => D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            Interpolation::Linear => D2D1_INTERPOLATION_MODE_LINEAR,
            Interpolation::Cubic => D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
        }
    }
}

/// A blurred backdrop for `Painter::glass`: `bitmap` (usually `Bitmap::glass_backdrop`) is the blurred version of
/// whatever is drawn at `dest` in the current coordinate space. `dim` is composited over the blur first, so glass on
/// a dimmed overlay matches its surroundings without a separate dimmed bitmap.
#[derive(Clone, Copy)]
pub struct Backdrop<'a> {
    pub bitmap: &'a Bitmap,
    pub dest: RectF,
    pub dim: Color,
}

impl<'a> Backdrop<'a> {
    pub fn new(bitmap: &'a Bitmap, dest: RectF) -> Self {
        Self { bitmap, dest, dim: Color::TRANSPARENT }
    }

    pub fn dimmed(mut self, dim: Color) -> Self {
        self.dim = dim;
        self
    }
}

/// Device-independent path geometry; build once and draw every frame.
#[derive(Clone)]
pub struct Path {
    pub(crate) geometry: ID2D1Geometry,
}

impl Path {
    pub fn raw(&self) -> &ID2D1Geometry {
        &self.geometry
    }

    pub fn from_svg(gfx: &Gfx, data: &str) -> Result<Path> {
        PathBuilder::from_segments(svg::parse_path(data)?).build(gfx)
    }

    /// Bounds in path coordinates.
    pub fn bounds(&self) -> RectF {
        // SAFETY: plain query with no world transform.
        let r = unsafe { self.geometry.GetBounds(None) }.unwrap_or_default();
        RectF::from_ltrb(r.left, r.top, r.right, r.bottom)
    }

    /// True if `point` is inside the filled path.
    pub fn contains(&self, point: PointF) -> bool {
        // SAFETY: plain hit test query.
        unsafe { self.geometry.FillContainsPoint(vec2(point), None, 0.25) }.map(|b| b.as_bool()).unwrap_or(false)
    }
}

#[derive(Clone, Debug, Default)]
pub struct PathBuilder {
    segments: Vec<Segment>,
    current: PointF,
    start: PointF,
    even_odd: bool,
}

impl PathBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_segments(segments: Vec<Segment>) -> Self {
        Self { segments, ..Self::default() }
    }

    pub fn even_odd(mut self) -> Self {
        self.even_odd = true;
        self
    }

    pub fn move_to(&mut self, p: PointF) -> &mut Self {
        self.segments.push(Segment::MoveTo(p));
        self.current = p;
        self.start = p;
        self
    }

    pub fn line_to(&mut self, p: PointF) -> &mut Self {
        self.segments.push(Segment::LineTo(p));
        self.current = p;
        self
    }

    pub fn cubic_to(&mut self, c1: PointF, c2: PointF, p: PointF) -> &mut Self {
        self.segments.push(Segment::CubicTo(c1, c2, p));
        self.current = p;
        self
    }

    pub fn quad_to(&mut self, control: PointF, p: PointF) -> &mut Self {
        let s = self.current;
        let lerp = |a: PointF, b: PointF| PointF::new(a.x + (b.x - a.x) * 2.0 / 3.0, a.y + (b.y - a.y) * 2.0 / 3.0);
        self.cubic_to(lerp(s, control), lerp(p, control), p)
    }

    /// SVG-style elliptical arc to `p`.
    pub fn arc_to(&mut self, rx: f32, ry: f32, rotation_deg: f32, large_arc: bool, sweep: bool, p: PointF) -> &mut Self {
        svg::arc_to_cubics(self.current, rx, ry, rotation_deg, large_arc, sweep, p, &mut self.segments);
        self.current = p;
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.segments.push(Segment::Close);
        self.current = self.start;
        self
    }

    pub fn polyline(&mut self, points: &[PointF], closed: bool) -> &mut Self {
        for (i, p) in points.iter().enumerate() {
            if i == 0 {
                self.move_to(*p);
            } else {
                self.line_to(*p);
            }
        }
        if closed && !points.is_empty() {
            self.close();
        }
        self
    }

    pub fn round_rect(&mut self, r: RectF, radius: f32) -> &mut Self {
        let radius = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        self.move_to(PointF::new(r.x + radius, r.y));
        self.line_to(PointF::new(r.right() - radius, r.y));
        self.arc_to(radius, radius, 0.0, false, true, PointF::new(r.right(), r.y + radius));
        self.line_to(PointF::new(r.right(), r.bottom() - radius));
        self.arc_to(radius, radius, 0.0, false, true, PointF::new(r.right() - radius, r.bottom()));
        self.line_to(PointF::new(r.x + radius, r.bottom()));
        self.arc_to(radius, radius, 0.0, false, true, PointF::new(r.x, r.bottom() - radius));
        self.line_to(PointF::new(r.x, r.y + radius));
        self.arc_to(radius, radius, 0.0, false, true, PointF::new(r.x + radius, r.y));
        self.close()
    }

    pub fn ellipse(&mut self, center: PointF, rx: f32, ry: f32) -> &mut Self {
        self.move_to(PointF::new(center.x + rx, center.y));
        self.arc_to(rx, ry, 0.0, false, true, PointF::new(center.x - rx, center.y));
        self.arc_to(rx, ry, 0.0, false, true, PointF::new(center.x + rx, center.y));
        self.close()
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn build(&self, gfx: &Gfx) -> Result<Path> {
        let geometry = build_geometry(gfx.factory(), [self.segments.as_slice()], true, self.even_odd)?;
        Ok(Path { geometry: geometry.cast()? })
    }
}

/// Filled glyphs read larger than line icons of the same box; shrink them so a row of mixed icons looks even.
const SOLID_ICON_OPTICAL_SCALE: f32 = 0.84;

pub(crate) fn vec2(p: PointF) -> Vector2 {
    Vector2 { X: p.x, Y: p.y }
}

pub(crate) fn d2d_rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

fn rounded(r: RectF, radius: f32) -> D2D1_ROUNDED_RECT {
    let radius = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    D2D1_ROUNDED_RECT { rect: d2d_rect(r), radiusX: radius, radiusY: radius }
}

fn infinite_rect() -> D2D_RECT_F {
    D2D_RECT_F { left: -1.0e7, top: -1.0e7, right: 1.0e7, bottom: 1.0e7 }
}

fn outset(r: RectF, d: f32) -> RectF {
    r.inset(-d)
}

/// Immediate-mode painter. All coordinates are DIPs; the device context's DPI maps them to physical pixels.
pub struct Painter<'a> {
    gfx: &'a Gfx,
    context: ID2D1DeviceContext,
    scale: f32,
    theme: &'a Theme,
    size: SizeF,
    transform: Matrix3x2,
}

impl<'a> Painter<'a> {
    pub(crate) fn new(gfx: &'a Gfx, context: ID2D1DeviceContext, scale: f32, theme: &'a Theme, size: SizeF) -> Self {
        // SAFETY: resets the world transform of a context that is between BeginDraw and EndDraw.
        unsafe { context.SetTransform(&Matrix3x2::identity()) };
        Self { gfx, context, scale, theme, size, transform: Matrix3x2::identity() }
    }

    pub fn gfx(&self) -> &Gfx {
        self.gfx
    }

    /// The raw device context for anything the wrapper does not cover. Restore any state you change.
    pub fn context(&self) -> &ID2D1DeviceContext {
        &self.context
    }

    /// Physical pixels per DIP.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn theme(&self) -> &Theme {
        self.theme
    }

    pub fn size(&self) -> SizeF {
        self.size
    }

    pub fn bounds(&self) -> RectF {
        RectF::new(0.0, 0.0, self.size.w, self.size.h)
    }

    /// One physical pixel in DIP.
    pub fn px(&self) -> f32 {
        1.0 / self.scale
    }

    /// Rounds a DIP coordinate to the physical pixel grid.
    pub fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    pub fn snap_point(&self, p: PointF) -> PointF {
        PointF::new(self.snap(p.x), self.snap(p.y))
    }

    /// Rounds every edge to the physical pixel grid.
    pub fn snap_rect(&self, r: RectF) -> RectF {
        RectF::from_ltrb(self.snap(r.x), self.snap(r.y), self.snap(r.right()), self.snap(r.bottom()))
    }

    fn brush(&self, brush: &Brush) -> Option<ID2D1Brush> {
        let result = match brush {
            Brush::Solid(color) => self.gfx.solid_brush(color.to_d2d()).and_then(|b| Ok(b.cast()?)),
            Brush::Linear { start, end, stops } => self.gradient_brush(*start, *end, stops),
        };
        result.map_err(|e| log::debug!("brush: {e:#}")).ok()
    }

    fn gradient_brush(&self, start: PointF, end: PointF, stops: &[(f32, Color)]) -> Result<ID2D1Brush> {
        let key: Vec<u32> = stops
            .iter()
            .flat_map(|(o, c)| [o.to_bits(), c.r.to_bits(), c.g.to_bits(), c.b.to_bits(), c.a.to_bits()])
            .collect();
        let existing = self.gfx.cache.borrow().gradients.get(&key).cloned();
        let collection = match existing {
            Some(c) => c,
            None => {
                let d2d_stops: Vec<D2D1_GRADIENT_STOP> =
                    stops.iter().map(|(o, c)| D2D1_GRADIENT_STOP { position: *o, color: c.to_d2d() }).collect();
                // SAFETY: stop slice valid for the call.
                let c = unsafe {
                    self.context.CreateGradientStopCollection(
                        &d2d_stops,
                        D2D1_COLOR_SPACE_SRGB,
                        D2D1_COLOR_SPACE_SRGB,
                        D2D1_BUFFER_PRECISION_8BPC_UNORM,
                        D2D1_EXTEND_MODE_CLAMP,
                        D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
                    )?
                };
                let mut cache = self.gfx.cache.borrow_mut();
                if cache.gradients.len() > 256 {
                    cache.gradients.clear();
                }
                cache.gradients.insert(key, c.clone());
                c
            }
        };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: vec2(start), endPoint: vec2(end) };
        // SAFETY: property pointers valid for the call.
        let brush = unsafe { self.context.CreateLinearGradientBrush(&props, None, &collection)? };
        Ok(brush.cast()?)
    }

    fn stroke_style(&self, style: &StrokeStyle) -> Option<ID2D1StrokeStyle> {
        if *style == StrokeStyle::default() {
            return None;
        }
        let key = style.key();
        if let Some(s) = self.gfx.strokes.borrow().get(&key) {
            return s.cast().ok();
        }
        let cap = |c: LineCap| -> D2D1_CAP_STYLE {
            match c {
                LineCap::Flat => D2D1_CAP_STYLE_FLAT,
                LineCap::Round => D2D1_CAP_STYLE_ROUND,
                LineCap::Square => D2D1_CAP_STYLE_SQUARE,
            }
        };
        let join: D2D1_LINE_JOIN = match style.join {
            LineJoin::Miter => D2D1_LINE_JOIN_MITER,
            LineJoin::Round => D2D1_LINE_JOIN_ROUND,
            LineJoin::Bevel => D2D1_LINE_JOIN_BEVEL,
        };
        let props = D2D1_STROKE_STYLE_PROPERTIES1 {
            startCap: cap(style.cap),
            endCap: cap(style.cap),
            dashCap: cap(style.cap),
            lineJoin: join,
            miterLimit: 10.0,
            dashStyle: if style.dashes.is_empty() { D2D1_DASH_STYLE_SOLID } else { D2D1_DASH_STYLE_CUSTOM },
            dashOffset: style.dash_offset,
            transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
        };
        let dashes = (!style.dashes.is_empty()).then_some(style.dashes.as_slice());
        // SAFETY: props and dash slice valid for the call.
        let created: windows::core::Result<ID2D1StrokeStyle1> = unsafe { self.gfx.factory().CreateStrokeStyle(&props, dashes) };
        let created = created.map_err(|e| log::debug!("stroke style: {e}")).ok()?;
        self.gfx.strokes.borrow_mut().insert(key, created.clone());
        created.cast().ok()
    }

    pub fn clear(&mut self, color: Color) {
        // SAFETY: valid color pointer.
        unsafe { self.context.Clear(Some(&color.to_d2d())) };
    }

    pub fn fill_rect(&mut self, r: RectF, brush: impl Into<Brush>) {
        if let Some(b) = self.brush(&brush.into()) {
            // SAFETY: valid rect and brush.
            unsafe { self.context.FillRectangle(&d2d_rect(r), &b) };
        }
    }

    pub fn stroke_rect(&mut self, r: RectF, brush: impl Into<Brush>, width: f32) {
        if let Some(b) = self.brush(&brush.into()) {
            // SAFETY: valid rect and brush.
            unsafe { self.context.DrawRectangle(&d2d_rect(r), &b, width, None::<&ID2D1StrokeStyle>) };
        }
    }

    pub fn fill_round_rect(&mut self, r: RectF, radius: f32, brush: impl Into<Brush>) {
        if let Some(b) = self.brush(&brush.into()) {
            // SAFETY: valid rect and brush.
            unsafe { self.context.FillRoundedRectangle(&rounded(r, radius), &b) };
        }
    }

    pub fn stroke_round_rect(&mut self, r: RectF, radius: f32, brush: impl Into<Brush>, width: f32) {
        if let Some(b) = self.brush(&brush.into()) {
            // SAFETY: valid rect and brush.
            unsafe { self.context.DrawRoundedRectangle(&rounded(r, radius), &b, width, None::<&ID2D1StrokeStyle>) };
        }
    }

    /// A 1-physical-pixel line just inside (`inset` true) or just outside the rounded rect's edge.
    pub fn hairline_round_rect(&mut self, r: RectF, radius: f32, color: Color, inset: bool) {
        let half = self.px() * 0.5;
        let (rect, radius) = if inset { (r.inset(half), radius - half) } else { (outset(r, half), radius + half) };
        self.stroke_round_rect(rect, radius.max(0.0), color, self.px());
    }

    pub fn fill_ellipse(&mut self, center: PointF, rx: f32, ry: f32, brush: impl Into<Brush>) {
        if let Some(b) = self.brush(&brush.into()) {
            let e = D2D1_ELLIPSE { point: vec2(center), radiusX: rx, radiusY: ry };
            // SAFETY: valid ellipse and brush.
            unsafe { self.context.FillEllipse(&e, &b) };
        }
    }

    pub fn fill_circle(&mut self, center: PointF, radius: f32, brush: impl Into<Brush>) {
        self.fill_ellipse(center, radius, radius, brush);
    }

    pub fn stroke_ellipse(&mut self, center: PointF, rx: f32, ry: f32, brush: impl Into<Brush>, width: f32) {
        if let Some(b) = self.brush(&brush.into()) {
            let e = D2D1_ELLIPSE { point: vec2(center), radiusX: rx, radiusY: ry };
            // SAFETY: valid ellipse and brush.
            unsafe { self.context.DrawEllipse(&e, &b, width, None::<&ID2D1StrokeStyle>) };
        }
    }

    pub fn line(&mut self, a: PointF, b: PointF, brush: impl Into<Brush>, width: f32, style: &StrokeStyle) {
        if let Some(br) = self.brush(&brush.into()) {
            let s = self.stroke_style(style);
            // SAFETY: valid points, brush and optional style.
            unsafe { self.context.DrawLine(vec2(a), vec2(b), &br, width, s.as_ref()) };
        }
    }

    pub fn polyline(&mut self, points: &[PointF], brush: impl Into<Brush>, width: f32, style: &StrokeStyle, closed: bool) {
        if points.len() < 2 {
            return;
        }
        let mut builder = PathBuilder::new();
        builder.polyline(points, closed);
        if let Ok(path) = builder.build(self.gfx) {
            self.stroke_path(&path, brush, width, style);
        }
    }

    pub fn fill_path(&mut self, path: &Path, brush: impl Into<Brush>) {
        if let Some(b) = self.brush(&brush.into()) {
            // SAFETY: valid geometry and brush.
            unsafe { self.context.FillGeometry(&path.geometry, &b, None::<&ID2D1Brush>) };
        }
    }

    pub fn stroke_path(&mut self, path: &Path, brush: impl Into<Brush>, width: f32, style: &StrokeStyle) {
        if let Some(b) = self.brush(&brush.into()) {
            let s = self.stroke_style(style);
            // SAFETY: valid geometry, brush and optional style.
            unsafe { self.context.DrawGeometry(&path.geometry, &b, width, s.as_ref()) };
        }
    }

    /// Draws `bitmap` into `dest` (DIP). `src` is in bitmap pixels; None = whole bitmap.
    pub fn bitmap(&mut self, bitmap: &Bitmap, dest: RectF, src: Option<RectF>, opacity: f32, interpolation: Interpolation) {
        let Ok(d2d) = bitmap.d2d(self.gfx).map_err(|e| log::debug!("bitmap: {e:#}")) else { return };
        let src = src.map(d2d_rect);
        // SAFETY: rect pointers valid for the call.
        unsafe {
            self.context.DrawBitmap(
                &d2d,
                Some(&d2d_rect(dest)),
                opacity,
                interpolation.d2d(),
                src.as_ref().map(|s| s as *const _),
                None,
            )
        };
    }

    fn set_transform(&mut self, m: Matrix3x2) {
        self.transform = m;
        // SAFETY: valid matrix pointer.
        unsafe { self.context.SetTransform(&m) };
    }

    pub fn transform(&self) -> Matrix3x2 {
        self.transform
    }

    /// Runs `f` with `m` applied before the current transform.
    pub fn with_transform<R>(&mut self, m: Matrix3x2, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.transform;
        self.set_transform(m * previous);
        let result = f(self);
        self.set_transform(previous);
        result
    }

    pub fn translate<R>(&mut self, dx: f32, dy: f32, f: impl FnOnce(&mut Self) -> R) -> R {
        self.with_transform(Matrix3x2::translation(dx, dy), f)
    }

    pub fn scale_around<R>(&mut self, scale: f32, center: PointF, f: impl FnOnce(&mut Self) -> R) -> R {
        self.with_transform(Matrix3x2::scale_around(scale, scale, vec2(center)), f)
    }

    pub fn clip_rect<R>(&mut self, r: RectF, f: impl FnOnce(&mut Self) -> R) -> R {
        // SAFETY: balanced push/pop around `f`.
        unsafe { self.context.PushAxisAlignedClip(&d2d_rect(r), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let result = f(self);
        // SAFETY: matches the push above.
        unsafe { self.context.PopAxisAlignedClip() };
        result
    }

    fn push_layer(&mut self, mask: Option<ID2D1Geometry>, opacity: f32) {
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: infinite_rect(),
            geometricMask: std::mem::ManuallyDrop::new(mask),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        // SAFETY: params valid for the call; D2D AddRefs the mask.
        unsafe { self.context.PushLayer(&params, None) };
        let mut params = params;
        // SAFETY: releases our reference to the mask exactly once.
        unsafe { std::mem::ManuallyDrop::drop(&mut params.geometricMask) };
    }

    fn pop_layer(&mut self) {
        // SAFETY: matches a push_layer.
        unsafe { self.context.PopLayer() };
    }

    pub fn clip_round_rect<R>(&mut self, r: RectF, radius: f32, f: impl FnOnce(&mut Self) -> R) -> R {
        // SAFETY: plain factory call.
        let mask = unsafe { self.gfx.factory().CreateRoundedRectangleGeometry(&rounded(r, radius)) }
            .ok()
            .and_then(|g| g.cast::<ID2D1Geometry>().ok());
        self.push_layer(mask, 1.0);
        let result = f(self);
        self.pop_layer();
        result
    }

    pub fn clip_path<R>(&mut self, path: &Path, f: impl FnOnce(&mut Self) -> R) -> R {
        self.push_layer(Some(path.geometry.clone()), 1.0);
        let result = f(self);
        self.pop_layer();
        result
    }

    /// Composites everything drawn in `f` as one group at `opacity`.
    pub fn layer<R>(&mut self, opacity: f32, f: impl FnOnce(&mut Self) -> R) -> R {
        if opacity >= 0.999 {
            return f(self);
        }
        self.push_layer(None, opacity.max(0.0));
        let result = f(self);
        self.pop_layer();
        result
    }

    fn shadow_bitmap(&self, width: u32, height: u32, radius_px: f32, sigma_px: f32, color: Color) -> Result<(ID2D1Bitmap1, u32)> {
        let pad = (3.0 * sigma_px).ceil().max(1.0) as u32;
        let key = ShadowKey {
            width,
            height,
            radius_quarter_px: (radius_px * 4.0).round() as u32,
            sigma_quarter_px: (sigma_px * 4.0).round() as u32,
            color: color.to_rgba8(),
        };
        if let Some(b) = self.gfx.cache.borrow().shadows.get(&key) {
            return Ok((b.clone(), pad));
        }
        let gfx = self.gfx;
        let aux = gfx.devices().aux_context;
        // SAFETY: the command list is recorded and closed on the aux context before it is used as effect input.
        let shape = unsafe {
            let list = aux.CreateCommandList()?;
            aux.SetTarget(&list);
            aux.SetDpi(96.0, 96.0);
            aux.BeginDraw();
            let white = gfx.solid_brush(D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 })?;
            let shape = RectF::new(pad as f32, pad as f32, width as f32, height as f32);
            aux.FillRoundedRectangle(&rounded(shape, radius_px), &white);
            let ended = aux.EndDraw(None, None);
            aux.SetTarget(None::<&ID2D1Image>);
            ended?;
            list.Close()?;
            list
        };
        let bitmap = render_to_bitmap(gfx, width + 2 * pad, height + 2 * pad, |context| {
            // SAFETY: effect created on and drawn by the same context.
            unsafe {
                let effect = context.CreateEffect(&CLSID_D2D1Shadow)?;
                effect.SetInput(0, &shape, true);
                set_float(&effect, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32, sigma_px)?;
                set_enum(&effect, D2D1_SHADOW_PROP_OPTIMIZATION.0 as u32, D2D1_SHADOW_OPTIMIZATION_QUALITY.0 as u32)?;
                let rgba: Vec<u8> = [color.r, color.g, color.b, color.a].iter().flat_map(|v| v.to_le_bytes()).collect();
                set_property(&effect, D2D1_SHADOW_PROP_COLOR.0 as u32, D2D1_PROPERTY_TYPE_VECTOR4, &rgba)?;
                context.DrawImage(
                    &effect.GetOutput()?,
                    None,
                    None,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    D2D1_COMPOSITE_MODE_SOURCE_OVER,
                );
            }
            Ok(())
        })?;
        let mut cache = self.gfx.cache.borrow_mut();
        if cache.shadows.len() > 128 {
            cache.shadows.clear();
        }
        cache.shadows.insert(key, bitmap.clone());
        Ok((bitmap, pad))
    }

    /// Soft shadow of a rounded rect (CSS `box-shadow` semantics; cached per size, radius, blur and color; large
    /// shapes are nine-sliced from one small cached bitmap).
    pub fn shadow(&mut self, rect: RectF, radius: f32, shadow: &Shadow) {
        if shadow.color.a <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let s = self.scale;
        let sigma_px = (shadow.blur * 0.5 * s).max(0.5);
        let radius_px = (radius.min(rect.w / 2.0).min(rect.h / 2.0) * s).max(0.0);
        let r = self.snap_rect(rect.offset(shadow.dx, shadow.dy));
        let (w_px, h_px) = ((r.w * s).round().max(1.0) as u32, (r.h * s).round().max(1.0) as u32);
        let reach = (radius_px + 3.0 * sigma_px).ceil() as u32;
        let slice = 2 * reach + 1;
        let nine_slice = w_px >= slice && h_px >= slice;
        let (bw, bh) = if nine_slice { (slice, slice) } else { (w_px, h_px) };
        let Ok((bitmap, pad)) = self
            .shadow_bitmap(bw, bh, radius_px, sigma_px, shadow.color)
            .map_err(|e| log::debug!("shadow: {e:#}"))
        else {
            return;
        };
        let pad_dip = pad as f32 / s;
        let outer = RectF::new(r.x - pad_dip, r.y - pad_dip, r.w + 2.0 * pad_dip, r.h + 2.0 * pad_dip);
        let draw = |p: &mut Self, dest: RectF, src: RectF| {
            // SAFETY: rect pointers valid for the call.
            unsafe {
                p.context.DrawBitmap(
                    &bitmap,
                    Some(&d2d_rect(dest)),
                    1.0,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    Some(&d2d_rect(src)),
                    None,
                )
            };
        };
        if !nine_slice {
            let full = RectF::new(0.0, 0.0, (bw + 2 * pad) as f32, (bh + 2 * pad) as f32);
            draw(self, outer, full);
            return;
        }
        let total = (slice + 2 * pad) as f32;
        let c = ((slice + 2 * pad - 1) / 2) as f32;
        let xs_src = [0.0, c, c + 1.0, total];
        let ys_src = xs_src;
        let c_dip = c / s;
        let xs_dst = [outer.x, outer.x + c_dip, outer.right() - c_dip, outer.right()];
        let ys_dst = [outer.y, outer.y + c_dip, outer.bottom() - c_dip, outer.bottom()];
        for row in 0..3 {
            for col in 0..3 {
                let dest = RectF::from_ltrb(xs_dst[col], ys_dst[row], xs_dst[col + 1], ys_dst[row + 1]);
                if dest.w <= 0.0 || dest.h <= 0.0 {
                    continue;
                }
                let src = RectF::from_ltrb(xs_src[col], ys_src[row], xs_src[col + 1], ys_src[row + 1]);
                let src = if col == 1 || row == 1 {
                    let (cx, cy) = (src.center().x, src.center().y);
                    RectF::from_ltrb(
                        if col == 1 { cx - 0.25 } else { src.x },
                        if row == 1 { cy - 0.25 } else { src.y },
                        if col == 1 { cx + 0.25 } else { src.right() },
                        if row == 1 { cy + 0.25 } else { src.bottom() },
                    )
                } else {
                    src
                };
                draw(self, dest, src);
            }
        }
    }

    /// Shadow drawn only outside the rounded rect, so translucent shapes on top are not darkened.
    pub fn shadow_outside(&mut self, rect: RectF, radius: f32, shadow: &Shadow) {
        let factory = self.gfx.factory();
        // SAFETY: plain factory calls.
        let mask = unsafe {
            let spread = shadow.blur * 2.0 + shadow.dx.abs() + shadow.dy.abs() + 4.0;
            let outer = factory.CreateRectangleGeometry(&d2d_rect(outset(rect, spread))).ok().and_then(|g| g.cast().ok());
            let inner = factory.CreateRoundedRectangleGeometry(&rounded(rect, radius)).ok().and_then(|g| g.cast().ok());
            factory
                .CreateGeometryGroup(D2D1_FILL_MODE_ALTERNATE, &[outer, inner])
                .ok()
                .and_then(|g| g.cast::<ID2D1Geometry>().ok())
        };
        self.push_layer(mask, 1.0);
        self.shadow(rect, radius, shadow);
        self.pop_layer();
    }

    /// Frosted glass panel per DESIGN §5: two-layer shadow, blurred+saturated backdrop (or a solid translucent fill
    /// without one), tint, 1 physical px inner hairline and outer border.
    pub fn glass(&mut self, rect: RectF, radius: f32, backdrop: Option<&Backdrop>) {
        let theme = self.theme.clone();
        let rect = self.snap_rect(rect);
        for shadow in &theme.shadows {
            if backdrop.is_some() {
                self.shadow(rect, radius, shadow);
            } else {
                self.shadow_outside(rect, radius, shadow);
            }
        }
        match backdrop {
            Some(b) => {
                self.fill_backdrop(rect, radius, b);
                if b.dim.a > 0.0 {
                    self.fill_round_rect(rect, radius, b.dim);
                }
                self.fill_round_rect(rect, radius, theme.glass_fill);
            }
            None => self.fill_round_rect(rect, radius, theme.glass_fill_solid),
        }
        self.hairline_round_rect(rect, radius, theme.hairline, true);
        self.hairline_round_rect(rect, radius, theme.outer_border, false);
    }

    /// Fills the rounded rect with the backdrop bitmap mapped to `backdrop.dest`.
    pub fn fill_backdrop(&mut self, rect: RectF, radius: f32, backdrop: &Backdrop) {
        let bitmap = backdrop.bitmap;
        let Ok(d2d) = bitmap.d2d(self.gfx) else { return };
        let sx = backdrop.dest.w / bitmap.width().max(1) as f32;
        let sy = backdrop.dest.h / bitmap.height().max(1) as f32;
        let transform = Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: backdrop.dest.x, M32: backdrop.dest.y };
        let props = D2D1_BITMAP_BRUSH_PROPERTIES1 {
            extendModeX: D2D1_EXTEND_MODE_CLAMP,
            extendModeY: D2D1_EXTEND_MODE_CLAMP,
            interpolationMode: D2D1_INTERPOLATION_MODE_LINEAR,
        };
        let brush_props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform };
        // SAFETY: property pointers valid for the call.
        let brush = unsafe { self.context.CreateBitmapBrush(&d2d, Some(&props), Some(&brush_props)) };
        if let Ok(brush) = brush {
            // SAFETY: valid rect and brush.
            unsafe { self.context.FillRoundedRectangle(&rounded(rect, radius), &brush) };
        }
    }

    pub fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Option<Rc<TextLayout>> {
        self.gfx.text_layout(text, style, max_width).map_err(|e| log::debug!("text layout: {e:#}")).ok()
    }

    pub fn measure(&self, text: &str, style: &TextStyle) -> SizeF {
        self.gfx.measure_text(text, style)
    }

    /// Draws a layout with its top-left at `origin`.
    pub fn draw_layout(&mut self, layout: &TextLayout, origin: PointF, color: Color) {
        if let Some(b) = self.brush(&Brush::Solid(color)) {
            // SAFETY: valid layout and brush.
            unsafe { self.context.DrawTextLayout(vec2(origin), &layout.layout, &b, D2D1_DRAW_TEXT_OPTIONS_NONE) };
        }
    }

    /// Single-line text in `rect`, aligned per `style.align`, optically centered on the cap height and trimmed with
    /// an ellipsis. Wrapping styles flow from the top of `rect`. Returns the drawn text width.
    pub fn text(&mut self, text: &str, style: &TextStyle, color: Color, rect: RectF) -> f32 {
        let Some(layout) = self.layout(text, style, Some(rect.w.max(0.0))) else { return 0.0 };
        let y = if style.wrap {
            rect.y
        } else {
            let baseline = self.snap(rect.y + rect.h * 0.5 + layout.metrics.cap_height * 0.5);
            baseline - layout.baseline
        };
        self.draw_layout(&layout, PointF::new(rect.x, y), color);
        layout.width
    }

    /// Left-aligned single-line text whose baseline starts at `origin`.
    pub fn text_at(&mut self, text: &str, style: &TextStyle, color: Color, origin: PointF) -> f32 {
        let style = TextStyle { align: crate::text::TextAlign::Leading, wrap: false, ..*style };
        let Some(layout) = self.layout(text, &style, None) else { return 0.0 };
        let baseline = self.snap(origin.y);
        self.draw_layout(&layout, PointF::new(origin.x, baseline - layout.baseline), color);
        layout.width
    }

    /// Line icon centered at `center`, `size` DIP square, stroke 1.75 on the 24 grid.
    pub fn icon(&mut self, icon: Icon, center: PointF, size: f32, color: impl Into<Brush>) {
        self.icon_with_stroke(icon, center, size, color, ICON_STROKE);
    }

    pub fn icon_with_stroke(&mut self, icon: Icon, center: PointF, size: f32, color: impl Into<Brush>, stroke: f32) {
        let geometry = {
            let cached = self.gfx.icons.borrow().get(&icon).cloned();
            match cached {
                Some(g) => g,
                None => match icon_geometry(self.gfx.factory(), icon) {
                    Ok(g) => {
                        self.gfx.icons.borrow_mut().insert(icon, g.clone());
                        g
                    }
                    Err(e) => {
                        log::debug!("icon {icon:?}: {e:#}");
                        return;
                    }
                },
            }
        };
        let Some(brush) = self.brush(&color.into()) else { return };
        let style = self.stroke_style(&StrokeStyle::round());
        let size = if icon.is_solid() { size * SOLID_ICON_OPTICAL_SCALE } else { size };
        let unit = size / ICON_GRID;
        let origin = self.snap_point(PointF::new(center.x - size * 0.5, center.y - size * 0.5));
        let m = Matrix3x2 { M11: unit, M12: 0.0, M21: 0.0, M22: unit, M31: origin.x, M32: origin.y };
        self.with_transform(m, |p| {
            // SAFETY: valid geometry, brush and style.
            unsafe {
                if let Some(fill) = &geometry.fill {
                    p.context.FillGeometry(fill, &brush, None::<&ID2D1Brush>);
                }
                p.context.DrawGeometry(&geometry.stroke, &brush, stroke, style.as_ref());
            }
        });
    }
}

impl Gfx {
    /// Shaped, cached text layout. `max_width` None measures unconstrained.
    pub fn text_layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Result<Rc<TextLayout>> {
        self.text.layout(text, style, max_width)
    }

    /// Width (without trailing whitespace) and line height of single-line text.
    pub fn measure_text(&self, text: &str, style: &TextStyle) -> SizeF {
        let style = TextStyle { align: crate::text::TextAlign::Leading, wrap: false, ..*style };
        self.text.layout(text, &style, None).map(|l| SizeF::new(l.width, l.height)).unwrap_or_default()
    }

    /// The resolved (text, display) font family names.
    pub fn font_families(&self) -> (String, String) {
        self.text.family_names()
    }
}
