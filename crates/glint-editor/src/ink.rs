//! Ink geometry: input filtering, centripetal Catmull-Rom → cubic Bézier smoothing, pressure-driven outlines with
//! round caps, and arrow heads.

use glint_core::PointF;

use crate::math::Vec2;
use crate::model::InkPoint;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cubic {
    pub p0: PointF,
    pub c1: PointF,
    pub c2: PointF,
    pub p3: PointF,
}

impl Cubic {
    pub fn at(&self, t: f32) -> PointF {
        let u = 1.0 - t;
        self.p0
            .times(u * u * u)
            .plus(self.c1.times(3.0 * u * u * t))
            .plus(self.c2.times(3.0 * u * t * t))
            .plus(self.p3.times(t * t * t))
    }

    pub fn tangent(&self, t: f32) -> PointF {
        let u = 1.0 - t;
        self.c1
            .minus(self.p0)
            .times(3.0 * u * u)
            .plus(self.c2.minus(self.c1).times(6.0 * u * t))
            .plus(self.p3.minus(self.c2).times(3.0 * t * t))
    }

    /// Control-polygon length: an upper bound of the arc length, plenty for choosing sample counts.
    pub fn rough_length(&self) -> f32 {
        self.p0.distance(self.c1) + self.c1.distance(self.c2) + self.c2.distance(self.p3)
    }
}

/// Drops samples closer than `min_distance` to the previous kept one (the last sample always survives) so jitter
/// from high-rate digitizers never turns into wobble.
pub fn simplify(points: &[InkPoint], min_distance: f32) -> Vec<InkPoint> {
    let mut out: Vec<InkPoint> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let last = i + 1 == points.len();
        match out.last() {
            Some(prev) if prev.pos.distance(p.pos) < min_distance => {
                if last && out.len() > 1 {
                    if let Some(previous) = out.last_mut() {
                        *previous = *p;
                    }
                } else if last {
                    out.push(*p);
                }
            }
            _ => out.push(*p),
        }
    }
    out
}

/// One pass of [1 2 1]/4 smoothing on positions and three on pressure; endpoints stay put.
pub fn relax(points: &mut [InkPoint]) {
    let n = points.len();
    if n < 3 {
        return;
    }
    let original: Vec<InkPoint> = points.to_vec();
    for i in 1..n - 1 {
        let (a, b, c) = (original[i - 1].pos, original[i].pos, original[i + 1].pos);
        points[i].pos = a.plus(b.times(2.0)).plus(c).times(0.25);
    }
    for _ in 0..3 {
        let pressures: Vec<f32> = points.iter().map(|p| p.pressure).collect();
        for i in 1..n - 1 {
            points[i].pressure = (pressures[i - 1] + 2.0 * pressures[i] + pressures[i + 1]) * 0.25;
        }
    }
}

/// The processing every finished stroke goes through.
pub fn finalize(raw: &[InkPoint], min_distance: f32) -> Vec<InkPoint> {
    let mut points = simplify(raw, min_distance);
    relax(&mut points);
    points
}

/// Centripetal (α = 0.5) Catmull-Rom spline through `points` as cubic Béziers; no cusps or overshoot on uneven
/// spacing. The phantom end points are reflections, so the curve leaves and enters its ends straight.
pub fn catmull_rom(points: &[PointF]) -> Vec<Cubic> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    (0..n - 1)
        .map(|i| {
            let p1 = points[i];
            let p2 = points[i + 1];
            let p0 = if i == 0 { p1.plus(p1.minus(p2)) } else { points[i - 1] };
            let p3 = if i + 2 < n { points[i + 2] } else { p2.plus(p2.minus(p1)) };
            centripetal_segment(p0, p1, p2, p3)
        })
        .collect()
}

fn centripetal_segment(p0: PointF, p1: PointF, p2: PointF, p3: PointF) -> Cubic {
    const EPSILON: f32 = 1e-4;
    let d1 = p0.distance(p1).sqrt();
    let d2 = p1.distance(p2).sqrt();
    let d3 = p2.distance(p3).sqrt();
    if d2 < EPSILON {
        return Cubic { p0: p1, c1: p1, c2: p2, p3: p2 };
    }
    let c1 = if d1 < EPSILON {
        p1.plus(p2.minus(p0).times(1.0 / 6.0))
    } else {
        let (a, b) = (d1 * d1, d2 * d2);
        p2.times(a).minus(p0.times(b)).plus(p1.times(2.0 * a + 3.0 * d1 * d2 + b)).times(1.0 / (3.0 * d1 * (d1 + d2)))
    };
    let c2 = if d3 < EPSILON {
        p2.minus(p3.minus(p1).times(1.0 / 6.0))
    } else {
        let (a, b) = (d3 * d3, d2 * d2);
        p1.times(a).minus(p3.times(b)).plus(p2.times(2.0 * a + 3.0 * d3 * d2 + b)).times(1.0 / (3.0 * d3 * (d3 + d2)))
    };
    Cubic { p0: p1, c1, c2, p3: p2 }
}

