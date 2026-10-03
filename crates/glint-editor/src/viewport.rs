//! Canvas zoom and pan. `Mapping` is the pure image-pixel → DIP transform; `Viewport` animates between mappings
//! by springing the scale about the mapping pair's fixed point, so zooming around the cursor keeps the pixel under
//! it exactly in place for the whole animation.

use glint_core::{PointF, RectF, SizeF};
use glint_ui::Animated;

use crate::math::{Vec2, clamp_safe};

/// Physical pixels per image pixel.
pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 32.0;
const STEPS: [f32; 17] =
    [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 32.0];

/// `view = origin + image × scale`; scale is DIPs per image pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mapping {
    pub scale: f32,
    pub origin: PointF,
}

impl Default for Mapping {
    fn default() -> Self {
        Self { scale: 1.0, origin: PointF::default() }
    }
}

impl Mapping {
    pub fn to_view(self, p: PointF) -> PointF {
        self.origin.plus(p.times(self.scale))
    }

    pub fn to_image(self, v: PointF) -> PointF {
        v.minus(self.origin).times(1.0 / self.scale)
    }

    pub fn rect_to_view(self, r: RectF) -> RectF {
        let o = self.to_view(PointF::new(r.x, r.y));
        RectF::new(o.x, o.y, r.w * self.scale, r.h * self.scale)
    }
}

/// Largest scale that shows `content` whole inside `area`, never above `max_scale` (100 %).
pub fn fit_scale(content: SizeF, area: RectF, max_scale: f32) -> f32 {
    if content.w <= 0.0 || content.h <= 0.0 {
        return max_scale;
    }
    (area.w / content.w).min(area.h / content.h).min(max_scale).max(1e-4)
}

/// `content` (image px) centered in `area` at `scale`.
pub fn centered(content: RectF, scale: f32, area: RectF) -> Mapping {
    let c = area.center();
    Mapping { scale, origin: PointF::new(c.x - content.center().x * scale, c.y - content.center().y * scale) }
}

/// The mapping at `scale` that keeps the image point under `anchor` (view DIP) where it is.
pub fn zoom_about(m: Mapping, anchor: PointF, scale: f32) -> Mapping {
    let image = m.to_image(anchor);
    Mapping { scale, origin: anchor.minus(image.times(scale)) }
}

/// Keeps content that fits centered and content that overflows covering `area` edge to edge.
pub fn clamp(m: Mapping, content: RectF, area: RectF) -> Mapping {
    let view = m.rect_to_view(content);
    let axis = |origin: f32, start: f32, length: f32, area_start: f32, area_length: f32| {
        if length <= area_length {
            area_start + (area_length - length) / 2.0 - start * m.scale
        } else {
            let lo = area_start + area_length - length - start * m.scale;
            let hi = area_start - start * m.scale;
            clamp_safe(origin, lo, hi)
        }
    };
    Mapping {
        scale: m.scale,
        origin: PointF::new(
            axis(m.origin.x, content.x, view.w, area.x, area.w),
            axis(m.origin.y, content.y, view.h, area.y, area.h),
        ),
    }
}

/// The view point both mappings send the same image point to, if the scales differ.
pub fn fixed_point(a: Mapping, b: Mapping) -> Option<PointF> {
    let ds = b.scale - a.scale;
    if ds.abs() < 1e-6 {
        return None;
    }
    let p = a.origin.minus(b.origin).times(1.0 / ds);
    Some(a.to_view(p))
}

/// Next preset zoom (physical px per image px) above or below `zoom`.
pub fn step_zoom(zoom: f32, up: bool) -> f32 {
    let next = if up {
        STEPS.iter().copied().find(|s| *s > zoom * 1.001)
    } else {
        STEPS.iter().rev().copied().find(|s| *s < zoom * 0.999)
    };
    next.unwrap_or(if up { MAX_ZOOM } else { MIN_ZOOM })
}

#[derive(Clone, Debug)]
pub struct Viewport {
    scale: Animated<f32>,
    origin: Animated<PointF>,
    /// (image point, view point) held together while the scale animates.
    pivot: Option<(PointF, PointF)>,
    target: Mapping,
    /// Follow window resizes by re-fitting (until the user zooms or pans).
    pub fit: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new(Mapping::default())
    }
}

impl Viewport {
    pub fn new(m: Mapping) -> Self {
        Self {
            scale: Animated::new(m.scale),
            origin: Animated::new(m.origin),
            pivot: None,
            target: m,
            fit: true,
        }
    }

    pub fn mapping(&self) -> Mapping {
        let scale = self.scale.get();
        match self.pivot {
            Some((image, view)) if self.scale.is_animating() => {
                Mapping { scale, origin: view.minus(image.times(scale)) }
            }
            Some(_) => self.target,
            None => Mapping { scale, origin: self.origin.get() },
        }
    }

    pub fn target(&self) -> Mapping {
        self.target
    }

    pub fn is_animating(&self) -> bool {
        self.scale.is_animating() || self.origin.is_animating()
    }

    pub fn snap_to(&mut self, m: Mapping) {
        self.scale.snap(m.scale);
        self.origin.snap(m.origin);
        self.pivot = None;
        self.target = m;
    }

