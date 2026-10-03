use glint_core::{PointF, RectF, SizeF};

use super::{Interaction, Response, hover_tooltip};
use crate::anim::Animated;
use crate::color::Color;
use crate::event::Event;
use crate::gfx::Gfx;
use crate::icons::Icon;
use crate::painter::Painter;
use crate::text::{TextStyle, Weight};
use crate::view::Ctx;

const SIZE: f32 = 32.0;
const ICON: f32 = 18.0;
const RADIUS: f32 = 8.0;
const LABEL_LEAD: f32 = 7.0;
const LABEL_GAP: f32 = 6.0;
const LABEL_TRAIL: f32 = 12.0;

/// 32×32 icon button (optionally with a label) with hover, press, selected and disabled states.
#[derive(Clone, Debug)]
pub struct IconButton {
    pub icon: Icon,
    pub label: Option<String>,
    pub tooltip: Option<String>,
    pub shortcut: Option<String>,
    pub enabled: bool,
    /// Icon color override (e.g. destructive red for a record button).
    pub tint: Option<Color>,
    selected: bool,
    rect: RectF,
    interaction: Interaction,
    selected_amount: Animated<f32>,
}

impl IconButton {
    pub fn new(icon: Icon) -> Self {
        Self {
            icon,
            label: None,
            tooltip: None,
            shortcut: None,
            enabled: true,
            tint: None,
            selected: false,
            rect: RectF::new(0.0, 0.0, SIZE, SIZE),
            interaction: Interaction::new(),
            selected_amount: Animated::fade(0.0),
        }
    }

    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }

    /// Tooltip text and optional key hint (e.g. `("Rectangle", Some("R"))`).
    pub fn tooltip(mut self, text: &str, shortcut: Option<&str>) -> Self {
        self.tooltip = Some(text.to_string());
        self.shortcut = shortcut.map(str::to_string);
        self
    }

    pub fn tint(mut self, color: Color) -> Self {
        self.tint = Some(color);
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn with_selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self.selected_amount.snap(if selected { 1.0 } else { 0.0 });
        self
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    pub fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
        self.selected_amount.set(if selected { 1.0 } else { 0.0 });
    }

    /// Snaps hover/press visuals (previews and tests).
    pub fn force_state(&mut self, hovered: bool, pressed: bool) {
        self.interaction.force(hovered, pressed);
    }

    pub fn label_style() -> TextStyle {
        TextStyle::body().weight(Weight::Medium)
    }

    pub fn preferred_size(&self, gfx: &Gfx) -> SizeF {
        match &self.label {
            Some(label) => {
                let text = gfx.measure_text(label, &Self::label_style()).w;
                SizeF::new((LABEL_LEAD + ICON + LABEL_GAP + text + LABEL_TRAIL).ceil(), SIZE)
            }
            None => SizeF::new(SIZE, SIZE),
        }
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    pub fn set_rect(&mut self, rect: RectF) {
        self.rect = rect;
    }

    pub fn is_hovered(&self) -> bool {
        self.interaction.hovered
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        let was_hovered = self.interaction.hovered;
        let response = self.interaction.update(event, self.rect, self.enabled);
        let tooltip = self.tooltip.as_deref().map(|t| (t, self.shortcut.as_deref()));
        hover_tooltip(cx, &self.interaction, was_hovered, self.rect, tooltip);
        if matches!(event, Event::PointerDown(e) if self.rect.contains(e.pos)) && self.tooltip.is_some() {
            cx.hide_tooltip_for(self.rect);
        }
        response
    }

    /// Background fill for the current state.
    pub fn background(&self, p: &Painter) -> Color {
        let theme = p.theme();
        let hover = self.interaction.hover_amount();
        let press = self.interaction.press_amount();
        let selected = self.selected_amount.get();
        let interactive = Color::TRANSPARENT.lerp(&theme.hover, hover).lerp(&theme.pressed, press);
        interactive.lerp(&theme.selected, selected)
    }

    pub fn paint(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let rect = p.snap_rect(self.rect);
        let background = self.background(p);
        if background.a > 0.0 {
            p.fill_round_rect(rect, RADIUS, background);
        }
        let color = if self.enabled { self.tint.unwrap_or(theme.text) } else { theme.text_tertiary };
        match &self.label {
            None => p.icon(self.icon, rect.center(), ICON, color),
            Some(label) => {
                let icon_center = PointF::new(rect.x + LABEL_LEAD + ICON / 2.0, rect.center().y);
                p.icon(self.icon, icon_center, ICON, color);
                let text_x = rect.x + LABEL_LEAD + ICON + LABEL_GAP;
                let text_color = if self.enabled { theme.text } else { theme.text_tertiary };
                p.text(label, &Self::label_style(), text_color, RectF::new(text_x, rect.y, rect.right() - text_x, rect.h));
            }
        }
    }
}
