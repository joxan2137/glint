use std::f32::consts::TAU;

use glint_core::{PointF, RectF};

use super::{Interaction, Response, hover_tooltip};
use crate::anim::{Animated, Motion, Spring};
use crate::color::Color;
use crate::event::Event;
use crate::icons::Icon;
use crate::painter::{LineCap, Painter, PathBuilder, StrokeStyle};
use crate::view::Ctx;

const SIZE: f32 = 28.0;
const DOT_RADIUS: f32 = 10.0;
const RING_RADIUS: f32 = 13.0;
const RING_WIDTH: f32 = 2.0;
const RAINBOW_STEPS: usize = 48;

fn hue(h: f32) -> Color {
    let h = (h.rem_euclid(1.0)) * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    Color::rgba(r, g, b, 1.0)
}

/// Round color swatch with an animated selection ring, or the custom-color well (rainbow ring around the custom
/// color, or a plus when none is set).
#[derive(Clone, Debug)]
pub struct ColorSwatch {
    pub color: Option<Color>,
    pub well: bool,
    pub tooltip: Option<String>,
    selected: bool,
    rect: RectF,
    interaction: Interaction,
    ring: Animated<f32>,
    grow: Animated<f32>,
}

impl ColorSwatch {
    pub const SIZE: f32 = SIZE;

    pub fn new(color: Color) -> Self {
        Self::build(Some(color), false)
    }

    /// The custom-color well.
    pub fn well(color: Option<Color>) -> Self {
        Self::build(color, true)
    }

    fn build(color: Option<Color>, well: bool) -> Self {
        Self {
            color,
            well,
            tooltip: None,
            selected: false,
            rect: RectF::new(0.0, 0.0, SIZE, SIZE),
            interaction: Interaction::new(),
            ring: Animated::with_motion(0.0, Motion::Spring(Spring::SNAPPY)),
            grow: Animated::with_motion(0.0, Motion::Spring(Spring::SNAPPY)),
        }
    }

    pub fn tooltip(mut self, text: &str) -> Self {
        self.tooltip = Some(text.to_string());
        self
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    pub fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
        self.ring.set(if selected { 1.0 } else { 0.0 });
    }

    pub fn with_selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self.ring.snap(if selected { 1.0 } else { 0.0 });
        self
    }

    pub fn force_state(&mut self, hovered: bool, pressed: bool) {
        self.interaction.force(hovered, pressed);
        self.grow.snap(if hovered { 1.0 } else { 0.0 });
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    /// Places the 28×28 swatch centered at `center`.
    pub fn set_center(&mut self, center: PointF) {
        self.rect = RectF::new(center.x - SIZE / 2.0, center.y - SIZE / 2.0, SIZE, SIZE);
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        let was = self.interaction.hovered;
        let response = self.interaction.update(event, self.rect, true);
        hover_tooltip(cx, &self.interaction, was, self.rect, self.tooltip.as_deref().map(|t| (t, None)));
        let target = if self.interaction.pressed && self.interaction.hovered {
            -0.6
        } else if self.interaction.hovered {
            1.0
        } else {
            0.0
        };
        self.grow.set(target);
        response
    }

    fn ring_color(&self, p: &Painter) -> Color {
        let theme = p.theme();
        match self.color {
            Some(c) if !self.well => {
                let invisible = if theme.is_dark() { c.luminance() < 0.03 } else { c.luminance() > 0.85 };
                if invisible { theme.text_secondary } else { c }
            }
            _ => theme.text,
        }
    }

    pub fn paint(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let center = p.snap_point(self.rect.center());
        let grow = 1.0 + 0.08 * self.grow.get();
        let ring = self.ring.get().clamp(0.0, 1.0);
        if ring > 0.0 {
            let color = self.ring_color(p).multiply_alpha(ring);
            let r = RING_RADIUS - RING_WIDTH / 2.0 + (1.0 - ring) * 2.0;
            p.stroke_ellipse(center, r, r, color, RING_WIDTH);
        }
        let outline = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.16) } else { Color::rgba(0.0, 0.0, 0.0, 0.14) };
        if self.well {
            let radius = (DOT_RADIUS - 1.5) * grow;
            let step = TAU / RAINBOW_STEPS as f32;
            let flat = StrokeStyle { cap: LineCap::Flat, ..StrokeStyle::default() };
            for i in 0..RAINBOW_STEPS {
                let a0 = i as f32 * step - TAU / 4.0;
                let a1 = a0 + step * 1.08;
                let mut path = PathBuilder::new();
                path.move_to(PointF::new(center.x + radius * a0.cos(), center.y + radius * a0.sin()));
                path.arc_to(radius, radius, 0.0, false, true, PointF::new(center.x + radius * a1.cos(), center.y + radius * a1.sin()));
                if let Ok(path) = path.build(p.gfx()) {
                    p.stroke_path(&path, hue(i as f32 / RAINBOW_STEPS as f32), 3.0 * grow, &flat);
                }
            }
            let inner = 5.5 * grow;
            match self.color {
                Some(c) => {
                    p.fill_circle(center, inner, c);
                    p.stroke_ellipse(center, inner - p.px() / 2.0, inner - p.px() / 2.0, outline, p.px());
                }
                None => p.icon_with_stroke(Icon::Plus, center, 12.0 * grow, theme.text, 2.4),
            }
            return;
        }
        let radius = DOT_RADIUS * grow;
        let color = self.color.unwrap_or(Color::TRANSPARENT);
        p.fill_circle(center, radius, color);
        p.stroke_ellipse(center, radius - p.px() / 2.0, radius - p.px() / 2.0, outline, p.px());
    }
}
