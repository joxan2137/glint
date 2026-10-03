//! Pure selection math in physical pixels (virtual-desktop coordinates) and DIP placement helpers.

use glint_core::{PointF, PointI, RectF, RectI, SizeF, WindowInfo};

/// Selections smaller than this on either side are treated as a click.
pub const MIN_SELECTION_PX: i32 = 4;

/// Maps one overlay window's DIP client space to virtual-desktop physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Space {
    pub origin: PointI,
    pub scale: f32,
}

impl Space {
    pub fn new(monitor_rect: RectI, scale: f32) -> Self {
        Self { origin: PointI::new(monitor_rect.x, monitor_rect.y), scale }
    }

    pub fn point_to_dip(&self, p: PointF) -> PointF {
        PointF::new((p.x - self.origin.x as f32) / self.scale, (p.y - self.origin.y as f32) / self.scale)
    }

    pub fn to_dip(self, p: PointI) -> PointF {
        self.point_to_dip(PointF::new(p.x as f32, p.y as f32))
    }

    pub fn rect_to_dip(&self, r: RectI) -> RectF {
        let origin = self.to_dip(PointI::new(r.x, r.y));
        RectF::new(origin.x, origin.y, r.w as f32 / self.scale, r.h as f32 / self.scale)
    }

    /// DIP to physical px without rounding (for smooth lasso samples).
    pub fn to_px_f(self, p: PointF) -> PointF {
        PointF::new(self.origin.x as f32 + p.x * self.scale, self.origin.y as f32 + p.y * self.scale)
    }

    pub fn to_px(self, p: PointF) -> PointI {
        PointI::new(self.origin.x + (p.x * self.scale).floor() as i32, self.origin.y + (p.y * self.scale).floor() as i32)
    }
}

/// A point snapped into `bounds`, where the last pixel row/column counts as the far edge so a drag can reach it.
fn clamp_to_edges(p: PointI, bounds: RectI) -> PointI {
    let axis = |v: i32, lo: i32, hi: i32| if v >= hi - 1 { hi } else { v.max(lo) };
    PointI::new(axis(p.x, bounds.x, bounds.right()), axis(p.y, bounds.y, bounds.bottom()))
}

/// A rubber-band selection confined to the monitor it started on.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub bounds: RectI,
    anchor: PointI,
    corner: PointI,
    pointer: PointI,
}

impl Selection {
    pub fn begin(bounds: RectI, at: PointI) -> Self {
        let start = clamp_to_edges(at, bounds);
        Self { bounds, anchor: start, corner: start, pointer: at }
    }

    /// Follows the pointer. `square` constrains to a square, `moving` (Space held) translates the whole selection.
    pub fn drag_to(&mut self, pointer: PointI, square: bool, moving: bool) {
        if moving {
            let rect = self.rect();
            let dx = (pointer.x - self.pointer.x).clamp(self.bounds.x - rect.x, self.bounds.right() - rect.right());
            let dy = (pointer.y - self.pointer.y).clamp(self.bounds.y - rect.y, self.bounds.bottom() - rect.bottom());
            self.anchor = PointI::new(self.anchor.x + dx, self.anchor.y + dy);
            self.corner = PointI::new(self.corner.x + dx, self.corner.y + dy);
        } else {
            let target = clamp_to_edges(pointer, self.bounds);
            self.corner = if square { self.squared(target) } else { target };
        }
        self.pointer = pointer;
    }

    /// Re-applies the pointer after a modifier change (Shift pressed or released without moving).
    pub fn refresh(&mut self, square: bool) {
        self.drag_to(self.pointer, square, false);
    }

    fn squared(&self, target: PointI) -> PointI {
        let (dx, dy) = (target.x - self.anchor.x, target.y - self.anchor.y);
        let (sx, sy) = (if dx < 0 { -1 } else { 1 }, if dy < 0 { -1 } else { 1 });
        let room_x = if sx > 0 { self.bounds.right() - self.anchor.x } else { self.anchor.x - self.bounds.x };
        let room_y = if sy > 0 { self.bounds.bottom() - self.anchor.y } else { self.anchor.y - self.bounds.y };
        let side = dx.abs().max(dy.abs()).min(room_x).min(room_y);
        PointI::new(self.anchor.x + sx * side, self.anchor.y + sy * side)
    }

    pub fn rect(&self) -> RectI {
        RectI::from_points(self.anchor, self.corner)
    }

