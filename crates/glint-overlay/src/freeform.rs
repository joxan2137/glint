//! Freeform lasso: points in virtual-desktop pixels, a smoothed path, and the antialiased alpha mask of the result.

use std::rc::Rc;

use anyhow::{Context, Result};
use glint_core::{Image, PointF, RectF, RectI};
use glint_ui::{Color, Gfx, OffscreenSpec, PathBuilder, Theme, render_offscreen};

use crate::geometry::is_meaningful;

/// Samples closer than this to the previous point are dropped (keeps the path light and smooth).
const MIN_STEP_PX: f32 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Lasso {
    bounds: RectI,
    points: Vec<PointF>,
}

impl Lasso {
    /// `at` is a pixel position; the path runs through pixel centers.
    pub fn begin(bounds: RectI, at: PointF) -> Self {
        let mut lasso = Self { bounds, points: Vec::new() };
        lasso.points.push(lasso.clamp(at));
        lasso
    }

    fn clamp(&self, p: PointF) -> PointF {
        let b = self.bounds.to_f();
        PointF::new((p.x + 0.5).clamp(b.x, b.right()), (p.y + 0.5).clamp(b.y, b.bottom()))
    }

    pub fn add(&mut self, p: PointF) {
        let p = self.clamp(p);
        if self.points.last().is_none_or(|last| last.distance(p) >= MIN_STEP_PX) {
            self.points.push(p);
        }
    }

    pub fn points(&self) -> &[PointF] {
        &self.points
    }

    /// Pixel bounding box of the lasso within its monitor.
    pub fn bbox(&self) -> RectI {
        points_bbox(&self.points).and_then(|r| r.intersect(&self.bounds)).unwrap_or_default()
    }

    pub fn is_meaningful(&self) -> bool {
        self.points.len() >= 3 && is_meaningful(self.bbox())
    }
}

pub fn points_bbox(points: &[PointF]) -> Option<RectI> {
    let first = points.first()?;
    let bounds = points.iter().fold(RectF::new(first.x, first.y, 0.0, 0.0), |r, p| {
        RectF::from_ltrb(r.x.min(p.x), r.y.min(p.y), r.right().max(p.x), r.bottom().max(p.y))
    });
    Some(bounds.round_out())
}

/// Cubic Bézier `[start, control1, control2, end]` segments of the Catmull-Rom spline through `points`.
fn catmull_rom(points: &[PointF], closed: bool) -> Vec<[PointF; 4]> {
    let n = points.len();
    let at = |i: isize| -> PointF {
        if closed { points[i.rem_euclid(n as isize) as usize] } else { points[i.clamp(0, n as isize - 1) as usize] }
    };
    let segments = if closed { n } else { n.saturating_sub(1) };
    (0..segments as isize)
        .map(|i| {
            let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            let c1 = PointF::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
            let c2 = PointF::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
            [p1, c1, c2, p2]
        })
        .collect()
}

/// Catmull-Rom spline through `points` as cubic Béziers.
pub fn smooth_path(builder: &mut PathBuilder, points: &[PointF], closed: bool) {
    let Some(&first) = points.first() else { return };
    builder.move_to(first);
    if points.len() < 3 {
        for p in &points[1..] {
            builder.line_to(*p);
        }
    } else {
        for [_, c1, c2, end] in catmull_rom(points, closed) {
            builder.cubic_to(c1, c2, end);
        }
    }
    if closed {
        builder.close();
    }
}

/// The closed smoothed lasso flattened to a polygon (the curve `smooth_path` draws).
fn smooth_polygon(points: &[PointF]) -> Vec<PointF> {
    const STEPS: usize = 4;
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut polygon = Vec::with_capacity(points.len() * STEPS);
    for [a, b, c, d] in catmull_rom(points, true) {
        for k in 0..STEPS {
            let t = k as f32 / STEPS as f32;
            let u = 1.0 - t;
            let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            polygon.push(PointF::new(w0 * a.x + w1 * b.x + w2 * c.x + w3 * d.x, w0 * a.y + w1 * b.y + w2 * c.y + w3 * d.y));
        }
    }
    polygon
}

