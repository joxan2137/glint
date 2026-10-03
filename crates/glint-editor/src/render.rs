//! Drawing the document in image-pixel coordinates. The canvas draws it under the zoom transform and export draws
//! it 1:1 offscreen, through the same functions, so what is copied is what was on screen.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use anyhow::{Context, Result};
use glint_core::{Image, PointF, RectF, RectI, SizeF};
use glint_ui::{
    Bitmap, Color, Gfx, Interpolation, LineCap, LineJoin, OffscreenSpec, Painter, Path, PathBuilder, StrokeStyle,
    TextStyle, Theme, Weight, render_offscreen,
};

use crate::ink;
use crate::model::{Annotation, Body, Document, RedactKind, Redaction, Shape, ShapeKind, Stroke, StrokeKind, TextMeasure, TextNote};
use crate::pixels;

pub const HIGHLIGHTER_OPACITY: f32 = 0.4;

pub fn note_style(size: f32) -> TextStyle {
    TextStyle::new(size).weight(Weight::Medium)
}

/// DirectWrite measurement for hit testing and selection frames.
pub struct GfxMeasure<'a>(pub &'a Gfx);

impl TextMeasure for GfxMeasure<'_> {
    fn text_size(&self, note: &TextNote) -> SizeF {
        let text = if note.text.is_empty() { " " } else { note.text.as_str() };
        self.0.text_layout(text, &note_style(note.size), None).map(|l| SizeF::new(l.width, l.height)).unwrap_or_default()
    }
}

/// Built stroke geometry per annotation id, rebuilt only when the stroke's content changes.
#[derive(Default)]
pub struct GeometryCache {
    entries: RefCell<HashMap<u64, (u64, Path)>>,
}

impl GeometryCache {
    fn get_or_build(&self, id: u64, fingerprint: u64, build: impl FnOnce() -> Option<Path>) -> Option<Path> {
        if let Some((print, path)) = self.entries.borrow().get(&id)
            && *print == fingerprint
        {
            return Some(path.clone());
        }
        let path = build()?;
        let mut entries = self.entries.borrow_mut();
        if entries.len() > 4096 {
            entries.clear();
        }
        entries.insert(id, (fingerprint, path.clone()));
        Some(path)
    }
}

fn stroke_fingerprint(stroke: &Stroke) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    (stroke.kind == StrokeKind::Pen).hash(&mut h);
    stroke.pressure.hash(&mut h);
    stroke.width.to_bits().hash(&mut h);
    for p in &stroke.points {
        p.pos.x.to_bits().hash(&mut h);
        p.pos.y.to_bits().hash(&mut h);
        p.pressure.to_bits().hash(&mut h);
    }
    h.finish()
}

/// A GPU bitmap and the image-pixel rectangle it covers: the whole image, a box-downscaled stand-in for images
/// beyond the GPU's bitmap size limit, or just the exported region.
#[derive(Clone)]
pub struct Layer {
    pub bitmap: Rc<Bitmap>,
    pub rect: RectF,
}

impl Layer {
    /// `image` at full resolution when both sides fit `max_side`, else reduced by a whole factor.
    pub fn fitted(image: &Rc<Image>, max_side: u32) -> Layer {
        let factor = pixels::reduction(image.width, image.height, max_side);
        if factor == 1 {
            let rect = RectF::new(0.0, 0.0, image.width as f32, image.height as f32);
            return Layer { bitmap: Bitmap::from_shared(image.clone()), rect };
        }
        let small = pixels::downscale(image, factor);
        let rect = RectF::new(0.0, 0.0, (small.width * factor) as f32, (small.height * factor) as f32);
        Layer { bitmap: Bitmap::new(small), rect }
    }

    /// The `region` of `image` at full resolution, placed where it sits in the image.
    pub fn region(image: &Image, region: RectI) -> Layer {
        Layer { bitmap: Bitmap::new(image.crop(region)), rect: region.to_f() }
    }

    /// Bitmap pixels per image pixel.
    fn density(&self) -> f32 {
        self.bitmap.width() as f32 / self.rect.w.max(1e-3)
    }

    /// `area` (image px) in bitmap pixels.
    fn source(&self, area: RectF) -> RectF {
        let d = self.density();
        RectF::new((area.x - self.rect.x) * d, (area.y - self.rect.y) * d, area.w * d, area.h * d)
    }
}

/// What annotations are drawn against: the base image and its derived redaction sources.
pub struct Scene<'a> {
    pub base: &'a Layer,
    /// Block-pixelated copy of the base (only built when a pixelation exists).
    pub pixelated: Option<&'a Layer>,
    /// The image's true size in pixels; redaction strength follows it.
    pub image: (u32, u32),
    pub cache: &'a GeometryCache,
}

impl Scene<'_> {
    pub fn size(&self) -> (u32, u32) {
        self.image
    }
}

/// The pixelation of `image` (blocks aligned to the image origin).
pub fn pixelated_image(image: &Image) -> Image {
    pixels::pixelate(image, pixels::block_size(image.width, image.height))
}