    /// The corner that follows the pointer.
    pub fn corner(&self) -> PointI {
        self.corner
    }

    pub fn anchor(&self) -> PointI {
        self.anchor
    }

    pub fn is_meaningful(&self) -> bool {
        is_meaningful(self.rect())
    }
}

pub fn is_meaningful(rect: RectI) -> bool {
    rect.w >= MIN_SELECTION_PX && rect.h >= MIN_SELECTION_PX
}

/// Bounding box of all monitor rects.
pub fn virtual_bounds(monitors: impl IntoIterator<Item = RectI>) -> RectI {
    monitors.into_iter().reduce(|a, b| a.union(&b)).unwrap_or_default()
}

pub fn monitor_at(monitors: &[RectI], p: PointI) -> Option<usize> {
    monitors.iter().position(|m| m.contains(p))
}

/// Index of the monitor with the largest overlap with `rect`.
pub fn largest_overlap(monitors: &[RectI], rect: RectI) -> Option<usize> {
    monitors
        .iter()
        .enumerate()
        .filter_map(|(i, m)| m.intersect(&rect).map(|r| (i, r.w as i64 * r.h as i64)))
        .max_by_key(|&(i, area)| (area, std::cmp::Reverse(i)))
        .map(|(i, _)| i)
}

/// Rect of the topmost snapshot window under `p`, clipped to the virtual desktop.
pub fn hovered_window(windows: &[WindowInfo], desktop: RectI, p: PointI) -> Option<RectI> {
    glint_capture::window_at(windows, p).and_then(|w| w.rect.intersect(&desktop))
}

fn fits(r: RectF, bounds: RectF) -> bool {
    r.x >= bounds.x && r.y >= bounds.y && r.right() <= bounds.right() && r.bottom() <= bounds.bottom()
}

fn overlaps(a: RectF, b: RectF) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

fn clamp_into(r: RectF, bounds: RectF) -> RectF {
    let x = r.x.min(bounds.right() - r.w).max(bounds.x);
    let y = r.y.min(bounds.bottom() - r.h).max(bounds.y);
    RectF::new(x, y, r.w, r.h)
}

/// Places a `size` box centered under `target` (`gap` away), else above it, else inside its bottom edge; always
/// within `bounds`, preferring spots that do not cover `avoid`.
pub fn place_near(target: RectF, size: SizeF, gap: f32, bounds: RectF, avoid: Option<RectF>) -> RectF {
    let x = target.center().x - size.w / 2.0;
    let candidates = [
        RectF::new(x, target.bottom() + gap, size.w, size.h),
        RectF::new(x, target.y - gap - size.h, size.w, size.h),
        RectF::new(x, target.bottom() - gap - size.h, size.w, size.h),
    ];
    let clear = |r: &RectF| avoid.is_none_or(|a| !overlaps(*r, a));
    let in_bounds = |r: &RectF| fits(RectF::new(bounds.x, r.y, r.w, r.h), bounds);
    candidates
        .iter()
        .find(|r| in_bounds(r) && clear(&clamp_into(**r, bounds)))
        .or_else(|| candidates.iter().find(|r| in_bounds(r)))
        .map(|r| clamp_into(*r, bounds))
        .unwrap_or_else(|| clamp_into(candidates[0], bounds))
}

/// Picks the side (+1/-1) along one axis: `preferred` if the span fits, else the other, else the roomier one.
fn side(cursor: f32, preferred: f32, reach: f32, before: f32, after: f32, lo: f32, hi: f32) -> f32 {
    let fits = |s: f32| {
        let c = cursor + s * reach;
        c - before >= lo && c + after <= hi
    };
    if fits(preferred) {
        preferred
    } else if fits(-preferred) {
        -preferred
    } else if cursor - lo > hi - cursor {
        -1.0
    } else {
        1.0
    }
}