    /// Springs to `m`: a zoom about the fixed point when the scale changes, a plain glide otherwise.
    pub fn animate_to(&mut self, m: Mapping) {
        let current = self.mapping();
        self.target = m;
        match fixed_point(current, m).filter(|q| q.x.abs() < 1.0e5 && q.y.abs() < 1.0e5) {
            Some(view) => {
                self.pivot = Some((current.to_image(view), view));
                self.scale.snap(current.scale);
                self.scale.set(m.scale);
                self.origin.snap(m.origin);
            }
            None => {
                self.pivot = None;
                self.scale.snap(current.scale);
                self.scale.set(m.scale);
                self.origin.snap(current.origin);
                self.origin.set(m.origin);
            }
        }
    }

    /// Moves the content by `delta` DIP immediately (scroll, trackpad, Space+drag), even mid-animation.
    pub fn pan(&mut self, delta: PointF, content: RectF, area: RectF) {
        if let Some((image, view)) = self.pivot
            && self.scale.is_animating()
        {
            self.pivot = Some((image, view.plus(delta)));
            self.target = clamp(Mapping { scale: self.target.scale, origin: self.target.origin.plus(delta) }, content, area);
            return;
        }
        let current = self.mapping();
        let moved = Mapping { scale: current.scale, origin: current.origin.plus(delta) };
        self.snap_to(clamp(moved, content, area));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_round_trips() {
        let m = Mapping { scale: 0.5, origin: PointF::new(10.0, 20.0) };
        let p = PointF::new(300.0, 120.0);
        let back = m.to_image(m.to_view(p));
        assert!(back.distance(p) < 1e-4);
        assert_eq!(m.rect_to_view(RectF::new(0.0, 0.0, 100.0, 50.0)), RectF::new(10.0, 20.0, 50.0, 25.0));
    }

    #[test]
    fn zoom_about_keeps_the_cursor_pixel() {
        let m = Mapping { scale: 0.75, origin: PointF::new(-40.0, 12.0) };
        let cursor = PointF::new(333.0, 222.0);
        let under = m.to_image(cursor);
        let zoomed = zoom_about(m, cursor, 2.5);
        assert!(zoomed.to_view(under).distance(cursor) < 1e-3);
    }

    #[test]
    fn fixed_point_is_shared_by_both_mappings() {
        let a = Mapping { scale: 0.5, origin: PointF::new(100.0, 50.0) };
        let b = zoom_about(a, PointF::new(400.0, 300.0), 2.0);
        let q = fixed_point(a, b).unwrap();
        assert!(q.distance(PointF::new(400.0, 300.0)) < 1e-3);
        assert!(fixed_point(a, a).is_none());
    }

    #[test]
    fn pivot_animation_holds_the_anchor_throughout() {
        glint_ui::anim::set_reduced_motion(false);
        glint_ui::anim::set_clock(0.0);
        let start = Mapping { scale: 0.5, origin: PointF::new(100.0, 50.0) };
        let mut vp = Viewport::new(start);
        let anchor = PointF::new(300.0, 200.0);
        let pixel = start.to_image(anchor);
        vp.animate_to(zoom_about(start, anchor, 3.0));
        for i in 1..40 {
            glint_ui::anim::set_clock(i as f64 / 120.0);
            assert!(vp.mapping().to_view(pixel).distance(anchor) < 1e-2);
        }
        glint_ui::anim::set_clock(5.0);
        assert_eq!(vp.mapping().scale, 3.0);
    }

    #[test]
    fn fit_never_upscales_past_actual_size() {
        let area = RectF::new(0.0, 0.0, 1000.0, 800.0);
        assert_eq!(fit_scale(SizeF::new(200.0, 100.0), area, 0.5), 0.5);
        assert_eq!(fit_scale(SizeF::new(4000.0, 1000.0), area, 0.5), 0.25);
    }

    #[test]
    fn clamp_centers_small_and_bounds_large_content() {
        let area = RectF::new(0.0, 0.0, 100.0, 100.0);
        let content = RectF::new(0.0, 0.0, 50.0, 50.0);
        let small = clamp(Mapping { scale: 1.0, origin: PointF::new(-500.0, 9.0) }, content, area);
        assert_eq!(small.origin, PointF::new(25.0, 25.0));
        let big = clamp(Mapping { scale: 4.0, origin: PointF::new(50.0, -500.0) }, content, area);
        assert_eq!(big.origin, PointF::new(0.0, -100.0));
        let cropped = RectF::new(20.0, 20.0, 10.0, 10.0);
        let c = clamp(Mapping { scale: 2.0, origin: PointF::default() }, cropped, area);
        assert!(c.rect_to_view(cropped).center().distance(area.center()) < 1e-4);
    }

    #[test]
    fn zoom_steps() {
        assert_eq!(step_zoom(1.0, true), 1.25);
        assert_eq!(step_zoom(1.0, false), 0.75);
        assert_eq!(step_zoom(0.9, true), 1.0);
        assert_eq!(step_zoom(32.0, true), MAX_ZOOM);
        assert_eq!(step_zoom(0.1, false), MIN_ZOOM);
    }
}