/// Pen width for a pressure sample: ≈ the nominal width at mid pressure, thin at a feather touch, wider when
/// pressed hard.
pub fn pressure_width(width: f32, pressure: f32) -> f32 {
    let pressure = if pressure.is_finite() { pressure.clamp(0.0, 1.0) } else { 0.5 };
    width * (0.3 + 1.15 * pressure.powf(0.8))
}

/// Closed polygon of a variable-width stroke with round caps. Single points become a circle.
pub fn variable_outline(points: &[InkPoint], width: f32, spacing: f32) -> Vec<PointF> {
    let spacing = spacing.max(0.05);
    match points {
        [] => Vec::new(),
        [only] => circle(only.pos, pressure_width(width, only.pressure) * 0.5),
        _ => {
            let positions: Vec<PointF> = points.iter().map(|p| p.pos).collect();
            let half_widths: Vec<f32> = points.iter().map(|p| pressure_width(width, p.pressure) * 0.5).collect();
            let mut centers = Vec::new();
            let mut normals = Vec::new();
            let mut halves = Vec::new();
            for (i, cubic) in catmull_rom(&positions).iter().enumerate() {
                let steps = (cubic.rough_length() / spacing).ceil().clamp(1.0, 128.0) as usize;
                let first = if i == 0 { 0 } else { 1 };
                for k in first..=steps {
                    let t = k as f32 / steps as f32;
                    let mut tangent = cubic.tangent(t).normalized();
                    if tangent == PointF::default() {
                        tangent = cubic.p3.minus(cubic.p0).normalized();
                    }
                    centers.push(cubic.at(t));
                    normals.push(tangent.perp());
                    halves.push(half_widths[i] + (half_widths[i + 1] - half_widths[i]) * t);
                }
            }
            let last = centers.len() - 1;
            let mut outline = Vec::with_capacity(centers.len() * 2 + 48);
            outline.extend((0..=last).map(|i| centers[i].plus(normals[i].times(halves[i]))));
            outline.extend(cap(centers[last], normals[last], halves[last], true));
            outline.extend((0..=last).rev().map(|i| centers[i].minus(normals[i].times(halves[i]))));
            outline.extend(cap(centers[0], normals[0], halves[0], false));
            outline
        }
    }
}

/// Semicircle points strictly between the two sides: from +normal through the travel direction to −normal at the
/// end, from −normal backwards to +normal at the start.
fn cap(center: PointF, normal: PointF, half: f32, end: bool) -> Vec<PointF> {
    let forward = PointF::new(normal.y, -normal.x);
    let segments = (half * 1.5).clamp(6.0, 24.0) as usize;
    (1..segments)
        .map(|k| {
            let theta = std::f32::consts::PI * k as f32 / segments as f32;
            let (sin, cos) = theta.sin_cos();
            if end {
                center.plus(normal.times(cos * half)).plus(forward.times(sin * half))
            } else {
                center.minus(normal.times(cos * half)).minus(forward.times(sin * half))
            }
        })
        .collect()
}

fn circle(center: PointF, radius: f32) -> Vec<PointF> {
    let segments = (radius * 2.0).clamp(12.0, 48.0) as usize;
    (0..segments)
        .map(|k| {
            let theta = std::f32::consts::TAU * k as f32 / segments as f32;
            center.plus(PointF::new(theta.cos(), theta.sin()).times(radius))
        })
        .collect()
}

/// Arrow geometry: the shaft ends inside the head so the joint never shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrow {
    pub shaft_start: PointF,
    pub shaft_end: PointF,
    /// Tip, left wing, back notch, right wing.
    pub head: [PointF; 4],
    /// Stroke width that rounds the head's corners.
    pub corner_round: f32,
}