/// Lasso coverage over `bbox`: Direct2D when it works, else the CPU rasteriser (never an unmasked rect).
pub fn lasso_mask(gfx: &Rc<Gfx>, points: &[PointF], bbox: RectI) -> Vec<u8> {
    rasterize_mask(gfx, points, bbox).unwrap_or_else(|e| {
        log::warn!("freeform mask on the GPU failed, rasterising on the CPU: {e:#}");
        rasterize_mask_cpu(points, bbox)
    })
}

const SUBSAMPLES: usize = 4;

/// Nonzero-winding scanline fill of the smoothed lasso with 4×4 supersampled edges.
pub fn rasterize_mask_cpu(points: &[PointF], bbox: RectI) -> Vec<u8> {
    let (w, h) = (bbox.w.max(0) as usize, bbox.h.max(0) as usize);
    let polygon: Vec<PointF> =
        smooth_polygon(points).iter().map(|p| PointF::new(p.x - bbox.x as f32, p.y - bbox.y as f32)).collect();
    let mut coverage = vec![0u16; w * h];
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for row in 0..h {
        for sub_row in 0..SUBSAMPLES {
            let y = row as f32 + (sub_row as f32 + 0.5) / SUBSAMPLES as f32;
            crossings.clear();
            for (i, a) in polygon.iter().enumerate() {
                let b = polygon[(i + 1) % polygon.len()];
                let (upward, downward) = (a.y <= y && b.y > y, b.y <= y && a.y > y);
                if upward || downward {
                    let x = a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y);
                    crossings.push((x, if upward { 1 } else { -1 }));
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                if winding != 0 {
                    add_span(&mut coverage[row * w..(row + 1) * w], pair[0].0, pair[1].0);
                }
            }
        }
    }
    let full = (SUBSAMPLES * SUBSAMPLES) as u32;
    coverage.iter().map(|&c| ((c as u32 * 255 + full / 2) / full) as u8).collect()
}

/// Counts the sub-sample columns inside `[from, to)` into one row of coverage counters.
fn add_span(row: &mut [u16], from: f32, to: f32) {
    let samples = SUBSAMPLES as f32;
    let limit = (row.len() * SUBSAMPLES) as f32;
    let first = (from * samples - 0.5).ceil().clamp(0.0, limit) as usize;
    let last = (to * samples - 0.5).ceil().clamp(0.0, limit) as usize;
    for sample in first..last {
        row[sample / SUBSAMPLES] += 1;
    }
}

/// Coverage (0..=255) of the closed lasso over `bbox`, one byte per pixel, rasterised with Direct2D antialiasing.
pub fn rasterize_mask(gfx: &Rc<Gfx>, points: &[PointF], bbox: RectI) -> Result<Vec<u8>> {
    let local: Vec<PointF> = points.iter().map(|p| PointF::new(p.x - bbox.x as f32, p.y - bbox.y as f32)).collect();
    let mut builder = PathBuilder::new();
    smooth_path(&mut builder, &local, true);
    let path = builder.build(gfx).context("building the lasso path")?;
    let spec = OffscreenSpec::pixels(bbox.w as u32, bbox.h as u32, 1.0, Theme::dark());
    let coverage = render_offscreen(gfx, &spec, |_, p| p.fill_path(&path, Color::WHITE))?;
    Ok(coverage.data.chunks_exact(4).map(|px| px[3]).collect())
}

