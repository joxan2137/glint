//! Ink geometry: input filtering, adjustable smoothing, centripetal Catmull-Rom → cubic Bézier curves,
//! pressure-driven outlines with round caps, and line caps (arrowheads, dots).

use glint_core::PointF;

use crate::math::{Vec2, clamp_safe};
use crate::model::{Cap, InkPoint};

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

/// Most [1 2 1]/4 position passes `smoothing` = 100 applies.
const MAX_SMOOTHING_PASSES: f32 = 4.0;

/// One [1 2 1]/4 pass on positions blended in by `amount` (0..=1); endpoints stay put.
fn relax_positions(points: &mut [InkPoint], amount: f32) {
    let n = points.len();
    if n < 3 || amount <= 0.0 {
        return;
    }
    let original: Vec<PointF> = points.iter().map(|p| p.pos).collect();
    for i in 1..n - 1 {
        let relaxed = original[i - 1].plus(original[i].times(2.0)).plus(original[i + 1]).times(0.25);
        points[i].pos = original[i].lerp_to(relaxed, amount);
    }
}

fn relax_pressure(points: &mut [InkPoint]) {
    let n = points.len();
    if n < 3 {
        return;
    }
    for _ in 0..3 {
        let pressures: Vec<f32> = points.iter().map(|p| p.pressure).collect();
        for i in 1..n - 1 {
            points[i].pressure = (pressures[i - 1] + 2.0 * pressures[i] + pressures[i + 1]) * 0.25;
        }
    }
}

/// The samples the curve goes through: positions relaxed by `smoothing` (0..=100, up to four passes, the last one
/// partial so the slider is continuous) and pressure always lightly smoothed. Endpoints never move.
pub fn smoothed(points: &[InkPoint], smoothing: f32) -> Vec<InkPoint> {
    let mut out = points.to_vec();
    let passes = clamp_safe(smoothing, 0.0, 100.0) / 100.0 * MAX_SMOOTHING_PASSES;
    let mut remaining = passes;
    while remaining > 0.0 {
        relax_positions(&mut out, remaining.min(1.0));
        remaining -= 1.0;
    }
    relax_pressure(&mut out);
    out
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

/// A cap at one end of a line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Head {
    /// Apple-style filled arrowhead: tip, wing, back notch, wing; corners rounded by stroking at `round`.
    Filled { points: [PointF; 4], round: f32 },
    /// Open chevron: wing, tip, wing, stroked at the line width with round joins.
    Open([PointF; 3]),
    Dot { center: PointF, radius: f32 },
}

/// A line's shaft (trimmed so it ends inside its caps) and its caps.
#[derive(Clone, Debug, PartialEq)]
pub struct LineGeometry {
    pub shaft: (PointF, PointF),
    pub heads: Vec<Head>,
}

impl LineGeometry {
    /// Every point the geometry reaches (dots contribute their extremes), for bounds.
    pub fn points(&self) -> Vec<PointF> {
        let mut out = vec![self.shaft.0, self.shaft.1];
        for head in &self.heads {
            match head {
                Head::Filled { points, .. } => out.extend(points),
                Head::Open(points) => out.extend(points),
                Head::Dot { center, radius } => {
                    out.extend([PointF::new(center.x - radius, center.y - radius), PointF::new(center.x + radius, center.y + radius)])
                }
            }
        }
        out
    }
}

/// Filled arrowhead length for a stroke width at `scale` 1.
pub fn filled_head_length(width: f32) -> f32 {
    width * 3.4 + 6.0
}

