//! Small vector helpers over `PointF`/`RectF` (glint-core keeps its geometry types operator-free).

use glint_core::{PointF, RectF};

pub(crate) trait Vec2: Copy {
    fn plus(self, other: PointF) -> PointF;
    fn minus(self, other: PointF) -> PointF;
    fn times(self, s: f32) -> PointF;
    fn dot(self, other: PointF) -> f32;
    fn length(self) -> f32;
    /// Unit vector; zero stays zero.
    fn normalized(self) -> PointF;
    /// Rotated 90° (x, y) → (−y, x).
    fn perp(self) -> PointF;
    fn lerp_to(self, other: PointF, t: f32) -> PointF;
}

impl Vec2 for PointF {
    fn plus(self, other: PointF) -> PointF {
        PointF::new(self.x + other.x, self.y + other.y)
    }

    fn minus(self, other: PointF) -> PointF {
        PointF::new(self.x - other.x, self.y - other.y)
    }

    fn times(self, s: f32) -> PointF {
        PointF::new(self.x * s, self.y * s)
    }

    fn dot(self, other: PointF) -> f32 {
        self.x * other.x + self.y * other.y
    }

    fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    fn normalized(self) -> PointF {
        let len = self.length();
        if len > 1e-6 { self.times(1.0 / len) } else { PointF::default() }
    }

    fn perp(self) -> PointF {
        PointF::new(-self.y, self.x)
    }

    fn lerp_to(self, other: PointF, t: f32) -> PointF {
        self.plus(other.minus(self).times(t))
    }
}

/// `v` limited to `lo..=hi` without panicking: an inverted range collapses to `lo`, NaN bounds are ignored.
pub(crate) fn clamp_safe(v: f32, lo: f32, hi: f32) -> f32 {
    v.min(hi).max(lo)
}

pub(crate) fn distance_to_segment(p: PointF, a: PointF, b: PointF) -> f32 {
    let ab = b.minus(a);
    let len2 = ab.dot(ab);
    if len2 <= 1e-12 {
        return p.distance(a);
    }
    let t = (p.minus(a).dot(ab) / len2).clamp(0.0, 1.0);
    p.distance(a.plus(ab.times(t)))
}

pub(crate) fn outset(r: RectF, d: f32) -> RectF {
    r.inset(-d)
}

pub(crate) fn union(a: RectF, b: RectF) -> RectF {
    RectF::from_ltrb(a.x.min(b.x), a.y.min(b.y), a.right().max(b.right()), a.bottom().max(b.bottom()))
}

pub(crate) fn intersects(a: RectF, b: RectF) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

pub(crate) fn points_bounds(points: impl IntoIterator<Item = PointF>) -> Option<RectF> {
    points.into_iter().fold(None, |acc, p| {
        let point = RectF::new(p.x, p.y, 0.0, 0.0);
        Some(acc.map_or(point, |r| union(r, point)))
    })
}

/// Snaps the vector `end − start` to the nearest multiple of 45°.
pub(crate) fn snap_angle(start: PointF, end: PointF) -> PointF {
    let d = end.minus(start);
    let len = d.length();
    if len < 1e-6 {
        return end;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let angle = (d.y.atan2(d.x) / step).round() * step;
    start.plus(PointF::new(angle.cos(), angle.sin()).times(len))
}

/// Makes the box spanned by `start`/`end` square, keeping the drag direction.
pub(crate) fn square_up(start: PointF, end: PointF) -> PointF {
    let d = end.minus(start);
    let side = d.x.abs().max(d.y.abs());
    PointF::new(start.x + side.copysign(d.x), start.y + side.copysign(d.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_distance() {
        let a = PointF::new(0.0, 0.0);
        let b = PointF::new(10.0, 0.0);
        assert_eq!(distance_to_segment(PointF::new(5.0, 3.0), a, b), 3.0);
        assert_eq!(distance_to_segment(PointF::new(-4.0, 3.0), a, b), 5.0);
        assert_eq!(distance_to_segment(PointF::new(2.0, 0.0), a, a), 2.0);
    }

    #[test]
    fn safe_clamp_never_panics() {
        assert_eq!(clamp_safe(5.0, 0.0, 10.0), 5.0);
        assert_eq!(clamp_safe(5.0, 8.0, 2.0), 8.0);
        assert_eq!(clamp_safe(5.0, f32::NAN, 3.0), 3.0);
        assert!(!clamp_safe(f32::NAN, 0.0, 1.0).is_nan());
    }

    #[test]
    fn angle_snapping() {
        let s = snap_angle(PointF::new(0.0, 0.0), PointF::new(10.0, 1.0));
        assert!((s.y).abs() < 1e-4 && (s.x - 10.05).abs() < 0.01);
        let d = snap_angle(PointF::new(0.0, 0.0), PointF::new(10.0, 9.0));
        assert!((d.x - d.y).abs() < 1e-4);
        let q = square_up(PointF::new(0.0, 0.0), PointF::new(-3.0, 8.0));
        assert_eq!(q, PointF::new(-8.0, 8.0));
    }
}