/// Apple-style filled head: length and wing span proportional to the stroke width, a shallow swallowtail notch,
/// scaled down on short arrows so the head never swallows the shaft.
pub fn arrow(start: PointF, end: PointF, width: f32) -> Arrow {
    let span = end.minus(start);
    let length = span.length();
    let dir = if length > 1e-4 { span.times(1.0 / length) } else { PointF::new(1.0, 0.0) };
    let head_length = (width * 3.4 + 6.0).min(length * 0.55).max(width * 1.2);
    let wing = head_length * 0.56;
    let side = dir.perp();
    let base = end.minus(dir.times(head_length));
    let notch = end.minus(dir.times(head_length * 0.74));
    Arrow {
        shaft_start: start,
        shaft_end: notch.plus(dir.times(width * 0.5)),
        head: [end, base.plus(side.times(wing)), notch, base.minus(side.times(wing))],
        corner_round: width * 0.35,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize, step: f32) -> Vec<InkPoint> {
        (0..n).map(|i| InkPoint::new(i as f32 * step, 0.0, 0.5)).collect()
    }

    #[test]
    fn simplify_drops_jitter_but_keeps_ends() {
        let raw = vec![
            InkPoint::new(0.0, 0.0, 0.5),
            InkPoint::new(0.1, 0.0, 0.5),
            InkPoint::new(0.2, 0.1, 0.5),
            InkPoint::new(3.0, 0.0, 0.5),
            InkPoint::new(3.1, 0.0, 0.5),
        ];
        let out = simplify(&raw, 1.0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].pos, PointF::new(0.0, 0.0));
        assert_eq!(out[1].pos, PointF::new(3.1, 0.0), "last sample replaces a too-close predecessor");
    }

    #[test]
    fn catmull_rom_interpolates_every_point() {
        let pts = [PointF::new(0.0, 0.0), PointF::new(10.0, 5.0), PointF::new(20.0, -3.0), PointF::new(26.0, 8.0)];
        let curve = catmull_rom(&pts);
        assert_eq!(curve.len(), 3);
        for (i, c) in curve.iter().enumerate() {
            assert_eq!(c.at(0.0), pts[i]);
            let end = c.at(1.0);
            assert!(end.distance(pts[i + 1]) < 1e-4);
        }
        for pair in curve.windows(2) {
            let out = pair[0].p3.minus(pair[0].c2).normalized();
            let into = pair[1].c1.minus(pair[1].p0).normalized();
            assert!(out.dot(into) > 0.999, "tangent continuity at joints");
        }
    }

    #[test]
    fn straight_input_stays_straight() {
        let pts: Vec<PointF> = (0..6).map(|i| PointF::new(i as f32 * 3.0, i as f32 * 3.0)).collect();
        for c in catmull_rom(&pts) {
            for k in 0..=10 {
                let p = c.at(k as f32 / 10.0);
                assert!((p.x - p.y).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn uniform_spacing_matches_classic_catmull_rom() {
        let pts = [PointF::new(0.0, 0.0), PointF::new(1.0, 0.0), PointF::new(2.0, 0.0), PointF::new(3.0, 0.0)];
        let c = catmull_rom(&pts)[1];
        assert!((c.c1.x - 4.0 / 3.0).abs() < 1e-5);
        assert!((c.c2.x - 5.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn relax_smooths_a_zigzag_and_pins_ends() {
        let mut pts: Vec<InkPoint> =
            (0..9).map(|i| InkPoint::new(i as f32, if i % 2 == 0 { 0.0 } else { 2.0 }, 0.5)).collect();
        let (first, last) = (pts[0].pos, pts[8].pos);
        relax(&mut pts);
        assert_eq!(pts[0].pos, first);
        assert_eq!(pts[8].pos, last);
        for p in &pts[1..8] {
            assert!((p.pos.y - 1.0).abs() <= 0.5 + 1e-6);
        }
    }

    #[test]
    fn outline_width_follows_pressure() {
        let mut pts = line(5, 10.0);
        let flat = variable_outline(&pts, 4.0, 1.0);
        let half = pressure_width(4.0, 0.5) * 0.5;
        let top = flat.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        assert!((top - half).abs() < 1e-3, "{top} vs {half}");
        pts[4].pressure = 1.0;
        let flared = variable_outline(&pts, 4.0, 1.0);
        let top = flared.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        assert!((top - pressure_width(4.0, 1.0) * 0.5).abs() < 1e-3);
        let left_of_start = flat.iter().map(|p| p.x).fold(f32::MAX, f32::min);
        assert!((left_of_start + half).abs() < 0.05, "round cap reaches one radius behind the start");
    }

    #[test]
    fn single_point_is_a_dot() {
        let dot = variable_outline(&[InkPoint::new(5.0, 5.0, 0.5)], 6.0, 1.0);
        let r = pressure_width(6.0, 0.5) * 0.5;
        assert!(dot.iter().all(|p| (p.distance(PointF::new(5.0, 5.0)) - r).abs() < 1e-4));
    }

    #[test]
    fn arrow_head_scales_with_width_and_stays_on_axis() {
        let thin = arrow(PointF::new(0.0, 0.0), PointF::new(300.0, 0.0), 2.0);
        let thick = arrow(PointF::new(0.0, 0.0), PointF::new(300.0, 0.0), 8.0);
        let len = |a: &Arrow| a.head[0].x - a.head[1].x;
        assert!(len(&thick) > len(&thin) * 2.0);
        assert_eq!(thin.head[0], PointF::new(300.0, 0.0));
        assert!((thin.head[1].y + thin.head[3].y).abs() < 1e-4, "wings are symmetric");
        assert!(thin.shaft_end.x < thin.head[0].x && thin.shaft_end.x > thin.head[2].x);
        let short = arrow(PointF::new(0.0, 0.0), PointF::new(20.0, 0.0), 8.0);
        assert!(short.head[0].x - short.head[1].x <= 20.0 * 0.55 + 1e-4);
    }
}
