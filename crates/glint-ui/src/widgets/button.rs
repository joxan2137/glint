use glint_core::{PointF, RectF, SizeF};

use super::{Interaction, Response};
use crate::color::Color;
use crate::event::{Event, Key};
use crate::gfx::Gfx;
use crate::icons::Icon;
use crate::painter::Painter;
use crate::text::{TextStyle, Weight};
use crate::theme::Shadow;
use crate::view::Ctx;

const HEIGHT: f32 = 28.0;
const RADIUS: f32 = 8.0;
const PADDING: f32 = 14.0;
const ICON: f32 = 14.0;
const ICON_GAP: f32 = 6.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Accent fill, white label.
    Primary,
    /// Translucent glass fill with hairline.
    #[default]
    Secondary,
    /// Accent-colored label, highlight only on hover.
    Plain,
    /// Red fill, white label.
    Destructive,
}

/// Text button (optionally with a leading icon), 28 DIP high.
#[derive(Clone, Debug)]
pub struct Button {
    pub label: String,
    pub icon: Option<Icon>,
    pub style: ButtonStyle,
    pub enabled: bool,
    /// Enter activates it (drawn the same; documented for dialogs).
    pub default_action: bool,
    rect: RectF,
    interaction: Interaction,
}

impl Button {
    pub fn new(label: &str, style: ButtonStyle) -> Self {
        Self {
            label: label.to_string(),
            icon: None,
            style,
            enabled: true,
            default_action: false,
            rect: RectF::default(),
            interaction: Interaction::new(),
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn default_action(mut self) -> Self {
        self.default_action = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn force_state(&mut self, hovered: bool, pressed: bool) {
        self.interaction.force(hovered, pressed);
    }

    fn text_style() -> TextStyle {
        TextStyle::body().weight(Weight::Medium)
    }

    pub fn preferred_size(&self, gfx: &Gfx) -> SizeF {
        let text = gfx.measure_text(&self.label, &Self::text_style()).w;
        let icon = if self.icon.is_some() { ICON + ICON_GAP } else { 0.0 };
        SizeF::new((2.0 * PADDING + icon + text).ceil().max(64.0), HEIGHT)
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    pub fn set_rect(&mut self, rect: RectF) {
        self.rect = rect;
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        let _ = cx;
        if let Event::KeyDown(k) = event
            && self.default_action
            && self.enabled
            && k.key == Key::Enter
            && k.mods.is_empty()
        {
            return Response::Action(());
        }
        self.interaction.update(event, self.rect, self.enabled)
    }

    pub fn paint(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let rect = p.snap_rect(self.rect);
        let hover = self.interaction.hover_amount();
        let press = self.interaction.press_amount();
        let shade = |base: Color| {
            let lighter = base.lerp(&Color::WHITE.with_alpha(base.a), 0.10);
            let darker = base.lerp(&Color::BLACK.with_alpha(base.a), 0.12);
            base.lerp(&lighter, hover).lerp(&darker, press)
        };
        let (fill, label) = match self.style {
            ButtonStyle::Primary => (shade(theme.accent), theme.on_accent),
            ButtonStyle::Destructive => (shade(theme.destructive), theme.on_accent),
            ButtonStyle::Secondary => {
                let base = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.12) } else { Color::rgba(1.0, 1.0, 1.0, 0.85) };
                let fill = base.lerp(&base.with_alpha(base.a + 0.06), hover).lerp(&theme.pressed.over(&base), press);
                (fill, theme.text)
            }
            ButtonStyle::Plain => {
                let fill = Color::TRANSPARENT.lerp(&theme.hover, hover).lerp(&theme.pressed, press);
                (fill, theme.accent)
            }
        };
        let alpha = if self.enabled { 1.0 } else { 0.45 };
        p.layer(alpha, |p| {
            match self.style {
                ButtonStyle::Secondary => {
                    if !theme.is_dark() {
                        p.shadow(rect, RADIUS, &Shadow::new(0.5, 2.0, Color::rgba(0.0, 0.0, 0.0, 0.10)));
                    }
                    p.fill_round_rect(rect, RADIUS, fill);
                    let line = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.10) } else { Color::rgba(0.0, 0.0, 0.0, 0.10) };
                    p.hairline_round_rect(rect, RADIUS, line, true);
                }
                ButtonStyle::Primary | ButtonStyle::Destructive => {
                    p.fill_round_rect(rect, RADIUS, fill);
                    p.hairline_round_rect(rect, RADIUS, Color::rgba(1.0, 1.0, 1.0, 0.12), true);
                }
                ButtonStyle::Plain => {
                    if fill.a > 0.0 {
                        p.fill_round_rect(rect, RADIUS, fill);
                    }
                }
            }
            let style = Self::text_style();
            let text_w = p.measure(&self.label, &style).w;
            let icon_w = if self.icon.is_some() { ICON + ICON_GAP } else { 0.0 };
            let x = p.snap(rect.center().x - (icon_w + text_w) / 2.0);
            if let Some(icon) = self.icon {
                p.icon(icon, PointF::new(x + ICON / 2.0, rect.center().y), ICON, label);
            }
            p.text(&self.label, &style, label, RectF::new(x + icon_w, rect.y, text_w + 1.0, rect.h));
        });
    }
}
