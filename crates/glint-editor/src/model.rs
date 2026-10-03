//! The markup document: vector annotations in image pixel coordinates, the crop and the tone-map parameters.

use glint_core::{PointF, RectF, RectI, SizeF, ToneMapParams};
use glint_ui::Color;

use crate::ink;
use crate::math::{Vec2, outset, points_bounds};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StrokeKind {
    Pen,
    Highlighter,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InkPoint {
    pub pos: PointF,
    /// 0..=1; 0.5 when the device reports none.
    pub pressure: f32,
}

impl InkPoint {
    #[cfg(test)]
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self { pos: PointF::new(x, y), pressure }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub kind: StrokeKind,
    pub points: Vec<InkPoint>,
    pub color: Color,
    /// Nominal width in image pixels.
    pub width: f32,
    /// Pen input: width follows pressure. Mouse strokes are uniform.
    pub pressure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Line,
    Arrow,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 4] = [ShapeKind::Rectangle, ShapeKind::Ellipse, ShapeKind::Line, ShapeKind::Arrow];

    pub fn is_linear(self) -> bool {
        matches!(self, ShapeKind::Line | ShapeKind::Arrow)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub kind: ShapeKind,
    /// Rectangles and ellipses span the box of `start`/`end`; lines and arrows run from `start` to `end`.
    pub start: PointF,
    pub end: PointF,
    pub color: Color,
    pub width: f32,
    pub filled: bool,
}

impl Shape {
    pub fn rect(&self) -> RectF {
        RectF::from_points(self.start, self.end)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextNote {
    pub text: String,
    /// Top-left of the text layout.
    pub origin: PointF,
    /// Font size in image pixels.
    pub size: f32,
    pub color: Color,
    /// A soft contrasting plate behind the text.
    pub background: bool,
}

impl TextNote {
    /// Padding of the background plate around the layout.
    pub fn plate_padding(&self) -> SizeF {
        SizeF::new(self.size * 0.32, self.size * 0.14)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RedactKind {
    Blur,
    Pixelate,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Redaction {
    pub rect: RectF,
    pub kind: RedactKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Stroke(Stroke),
    Shape(Shape),
    Text(TextNote),
    Redact(Redaction),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: u64,
    pub body: Body,
}

/// Text layout size for hit testing and selection bounds (DirectWrite in the app, a stub in tests).
pub trait TextMeasure {
    fn text_size(&self, note: &TextNote) -> SizeF;
}

impl Annotation {
    /// Ink bounds in image pixels (stroke widths and arrow heads included).
    pub fn bounds(&self, measure: &dyn TextMeasure) -> RectF {
        match &self.body {
            Body::Stroke(s) => {
                let r = points_bounds(s.points.iter().map(|p| p.pos)).unwrap_or_default();
                let reach = if s.pressure { ink::pressure_width(s.width, 1.0) } else { s.width };
                outset(r, reach * 0.5)
            }
            Body::Shape(s) => match s.kind {
                ShapeKind::Line => outset(s.rect(), s.width * 0.5),
                ShapeKind::Arrow => {
                    let head = ink::arrow(s.start, s.end, s.width);
                    let pts = head.head.iter().copied().chain([s.start, s.end]);
                    outset(points_bounds(pts).unwrap_or_default(), s.width * 0.5)
                }
                ShapeKind::Rectangle | ShapeKind::Ellipse => {
                    if s.filled { s.rect() } else { outset(s.rect(), s.width * 0.5) }
                }
            },
            Body::Text(t) => text_bounds(t, measure),
            Body::Redact(r) => r.rect,
        }
    }

    /// The box selection handles act on: shape/redact rects, text bounds, stroke ink bounds.
    pub fn frame(&self, measure: &dyn TextMeasure) -> RectF {
        match &self.body {
            Body::Shape(s) => s.rect(),
            Body::Redact(r) => r.rect,
            Body::Text(t) => text_bounds(t, measure),
            Body::Stroke(_) => self.bounds(measure),
        }
    }

    pub fn translate(&mut self, d: PointF) {
        match &mut self.body {
            Body::Stroke(s) => s.points.iter_mut().for_each(|p| p.pos = p.pos.plus(d)),
            Body::Shape(s) => {
                s.start = s.start.plus(d);
                s.end = s.end.plus(d);
            }
            Body::Text(t) => t.origin = t.origin.plus(d),
            Body::Redact(r) => r.rect = r.rect.offset(d.x, d.y),
        }
    }

}

pub fn text_bounds(note: &TextNote, measure: &dyn TextMeasure) -> RectF {
    let size = measure.text_size(note);
    let r = RectF::new(note.origin.x, note.origin.y, size.w.max(note.size * 0.3), size.h.max(note.size * 1.2));
    if note.background {
        let pad = note.plate_padding();
        RectF::new(r.x - pad.w, r.y - pad.h, r.w + 2.0 * pad.w, r.h + 2.0 * pad.h)
    } else {
        r
    }
}

/// Everything the undo stack snapshots.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub annotations: Vec<Annotation>,
    /// Non-destructive crop in image pixels; None = the whole image.
    pub crop: Option<RectI>,
    pub tone_map: ToneMapParams,
    next_id: u64,
}

impl Document {
    pub fn new(tone_map: ToneMapParams) -> Self {
        Self { annotations: Vec::new(), crop: None, tone_map, next_id: 1 }
    }

    pub fn add(&mut self, body: Body) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.annotations.push(Annotation { id, body });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Annotation> {
        self.annotations.iter_mut().find(|a| a.id == id)
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.annotations.len();
        self.annotations.retain(|a| a.id != id);
        self.annotations.len() != before
    }

    /// The visible part of the image: the crop, or all of it.
    pub fn content_rect(&self, image: SizeF) -> RectI {
        let full = RectI::new(0, 0, image.w as i32, image.h as i32);
        self.crop.and_then(|c| c.intersect(&full)).unwrap_or(full)
    }

    pub fn has_pixelation(&self) -> bool {
        self.annotations.iter().any(|a| matches!(&a.body, Body::Redact(r) if r.kind == RedactKind::Pixelate))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub struct FixedMeasure;

    impl TextMeasure for FixedMeasure {
        fn text_size(&self, note: &TextNote) -> SizeF {
            let longest = note.text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
            let lines = note.text.split('\n').count().max(1);
            SizeF::new(longest as f32 * note.size * 0.5, lines as f32 * note.size * 1.25)
        }
    }

    #[test]
    fn ids_are_unique_and_removal_works() {
        let mut doc = Document::new(ToneMapParams::default());
        let a = doc.add(Body::Redact(Redaction { rect: RectF::new(0.0, 0.0, 4.0, 4.0), kind: RedactKind::Blur }));
        let b = doc.add(Body::Redact(Redaction { rect: RectF::new(1.0, 1.0, 4.0, 4.0), kind: RedactKind::Pixelate }));
        assert_ne!(a, b);
        assert!(doc.has_pixelation());
        assert!(doc.remove(b));
        assert!(!doc.remove(b));
        assert!(!doc.has_pixelation());
        let c = doc.add(Body::Redact(Redaction { rect: RectF::default(), kind: RedactKind::Blur }));
        assert!(c > b, "ids never repeat after removal");
    }

    #[test]
    fn content_rect_clamps_crop() {
        let mut doc = Document::new(ToneMapParams::default());
        let size = SizeF::new(100.0, 50.0);
        assert_eq!(doc.content_rect(size), RectI::new(0, 0, 100, 50));
        doc.crop = Some(RectI::new(80, 10, 40, 20));
        assert_eq!(doc.content_rect(size), RectI::new(80, 10, 20, 20));
    }

    #[test]
    fn translate_moves_every_kind() {
        let mut a = Annotation {
            id: 1,
            body: Body::Stroke(Stroke {
                kind: StrokeKind::Pen,
                points: vec![InkPoint::new(0.0, 0.0, 0.5), InkPoint::new(10.0, 0.0, 0.5)],
                color: Color::BLACK,
                width: 2.0,
                pressure: false,
            }),
        };
        a.translate(PointF::new(5.0, 5.0));
        assert_eq!(a.bounds(&FixedMeasure), RectF::new(4.0, 4.0, 12.0, 2.0));
    }
}
