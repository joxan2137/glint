//! Shared pieces of the bottom-right popups (floating thumbnail and toasts): placement, card chrome, the slide
//! motion and the pausable auto-dismiss countdown.

use std::time::Duration;

use glint_ui::{
    Animated, Color, CubicBezier, Ctx, Motion, Painter, PointI, RectF, RectI, Shadow, SizeF, Theme, TimerId, Tween,
};

/// Distance between the card and the work-area edges (DESIGN §7).
pub const EDGE_MARGIN: f32 = 16.0;
/// Transparent room left of and above the card so its shadow is not clipped.
pub const SHADOW_MARGIN: f32 = 28.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupLayout {
    /// Window origin in physical pixels (virtual desktop).
    pub origin_px: PointI,
    /// Window size in DIP.
    pub size: SizeF,
    /// The card inside the window, in DIP.
    pub card: RectF,
}

/// A card of `card` DIP placed `EDGE_MARGIN` from the bottom-right corner of `work_px`, raised by `lift_px`.
pub fn corner_layout(card: SizeF, work_px: RectI, scale: f32, lift_px: i32) -> PopupLayout {
    let size = SizeF::new(SHADOW_MARGIN + card.w + EDGE_MARGIN, SHADOW_MARGIN + card.h + EDGE_MARGIN);
    let width_px = (size.w * scale).round() as i32;
    let height_px = (size.h * scale).round() as i32;
    PopupLayout {
        origin_px: PointI::new(work_px.right() - width_px, work_px.bottom() - height_px - lift_px),
        size,
        card: RectF::new(SHADOW_MARGIN, SHADOW_MARGIN, card.w, card.h),
    }
}

/// Physical-pixel height a popup occupies above the work-area bottom (for stacking a toast over the thumbnail).
pub fn occupied_height_px(layout: &PopupLayout, scale: f32) -> i32 {
    ((layout.card.h + EDGE_MARGIN) * scale).round() as i32
}

/// Soft two-layer card shadow, lighter than the glass toolbar shadow so small popups stay calm.
pub fn paint_card_shadow(p: &mut Painter, card: RectF, radius: f32) {
    let strength = if p.theme().is_dark() { 1.0 } else { 0.55 };
    for shadow in [
        Shadow::new(6.0, 20.0, Color::rgba(0.0, 0.0, 0.0, 0.34 * strength)),
        Shadow::new(1.0, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.22 * strength)),
    ] {
        p.shadow(card, radius, &shadow);
    }
}

/// Inner light and outer dark hairlines that keep a card's edge visible on any background.
pub fn paint_card_edges(p: &mut Painter, card: RectF, radius: f32, theme: &Theme) {
    let inner = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.16) } else { Color::rgba(1.0, 1.0, 1.0, 0.55) };
    let outer = if theme.is_dark() { Color::rgba(0.0, 0.0, 0.0, 0.5) } else { Color::rgba(0.0, 0.0, 0.0, 0.14) };
    p.hairline_round_rect(card, radius, inner, true);
    p.hairline_round_rect(card, radius, outer, false);
}

/// Horizontal offset of a popup's content: slides in from beyond the right edge with the default spring and leaves
/// with a short ease-in.
pub struct Slide {
    offset: Animated<f32>,
    hidden: f32,
}

const EXIT: Motion = Motion::Tween(Tween { duration: 0.2, curve: CubicBezier { x1: 0.4, y1: 0.0, x2: 1.0, y2: 1.0 } });
pub const EXIT_DURATION: Duration = Duration::from_millis(210);

impl Slide {
    /// Starts hidden `hidden` DIP to the right.
    pub fn new(hidden: f32) -> Self {
        Self { offset: Animated::new(hidden), hidden }
    }

    /// Already in place (previews).
    pub fn settled() -> Self {
        Self { offset: Animated::new(0.0), hidden: 0.0 }
    }

    pub fn enter(&mut self) {
        self.offset.set_with(0.0, Motion::default());
    }

