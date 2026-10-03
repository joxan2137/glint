//! Hit testing (topmost first, eraser sweeps) and selection handles with their resize math.

use glint_core::{PointF, RectF};

use crate::ink;
use crate::math::{Vec2, distance_to_segment, outset};
use crate::model::{Annotation, Body, Document, ShapeKind, StrokeKind, TextMeasure, text_bounds};

/// True when `p` (image px) touches the annotation's ink within `tolerance` image px.
pub fn hit(a: &Annotation, p: PointF, tolerance: f32, measure: &dyn TextMeasure) -> bool {
    match &a.body {
        Body::Stroke(s) => {
            let reach = match s.kind {
                StrokeKind::Highlighter => s.width * 0.5,
                StrokeKind::Pen if s.pressure => ink::pressure_width(s.width, 1.0) * 0.5,
                StrokeKind::Pen => s.width * 0.5,
            } + tolerance;
            match s.points.as_slice() {
                [] => false,
                [only] => only.pos.distance(p) <= reach,
                points => points.windows(2).any(|w| distance_to_segment(p, w[0].pos, w[1].pos) <= reach),
            }
        }
        Body::Shape(s) => {
            let reach = s.width * 0.5 + tolerance;
            match s.kind {
                ShapeKind::Line => distance_to_segment(p, s.start, s.end) <= reach,
                ShapeKind::Arrow => {
                    let arrow = ink::arrow(s.start, s.end, s.width);
                    let head = arrow.head;
                    distance_to_segment(p, s.start, s.end) <= reach
                        || distance_to_segment(p, head[1], head[3]) <= reach
                        || in_triangle(p, head[0], head[1], head[3])
                }
                ShapeKind::Rectangle => {
                    let r = s.rect();
                    if s.filled {
                        return outset(r, reach).contains(p);
                    }
                    let corners = [
                        PointF::new(r.x, r.y),
                        PointF::new(r.right(), r.y),
                        PointF::new(r.right(), r.bottom()),
                        PointF::new(r.x, r.bottom()),
                    ];
                    (0..4).any(|i| distance_to_segment(p, corners[i], corners[(i + 1) % 4]) <= reach)
                }
                ShapeKind::Ellipse => {
                    let r = s.rect();
                    let (rx, ry) = ((r.w * 0.5).max(0.5), (r.h * 0.5).max(0.5));
                    let d = p.minus(r.center());
                    let normalized = ((d.x / rx).powi(2) + (d.y / ry).powi(2)).sqrt();
                    let radial_error = (normalized - 1.0) * rx.min(ry);
                    if s.filled { normalized <= 1.0 || radial_error <= reach } else { radial_error.abs() <= reach }
                }
            }
        }
        Body::Text(t) => outset(text_bounds(t, measure), tolerance).contains(p),
        Body::Redact(r) => outset(r.rect, tolerance).contains(p),
    }
}