/// Multiplies the image's alpha by `mask` (same pixel count).
pub fn apply_mask(image: &mut Image, mask: &[u8]) {
    for (px, &coverage) in image.data.chunks_exact_mut(4).zip(mask) {
        px[3] = ((px[3] as u32 * coverage as u32 + 127) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_covers_the_points_inside_the_monitor() {
        let monitor = RectI::new(0, 0, 200, 100);
        let mut lasso = Lasso::begin(monitor, PointF::new(10.0, 10.0));
        lasso.add(PointF::new(60.0, 12.0));
        lasso.add(PointF::new(61.0, 12.0));
        lasso.add(PointF::new(300.0, 80.0));
        lasso.add(PointF::new(20.0, 70.0));
        assert_eq!(lasso.points().len(), 4, "samples closer than the step are dropped");
        assert_eq!(lasso.bbox(), RectI::from_ltrb(10, 10, 200, 81));
        assert!(lasso.is_meaningful());
    }

    #[test]
    fn a_scribble_without_area_is_not_meaningful() {
        let mut lasso = Lasso::begin(RectI::new(0, 0, 100, 100), PointF::new(10.0, 10.0));
        lasso.add(PointF::new(50.0, 11.0));
        lasso.add(PointF::new(90.0, 12.0));
        assert!(!lasso.is_meaningful());
    }

    #[test]
    fn mask_multiplies_alpha() {
        let mut image = Image::from_bgra(3, 1, vec![9, 9, 9, 255, 9, 9, 9, 255, 9, 9, 9, 128]);
        apply_mask(&mut image, &[255, 0, 128]);
        assert_eq!([image.data[3], image.data[7], image.data[11]], [255, 0, 64]);
        assert_eq!(&image.data[..3], &[9, 9, 9], "colors stay straight");
    }

    fn dense_triangle() -> Vec<PointF> {
        let corners = [PointF::new(10.5, 10.5), PointF::new(90.5, 10.5), PointF::new(10.5, 90.5)];
        (0..3)
            .flat_map(|i| {
                let (a, b) = (corners[i], corners[(i + 1) % 3]);
                (0..20).map(move |k| {
                    let t = k as f32 / 20.0;
                    PointF::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
                })
            })
            .collect()
    }

    #[test]
    fn cpu_mask_matches_the_gpu_mask() {
        let triangle = dense_triangle();
        let bbox = points_bbox(&triangle).unwrap();
        let cpu = rasterize_mask_cpu(&triangle, bbox);
        let at = |x: i32, y: i32| cpu[((y - bbox.y) * bbox.w + (x - bbox.x)) as usize];
        assert_eq!(cpu.len(), (bbox.w * bbox.h) as usize);
        assert_eq!(at(20, 20), 255);
        assert_eq!(at(85, 85), 0);
        let edge = at(50, 50);
        assert!(edge > 64 && edge < 192, "supersampled edge, got {edge}");
        let gpu = rasterize_mask(&Gfx::new().unwrap(), &triangle, bbox).unwrap();
        let off = cpu.iter().zip(&gpu).filter(|(a, b)| (**a as i32 - **b as i32).abs() > 48).count();
        assert!(off * 200 < cpu.len(), "{off} of {} pixels differ noticeably", cpu.len());
    }

    #[test]
    fn rasterized_mask_is_opaque_inside_clear_outside_and_soft_on_the_edge() {
        let gfx = Gfx::new().expect("graphics device");
        let corners = [PointF::new(10.5, 10.5), PointF::new(90.5, 10.5), PointF::new(10.5, 90.5)];
        let triangle: Vec<PointF> = (0..3)
            .flat_map(|i| {
                let (a, b) = (corners[i], corners[(i + 1) % 3]);
                (0..20).map(move |k| {
                    let t = k as f32 / 20.0;
                    PointF::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
                })
            })
            .collect();
        let bbox = points_bbox(&triangle).unwrap();
        let mask = rasterize_mask(&gfx, &triangle, bbox).unwrap();
        let at = |x: i32, y: i32| mask[((y - bbox.y) * bbox.w + (x - bbox.x)) as usize];
        assert_eq!(mask.len(), (bbox.w * bbox.h) as usize);
        assert_eq!(at(20, 20), 255);
        assert_eq!(at(85, 85), 0);
        let edge = at(50, 50);
        assert!(edge > 0 && edge < 255, "antialiased edge, got {edge}");
    }
}
