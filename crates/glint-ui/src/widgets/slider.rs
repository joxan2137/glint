use glint_core::{PointF, RectF};

use super::{Interaction, Response};
use crate::anim::{Animated, Motion, Spring};
use crate::color::Color;
use crate::event::{Event, Key, MouseButton};
use crate::painter::Painter;
use crate::theme::Shadow;
use crate::view::Ctx;

const TRACK: f32 = 4.0;
const KNOB: f32 = 20.0;
const HEIGHT: f32 = 24.0;

/// Thin-track slider with a white knob, optional tick at a reference value (e.g. 0 stops).
#[derive(Clone, Debug)]
pub struct Slider {
    value: f32,
    pub min: f32,
    pub max: f32,
    /// Snap increment; 0 = continuous.
    pub step: f32,
    /// Draw a small tick and fill from this value instead of from `min`.
    pub origin: Option<f32>,
    pub enabled: bool,
    rect: RectF,
    interaction: Interaction,
    dragging: bool,
    knob_scale: Animated<f32>,
}

impl Slider {
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self {
            value: value.clamp(min, max),
            min,
            max,
            step: 0.0,
            origin: None,
            enabled: true,
            rect: RectF::default(),
            interaction: Interaction::new(),
            dragging: false,
            knob_scale: Animated::with_motion(1.0, Motion::Spring(Spring::SNAPPY)),
        }
    }

    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    pub fn origin(mut self, origin: f32) -> Self {
        self.origin = Some(origin);
        self
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn set_value(&mut self, value: f32) {
        self.value = value.clamp(self.min, self.max);
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    /// `rect` is the full hit area; height 24 DIP looks right, the track is centered vertically.
    pub fn set_rect(&mut self, rect: RectF) {
        self.rect = rect;
    }

    pub const HEIGHT: f32 = HEIGHT;

    pub fn force_state(&mut self, hovered: bool, pressed: bool) {
        self.interaction.force(hovered, pressed);
    }

    fn track_rect(&self) -> RectF {
        let pad = KNOB / 2.0;
        RectF::new(self.rect.x + pad, self.rect.center().y - TRACK / 2.0, (self.rect.w - KNOB).max(0.0), TRACK)
    }

    fn fraction(&self, value: f32) -> f32 {
        if self.max > self.min { ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0) } else { 0.0 }
    }

    fn value_at(&self, x: f32) -> f32 {
        let track = self.track_rect();
        let t = if track.w > 0.0 { ((x - track.x) / track.w).clamp(0.0, 1.0) } else { 0.0 };
        let raw = self.min + t * (self.max - self.min);
        let stepped = if self.step > 0.0 { self.min + ((raw - self.min) / self.step).round() * self.step } else { raw };
        let snapped = match self.origin {
            Some(o) if self.step <= 0.0 && (stepped - o).abs() < (self.max - self.min) * 0.015 => o,
            _ => stepped,
        };
        snapped.clamp(self.min, self.max)
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<f32> {
        if !self.enabled {
            return Response::Ignored;
        }
        let before = self.value;
        let response = match event {
            Event::PointerDown(e) if e.button == Some(MouseButton::Left) && self.rect.contains(e.pos) => {
                self.dragging = true;
                cx.capture_pointer();
                self.value = self.value_at(e.pos.x);
                Response::Consumed
            }
            Event::PointerMove(e) if self.dragging => {
                self.value = self.value_at(e.pos.x);
                Response::Consumed
            }
            Event::PointerUp(_) if self.dragging => {
                self.dragging = false;
                Response::Consumed
            }
            Event::PointerCancel => {
                self.dragging = false;
                Response::Ignored
            }
            Event::KeyDown(k) if self.interaction.hovered => {
                let step = if self.step > 0.0 { self.step } else { (self.max - self.min) / 100.0 };
                match k.key {
                    Key::Left | Key::Down => self.value = (self.value - step).max(self.min),
                    Key::Right | Key::Up => self.value = (self.value + step).min(self.max),
                    _ => {}
                }
                Response::Ignored
            }
            _ => Response::Ignored,
        };
        self.interaction.update(event, self.rect, self.enabled);
        self.knob_scale.set(if self.dragging { 1.1 } else { 1.0 });
        if self.value != before { Response::Action(self.value) } else { response }
    }

    pub fn paint(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let track = self.track_rect();
        let track_px = p.snap_rect(track);
        p.fill_round_rect(track_px, TRACK / 2.0, theme.control_track);
        let from = self.fraction(self.origin.unwrap_or(self.min));
        let to = self.fraction(self.value);
        let (a, b) = (from.min(to), from.max(to));
        let fill = RectF::from_ltrb(track.x + track.w * a, track.y, track.x + track.w * b, track.bottom());
        let accent = if self.enabled { theme.accent } else { theme.text_tertiary };
        if fill.w > 0.0 {
            p.fill_round_rect(p.snap_rect(fill), TRACK / 2.0, accent);
        }
        if let Some(origin) = self.origin {
            let x = track.x + track.w * self.fraction(origin);
            let tick = RectF::new(p.snap(x - 1.0), p.snap(track.center().y - 5.0), 2.0, 10.0);
            p.fill_round_rect(tick, 1.0, theme.text_tertiary);
        }
        let center = PointF::new(track.x + track.w * to, track.center().y);
        let size = KNOB * self.knob_scale.get();
        let knob = RectF::new(center.x - size / 2.0, center.y - size / 2.0, size, size);
        p.shadow(knob, size / 2.0, &Shadow::new(1.0, 5.0, Color::rgba(0.0, 0.0, 0.0, if theme.is_dark() { 0.35 } else { 0.22 })));
        p.fill_ellipse(center, size / 2.0, size / 2.0, theme.knob);
        p.stroke_ellipse(center, size / 2.0 - p.px() / 2.0, size / 2.0 - p.px() / 2.0, Color::rgba(0.0, 0.0, 0.0, 0.08), p.px());
    }
}
