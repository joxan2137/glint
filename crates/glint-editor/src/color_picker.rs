//! Custom color popover content: a saturation/brightness square over the hue, a hue strip, and a preview chip with
//! the hex code.

use glint_core::{PointF, RectF, SizeF};
use glint_ui::widgets::Response;
use glint_ui::{Brush, Color, Ctx, Event, MouseButton, Painter, Shadow, TextStyle, Weight};

const SQUARE_H: f32 = 140.0;
const STRIP_H: f32 = 14.0;
const GAP: f32 = 12.0;
const FOOTER_H: f32 = 24.0;
pub const WIDTH: f32 = 220.0;

pub fn content_size() -> SizeF {
    SizeF::new(WIDTH, SQUARE_H + GAP + STRIP_H + GAP + FOOTER_H)
}

/// Hue in turns (0..1), saturation and value in 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    pub fn to_color(self) -> Color {
        let h = self.h.rem_euclid(1.0) * 6.0;
        let c = self.v * self.s;
        let x = c * (1.0 - (h % 2.0 - 1.0).abs());
        let m = self.v - c;
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Color::rgba(r + m, g + m, b + m, 1.0)
    }

    pub fn from_color(c: Color) -> Hsv {
        let max = c.r.max(c.g).max(c.b);
        let min = c.r.min(c.g).min(c.b);
        let d = max - min;
        let h = if d <= 1e-6 {
            0.0
        } else if max == c.r {
            ((c.g - c.b) / d).rem_euclid(6.0) / 6.0
        } else if max == c.g {
            ((c.b - c.r) / d + 2.0) / 6.0
        } else {
            ((c.r - c.g) / d + 4.0) / 6.0
        };
        Hsv { h, s: if max <= 1e-6 { 0.0 } else { d / max }, v: max }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Square,
    Strip,
}

#[derive(Clone, Debug)]
pub struct ColorPicker {
    pub hsv: Hsv,
    rect: RectF,
    dragging: Option<Part>,
}

impl ColorPicker {
    pub fn new(color: Color) -> Self {
        Self { hsv: Hsv::from_color(color), rect: RectF::default(), dragging: None }
    }

    pub fn set_color(&mut self, color: Color) {
        if self.dragging.is_none() && self.hsv.to_color().to_rgba8() != color.to_rgba8() {
            self.hsv = Hsv::from_color(color);
        }
    }

    pub fn set_rect(&mut self, content: RectF) {
        self.rect = content;
    }

    fn square(&self) -> RectF {
        RectF::new(self.rect.x, self.rect.y, self.rect.w, SQUARE_H)
    }

    fn strip(&self) -> RectF {
        RectF::new(self.rect.x, self.rect.y + SQUARE_H + GAP, self.rect.w, STRIP_H)
    }

    fn apply(&mut self, part: Part, pos: PointF) {
        match part {
            Part::Square => {
                let r = self.square();
                self.hsv.s = ((pos.x - r.x) / r.w).clamp(0.0, 1.0);
                self.hsv.v = 1.0 - ((pos.y - r.y) / r.h).clamp(0.0, 1.0);
            }
            Part::Strip => {
                let r = self.strip();
                self.hsv.h = ((pos.x - r.x) / r.w).clamp(0.0, 0.9999);
            }
        }
    }

    /// `Action(color)` while dragging; the caller applies it live.
    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<Color> {
        match event {
            Event::PointerDown(e) if e.button == Some(MouseButton::Left) => {
                let part = if self.square().inset(-4.0).contains(e.pos) {
                    Part::Square
                } else if self.strip().inset(-6.0).contains(e.pos) {
                    Part::Strip
                } else {
                    return Response::Ignored;
                };
                cx.capture_pointer();
                self.dragging = Some(part);
                self.apply(part, e.pos);
                Response::Action(self.hsv.to_color())
            }
            Event::PointerMove(e) => match self.dragging {
                Some(part) => {
                    self.apply(part, e.pos);
                    Response::Action(self.hsv.to_color())
                }
                None => Response::Ignored,
            },
            Event::PointerUp(_) | Event::PointerCancel if self.dragging.is_some() => {
                self.dragging = None;
                Response::Consumed
            }
            _ => Response::Ignored,
        }
    }