/// Center of a circle of `diameter` beside `cursor`, `offset` away diagonally, on the side away from `away_from`
/// (the selection anchor) when given, flipped (and finally clamped) to stay inside `bounds` with `below` extra room
/// under the circle.
pub fn magnifier_center(cursor: PointF, away_from: Option<PointF>, diameter: f32, offset: f32, below: f32, bounds: RectF) -> PointF {
    let reach = offset + diameter / 2.0;
    let half = diameter / 2.0;
    let prefer_x = away_from.map_or(1.0, |a| if cursor.x < a.x { -1.0 } else { 1.0 });
    let prefer_y = away_from.map_or(1.0, |a| if cursor.y < a.y { -1.0 } else { 1.0 });
    let sx = side(cursor.x, prefer_x, reach, half, half, bounds.x, bounds.right());
    let sy = side(cursor.y, prefer_y, reach, half, half + below, bounds.y, bounds.bottom());
    let x = (cursor.x + sx * reach).clamp(bounds.x + half, (bounds.right() - half).max(bounds.x + half));
    let y = (cursor.y + sy * reach).clamp(bounds.y + half, (bounds.bottom() - half - below).max(bounds.y + half));
    PointF::new(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: RectI = RectI::new(0, 0, 1920, 1080);

    fn window(rect: RectI, z: u32) -> WindowInfo {
        WindowInfo { hwnd: z as isize + 1, title: format!("w{z}"), class_name: "C".into(), process_id: 1, rect, z_order: z }
    }

    #[test]
    fn drag_selects_between_anchor_and_pointer_in_any_direction() {
        let mut s = Selection::begin(MONITOR, PointI::new(100, 200));
        s.drag_to(PointI::new(400, 500), false, false);
        assert_eq!(s.rect(), RectI::new(100, 200, 300, 300));
        s.drag_to(PointI::new(40, 50), false, false);
        assert_eq!(s.rect(), RectI::new(40, 50, 60, 150));
    }

    #[test]
    fn selection_is_clamped_to_its_monitor_and_reaches_the_far_edges() {
        let monitor = RectI::new(1920, 0, 2560, 1440);
        let mut s = Selection::begin(monitor, PointI::new(2000, 100));
        s.drag_to(PointI::new(9000, -50), false, false);
        assert_eq!(s.rect(), RectI::from_ltrb(2000, 0, 4480, 100));
        s.drag_to(PointI::new(4479, 1439), false, false);
        assert_eq!(s.rect(), RectI::from_ltrb(2000, 100, 4480, 1440), "last pixel counts as the edge");
        s.drag_to(PointI::new(0, 100), false, false);
        assert_eq!(s.rect().x, 1920, "dragging onto another monitor stops at this one");
    }

    #[test]
    fn shift_makes_a_square_that_still_fits() {
        let mut s = Selection::begin(MONITOR, PointI::new(100, 100));
        s.drag_to(PointI::new(400, 180), true, false);
        assert_eq!(s.rect(), RectI::new(100, 100, 300, 300));
        s.drag_to(PointI::new(20, 400), true, false);
        assert_eq!(s.rect(), RectI::new(0, 100, 100, 100), "limited by the room to the left");
        let mut corner = Selection::begin(MONITOR, PointI::new(1800, 1000));
        corner.drag_to(PointI::new(1900, 1079), true, false);
        assert_eq!(corner.rect(), RectI::new(1800, 1000, 80, 80), "limited by the bottom edge");
    }

    #[test]
    fn space_moves_the_selection_and_keeps_it_inside() {
        let mut s = Selection::begin(MONITOR, PointI::new(100, 100));
        s.drag_to(PointI::new(300, 200), false, false);
        s.drag_to(PointI::new(350, 260), false, true);
        assert_eq!(s.rect(), RectI::new(150, 160, 200, 100));
        s.drag_to(PointI::new(-5000, -5000), false, true);
        assert_eq!(s.rect(), RectI::new(0, 0, 200, 100));
        s.drag_to(PointI::new(5000, 5000), false, true);
        assert_eq!(s.rect(), RectI::new(1720, 980, 200, 100));
        s.drag_to(PointI::new(5010, 5010), false, false);
        assert_eq!(s.rect(), RectI::from_ltrb(1720, 980, 1920, 1080), "resizing resumes from the moved anchor");
    }

    #[test]
    fn tiny_selections_are_clicks() {
        let mut s = Selection::begin(MONITOR, PointI::new(10, 10));
        s.drag_to(PointI::new(13, 40), false, false);
        assert!(!s.is_meaningful());
        s.drag_to(PointI::new(14, 14), false, false);
        assert!(s.is_meaningful());
    }

    #[test]
    fn window_hit_testing_prefers_the_topmost_and_clips_to_the_desktop() {
        let windows = vec![
            window(RectI::new(100, 100, 300, 200), 0),
            window(RectI::new(0, 0, 1000, 800), 1),
            window(RectI::new(-200, 900, 600, 400), 2),
        ];
        let desktop = MONITOR;
        assert_eq!(hovered_window(&windows, desktop, PointI::new(150, 150)), Some(RectI::new(100, 100, 300, 200)));
        assert_eq!(hovered_window(&windows, desktop, PointI::new(50, 50)), Some(RectI::new(0, 0, 1000, 800)));
        assert_eq!(hovered_window(&windows, desktop, PointI::new(10, 1000)), Some(RectI::new(0, 900, 400, 180)));
        assert_eq!(hovered_window(&windows, desktop, PointI::new(1500, 100)), None);
    }

    #[test]
    fn largest_overlap_picks_the_majority_monitor() {
        let monitors = [RectI::new(0, 0, 1920, 1080), RectI::new(1920, 0, 2560, 1440)];
        assert_eq!(largest_overlap(&monitors, RectI::new(1800, 100, 400, 100)), Some(1));
        assert_eq!(largest_overlap(&monitors, RectI::new(1700, 100, 400, 100)), Some(0));
        assert_eq!(largest_overlap(&monitors, RectI::new(-500, -500, 10, 10)), None);
        assert_eq!(virtual_bounds(monitors), RectI::new(0, 0, 4480, 1440));
    }

    #[test]
    fn badge_goes_below_then_above_then_inside() {
        let bounds = RectF::new(0.0, 0.0, 1000.0, 600.0);
        let size = SizeF::new(100.0, 24.0);
        let below = place_near(RectF::new(100.0, 100.0, 200.0, 100.0), size, 8.0, bounds, None);
        assert_eq!(below, RectF::new(150.0, 208.0, 100.0, 24.0));
        let above = place_near(RectF::new(100.0, 400.0, 200.0, 195.0), size, 8.0, bounds, None);
        assert_eq!(above.y, 368.0);
        let inside = place_near(RectF::new(0.0, 0.0, 1000.0, 600.0), size, 8.0, bounds, None);
        assert_eq!(inside.y, 568.0);
        let clamped = place_near(RectF::new(-40.0, 100.0, 60.0, 50.0), size, 8.0, bounds, None);
        assert_eq!(clamped.x, 0.0);
        let avoided = place_near(RectF::new(100.0, 100.0, 200.0, 100.0), size, 8.0, bounds, Some(RectF::new(150.0, 210.0, 50.0, 50.0)));
        assert_eq!(avoided.y, 100.0 - 8.0 - 24.0, "moves above when the magnifier sits below");
    }

    #[test]
    fn magnifier_flips_away_from_edges_and_the_selection() {
        let bounds = RectF::new(0.0, 0.0, 1000.0, 800.0);
        let free = magnifier_center(PointF::new(500.0, 400.0), None, 112.0, 20.0, 30.0, bounds);
        assert!(free.x > 500.0 && free.y > 400.0);
        let corner = magnifier_center(PointF::new(990.0, 790.0), None, 112.0, 20.0, 30.0, bounds);
        assert!(corner.x < 990.0 && corner.y < 790.0);
        let dragging_left_up = magnifier_center(PointF::new(300.0, 300.0), Some(PointF::new(600.0, 600.0)), 112.0, 20.0, 30.0, bounds);
        assert!(dragging_left_up.x < 300.0 && dragging_left_up.y < 300.0, "stays outside the selection");
        let tight = magnifier_center(PointF::new(999.0, 799.0), Some(PointF::new(700.0, 500.0)), 112.0, 20.0, 32.0, bounds);
        assert!(tight.x + 56.0 <= 1000.0 && tight.y + 56.0 + 32.0 <= 800.0, "clamped inside: {tight:?}");
        assert!(tight.x < 999.0 && tight.y < 799.0);
    }

    #[test]
    fn space_maps_dips_to_physical_pixels() {
        let space = Space::new(RectI::new(-1920, 0, 1920, 1080), 1.5);
        assert_eq!(space.to_dip(PointI::new(-1920 + 300, 150)), PointF::new(200.0, 100.0));
        assert_eq!(space.to_px(PointF::new(200.0, 100.0)), PointI::new(-1620, 150));
        assert_eq!(space.rect_to_dip(RectI::new(-1920, 0, 300, 150)), RectF::new(0.0, 0.0, 200.0, 100.0));
    }
}