pub fn paint_base(p: &mut Painter, scene: &Scene, interpolation: Interpolation) {
    p.bitmap(&scene.base.bitmap, scene.base.rect, None, 1.0, interpolation);
}

/// Every annotation in z-order. `dimmed` (the eraser's hover target) draws faded.
pub fn paint_annotations(p: &mut Painter, doc: &Document, scene: &Scene, dimmed: Option<u64>) {
    for a in &doc.annotations {
        let opacity = if Some(a.id) == dimmed { 0.3 } else { 1.0 };
        p.layer(opacity, |p| paint_annotation(p, a, scene));
    }
}

pub fn paint_annotation(p: &mut Painter, a: &Annotation, scene: &Scene) {
    match &a.body {
        Body::Stroke(s) => paint_stroke(p, a.id, s, scene),
        Body::Shape(s) => paint_shape(p, s),
        Body::Text(t) => paint_text(p, t),
        Body::Redact(r) => paint_redaction(p, r, scene),
    }
}

fn bezier_path(p: &Painter, points: &[PointF]) -> Option<Path> {
    let mut builder = PathBuilder::new();
    builder.move_to(*points.first()?);
    for c in ink::catmull_rom(points) {
        builder.cubic_to(c.c1, c.c2, c.p3);
    }
    builder.build(p.gfx()).ok()
}

pub fn paint_stroke(p: &mut Painter, id: u64, stroke: &Stroke, scene: &Scene) {
    if stroke.points.is_empty() {
        return;
    }
    let print = stroke_fingerprint(stroke);
    match stroke.kind {
        StrokeKind::Pen if stroke.pressure => {
            let outline = scene.cache.get_or_build(id, print, || {
                let polygon = ink::variable_outline(&stroke.points, stroke.width, 0.75);
                let mut builder = PathBuilder::new();
                builder.polyline(&polygon, true);
                builder.build(p.gfx()).ok()
            });
            if let Some(path) = outline {
                p.fill_path(&path, stroke.color);
            }
        }
        StrokeKind::Pen => {
            if stroke.points.len() == 1 {
                p.fill_circle(stroke.points[0].pos, stroke.width * 0.5, stroke.color);
                return;
            }
            let positions: Vec<PointF> = stroke.points.iter().map(|q| q.pos).collect();
            if let Some(path) = scene.cache.get_or_build(id, print, || bezier_path(p, &positions)) {
                p.stroke_path(&path, stroke.color, stroke.width, &StrokeStyle::round());
            }
        }
        StrokeKind::Highlighter => {
            let positions: Vec<PointF> = stroke.points.iter().map(|q| q.pos).collect();
            let nib = StrokeStyle { cap: LineCap::Flat, join: LineJoin::Round, ..StrokeStyle::default() };
            p.layer(HIGHLIGHTER_OPACITY, |p| {
                if positions.len() == 1 {
                    let half = stroke.width * 0.5;
                    let at = positions[0];
                    p.fill_rect(RectF::new(at.x - half, at.y - half, stroke.width, stroke.width), stroke.color);
                } else if let Some(path) = scene.cache.get_or_build(id, print, || bezier_path(p, &positions)) {
                    p.stroke_path(&path, stroke.color, stroke.width, &nib);
                }
            });
        }
    }
}

pub fn paint_shape(p: &mut Painter, s: &Shape) {
    let round = StrokeStyle::round();
    match s.kind {
        ShapeKind::Rectangle => {
            let r = s.rect();
            let corners = [PointF::new(r.x, r.y), PointF::new(r.right(), r.y), PointF::new(r.right(), r.bottom()), PointF::new(r.x, r.bottom())];
            let mut builder = PathBuilder::new();
            builder.polyline(&corners, true);
            let Ok(path) = builder.build(p.gfx()) else { return };
            if s.filled {
                p.fill_path(&path, s.color);
            }
            p.stroke_path(&path, s.color, s.width, &StrokeStyle { join: LineJoin::Round, ..StrokeStyle::default() });
        }
        ShapeKind::Ellipse => {
            let r = s.rect();
            let (rx, ry) = (r.w * 0.5, r.h * 0.5);
            if s.filled {
                p.fill_ellipse(r.center(), rx + s.width * 0.5, ry + s.width * 0.5, s.color);
            } else {
                p.stroke_ellipse(r.center(), rx, ry, s.color, s.width);
            }
        }
        ShapeKind::Line => p.line(s.start, s.end, s.color, s.width, &round),
        ShapeKind::Arrow => {
            let arrow = ink::arrow(s.start, s.end, s.width);
            p.line(arrow.shaft_start, arrow.shaft_end, s.color, s.width, &round);
            let mut builder = PathBuilder::new();
            builder.polyline(&arrow.head, true);
            if let Ok(head) = builder.build(p.gfx()) {
                p.fill_path(&head, s.color);
                p.stroke_path(&head, s.color, arrow.corner_round, &round);
            }
        }
    }
}

/// Plate color behind text: dark under light text, light under dark text.
pub fn plate_color(text: Color) -> Color {
    if text.luminance() > 0.45 { Color::rgba(0.0, 0.0, 0.0, 0.62) } else { Color::rgba(1.0, 1.0, 1.0, 0.86) }
}