/// Caps proportional to the stroke width (times `head_scale`), never longer than 45 % of the line each (60 % when
/// only one end has a cap), so heads never swallow the shaft. The shaft is trimmed to end inside each cap.
pub fn line_geometry(start: PointF, end: PointF, width: f32, caps: [Cap; 2], head_scale: f32) -> LineGeometry {
    let span = end.minus(start);
    let length = span.length();
    let dir = if length > 1e-4 { span.times(1.0 / length) } else { PointF::new(1.0, 0.0) };
    let capped = caps.iter().filter(|c| **c != Cap::None).count();
    let limit = length * if capped == 2 { 0.45 } else { 0.6 };
    let scale = clamp_safe(head_scale, 0.25, 4.0);
    let mut shaft = [start, end];
    let mut heads = Vec::new();
    for (index, cap) in caps.iter().enumerate() {
        let (tip, outward) = if index == 0 { (start, dir.times(-1.0)) } else { (end, dir) };
        let side = outward.perp();
        match cap {
            Cap::None => {}
            Cap::FilledArrow => {
                let head = clamp_safe(filled_head_length(width) * scale, (width * 1.2).min(limit), limit);
                let wing = head * 0.56;
                let base = tip.minus(outward.times(head));
                let notch = tip.minus(outward.times(head * 0.74));
                heads.push(Head::Filled {
                    points: [tip, base.plus(side.times(wing)), notch, base.minus(side.times(wing))],
                    round: width * 0.35,
                });
                shaft[index] = notch.plus(outward.times(width * 0.5));
            }
            Cap::Arrow => {
                let head = clamp_safe((width * 2.6 + 6.0) * scale, (width * 1.2).min(limit), limit);
                let base = tip.minus(outward.times(head));
                let wing = head * 0.62;
                heads.push(Head::Open([base.plus(side.times(wing)), tip, base.minus(side.times(wing))]));
                shaft[index] = tip.minus(outward.times(width * 0.5));
            }
            Cap::Dot => {
                heads.push(Head::Dot { center: tip, radius: (width * 1.4 + 1.5) * scale });
            }
        }
    }
    if shaft[1].minus(shaft[0]).dot(dir) < 0.0 {
        let middle = shaft[0].lerp_to(shaft[1], 0.5);
        shaft = [middle, middle];
    }
    LineGeometry { shaft: (shaft[0], shaft[1]), heads }
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
    fn smoothing_reduces_roughness_continuously_and_pins_ends() {
        let pts: Vec<InkPoint> = (0..24)
            .map(|i| {
                let jitter = if i % 2 == 0 { 1.5 } else { -1.5 };
                InkPoint::new(i as f32 * 3.0, (i as f32 * 0.5).sin() * 10.0 + jitter, 0.5)
            })
            .collect();
        let roughness = |points: &[InkPoint]| {
            points.windows(3).map(|w| w[0].pos.minus(w[1].pos.times(2.0)).plus(w[2].pos).length()).sum::<f32>()
        };
        let raw = smoothed(&pts, 0.0);
        assert_eq!(raw.iter().map(|p| p.pos).collect::<Vec<_>>(), pts.iter().map(|p| p.pos).collect::<Vec<_>>());
        let (half, light, strong) = (smoothed(&pts, 12.5), smoothed(&pts, 25.0), smoothed(&pts, 100.0));
        assert!(roughness(&half) < roughness(&raw));
        assert!(roughness(&light) < roughness(&half), "fractional passes blend continuously");
        assert!(roughness(&strong) < roughness(&light));
        for out in [&half, &light, &strong] {
            assert_eq!(out[0].pos, pts[0].pos);
            assert_eq!(out[23].pos, pts[23].pos);
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

    fn filled(g: &LineGeometry, i: usize) -> [PointF; 4] {
        match g.heads[i] {
            Head::Filled { points, .. } => points,
            other => panic!("expected a filled head, got {other:?}"),
        }
    }

    const ARROW: [Cap; 2] = [Cap::None, Cap::FilledArrow];

    #[test]
    fn plain_lines_have_no_heads() {
        let g = line_geometry(PointF::new(0.0, 0.0), PointF::new(100.0, 0.0), 4.0, [Cap::None, Cap::None], 1.0);
        assert_eq!(g.shaft, (PointF::new(0.0, 0.0), PointF::new(100.0, 0.0)));
        assert!(g.heads.is_empty());
    }

    #[test]
    fn filled_head_scales_with_width_and_size_and_stays_on_axis() {
        let (a, b) = (PointF::new(0.0, 0.0), PointF::new(300.0, 0.0));
        let len = |g: &LineGeometry| filled(g, 0)[0].x - filled(g, 0)[1].x;
        let thin = line_geometry(a, b, 2.0, ARROW, 1.0);
        let thick = line_geometry(a, b, 8.0, ARROW, 1.0);
        let big = line_geometry(a, b, 2.0, ARROW, 2.0);
        assert!(len(&thick) > len(&thin) * 2.0);
        assert!((len(&big) - 2.0 * len(&thin)).abs() < 1e-3, "head size multiplies the length");
        let head = filled(&thin, 0);
        assert_eq!(head[0], b);
        assert!((head[1].y + head[3].y).abs() < 1e-4, "wings are symmetric");
        assert!(thin.shaft.1.x < head[0].x && thin.shaft.1.x > head[2].x, "the shaft ends inside the head");
        let short = line_geometry(a, PointF::new(20.0, 0.0), 8.0, ARROW, 1.0);
        assert!(filled(&short, 0)[0].x - filled(&short, 0)[1].x <= 20.0 * 0.6 + 1e-4);
    }

    #[test]
    fn caps_point_outward_at_both_ends() {
        let (a, b) = (PointF::new(0.0, 0.0), PointF::new(0.0, 200.0));
        let g = line_geometry(a, b, 4.0, [Cap::FilledArrow, Cap::Arrow], 1.0);
        assert_eq!(filled(&g, 0)[0], a);
        assert!(filled(&g, 0)[1].y > a.y, "the start head's wings lie inside the line");
        let Head::Open(chevron) = g.heads[1] else { panic!("open head expected") };
        assert_eq!(chevron[1], b);
        assert!(chevron[0].y < b.y && chevron[2].y < b.y);
        assert!(g.shaft.0.y > a.y && g.shaft.1.y < b.y);
    }

    #[test]
    fn dots_scale_with_width_and_heads_never_cross_on_short_lines() {
        let g = line_geometry(PointF::new(0.0, 0.0), PointF::new(100.0, 0.0), 4.0, [Cap::Dot, Cap::None], 1.0);
        let Head::Dot { center, radius } = g.heads[0] else { panic!("dot expected") };
        assert_eq!(center, PointF::new(0.0, 0.0));
        let wider = line_geometry(PointF::new(0.0, 0.0), PointF::new(100.0, 0.0), 8.0, [Cap::Dot, Cap::None], 1.0);
        let Head::Dot { radius: wider_radius, .. } = wider.heads[0] else { panic!("dot expected") };
        assert!(wider_radius > radius * 1.5);
        let tiny = line_geometry(PointF::new(0.0, 0.0), PointF::new(10.0, 0.0), 6.0, [Cap::FilledArrow; 2], 2.0);
        assert!(tiny.shaft.0.x <= tiny.shaft.1.x, "trimmed ends never cross");
        assert!(filled(&tiny, 0)[1].x <= filled(&tiny, 1)[1].x + 1e-4);
    }
}