fn in_triangle(p: PointF, a: PointF, b: PointF, c: PointF) -> bool {
    let cross = |o: PointF, u: PointF, v: PointF| (u.x - o.x) * (v.y - o.y) - (u.y - o.y) * (v.x - o.x);
    let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

/// The topmost annotation under `p`.
pub fn topmost(doc: &Document, p: PointF, tolerance: f32, measure: &dyn TextMeasure) -> Option<u64> {
    doc.annotations.iter().rev().find(|a| hit(a, p, tolerance, measure)).map(|a| a.id)
}

/// Every annotation an eraser of `radius` touches moving from `from` to `to`.
pub fn swept(doc: &Document, from: PointF, to: PointF, radius: f32, measure: &dyn TextMeasure) -> Vec<u64> {
    let steps = (from.distance(to) / (radius * 0.5).max(0.5)).ceil().clamp(1.0, 256.0) as usize;
    let samples: Vec<PointF> = (0..=steps).map(|i| from.lerp_to(to, i as f32 / steps as f32)).collect();
    doc.annotations
        .iter()
        .filter(|a| samples.iter().any(|s| hit(a, *s, radius, measure)))
        .map(|a| a.id)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    Start,
    End,
}

impl Handle {
    pub fn cursor(self) -> glint_ui::Cursor {
        use glint_ui::Cursor;
        match self {
            Handle::N | Handle::S => Cursor::ResizeNS,
            Handle::E | Handle::W => Cursor::ResizeEW,
            Handle::NE | Handle::SW => Cursor::ResizeNESW,
            Handle::NW | Handle::SE => Cursor::ResizeNWSE,
            Handle::Start | Handle::End => Cursor::Move,
        }
    }
}

/// Handle positions (image px): endpoints for lines and arrows, eight around the frame for boxes and text, none
/// for freehand ink (it moves only).
pub fn handles(a: &Annotation, frame: RectF) -> Vec<(Handle, PointF)> {
    match &a.body {
        Body::Shape(s) if s.kind.is_linear() => vec![(Handle::Start, s.start), (Handle::End, s.end)],
        Body::Stroke(_) => Vec::new(),
        _ => {
            let (l, t, r, b) = (frame.x, frame.y, frame.right(), frame.bottom());
            let (cx, cy) = (frame.center().x, frame.center().y);
            vec![
                (Handle::NW, PointF::new(l, t)),
                (Handle::N, PointF::new(cx, t)),
                (Handle::NE, PointF::new(r, t)),
                (Handle::E, PointF::new(r, cy)),
                (Handle::SE, PointF::new(r, b)),
                (Handle::S, PointF::new(cx, b)),
                (Handle::SW, PointF::new(l, b)),
                (Handle::W, PointF::new(l, cy)),
            ]
        }
    }
}

/// `frame` with the edges `handle` controls moved by `delta`, never thinner than `min`.
pub fn resize_frame(frame: RectF, handle: Handle, delta: PointF, min: f32) -> RectF {
    let (mut l, mut t, mut r, mut b) = (frame.x, frame.y, frame.right(), frame.bottom());
    let moves_left = matches!(handle, Handle::NW | Handle::W | Handle::SW);
    let moves_right = matches!(handle, Handle::NE | Handle::E | Handle::SE);
    let moves_top = matches!(handle, Handle::NW | Handle::N | Handle::NE);
    let moves_bottom = matches!(handle, Handle::SW | Handle::S | Handle::SE);
    if moves_left {
        l = (l + delta.x).min(r - min);
    }
    if moves_right {
        r = (r + delta.x).max(l + min);
    }
    if moves_top {
        t = (t + delta.y).min(b - min);
    }
    if moves_bottom {
        b = (b + delta.y).max(t + min);
    }
    RectF::from_ltrb(l, t, r, b)
}

/// The point that stays put while `handle` drags.
fn anchor(frame: RectF, handle: Handle) -> PointF {
    let (l, t, r, b) = (frame.x, frame.y, frame.right(), frame.bottom());
    let c = frame.center();
    match handle {
        Handle::NW => PointF::new(r, b),
        Handle::N => PointF::new(c.x, b),
        Handle::NE => PointF::new(l, b),
        Handle::E => PointF::new(l, c.y),
        Handle::SE => PointF::new(l, t),
        Handle::S => PointF::new(c.x, t),
        Handle::SW => PointF::new(r, t),
        Handle::W => PointF::new(r, c.y),
        Handle::Start | Handle::End => c,
    }
}

/// `original` resized by dragging `handle` by `delta` (image px). Text scales its font about the opposite corner.
pub fn resize(original: &Annotation, handle: Handle, delta: PointF, measure: &dyn TextMeasure) -> Annotation {
    let mut out = original.clone();
    let frame = original.frame(measure);
    match &mut out.body {
        Body::Shape(s) if s.kind.is_linear() => match handle {
            Handle::Start => s.start = s.start.plus(delta),
            Handle::End => s.end = s.end.plus(delta),
            _ => {}
        },
        Body::Shape(s) => {
            let r = resize_frame(frame, handle, delta, 2.0);
            s.start = PointF::new(r.x, r.y);
            s.end = PointF::new(r.right(), r.bottom());
        }
        Body::Redact(red) => red.rect = resize_frame(frame, handle, delta, 4.0),
        Body::Text(t) => {
            let r = resize_frame(frame, handle, delta, 4.0);
            let scale = match handle {
                Handle::E | Handle::W => r.w / frame.w.max(1e-3),
                _ => r.h / frame.h.max(1e-3),
            };
            let scale = scale.max(6.0 / t.size);
            let pivot = anchor(frame, handle);
            t.size *= scale;
            t.origin = pivot.plus(t.origin.minus(pivot).times(scale));
        }
        Body::Stroke(_) => {}
    }
    out
}

/// The handle under `p` (image px) within `reach`.
pub fn handle_at(list: &[(Handle, PointF)], p: PointF, reach: f32) -> Option<Handle> {
    list.iter()
        .map(|(h, at)| (*h, at.distance(p)))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(h, _)| h)
}

#[cfg(test)]
mod tests {
    use glint_core::ToneMapParams;
    use glint_ui::Color;

    use super::*;
    use crate::model::tests::FixedMeasure;
    use crate::model::{InkPoint, Redaction, RedactKind, Shape, Stroke, TextNote};

    fn doc() -> (Document, u64, u64, u64) {
        let mut doc = Document::new(ToneMapParams::default());
        let rect = doc.add(Body::Shape(Shape {
            kind: ShapeKind::Rectangle,
            start: PointF::new(10.0, 10.0),
            end: PointF::new(110.0, 60.0),
            color: Color::BLACK,
            width: 4.0,
            filled: false,
        }));
        let blur = doc.add(Body::Redact(Redaction { rect: RectF::new(50.0, 30.0, 100.0, 50.0), kind: RedactKind::Blur }));
        let ink = doc.add(Body::Stroke(Stroke {
            kind: StrokeKind::Pen,
            points: vec![InkPoint::new(0.0, 100.0, 0.5), InkPoint::new(200.0, 100.0, 0.5)],
            color: Color::BLACK,
            width: 6.0,
            pressure: false,
        }));
        (doc, rect, blur, ink)
    }