    pub fn leave(&mut self) {
        let hidden = self.hidden.max(1.0);
        self.offset.set_with(hidden, EXIT);
    }

    /// Follows a finger or mouse during a swipe; dragging left meets resistance.
    pub fn follow(&mut self, dx: f32) {
        self.offset.snap(if dx >= 0.0 { dx } else { dx * 0.2 });
    }

    pub fn spring_back(&mut self) {
        self.offset.set_with(0.0, Motion::default());
    }

    pub fn offset(&self) -> f32 {
        self.offset.get()
    }

    /// 1 when in place, fading toward 0 as the content leaves.
    pub fn opacity(&self) -> f32 {
        if self.hidden <= 0.0 { 1.0 } else { (1.0 - self.offset.get() / self.hidden).clamp(0.0, 1.0).powf(0.6) }
    }
}

/// Auto-dismiss that pauses while the pointer is over the popup and continues with the time that was left.
pub struct AutoDismiss {
    remaining: f64,
    resumed_at: Option<f64>,
    timer: Option<TimerId>,
    token: u64,
}

/// Never resume with less than this, so leaving the popup does not make it vanish instantly.
const MIN_REMAINING: f64 = 1.5;

impl AutoDismiss {
    pub fn new(after: Duration, token: u64) -> Self {
        Self { remaining: after.as_secs_f64(), resumed_at: None, timer: None, token }
    }

    pub fn resume(&mut self, cx: &mut Ctx) {
        if self.timer.is_some() {
            return;
        }
        self.timer = Some(cx.set_timer(Duration::from_secs_f64(self.remaining.max(0.0)), self.token));
        self.resumed_at = Some(cx.time());
    }

    pub fn pause(&mut self, cx: &mut Ctx) {
        if let Some(timer) = self.timer.take() {
            cx.cancel_timer(timer);
        }
        if let Some(started) = self.resumed_at.take() {
            self.remaining = (self.remaining - (cx.time() - started)).max(MIN_REMAINING);
        }
    }

    /// Call when `Event::Timer(token)` arrives; true when the popup should go.
    pub fn fired(&mut self, token: u64) -> bool {
        if token != self.token || self.timer.is_none() {
            return false;
        }
        self.timer = None;
        self.resumed_at = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_layout_hugs_the_work_area_corner() {
        let work = RectI::new(0, 0, 2560, 1400);
        let layout = corner_layout(SizeF::new(200.0, 150.0), work, 1.0, 0);
        assert_eq!(layout.size, SizeF::new(244.0, 194.0));
        assert_eq!(layout.origin_px, PointI::new(2560 - 244, 1400 - 194));
        let card_right_px = layout.origin_px.x as f32 + layout.card.right();
        let card_bottom_px = layout.origin_px.y as f32 + layout.card.bottom();
        assert_eq!((card_right_px, card_bottom_px), (2560.0 - EDGE_MARGIN, 1400.0 - EDGE_MARGIN));
    }

    #[test]
    fn corner_layout_scales_and_stacks() {
        let work = RectI::new(-1920, 200, 1920, 1040);
        let scale = 1.5;
        let thumb = corner_layout(SizeF::new(200.0, 112.5), work, scale, 0);
        assert_eq!(thumb.origin_px.x, -((244.0 * scale) as i32));
        let lift = occupied_height_px(&thumb, scale);
        assert_eq!(lift, ((112.5 + EDGE_MARGIN) * scale).round() as i32);
        let toast = corner_layout(SizeF::new(280.0, 56.0), work, scale, lift);
        let toast_bottom = toast.origin_px.y as f32 + (toast.card.bottom() + EDGE_MARGIN) * scale;
        let thumb_card_top = thumb.origin_px.y as f32 + thumb.card.y * scale;
        assert!(toast_bottom <= thumb_card_top + 1.0, "toast {toast_bottom} sits above the thumbnail {thumb_card_top}");
    }
}