    pub fn paint(&self, p: &mut Painter) {
        let theme = p.theme().clone();
        let square = self.square();
        let hue = Hsv { h: self.hsv.h, s: 1.0, v: 1.0 }.to_color();
        p.clip_round_rect(square, 8.0, |p| {
            p.fill_rect(square, hue);
            let white = Brush::linear(
                PointF::new(square.x, square.y),
                PointF::new(square.right(), square.y),
                &[(0.0, Color::WHITE), (1.0, Color::WHITE.with_alpha(0.0))],
            );
            p.fill_rect(square, white);
            p.fill_rect(square, Brush::vertical(square, Color::BLACK.with_alpha(0.0), Color::BLACK));
        });
        p.hairline_round_rect(square, 8.0, theme.outer_border.with_alpha(0.25), true);

        let strip = self.strip();
        let stops: Vec<(f32, Color)> =
            (0..=6).map(|i| (i as f32 / 6.0, Hsv { h: i as f32 / 6.0, s: 1.0, v: 1.0 }.to_color())).collect();
        p.fill_round_rect(
            strip,
            STRIP_H / 2.0,
            Brush::linear(PointF::new(strip.x, strip.y), PointF::new(strip.right(), strip.y), &stops),
        );

        let color = self.hsv.to_color();
        let knob = |p: &mut Painter, center: PointF, fill: Color, radius: f32| {
            p.shadow(
                RectF::new(center.x - radius, center.y - radius, radius * 2.0, radius * 2.0),
                radius,
                &Shadow::new(1.0, 4.0, Color::rgba(0.0, 0.0, 0.0, 0.35)),
            );
            p.fill_circle(center, radius, Color::WHITE);
            p.fill_circle(center, radius - 2.5, fill);
        };
        let sv = PointF::new(square.x + self.hsv.s * square.w, square.y + (1.0 - self.hsv.v) * square.h);
        knob(p, sv, color, 9.0);
        let h = PointF::new(strip.x + self.hsv.h * strip.w, strip.center().y);
        knob(p, PointF::new(crate::math::clamp_safe(h.x, strip.x + 8.0, strip.right() - 8.0), h.y), hue, 9.0);

        let footer = RectF::new(self.rect.x, strip.bottom() + GAP, self.rect.w, FOOTER_H);
        let chip = RectF::new(footer.x, footer.y, 36.0, FOOTER_H);
        p.fill_round_rect(chip, 6.0, color);
        p.hairline_round_rect(chip, 6.0, theme.outer_border.with_alpha(0.3), true);
        let style = TextStyle::body().weight(Weight::Medium).tabular();
        p.text(&color.to_hex(), &style, theme.text, RectF::new(chip.right() + 10.0, footer.y, 120.0, FOOTER_H));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_palette_colors() {
        for hex in ["#FF3B30", "#34C759", "#007AFF", "#AF52DE", "#FFCC00", "#808080"] {
            let c = Color::hex(hex).unwrap();
            assert_eq!(Hsv::from_color(c).to_color().to_rgba8(), c.to_rgba8(), "{hex}");
        }
    }

    #[test]
    fn hsv_extremes() {
        assert_eq!(Hsv { h: 0.3, s: 0.0, v: 1.0 }.to_color().to_rgba8(), [255, 255, 255, 255]);
        assert_eq!(Hsv { h: 0.7, s: 1.0, v: 0.0 }.to_color().to_rgba8(), [0, 0, 0, 255]);
        assert_eq!(Hsv { h: 0.0, s: 1.0, v: 1.0 }.to_color().to_rgba8(), [255, 0, 0, 255]);
        let blue = Hsv::from_color(Color::rgba(0.0, 0.0, 1.0, 1.0));
        assert!((blue.h - 2.0 / 3.0).abs() < 1e-5);
    }
}
