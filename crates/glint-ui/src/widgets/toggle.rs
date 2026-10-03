use glint_core::{PointF, RectF, SizeF};

use super::{Interaction, Response};
use crate::anim::{Animated, Motion, Spring};
use crate::color::Color;
use crate::event::{Event, Key};
use crate::painter::Painter;
use crate::theme::Shadow;
use crate::view::Ctx;

const WIDTH: f32 = 40.0;
const HEIGHT: f32 = 24.0;
const INSET: f32 = 2.0;
const STRETCH: f32 = 5.0;

/// Apple-style switch: accent track when on, white knob that slides with the spring and stretches while pressed.
#[derive(Clone, Debug)]
pub struct Toggle {
    on: bool,
    pub enabled: bool,
    rect: RectF,
    interaction: Interaction,
    position: Animated<f32>,
    stretch: Animated<f32>,
}

impl Toggle {
    pub fn new(on: bool) -> Self {
        Self {
            on,
            enabled: true,
            rect: RectF::new(0.0, 0.0, WIDTH, HEIGHT),
            interaction: Interaction::new(),
            position: Animated::new(if on { 1.0 } else { 0.0 }),
            stretch: Animated::with_motion(0.0, Motion::Spring(Spring::SNAPPY)),
        }
    }

    pub const SIZE: SizeF = SizeF::new(WIDTH, HEIGHT);

    pub fn is_on(&self) -> bool {
        self.on
    }

    pub fn set_on(&mut self, on: bool) {
        self.on = on;
        self.position.set(if on { 1.0 } else { 0.0 });
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    /// Places the 40×24 switch with its top-left at `origin`.
    pub fn set_origin(&mut self, origin: PointF) {
        self.rect = RectF::new(origin.x, origin.y, WIDTH, HEIGHT);
    }

    pub fn force_state(&mut self, hovered: bool, pressed: bool) {
        self.interaction.force(hovered, pressed);
        self.stretch.snap(if pressed { 1.0 } else { 0.0 });
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<bool> {
        let _ = cx;
        if let Event::KeyDown(k) = event
            && k.key == Key::Space
            && self.interaction.hovered
            && self.enabled
        {
            self.set_on(!self.on);
            return Response::Action(self.on);
        }
        let response = self.interaction.update(event, self.rect, self.enabled);
        self.stretch.set(if self.interaction.pressed { 1.0 } else { 0.0 });
        match response {
            Response::Action(()) => {
                self.set_on(!self.on);
                Response::Action(self.on)
            }
            other => other.map(|_| self.on),
        }
    }

    pub fn paint(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let rect = p.snap_rect(self.rect);
        let t = self.position.get();
        let off_track = theme.control_track.lerp(&theme.selected, self.interaction.hover_amount() * 0.5);
        let mut track = off_track.lerp(&theme.accent, t.clamp(0.0, 1.0));
        if !self.enabled {
            track = track.multiply_alpha(0.5);
        }
        p.fill_round_rect(rect, HEIGHT / 2.0, track);
        let knob_size = HEIGHT - 2.0 * INSET;
        let knob_w = knob_size + STRETCH * self.stretch.get();
        let travel = WIDTH - 2.0 * INSET - knob_w;
        let x = rect.x + INSET + travel * t;
        let knob = RectF::new(x, rect.y + INSET, knob_w, knob_size);
        p.shadow(knob, knob_size / 2.0, &Shadow::new(1.0, 4.0, Color::rgba(0.0, 0.0, 0.0, 0.24)));
        p.fill_round_rect(knob, knob_size / 2.0, if self.enabled { theme.knob } else { theme.knob.lerp(&track, 0.3) });
        p.hairline_round_rect(knob, knob_size / 2.0, Color::rgba(0.0, 0.0, 0.0, 0.06), true);
    }
}
