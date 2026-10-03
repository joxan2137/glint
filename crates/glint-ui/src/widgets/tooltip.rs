use std::time::Duration;

use glint_core::{RectF, SizeF};

use crate::anim::{Animated, now};
use crate::color::Color;
use crate::painter::Painter;
use crate::text::TextStyle;
use crate::theme::Shadow;

const HEIGHT: f32 = 24.0;
const PADDING: f32 = 8.0;
const GAP: f32 = 8.0;
const MARGIN: f32 = 8.0;
const SHORTCUT_GAP: f32 = 10.0;
const WARM_WINDOW: f64 = 0.6;

fn label_style() -> TextStyle {
    TextStyle::new(12.0)
}

/// Hover tooltip: name plus optional key hint, after a 500 ms delay (immediate while another tooltip was just
/// visible), 140 ms fade. Windows own one and paint it above the view; views trigger it via `Ctx::show_tooltip`.
#[derive(Clone, Debug)]
pub struct Tooltip {
    text: String,
    shortcut: Option<String>,
    anchor: RectF,
    bounds: SizeF,
    reveal_at: f64,
    active: bool,
    opacity: Animated<f32>,
    last_visible_at: f64,
}

impl Default for Tooltip {
    fn default() -> Self {
        Self::new()
    }
}

impl Tooltip {
    pub const DELAY: Duration = Duration::from_millis(500);

    pub fn new() -> Self {
        Self {
            text: String::new(),
            shortcut: None,
            anchor: RectF::default(),
            bounds: SizeF::default(),
            reveal_at: 0.0,
            active: false,
            opacity: Animated::fade(0.0),
            last_visible_at: f64::NEG_INFINITY,
        }
    }

    /// Starts (or retargets) the tooltip. Returns the delay until it should be repainted, or None when it shows
    /// immediately.
    pub fn request(&mut self, anchor: RectF, text: &str, shortcut: Option<&str>, bounds: SizeF) -> Option<Duration> {
        let t = now();
        let warm = self.opacity.target() > 0.0 || t - self.last_visible_at < WARM_WINDOW;
        self.text = text.to_string();
        self.shortcut = shortcut.map(str::to_string);
        self.anchor = anchor;
        self.bounds = bounds;
        self.active = true;
        if warm {
            self.reveal_at = t;
            self.opacity.set(1.0);
            None
        } else {
            self.reveal_at = t + Self::DELAY.as_secs_f64();
            Some(Self::DELAY)
        }
    }

    pub fn anchor(&self) -> RectF {
        self.anchor
    }

    /// Hides it; returns true if it was visible.
    pub fn dismiss(&mut self) -> bool {
        let visible = self.opacity.target() > 0.0;
        if visible {
            self.last_visible_at = now();
        }
        self.active = false;
        self.opacity.set(0.0);
        visible
    }

    pub fn paint(&mut self, p: &mut Painter) {
        if self.active && now() >= self.reveal_at && self.opacity.target() < 1.0 {
            self.opacity.set(1.0);
        }
        let opacity = self.opacity.get();
        if opacity <= 0.001 {
            return;
        }
        if self.active {
            self.last_visible_at = now();
        }
        Self::paint_bubble(p, self.anchor, &self.text, self.shortcut.as_deref(), opacity, self.bounds);
    }

    /// Size of the bubble for `text` and `shortcut`.
    pub fn measure(p: &Painter, text: &str, shortcut: Option<&str>) -> SizeF {
        let style = label_style();
        let mut width = p.measure(text, &style).w;
        if let Some(key) = shortcut {
            width += SHORTCUT_GAP + p.measure(key, &style).w;
        }
        SizeF::new((width + 2.0 * PADDING).ceil(), HEIGHT)
    }

    /// Where the bubble goes: centered under the anchor, above it if there is no room, clamped to `bounds`.
    pub fn frame(p: &Painter, anchor: RectF, text: &str, shortcut: Option<&str>, bounds: SizeF) -> RectF {
        let size = Self::measure(p, text, shortcut);
        let mut x = anchor.center().x - size.w / 2.0;
        let mut y = anchor.bottom() + GAP;
        if y + size.h > bounds.h - MARGIN {
            y = anchor.y - GAP - size.h;
        }
        x = x.clamp(MARGIN, (bounds.w - MARGIN - size.w).max(MARGIN));
        p.snap_rect(RectF::new(x, y, size.w, size.h))
    }

    pub fn paint_bubble(p: &mut Painter, anchor: RectF, text: &str, shortcut: Option<&str>, opacity: f32, bounds: SizeF) {
        let frame = Self::frame(p, anchor, text, shortcut, bounds);
        let theme = p.theme().clone();
        let style = label_style();
        let radius = 7.0;
        p.layer(opacity, |p| {
            let shadow = Shadow::new(4.0, 14.0, Color::rgba(0.0, 0.0, 0.0, if theme.is_dark() { 0.35 } else { 0.16 }));
            p.shadow_outside(frame, radius, &shadow);
            p.fill_round_rect(frame, radius, theme.glass_fill_solid.with_alpha(0.96));
            p.hairline_round_rect(frame, radius, theme.hairline, true);
            p.hairline_round_rect(frame, radius, theme.outer_border, false);
            let text_rect = RectF::new(frame.x + PADDING, frame.y, frame.w - 2.0 * PADDING, frame.h);
            let width = p.text(text, &style, theme.text, text_rect);
            if let Some(key) = shortcut {
                let key_rect = RectF::new(text_rect.x + width + SHORTCUT_GAP, frame.y, text_rect.w, frame.h);
                p.text(key, &style, theme.text_secondary, key_rect);
            }
        });
    }
}
