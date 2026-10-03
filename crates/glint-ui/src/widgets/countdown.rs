use glint_core::{PointF, RectF};

use crate::anim::{Animated, Motion, Spring};
use crate::color::Color;
use crate::painter::Painter;
use crate::text::TextStyle;

/// Big countdown digit (64 DIP semibold, tabular). Each new value pops in (scale 1.25→1, fade in) while the
/// previous one shrinks and fades out.
#[derive(Clone, Debug)]
pub struct Countdown {
    value: Option<u32>,
    previous: Option<u32>,
    transition: Animated<f32>,
}

impl Default for Countdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Countdown {
    pub fn new() -> Self {
        Self { value: None, previous: None, transition: Animated::with_motion(1.0, Motion::Spring(Spring::DEFAULT)) }
    }

    pub fn value(&self) -> Option<u32> {
        self.value
    }

    pub fn set(&mut self, value: u32) {
        if self.value == Some(value) {
            return;
        }
        self.previous = self.value;
        self.value = Some(value);
        self.transition.snap(0.0);
        self.transition.set(1.0);
    }

    /// Sets the value with the transition at `progress` (0..1) for previews.
    pub fn force(&mut self, previous: Option<u32>, value: u32, progress: f32) {
        self.previous = previous;
        self.value = Some(value);
        self.transition.snap(progress);
    }

    pub fn clear(&mut self) {
        self.previous = self.value.take();
        self.transition.snap(0.0);
        self.transition.set(1.0);
    }

    pub fn style() -> TextStyle {
        TextStyle::countdown().centered()
    }

    pub fn paint(&self, p: &mut Painter, center: PointF, color: Color) {
        let t = self.transition.get();
        let style = Self::style();
        let box_rect = RectF::new(center.x - 60.0, center.y - 40.0, 120.0, 80.0);
        let draw = |p: &mut Painter, value: u32, opacity: f32, scale: f32| {
            if opacity <= 0.001 {
                return;
            }
            p.layer(opacity.clamp(0.0, 1.0), |p| {
                p.scale_around(scale, center, |p| {
                    p.text(&value.to_string(), &style, color, box_rect);
                });
            });
        };
        if let Some(previous) = self.previous {
            draw(p, previous, 1.0 - t * 1.6, 1.0 - 0.18 * t);
        }
        if let Some(value) = self.value {
            draw(p, value, t * 1.25, 1.25 - 0.25 * t);
        }
    }
}