pub fn paint_text(p: &mut Painter, t: &TextNote) {
    let shown = if t.text.is_empty() { " " } else { t.text.as_str() };
    let Some(layout) = p.layout(shown, &note_style(t.size), None) else { return };
    if t.background {
        let pad = t.plate_padding();
        let plate = RectF::new(t.origin.x - pad.w, t.origin.y - pad.h, layout.width + 2.0 * pad.w, layout.height + 2.0 * pad.h);
        p.fill_round_rect(plate, t.size * 0.3, plate_color(t.color));
    }
    if !t.text.is_empty() {
        p.draw_layout(&layout, t.origin, t.color);
    }
}

pub fn paint_redaction(p: &mut Painter, r: &Redaction, scene: &Scene) {
    let (w, h) = scene.size();
    let clip = RectF::new(0.0, 0.0, w as f32, h as f32);
    match r.kind {
        RedactKind::Blur => {
            let base = scene.base;
            let blurred = base.bitmap.blurred(pixels::blur_sigma(w, h) * base.density(), 1.0);
            let Some(area) = intersection(r.rect, clip).and_then(|a| intersection(a, base.rect)) else { return };
            p.bitmap(&blurred, area, Some(base.source(area)), 1.0, Interpolation::Linear);
        }
        RedactKind::Pixelate => {
            let Some(layer) = scene.pixelated else { return };
            let aligned = pixels::block_aligned(r.rect, pixels::block_size(w, h), w, h);
            let Some(area) = intersection(aligned, clip).and_then(|a| intersection(a, layer.rect)) else { return };
            p.bitmap(&layer.bitmap, area, Some(layer.source(area)), 1.0, Interpolation::Nearest);
        }
    }
}

fn intersection(a: RectF, b: RectF) -> Option<RectF> {
    let r = RectF::from_ltrb(a.x.max(b.x), a.y.max(b.y), a.right().min(b.right()), a.bottom().min(b.bottom()));
    (r.w > 0.0 && r.h > 0.0).then_some(r)
}

/// Translation from image pixels to export pixels for a crop: the export's (0, 0) is the crop's top-left.
pub fn export_offset(content: RectI) -> PointF {
    PointF::new(-content.x as f32, -content.y as f32)
}

/// Base + annotations at full image resolution, cropped.
pub fn export(gfx: &Rc<Gfx>, doc: &Document, scene: &Scene) -> Result<Image> {
    let (w, h) = scene.size();
    let content = doc.content_rect(SizeF::new(w as f32, h as f32));
    let spec = OffscreenSpec::pixels(content.w.max(1) as u32, content.h.max(1) as u32, 1.0, Theme::dark());
    let offset = export_offset(content);
    render_offscreen(gfx, &spec, |_, p| {
        p.translate(offset.x, offset.y, |p| {
            paint_base(p, scene, Interpolation::Nearest);
            paint_annotations(p, doc, scene, None);
        });
    })
    .context("rendering the export")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_offset_maps_crop_origin_to_zero() {
        let content = RectI::new(120, 48, 300, 200);
        let o = export_offset(content);
        let image_point = PointF::new(125.0, 50.0);
        assert_eq!(PointF::new(image_point.x + o.x, image_point.y + o.y), PointF::new(5.0, 2.0));
    }

    #[test]
    fn layers_map_image_areas_to_bitmap_pixels() {
        let image = Rc::new(Image::new(10, 4));
        let whole = Layer::fitted(&image, 16);
        assert_eq!(whole.rect, RectF::new(0.0, 0.0, 10.0, 4.0));
        assert_eq!(whole.source(RectF::new(2.0, 1.0, 4.0, 2.0)), RectF::new(2.0, 1.0, 4.0, 2.0));
        let reduced = Layer::fitted(&image, 4);
        assert_eq!((reduced.bitmap.width(), reduced.bitmap.height()), (4, 2));
        assert_eq!(reduced.rect, RectF::new(0.0, 0.0, 12.0, 6.0));
        assert_eq!(reduced.source(RectF::new(3.0, 3.0, 6.0, 3.0)), RectF::new(1.0, 1.0, 2.0, 1.0));
        let region = Layer::region(&image, RectI::new(4, 1, 5, 2));
        assert_eq!(region.source(RectF::new(5.0, 2.0, 2.0, 1.0)), RectF::new(1.0, 1.0, 2.0, 1.0));
    }

    #[test]
    fn plates_contrast_with_text() {
        assert!(plate_color(Color::WHITE).luminance() < 0.1);
        assert!(plate_color(Color::BLACK).luminance() > 0.5);
    }

    #[test]
    fn intersections() {
        assert_eq!(intersection(RectF::new(-5.0, -5.0, 10.0, 10.0), RectF::new(0.0, 0.0, 100.0, 100.0)), Some(RectF::new(0.0, 0.0, 5.0, 5.0)));
        assert_eq!(intersection(RectF::new(200.0, 0.0, 10.0, 10.0), RectF::new(0.0, 0.0, 100.0, 100.0)), None);
    }
}
