use glint_core::{PointF, RectF, SizeF};

use crate::color::Color;
use crate::gfx::Gfx;
use crate::icons::Icon;
use crate::painter::Painter;
use crate::text::{TextStyle, Weight};
use crate::theme::Shadow;

const HEIGHT: f32 = 24.0;
const PADDING: f32 = 10.0;
const ICON: f32 = 14.0;
const ICON_GAP: f32 = 5.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeStyle {
    /// Dark/light glass with hairlines and a small shadow; legible over any content (selection size pill).
    #[default]
    Glass,
    /// Accent fill, white text (e.g. `HDR`).
    Accent,
    /// Faint fill inside toolbars and popovers.
    Subtle,
    /// Record red (e.g. a REC indicator).
    Destructive,
}

/// Pill label such as `1280 × 720` (tabular figures so digits never jitter).
#[derive(Clone, Debug, PartialEq)]
pub struct Badge {
    pub text: String,
    pub style: BadgeStyle,
    pub icon: Option<Icon>,
    /// A leading color chip (e.g. the picked color next to its hex code).
    pub chip: Option<Color>,
}

impl Badge {
    pub fn new(text: &str) -> Self {
        Self { text: text.to_string(), style: BadgeStyle::Glass, icon: None, chip: None }
    }

    pub fn style(mut self, style: BadgeStyle) -> Self {
        self.style = style;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn chip(mut self, color: Color) -> Self {
        self.chip = Some(color);
        self
    }

    fn lead_width(&self) -> f32 {
        if self.icon.is_some() || self.chip.is_some() { ICON + ICON_GAP } else { 0.0 }
    }

    pub fn text_style() -> TextStyle {
        TextStyle::new(12.0).weight(Weight::Semibold).tabular()
    }

    pub fn size(&self, gfx: &Gfx) -> SizeF {
        let text = gfx.measure_text(&self.text, &Self::text_style()).w;
        SizeF::new((2.0 * PADDING + self.lead_width() + text).ceil(), HEIGHT)
    }

    /// Paints centered on `center`; returns the pill rect.
    pub fn paint_centered(&self, p: &mut Painter, center: PointF) -> RectF {
        let size = self.size(p.gfx());
        let rect = p.snap_rect(RectF::new(center.x - size.w / 2.0, center.y - size.h / 2.0, size.w, size.h));
        self.paint(p, rect);
        rect
    }

    pub fn paint(&self, p: &mut Painter, rect: RectF) {
        let theme = p.theme().clone();
        let radius = rect.h / 2.0;
        let text_color = match self.style {
            BadgeStyle::Glass => {
                let shadow = Shadow::new(2.0, 8.0, Color::rgba(0.0, 0.0, 0.0, if theme.is_dark() { 0.35 } else { 0.15 }));
                p.shadow_outside(rect, radius, &shadow);
                p.fill_round_rect(rect, radius, theme.glass_fill_solid);
                p.hairline_round_rect(rect, radius, theme.hairline, true);
                p.hairline_round_rect(rect, radius, theme.outer_border, false);
                theme.text
            }
            BadgeStyle::Accent => {
                p.fill_round_rect(rect, radius, theme.accent);
                theme.on_accent
            }
            BadgeStyle::Destructive => {
                p.fill_round_rect(rect, radius, theme.destructive);
                theme.on_accent
            }
            BadgeStyle::Subtle => {
                p.fill_round_rect(rect, radius, theme.hover.with_alpha(theme.hover.a * 1.5));
                theme.text_secondary
            }
        };
        let style = Self::text_style();
        let text_w = p.measure(&self.text, &style).w;
        let icon_w = self.lead_width();
        let x = p.snap(rect.center().x - (icon_w + text_w) / 2.0);
        let lead_center = PointF::new(x + ICON / 2.0, rect.center().y);
        if let Some(icon) = self.icon {
            p.icon_with_stroke(icon, lead_center, ICON, text_color, 2.0);
        }
        if let Some(chip) = self.chip {
            let r = 5.5;
            p.fill_circle(lead_center, r, chip);
            let ring = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.35) } else { Color::rgba(0.0, 0.0, 0.0, 0.2) };
            p.stroke_ellipse(lead_center, r - p.px() / 2.0, r - p.px() / 2.0, ring, p.px());
        }
        p.text(&self.text, &style, text_color, RectF::new(x + icon_w, rect.y, text_w + 1.0, rect.h));
    }
}
