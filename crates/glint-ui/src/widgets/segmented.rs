use glint_core::{PointF, RectF, SizeF};

use super::{Interaction, Response};
use crate::anim::Animated;
use crate::color::Color;
use crate::event::Event;
use crate::gfx::Gfx;
use crate::icons::Icon;
use crate::painter::Painter;
use crate::text::{TextStyle, Weight};
use crate::theme::Shadow;
use crate::view::Ctx;

const ICON: f32 = 18.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segment {
    pub icon: Option<Icon>,
    pub label: Option<String>,
    /// A filled dot of this diameter in DIP (stroke-size pickers).
    pub dot: Option<f32>,
    pub tooltip: Option<String>,
    pub shortcut: Option<String>,
}

impl Segment {
    pub fn icon(icon: Icon) -> Self {
        Self { icon: Some(icon), ..Self::default() }
    }

    pub fn label(label: &str) -> Self {
        Self { label: Some(label.to_string()), ..Self::default() }
    }

    pub fn dot(diameter: f32) -> Self {
        Self { dot: Some(diameter), ..Self::default() }
    }

    pub fn icon_label(icon: Icon, label: &str) -> Self {
        Self { icon: Some(icon), label: Some(label.to_string()), ..Self::default() }
    }

    pub fn tooltip(mut self, text: &str, shortcut: Option<&str>) -> Self {
        self.tooltip = Some(text.to_string());
        self.shortcut = shortcut.map(str::to_string);
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SegmentedStyle {
    /// Inside a toolbar: 32 DIP items, no track, translucent selection pill.
    #[default]
    Toolbar,
    /// Standalone control: recessed track with a raised pill (Apple segmented control).
    Track,
}

/// Segmented control whose selection pill slides between segments with the default spring.
#[derive(Clone, Debug)]
pub struct Segmented {
    segments: Vec<Segment>,
    selected: usize,
    pub style: SegmentedStyle,
    pub enabled: bool,
    rect: RectF,
    item_rects: Vec<RectF>,
    pill: Animated<RectF>,
    pill_opacity: Animated<f32>,
    has_selection: bool,
    laid_out: bool,
    interactions: Vec<Interaction>,
}

impl Segmented {
    pub fn new(segments: Vec<Segment>, selected: usize) -> Self {
        let count = segments.len();
        Self {
            segments,
            selected: selected.min(count.saturating_sub(1)),
            style: SegmentedStyle::Toolbar,
            enabled: true,
            rect: RectF::default(),
            item_rects: vec![RectF::default(); count],
            pill: Animated::new(RectF::default()),
            pill_opacity: Animated::fade(1.0),
            has_selection: true,
            laid_out: false,
            interactions: vec![Interaction::new(); count],
        }
    }

    pub fn style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Selects `index`, sliding the pill (or fading it in at `index` after `clear_selection`).
    pub fn set_selected(&mut self, index: usize) {
        if index >= self.segments.len() {
            return;
        }
        let reappearing = !self.has_selection;
        self.selected = index;
        self.has_selection = true;
        self.set_pill_opacity(1.0);
        if self.laid_out {
            if reappearing {
                self.pill.snap(self.pill_rect(index));
            } else {
                self.pill.set(self.pill_rect(index));
            }
        }
    }

    /// Shows no segment as selected (the pill fades out); a click on any segment then selects it.
    pub fn clear_selection(&mut self) {
        self.has_selection = false;
        self.set_pill_opacity(0.0);
    }

    fn set_pill_opacity(&mut self, opacity: f32) {
        if self.laid_out {
            self.pill_opacity.set(opacity);
        } else {
            self.pill_opacity.snap(opacity);
        }
    }

    pub fn has_selection(&self) -> bool {
        self.has_selection
    }

    pub fn force_hover(&mut self, index: Option<usize>, pressed: bool) {
        for (i, interaction) in self.interactions.iter_mut().enumerate() {
            interaction.force(Some(i) == index, pressed && Some(i) == index);
        }
    }

    fn height(&self) -> f32 {
        match self.style {
            SegmentedStyle::Toolbar => 32.0,
            SegmentedStyle::Track => 28.0,
        }
    }

    fn label_style(&self) -> TextStyle {
        TextStyle::body().weight(Weight::Medium).centered()
    }

    fn segment_width(&self, gfx: &Gfx, segment: &Segment) -> f32 {
        let text = segment.label.as_ref().map(|l| gfx.measure_text(l, &self.label_style()).w);
        match (segment.icon, text) {
            (Some(_), None) => 32.0,
            (None, Some(w)) => (w + 24.0).ceil().max(48.0),
            (Some(_), Some(w)) => (ICON + 6.0 + w + 22.0).ceil(),
            (None, None) => 32.0,
        }
    }

    pub fn preferred_size(&self, gfx: &Gfx) -> SizeF {
        let widths: Vec<f32> = self.segments.iter().map(|s| self.segment_width(gfx, s)).collect();
        let total = match self.style {
            SegmentedStyle::Toolbar => widths.iter().sum::<f32>() + 2.0 * (widths.len().saturating_sub(1)) as f32,
            SegmentedStyle::Track => widths.iter().fold(0.0f32, |m, w| m.max(*w)) * widths.len() as f32 + 4.0,
        };
        SizeF::new(total, self.height())
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    /// Lays out segments inside `rect`; widths come from `gfx` text measurement.
    pub fn layout(&mut self, gfx: &Gfx, rect: RectF) {
        let moved = rect != self.rect;
        self.rect = rect;
        match self.style {
            SegmentedStyle::Toolbar => {
                let mut x = rect.x;
                for i in 0..self.segments.len() {
                    let w = self.segment_width(gfx, &self.segments[i]);
                    self.item_rects[i] = RectF::new(x, rect.y, w, rect.h);
                    x += w + 2.0;
                }
            }
            SegmentedStyle::Track => {
                let inner = rect.inset(2.0);
                let w = inner.w / self.segments.len().max(1) as f32;
                for i in 0..self.segments.len() {
                    self.item_rects[i] = RectF::new(inner.x + w * i as f32, inner.y, w, inner.h);
                }
            }
        }
        if moved || !self.laid_out {
            self.pill.snap(self.pill_rect(self.selected));
        }
        self.laid_out = true;
    }

    fn pill_rect(&self, index: usize) -> RectF {
        self.item_rects.get(index).copied().unwrap_or_default()
    }

    pub fn item_rect(&self, index: usize) -> Option<RectF> {
        self.item_rects.get(index).copied()
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<usize> {
        let mut result = Response::Ignored;
        for i in 0..self.segments.len() {
            let was_hovered = self.interactions[i].hovered;
            let bounds = self.item_rects[i];
            let response = self.interactions[i].update(event, bounds, self.enabled);
            let segment = &self.segments[i];
            let tooltip = segment.tooltip.as_deref().map(|t| (t, segment.shortcut.as_deref()));
            super::hover_tooltip(cx, &self.interactions[i], was_hovered, bounds, tooltip);
            match response {
                Response::Action(()) => {
                    if i != self.selected || !self.has_selection {
                        self.set_selected(i);
                        result = Response::Action(i);
                    } else {
                        result = Response::Consumed;
                    }
                }
                Response::Consumed if !matches!(result, Response::Action(_)) => result = Response::Consumed,
                _ => {}
            }
        }
        if matches!(event, Event::PointerDown(e) if self.rect.contains(e.pos)) {
            cx.hide_tooltip();
            if !result.consumed() {
                result = Response::Consumed;
            }
        }
        result
    }

    pub fn paint(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let pill = p.snap_rect(self.pill.get());
        let pill_opacity = self.pill_opacity.get().clamp(0.0, 1.0);
        let shows_selection = |i: usize| i == self.selected && self.has_selection;
        match self.style {
            SegmentedStyle::Toolbar => {
                for (i, interaction) in self.interactions.iter().enumerate() {
                    let amount = interaction.hover_amount() * if shows_selection(i) { 0.0 } else { 1.0 };
                    let press = interaction.press_amount();
                    if amount > 0.0 || press > 0.0 {
                        let color = Color::TRANSPARENT.lerp(&theme.hover, amount).lerp(&theme.pressed, press);
                        p.fill_round_rect(p.snap_rect(self.item_rects[i]), 8.0, color);
                    }
                }
                if pill_opacity > 0.0 {
                    p.fill_round_rect(pill, 8.0, theme.selected.multiply_alpha(pill_opacity));
                }
            }
            SegmentedStyle::Track => {
                let track = p.snap_rect(self.rect);
                let track_fill = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.08) } else { Color::rgba(0.0, 0.0, 0.0, 0.06) };
                p.fill_round_rect(track, 8.0, track_fill);
                let (pill_fill, shadow) = if theme.is_dark() {
                    (Color::rgba(1.0, 1.0, 1.0, 0.22), Shadow::new(1.0, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.30)))
                } else {
                    (Color::WHITE, Shadow::new(1.0, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.14)))
                };
                p.layer(pill_opacity, |p| {
                    p.shadow(pill, 6.0, &shadow);
                    p.fill_round_rect(pill, 6.0, pill_fill);
                    if !theme.is_dark() {
                        p.hairline_round_rect(pill, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.06), false);
                    }
                });
            }
        }
        let style = self.label_style();
        for (i, segment) in self.segments.iter().enumerate() {
            let r = self.item_rects[i];
            let selected = shows_selection(i);
            let color = if !self.enabled {
                theme.text_tertiary
            } else if selected || self.style == SegmentedStyle::Toolbar {
                theme.text
            } else {
                theme.text_secondary
            };
            if let Some(diameter) = segment.dot {
                let center = p.snap_point(r.center());
                p.fill_circle(center, diameter / 2.0, color);
                continue;
            }
            match (segment.icon, &segment.label) {
                (Some(icon), None) => p.icon(icon, r.center(), ICON, color),
                (None, Some(label)) => {
                    p.text(label, &style, color, r);
                }
                (Some(icon), Some(label)) => {
                    let text_w = p.measure(label, &style).w;
                    let total = ICON + 6.0 + text_w;
                    let x = r.center().x - total / 2.0;
                    p.icon(icon, PointF::new(x + ICON / 2.0, r.center().y), ICON, color);
                    let left = TextStyle { align: crate::text::TextAlign::Leading, ..style };
                    p.text(label, &left, color, RectF::new(x + ICON + 6.0, r.y, text_w + 2.0, r.h));
                }
                (None, None) => {}
            }
        }
    }
}