    #[test]
    fn topmost_wins() {
        let (doc, rect, blur, ink) = doc();
        assert_eq!(topmost(&doc, PointF::new(60.0, 40.0), 1.0, &FixedMeasure), Some(blur));
        assert_eq!(topmost(&doc, PointF::new(10.0, 35.0), 1.0, &FixedMeasure), Some(rect));
        assert_eq!(topmost(&doc, PointF::new(30.0, 30.0), 1.0, &FixedMeasure), None, "open rectangles are hollow");
        assert_eq!(topmost(&doc, PointF::new(100.0, 103.5), 1.0, &FixedMeasure), Some(ink));
        assert_eq!(topmost(&doc, PointF::new(100.0, 106.0), 1.0, &FixedMeasure), None);
    }

    #[test]
    fn eraser_sweeps_between_samples() {
        let (doc, _, _, ink) = doc();
        let hits = swept(&doc, PointF::new(175.0, 80.0), PointF::new(175.0, 130.0), 3.0, &FixedMeasure);
        assert_eq!(hits, vec![ink]);
    }

    #[test]
    fn ellipse_and_arrow_hits() {
        let ellipse = Annotation {
            id: 1,
            body: Body::Shape(Shape {
                kind: ShapeKind::Ellipse,
                start: PointF::new(0.0, 0.0),
                end: PointF::new(100.0, 50.0),
                color: Color::BLACK,
                width: 4.0,
                filled: false,
            }),
        };
        assert!(hit(&ellipse, PointF::new(100.0, 25.0), 1.0, &FixedMeasure));
        assert!(!hit(&ellipse, PointF::new(50.0, 25.0), 1.0, &FixedMeasure));
        let arrow = Annotation {
            id: 2,
            body: Body::Shape(Shape {
                kind: ShapeKind::Arrow,
                start: PointF::new(0.0, 0.0),
                end: PointF::new(200.0, 0.0),
                color: Color::BLACK,
                width: 4.0,
                filled: false,
            }),
        };
        assert!(hit(&arrow, PointF::new(185.0, 6.0), 0.5, &FixedMeasure));
        assert!(!hit(&arrow, PointF::new(100.0, 12.0), 0.5, &FixedMeasure));
    }

    #[test]
    fn handles_by_kind() {
        let (doc, rect, _, ink) = doc();
        let r = doc.get(rect).unwrap();
        assert_eq!(handles(r, r.frame(&FixedMeasure)).len(), 8);
        let i = doc.get(ink).unwrap();
        assert!(handles(i, i.frame(&FixedMeasure)).is_empty());
    }

    #[test]
    fn resize_frame_moves_only_its_edges_and_keeps_a_minimum() {
        let frame = RectF::new(0.0, 0.0, 100.0, 50.0);
        assert_eq!(resize_frame(frame, Handle::E, PointF::new(20.0, 99.0), 2.0), RectF::new(0.0, 0.0, 120.0, 50.0));
        assert_eq!(resize_frame(frame, Handle::NW, PointF::new(10.0, 5.0), 2.0), RectF::new(10.0, 5.0, 90.0, 45.0));
        let squashed = resize_frame(frame, Handle::S, PointF::new(0.0, -200.0), 2.0);
        assert_eq!(squashed.h, 2.0);
        assert_eq!(squashed.y, 0.0);
    }

    #[test]
    fn text_scales_about_the_opposite_corner() {
        let note = Annotation {
            id: 1,
            body: Body::Text(TextNote {
                text: "Hello".into(),
                origin: PointF::new(100.0, 100.0),
                size: 20.0,
                color: Color::BLACK,
                background: false,
            }),
        };
        let frame = note.frame(&FixedMeasure);
        let grown = resize(&note, Handle::SE, PointF::new(0.0, frame.h), &FixedMeasure);
        let Body::Text(t) = &grown.body else { unreachable!() };
        assert!((t.size - 40.0).abs() < 1e-3);
        assert_eq!(t.origin, PointF::new(100.0, 100.0), "the NW corner is the anchor for SE");
        let shrunk = resize(&note, Handle::NW, PointF::new(0.0, frame.h * 0.5), &FixedMeasure);
        let new_frame = shrunk.frame(&FixedMeasure);
        assert!((new_frame.right() - frame.right()).abs() < 1e-3);
        assert!((new_frame.bottom() - frame.bottom()).abs() < 1e-3);
    }

    #[test]
    fn line_handles_move_endpoints() {
        let line = Annotation {
            id: 1,
            body: Body::Shape(Shape {
                kind: ShapeKind::Arrow,
                start: PointF::new(0.0, 0.0),
                end: PointF::new(10.0, 0.0),
                color: Color::BLACK,
                width: 2.0,
                filled: false,
            }),
        };
        let moved = resize(&line, Handle::End, PointF::new(5.0, 5.0), &FixedMeasure);
        let Body::Shape(s) = &moved.body else { unreachable!() };
        assert_eq!((s.start, s.end), (PointF::new(0.0, 0.0), PointF::new(15.0, 5.0)));
        assert_eq!(handle_at(&handles(&moved, RectF::default()), PointF::new(14.0, 5.0), 3.0), Some(Handle::End));
    }
}
