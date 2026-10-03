//! Crop rectangle math: handle hit testing, dragging with clamping and a minimum size, pixel snapping.

use glint_core::{PointF, RectF, RectI, SizeF};

use crate::math::clamp_safe;

/// Smallest crop side in image pixels.
pub const MIN_SIDE: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CropHandle {
    Move,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl CropHandle {
    fn moves(self) -> (bool, bool, bool, bool) {
        use CropHandle::*;
        // (left, top, right, bottom)
        match self {
            Move => (true, true, true, true),
            N => (false, true, false, false),
            NE => (false, true, true, false),
            E => (false, false, true, false),
            SE => (false, false, true, true),
            S => (false, false, false, true),
            SW => (true, false, false, true),
            W => (true, false, false, false),
            NW => (true, true, false, false),
        }
    }

    pub fn cursor(self) -> glint_ui::Cursor {
        use CropHandle::*;
        match self {
            Move => glint_ui::Cursor::Move,
            N | S => glint_ui::Cursor::ResizeNS,
            E | W => glint_ui::Cursor::ResizeEW,
            NE | SW => glint_ui::Cursor::ResizeNESW,
            NW | SE => glint_ui::Cursor::ResizeNWSE,
        }
    }
}

/// Which handle of `frame` (view DIP) is under `pos`: corners win within `corner` DIP, edges within `edge` DIP,
/// anything inside moves the rectangle.
pub fn hit_handle(frame: RectF, pos: PointF, corner: f32, edge: f32) -> Option<CropHandle> {
    let near = |a: f32, b: f32, d: f32| (a - b).abs() <= d;
    let within_x = pos.x >= frame.x - edge && pos.x <= frame.right() + edge;
    let within_y = pos.y >= frame.y - edge && pos.y <= frame.bottom() + edge;
    let (left, right) = (near(pos.x, frame.x, corner), near(pos.x, frame.right(), corner));
    let (top, bottom) = (near(pos.y, frame.y, corner), near(pos.y, frame.bottom(), corner));
    let corner_hit = match (left, right, top, bottom) {
        (true, _, true, _) => Some(CropHandle::NW),
        (_, true, true, _) => Some(CropHandle::NE),
        (true, _, _, true) => Some(CropHandle::SW),
        (_, true, _, true) => Some(CropHandle::SE),
        _ => None,
    };
    if corner_hit.is_some() {
        return corner_hit;
    }
    if within_y && near(pos.x, frame.x, edge) {
        return Some(CropHandle::W);
    }
    if within_y && near(pos.x, frame.right(), edge) {
        return Some(CropHandle::E);
    }
    if within_x && near(pos.y, frame.y, edge) {
        return Some(CropHandle::N);
    }
    if within_x && near(pos.y, frame.bottom(), edge) {
        return Some(CropHandle::S);
    }
    frame.contains(pos).then_some(CropHandle::Move)
}

/// `start` dragged by `delta` (image px) with `handle`, kept inside `bounds` and at least `MIN_SIDE` wide/high (or
/// the whole image on images smaller than that). Moving keeps the size; edges stop short of the opposite edge.
pub fn drag(start: RectF, handle: CropHandle, delta: PointF, bounds: RectF) -> RectF {
    let min_w = MIN_SIDE.min(bounds.w).max(0.0);
    let min_h = MIN_SIDE.min(bounds.h).max(0.0);
    if handle == CropHandle::Move {
        let x = clamp_safe(start.x + delta.x, bounds.x, bounds.right() - start.w);
        let y = clamp_safe(start.y + delta.y, bounds.y, bounds.bottom() - start.h);
        return RectF::new(x, y, start.w, start.h);
    }
    let (ml, mt, mr, mb) = handle.moves();
    let mut l = start.x;
    let mut t = start.y;
    let mut r = start.right();
    let mut b = start.bottom();
    if ml {
        l = clamp_safe(l + delta.x, bounds.x, r - min_w);
    }
    if mr {
        r = clamp_safe(r + delta.x, l + min_w, bounds.right()).max(l);
    }
    if mt {
        t = clamp_safe(t + delta.y, bounds.y, b - min_h);
    }
    if mb {
        b = clamp_safe(b + delta.y, t + min_h, bounds.bottom()).max(t);
    }
    RectF::from_ltrb(l, t, r, b)
}

/// Whole-pixel crop inside the image; None when it covers the whole image.
pub fn snap(rect: RectF, image: SizeF) -> Option<RectI> {
    let full = RectI::new(0, 0, image.w as i32, image.h as i32);
    let r = RectI::from_ltrb(rect.x.round() as i32, rect.y.round() as i32, rect.right().round() as i32, rect.bottom().round() as i32);
    let r = r.intersect(&full)?;
    (r != full).then_some(r)
}

/// Rule-of-thirds lines: two verticals then two horizontals, each as (from, to).
pub fn thirds(frame: RectF) -> [(PointF, PointF); 4] {
    let x1 = frame.x + frame.w / 3.0;
    let x2 = frame.x + frame.w * 2.0 / 3.0;
    let y1 = frame.y + frame.h / 3.0;
    let y2 = frame.y + frame.h * 2.0 / 3.0;
    [
        (PointF::new(x1, frame.y), PointF::new(x1, frame.bottom())),
        (PointF::new(x2, frame.y), PointF::new(x2, frame.bottom())),
        (PointF::new(frame.x, y1), PointF::new(frame.right(), y1)),
        (PointF::new(frame.x, y2), PointF::new(frame.right(), y2)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: RectF = RectF::new(0.0, 0.0, 1000.0, 600.0);

    #[test]
    fn handles_are_found() {
        let frame = RectF::new(100.0, 100.0, 400.0, 300.0);
        assert_eq!(hit_handle(frame, PointF::new(102.0, 98.0), 16.0, 8.0), Some(CropHandle::NW));
        assert_eq!(hit_handle(frame, PointF::new(500.0, 400.0), 16.0, 8.0), Some(CropHandle::SE));
        assert_eq!(hit_handle(frame, PointF::new(300.0, 104.0), 16.0, 8.0), Some(CropHandle::N));
        assert_eq!(hit_handle(frame, PointF::new(497.0, 250.0), 16.0, 8.0), Some(CropHandle::E));
        assert_eq!(hit_handle(frame, PointF::new(300.0, 250.0), 16.0, 8.0), Some(CropHandle::Move));
        assert_eq!(hit_handle(frame, PointF::new(40.0, 250.0), 16.0, 8.0), None);
    }

    #[test]
    fn edges_respect_minimum_and_bounds() {
        let start = RectF::new(100.0, 100.0, 200.0, 100.0);
        let r = drag(start, CropHandle::W, PointF::new(500.0, 0.0), BOUNDS);
        assert_eq!(r.w, MIN_SIDE);
        assert_eq!(r.right(), 300.0);
        let r = drag(start, CropHandle::NW, PointF::new(-500.0, -500.0), BOUNDS);
        assert_eq!((r.x, r.y), (0.0, 0.0));
        assert_eq!((r.right(), r.bottom()), (300.0, 200.0));
        let r = drag(start, CropHandle::SE, PointF::new(5000.0, 5000.0), BOUNDS);
        assert_eq!((r.right(), r.bottom()), (1000.0, 600.0));
    }

    #[test]
    fn tiny_images_never_panic() {
        let bounds = RectF::new(0.0, 0.0, 4.0, 4.0);
        let start = bounds;
        for handle in [
            CropHandle::Move,
            CropHandle::N,
            CropHandle::NE,
            CropHandle::E,
            CropHandle::SE,
            CropHandle::S,
            CropHandle::SW,
            CropHandle::W,
            CropHandle::NW,
        ] {
            for delta in [PointF::new(-10.0, -10.0), PointF::new(10.0, 10.0), PointF::new(1.5, -0.5), PointF::new(f32::NAN, 2.0)] {
                let r = drag(start, handle, delta, bounds);
                assert!(r.w >= 0.0 && r.h >= 0.0, "{handle:?} {delta:?} -> {r:?}");
                assert!(r.x >= 0.0 && r.right() <= 4.0 + 1e-4, "{handle:?} {delta:?} -> {r:?}");
            }
        }
        let squeezed = drag(start, CropHandle::W, PointF::new(3.0, 0.0), bounds);
        assert_eq!(squeezed.w, 4.0, "a 4 px image cannot shrink below its own width");
        assert_eq!(snap(start, SizeF::new(4.0, 4.0)), None);
    }

    #[test]
    fn move_keeps_size_inside_bounds() {
        let start = RectF::new(100.0, 100.0, 200.0, 100.0);
        let r = drag(start, CropHandle::Move, PointF::new(5000.0, -5000.0), BOUNDS);
        assert_eq!(r, RectF::new(800.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn snapping_rounds_and_drops_full_crops() {
        let image = SizeF::new(1000.0, 600.0);
        assert_eq!(snap(RectF::new(10.4, 9.6, 100.2, 50.0), image), Some(RectI::new(10, 10, 101, 50)));
        assert_eq!(snap(RectF::new(-0.2, 0.0, 1000.3, 600.0), image), None);
    }

    #[test]
    fn thirds_split_evenly() {
        let lines = thirds(RectF::new(0.0, 0.0, 300.0, 90.0));
        assert_eq!(lines[0].0.x, 100.0);
        assert_eq!(lines[1].0.x, 200.0);
        assert_eq!(lines[2].0.y, 30.0);
        assert_eq!(lines[3].1, PointF::new(300.0, 60.0));
    }
}
