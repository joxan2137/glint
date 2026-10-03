//! Compact notifications in the thumbnail's style ("Text copied", "Glint is running", ...): icon plus one or two
//! lines, bottom-right, 2.5 s, paused while hovered, click to dismiss.

use std::time::Duration;

use glint_ui::{Color, Ctx, Event, Gfx, Icon, MouseButton, Painter, PointF, RectF, SizeF, TextStyle, View};

use crate::art::paint_app_icon;
use crate::popup::{AutoDismiss, EXIT_DURATION, Slide, paint_card_edges, paint_card_shadow};

pub const SHOW_FOR: Duration = Duration::from_millis(2500);
const RADIUS: f32 = 12.0;
const ICON: f32 = 32.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 12.0;
const MIN_WIDTH: f32 = 220.0;
const MAX_WIDTH: f32 = 360.0;
const DISMISS_TOKEN: u64 = 1;
const CLOSE_TOKEN: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Accent,
    Success,
    Neutral,
    Destructive,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToastIcon {
    App,
    Symbol(Icon, Tint),
    /// A picked color (BGRA).
    Swatch([u8; 4]),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub icon: ToastIcon,
    pub title: String,
    pub detail: Option<String>,
}

impl Toast {
    pub fn new(icon: ToastIcon, title: &str, detail: Option<&str>) -> Self {
        Self { icon, title: title.to_string(), detail: detail.map(str::to_string) }
    }
}

fn title_style() -> TextStyle {
    TextStyle::emphasized()
}

fn detail_style() -> TextStyle {
    TextStyle::new(12.0)
}

pub fn card_size(gfx: &Gfx, toast: &Toast) -> SizeF {
    let title = gfx.measure_text(&toast.title, &title_style()).w;
    let detail = toast.detail.as_ref().map_or(0.0, |d| gfx.measure_text(d, &detail_style()).w);
    let width = (PADDING + ICON + GAP + title.max(detail) + PADDING + 6.0).clamp(MIN_WIDTH, MAX_WIDTH).ceil();
    SizeF::new(width, if toast.detail.is_some() { 58.0 } else { 52.0 })
}

/// Posted when a toast window closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToastClosed(pub u64);

pub struct ToastView {
    id: u64,
    toast: Toast,
    card: RectF,
    slide: Slide,
    hovered: bool,
    leaving: bool,
    auto_dismiss: AutoDismiss,
}

impl ToastView {
    pub fn new(id: u64, toast: Toast, card: RectF) -> Self {
        Self {
            id,
            toast,
            card,
            slide: Slide::new(card.right() + 8.0),
            hovered: false,
            leaving: false,
            auto_dismiss: AutoDismiss::new(SHOW_FOR, DISMISS_TOKEN),
        }
    }

    pub fn settled(mut self) -> Self {
        self.slide = Slide::settled();
        self
    }

    fn leave(&mut self, cx: &mut Ctx) {
        if self.leaving {
            return;
        }
        self.leaving = true;
        self.auto_dismiss.pause(cx);
        self.slide.leave();
        cx.set_timer(EXIT_DURATION, CLOSE_TOKEN);
        cx.request_paint();
    }
}

impl View for ToastView {
    /// Only the card takes clicks; nothing while sliding out.
    fn interactive_region(&self) -> Option<Vec<RectF>> {
        if self.leaving {
            return Some(Vec::new());
        }
        Some(vec![self.card.offset(self.slide.offset(), 0.0)])
    }

    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        match event {
            Event::Shown => {
                self.slide.enter();
                self.auto_dismiss.resume(cx);
                cx.request_paint();
            }
            Event::PointerMove(e) => {
                let inside = self.card.contains(e.pos);
                if inside != self.hovered && !self.leaving {
                    self.hovered = inside;
                    if inside { self.auto_dismiss.pause(cx) } else { self.auto_dismiss.resume(cx) }
                }
            }
            Event::PointerLeave if self.hovered && !self.leaving => {
                self.hovered = false;
                self.auto_dismiss.resume(cx);
            }
            Event::PointerUp(e) if e.button == Some(MouseButton::Left) && self.card.contains(e.pos) => self.leave(cx),
            Event::Timer(CLOSE_TOKEN) => cx.close(),
            Event::Timer(token) if self.auto_dismiss.fired(*token) => {
                if self.hovered {
                    self.auto_dismiss.resume(cx);
                } else {
                    self.leave(cx);
                }
            }
            Event::Closed => cx.post(ToastClosed(self.id)),
            _ => return false,
        }
        true
    }

    fn paint(&mut self, _cx: &mut Ctx, p: &mut Painter) {
        let theme = p.theme().clone();
        let card = self.card;
        p.layer(self.slide.opacity(), |p| {
            p.translate(self.slide.offset(), 0.0, |p| {
                paint_card_shadow(p, card, RADIUS);
                p.fill_round_rect(card, RADIUS, theme.glass_fill_solid.with_alpha(0.97));
                paint_card_edges(p, card, RADIUS, &theme);
                let icon = RectF::new(card.x + PADDING, card.center().y - ICON / 2.0, ICON, ICON);
                paint_toast_icon(p, &self.toast.icon, p.snap_rect(icon));
                let text_x = icon.right() + GAP;
                let text_w = card.right() - PADDING - text_x;
                match &self.toast.detail {
                    Some(detail) => {
                        let title = RectF::new(text_x, card.center().y - 17.0, text_w, 17.0);
                        p.text(&self.toast.title, &title_style(), theme.text, title);
                        let below = RectF::new(text_x, card.center().y + 1.0, text_w, 16.0);
                        p.text(detail, &detail_style(), theme.text_secondary, below);
                    }
                    None => {
                        p.text(&self.toast.title, &title_style(), theme.text, RectF::new(text_x, card.y, text_w, card.h));
                    }
                }
            });
        });
    }
}

fn paint_toast_icon(p: &mut Painter, icon: &ToastIcon, rect: RectF) {
    let theme = p.theme().clone();
    match icon {
        ToastIcon::App => paint_app_icon(p, rect),
        ToastIcon::Symbol(symbol, tint) => {
            let fill = match tint {
                Tint::Accent => theme.accent,
                Tint::Success => theme.success,
                Tint::Destructive => theme.destructive,
                Tint::Neutral => if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.16) } else { Color::rgba(0.0, 0.0, 0.0, 0.08) },
            };
            let ink = if *tint == Tint::Neutral { theme.text } else { theme.on_accent };
            p.fill_round_rect(rect, 8.0, fill);
            p.icon(*symbol, rect.center(), 18.0, ink);
        }
        ToastIcon::Swatch(bgra) => {
            let center = rect.center();
            let radius = rect.w / 2.0 - 2.0;
            p.fill_circle(center, radius, Color::from_bgra8([bgra[0], bgra[1], bgra[2], 255]));
            let ring = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.3) } else { Color::rgba(0.0, 0.0, 0.0, 0.16) };
            p.stroke_ellipse(center, radius - p.px() / 2.0, radius - p.px() / 2.0, ring, p.px());
            let highlight = PointF::new(center.x - radius * 0.35, center.y - radius * 0.4);
            p.fill_circle(highlight, radius * 0.22, Color::rgba(1.0, 1.0, 1.0, 0.18));
        }
    }
}
