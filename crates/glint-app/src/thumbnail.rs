//! Floating thumbnail (DESIGN §7): the last capture in the bottom-right corner. Springs in from the right, dismisses
//! itself after 6 s (paused while hovered), shows Edit · Copy · Save · ✕ on hover, opens the editor on click, starts
//! a file drag when dragged and goes away when swiped right. Recordings show their first frame with a play badge.

use std::rc::Rc;
use std::time::Duration;

use glint_ui::widgets::{Interaction, Response};
use glint_ui::{
    Animated, Bitmap, Color, Ctx, Event, Icon, Image, Interpolation, MouseButton, Painter, PointF, RectF, SizeF,
    TextStyle, View, Weight,
};

use crate::popup::{AutoDismiss, EXIT_DURATION, Slide, paint_card_edges, paint_card_shadow};

pub const MAX_IMAGE: SizeF = SizeF::new(200.0, 150.0);
pub const MIN_CARD: SizeF = SizeF::new(124.0, 76.0);
pub const RADIUS: f32 = 10.0;
pub const DISMISS_AFTER: Duration = Duration::from_secs(6);

const DISMISS_TOKEN: u64 = 1;
const CLOSE_TOKEN: u64 = 2;
const FLASH_TOKEN: u64 = 3;
const DRAG_THRESHOLD: f32 = 5.0;
const SWIPE_DISMISS: f32 = 56.0;
const FLASH_SECONDS: f64 = 1.4;

/// Card size for an image of `image_px` physical pixels captured on a monitor with `image_scale` px per DIP: the
/// natural size fit inside 200×150 DIP (never enlarged), at least `MIN_CARD`.
pub fn card_size(image_w: u32, image_h: u32, image_scale: f32) -> SizeF {
    let natural = natural_size(image_w, image_h, image_scale);
    let fit = fit_inside(natural, MAX_IMAGE);
    SizeF::new(fit.w.max(MIN_CARD.w).round(), fit.h.max(MIN_CARD.h).round())
}

fn natural_size(w: u32, h: u32, scale: f32) -> SizeF {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    SizeF::new(w.max(1) as f32 / scale, h.max(1) as f32 / scale)
}

/// `size` scaled down (never up) to fit inside `bounds`, keeping its aspect ratio.
pub fn fit_inside(size: SizeF, bounds: SizeF) -> SizeF {
    let factor = (bounds.w / size.w).min(bounds.h / size.h).min(1.0);
    SizeF::new(size.w * factor, size.h * factor)
}

/// Where the image sits inside the card: aspect-fit, never enlarged past its natural size, centered.
pub fn image_rect(card: RectF, image_w: u32, image_h: u32, image_scale: f32) -> RectF {
    let natural = natural_size(image_w, image_h, image_scale);
    let fitted = fit_inside(natural, card.size());
    RectF::new(card.center().x - fitted.w / 2.0, card.center().y - fitted.h / 2.0, fitted.w, fitted.h)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThumbnailContent {
    Image,
    Video { duration: Duration },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailAction {
    /// Click on the card: open the editor (images) or the file (recordings).
    Open,
    Edit,
    Copy,
    Save,
    /// The user started dragging the card: run an OLE file drag.
    Drag,
    /// The window closed (dismissed, swiped, ✕ or replaced).
    Closed,
}

/// Posted to the app; `id` identifies the capture so stale events from a replaced thumbnail are ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbnailEvent {
    pub id: u64,
    pub action: ThumbnailAction,
}

struct HoverButton {
    action: ThumbnailAction,
    icon: Icon,
    rect: RectF,
    interaction: Interaction,
}

impl HoverButton {
    fn new(action: ThumbnailAction, icon: Icon) -> Self {
        Self { action, icon, rect: RectF::default(), interaction: Interaction::new() }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Gesture {
    Undecided,
    Swipe,
    Done,
}

struct Press {
    start: PointF,
    start_time: f64,
    gesture: Gesture,
    on_button: bool,
}

pub struct ThumbnailView {
    id: u64,
    bitmap: Rc<Bitmap>,
    image_scale: f32,
    content: ThumbnailContent,
    card: RectF,
    slide: Slide,
    hover: Animated<f32>,
    hovered: bool,
    held: bool,
    leaving: bool,
    actions: Vec<HoverButton>,
    close: HoverButton,
    press: Option<Press>,
    auto_dismiss: AutoDismiss,
    copied_at: Option<f64>,
}

impl ThumbnailView {
    /// `card` comes from `popup::corner_layout`; `image_scale` is the capture monitor's px per DIP.
    pub fn new(id: u64, image: Rc<Image>, image_scale: f32, content: ThumbnailContent, card: RectF) -> Self {
        let actions = match content {
            ThumbnailContent::Image => vec![
                HoverButton::new(ThumbnailAction::Edit, Icon::Pencil),
                HoverButton::new(ThumbnailAction::Copy, Icon::Copy),
                HoverButton::new(ThumbnailAction::Save, Icon::Download),
            ],
            ThumbnailContent::Video { .. } => Vec::new(),
        };
        let mut view = Self {
            id,
            bitmap: Bitmap::from_shared(image),
            image_scale,
            content,
            card,
            slide: Slide::new(card.right() + 8.0),
            hover: Animated::fade(0.0),
            hovered: false,
            held: false,
            leaving: false,
            actions,
            close: HoverButton::new(ThumbnailAction::Closed, Icon::X),
            press: None,
            auto_dismiss: AutoDismiss::new(DISMISS_AFTER, DISMISS_TOKEN),
            copied_at: None,
        };
        view.layout_buttons();
        view
    }

    /// Settled, optionally hovered state for previews.
    pub fn preview_state(mut self, hovered: bool) -> Self {
        self.slide = Slide::settled();
        self.hovered = hovered;
        self.hover.snap(if hovered { 1.0 } else { 0.0 });
        if hovered && let Some(copy) = self.actions.get_mut(1) {
            copy.interaction.force(true, false);
        }
        self
    }

    /// Keeps the thumbnail on screen while the app shows a dialog on its behalf.
    pub fn set_held(&mut self, cx: &mut Ctx, held: bool) {
        self.held = held;
        if held {
            self.auto_dismiss.pause(cx);
        } else if !self.hovered {
            self.auto_dismiss.resume(cx);
        }
    }

    /// Briefly swaps the Copy icon for a checkmark.
    pub fn flash_copied(&mut self, cx: &mut Ctx) {
        self.copied_at = Some(cx.time());
        cx.set_timer(Duration::from_secs_f64(FLASH_SECONDS), FLASH_TOKEN);
        cx.request_paint();
    }

    /// Slides out and closes.
    pub fn dismiss(&mut self, cx: &mut Ctx) {
        if self.leaving {
            return;
        }
        self.leaving = true;
        self.auto_dismiss.pause(cx);
        self.slide.leave();
        cx.set_timer(EXIT_DURATION, CLOSE_TOKEN);
        cx.request_paint();
    }

    fn post(&self, cx: &mut Ctx, action: ThumbnailAction) {
        cx.post(ThumbnailEvent { id: self.id, action });
    }

    fn set_hovered(&mut self, cx: &mut Ctx, hovered: bool) {
        if hovered == self.hovered {
            return;
        }
        self.hovered = hovered;
        self.hover.set(if hovered { 1.0 } else { 0.0 });
        if hovered {
            self.auto_dismiss.pause(cx);
        } else if !self.held && !self.leaving {
            self.auto_dismiss.resume(cx);
        }
    }

    fn layout_buttons(&mut self) {
        let size = 30.0;
        let gap = 4.0;
        let count = self.actions.len() as f32;
        let total = count * size + (count - 1.0).max(0.0) * gap;
        let center = self.card.center();
        for (i, button) in self.actions.iter_mut().enumerate() {
            let x = center.x - total / 2.0 + i as f32 * (size + gap);
            button.rect = RectF::new(x, center.y - size / 2.0, size, size);
        }
        let close = 22.0;
        self.close.rect = RectF::new(self.card.x - close / 2.0 + 4.0, self.card.y - close / 2.0 + 4.0, close, close);
    }

    fn route_buttons(&mut self, cx: &mut Ctx, event: &Event) -> Option<ThumbnailAction> {
        let enabled = self.hovered || self.press.is_some();
        let mut clicked = None;
        for button in self.actions.iter_mut().chain(std::iter::once(&mut self.close)) {
            if let Response::Action(()) = button.interaction.update(event, button.rect, enabled) {
                clicked = Some(button.action);
            }
        }
        if clicked.is_some() || self.actions.iter().chain([&self.close]).any(|b| b.interaction.hovered) {
            cx.request_paint();
        }
        clicked
    }

    fn on_button(&self, pos: PointF) -> bool {
        self.hovered && self.actions.iter().chain([&self.close]).any(|b| b.rect.contains(pos))
    }

    fn pointer_move(&mut self, cx: &mut Ctx, pos: PointF) {
        let inside = self.card.contains(pos) || self.close.rect.contains(pos);
        if self.press.is_none() {
            self.set_hovered(cx, inside);
        }
        let Some(press) = &mut self.press else { return };
        let dx = pos.x - press.start.x;
        let dy = pos.y - press.start.y;
        match press.gesture {
            Gesture::Undecided if !press.on_button && dx.hypot(dy) > DRAG_THRESHOLD => {
                if dx > 0.0 && dx.abs() > dy.abs() * 1.3 {
                    press.gesture = Gesture::Swipe;
                    self.slide.follow(dx);
                } else {
                    press.gesture = Gesture::Done;
                    self.post(cx, ThumbnailAction::Drag);
                }
                cx.request_paint();
            }
            Gesture::Swipe => {
                self.slide.follow(dx);
                cx.request_paint();
            }
            _ => {}
        }
    }

    fn pointer_up(&mut self, cx: &mut Ctx, pos: PointF, now: f64) {
        let Some(press) = self.press.take() else { return };
        match press.gesture {
            Gesture::Swipe => {
                let dx = pos.x - press.start.x;
                let velocity = dx / ((now - press.start_time).max(0.016) as f32);
                if dx > SWIPE_DISMISS || velocity > 600.0 {
                    self.dismiss(cx);
                } else {
                    self.slide.spring_back();
                    cx.request_paint();
                }
            }
            Gesture::Undecided if !press.on_button && self.card.contains(pos) => {
                self.post(cx, ThumbnailAction::Open);
            }
            _ => {}
        }
        let inside = self.card.contains(pos) || self.close.rect.contains(pos);
        self.set_hovered(cx, inside);
    }
}

impl View for ThumbnailView {
    /// The card (where it currently is, mid-swipe included) and, while hovered, the ✕ that overhangs its corner.
    /// Nothing while sliding out.
    fn interactive_region(&self) -> Option<Vec<RectF>> {
        if self.leaving {
            return Some(Vec::new());
        }
        let offset = self.slide.offset();
        let mut region = vec![self.card.offset(offset, 0.0)];
        if self.hovered {
            region.push(self.close.rect.offset(offset, 0.0));
        }
        Some(region)
    }

    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        if self.leaving && !matches!(event, Event::Timer(_) | Event::Closed) {
            return true;
        }
        self.layout_buttons();
        match event {
            Event::Shown => {
                self.slide.enter();
                self.auto_dismiss.resume(cx);
                cx.request_paint();
            }
            Event::PointerDown(e) if e.button == Some(MouseButton::Left) => {
                let on_button = self.on_button(e.pos);
                self.route_buttons(cx, event);
                if self.card.contains(e.pos) || on_button {
                    self.press = Some(Press { start: e.pos, start_time: cx.time(), gesture: Gesture::Undecided, on_button });
                }
            }
            Event::PointerMove(e) => {
                self.pointer_move(cx, e.pos);
                self.route_buttons(cx, event);
            }
            Event::PointerUp(e) => {
                let now = cx.time();
                let clicked = self.route_buttons(cx, event);
                match clicked {
                    Some(ThumbnailAction::Closed) => {
                        self.press = None;
                        self.dismiss(cx);
                    }
                    Some(action) => {
                        self.press = None;
                        self.post(cx, action);
                    }
                    None => self.pointer_up(cx, e.pos, now),
                }
            }
            Event::PointerLeave => {
                self.route_buttons(cx, event);
                if self.press.is_none() {
                    self.set_hovered(cx, false);
                }
            }
            Event::PointerCancel => {
                self.route_buttons(cx, event);
                if self.press.take().is_some_and(|p| p.gesture == Gesture::Swipe) {
                    self.slide.spring_back();
                }
                self.set_hovered(cx, false);
                cx.request_paint();
            }
            Event::Timer(token) => match *token {
                CLOSE_TOKEN => cx.close(),
                FLASH_TOKEN => cx.request_paint(),
                token if self.auto_dismiss.fired(token) => {
                    if self.hovered || self.held {
                        self.auto_dismiss.resume(cx);
                    } else {
                        self.dismiss(cx);
                    }
                }
                _ => {}
            },
            Event::Closed => self.post(cx, ThumbnailAction::Closed),
            _ => return false,
        }
        true
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.layout_buttons();
        let theme = p.theme().clone();
        let card = self.card;
        let hover = self.hover.get().clamp(0.0, 1.0);
        let opacity = self.slide.opacity();
        let copied = self.copied_at.is_some_and(|t| cx.time() - t < FLASH_SECONDS);
        p.layer(opacity, |p| {
            p.translate(self.slide.offset(), 0.0, |p| {
                paint_card_shadow(p, card, RADIUS);
                p.clip_round_rect(card, RADIUS, |p| {
                    p.fill_rect(card, theme.window_background);
                    let image = image_rect(card, self.bitmap.width(), self.bitmap.height(), self.image_scale);
                    p.bitmap(&self.bitmap, p.snap_rect(image), None, 1.0, Interpolation::Cubic);
                    let scrim = match self.content {
                        ThumbnailContent::Image => 0.34,
                        ThumbnailContent::Video { .. } => 0.18,
                    };
                    if hover > 0.0 {
                        p.fill_rect(card, Color::rgba(0.0, 0.0, 0.0, scrim * hover));
                    }
                });
                paint_card_edges(p, card, RADIUS, &theme);
                if let ThumbnailContent::Video { duration } = self.content {
                    paint_play_badge(p, card.center());
                    paint_duration(p, card, duration);
                }
                if hover > 0.001 {
                    p.layer(hover, |p| {
                        for button in &self.actions {
                            let icon = if button.action == ThumbnailAction::Copy && copied { Icon::Check } else { button.icon };
                            paint_round_button(p, button, icon);
                        }
                        paint_close_button(p, &self.close);
                    });
                }
            });
        });
    }
}

/// Dark glass circle with a white glyph, readable over any screenshot.
fn paint_round_button(p: &mut Painter, button: &HoverButton, icon: Icon) {
    let rect = p.snap_rect(button.rect);
    let radius = rect.w / 2.0;
    let hover = button.interaction.hover_amount();
    let press = button.interaction.press_amount();
    p.shadow(rect, radius, &glint_ui::Shadow::new(1.0, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.35)));
    let fill = Color::rgba(0.12, 0.12, 0.13, 0.78).lerp(&Color::rgba(0.22, 0.22, 0.24, 0.86), hover).lerp(&Color::rgba(0.08, 0.08, 0.09, 0.9), press);
    p.fill_round_rect(rect, radius, fill);
    p.hairline_round_rect(rect, radius, Color::rgba(1.0, 1.0, 1.0, 0.16), true);
    p.icon(icon, rect.center(), 16.0, Color::rgba(1.0, 1.0, 1.0, 0.95));
}

/// Apple-notification style ✕ that sits on the card's top-left corner.
fn paint_close_button(p: &mut Painter, button: &HoverButton) {
    let theme = p.theme().clone();
    let rect = p.snap_rect(button.rect);
    let radius = rect.w / 2.0;
    let hover = button.interaction.hover_amount();
    p.shadow(rect, radius, &glint_ui::Shadow::new(1.0, 4.0, Color::rgba(0.0, 0.0, 0.0, if theme.is_dark() { 0.45 } else { 0.2 })));
    let (fill, ink) = if theme.is_dark() {
        (Color::rgba(0.2, 0.2, 0.22, 0.96).lerp(&Color::rgba(0.3, 0.3, 0.32, 0.98), hover), theme.text)
    } else {
        (Color::rgba(0.98, 0.98, 0.99, 0.97).lerp(&Color::rgba(0.92, 0.92, 0.93, 0.98), hover), theme.text)
    };
    p.fill_round_rect(rect, radius, fill);
    p.hairline_round_rect(rect, radius, theme.outer_border, false);
    p.icon_with_stroke(Icon::X, rect.center(), 12.0, ink, 2.25);
}

fn paint_play_badge(p: &mut Painter, center: PointF) {
    let size = 40.0;
    let rect = p.snap_rect(RectF::new(center.x - size / 2.0, center.y - size / 2.0, size, size));
    p.shadow(rect, size / 2.0, &glint_ui::Shadow::new(2.0, 10.0, Color::rgba(0.0, 0.0, 0.0, 0.4)));
    p.fill_round_rect(rect, size / 2.0, Color::rgba(0.1, 0.1, 0.11, 0.62));
    p.hairline_round_rect(rect, size / 2.0, Color::rgba(1.0, 1.0, 1.0, 0.22), true);
    p.icon(Icon::PlayFill, PointF::new(rect.center().x + 1.5, rect.center().y), 20.0, Color::WHITE);
}

fn paint_duration(p: &mut Painter, card: RectF, duration: Duration) {
    let text = crate::pipeline::format_duration(duration);
    let style = TextStyle::new(10.5).weight(Weight::Semibold).tabular();
    let width = p.measure(&text, &style).w + 12.0;
    let pill = p.snap_rect(RectF::new(card.right() - width - 7.0, card.bottom() - 18.0 - 7.0, width, 18.0));
    p.fill_round_rect(pill, 9.0, Color::rgba(0.0, 0.0, 0.0, 0.5));
    p.text(&text, &style.centered(), Color::WHITE, pill);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_fits_large_captures_inside_200_by_150() {
        assert_eq!(card_size(3840, 2160, 1.5), SizeF::new(200.0, 113.0));
        assert_eq!(card_size(1000, 2000, 1.0), SizeF::new(MIN_CARD.w, 150.0));
        assert_eq!(card_size(400, 300, 2.0), SizeF::new(200.0, 150.0));
    }

    #[test]
    fn small_captures_are_never_enlarged() {
        assert_eq!(card_size(40, 30, 1.0), MIN_CARD);
        let card = RectF::new(28.0, 28.0, MIN_CARD.w, MIN_CARD.h);
        let image = image_rect(card, 40, 30, 1.0);
        assert_eq!((image.w, image.h), (40.0, 30.0));
        assert_eq!(image.center(), card.center());
    }

    #[test]
    fn extreme_aspect_ratios_letterbox_inside_the_minimum_card() {
        let size = card_size(4000, 40, 1.0);
        assert_eq!(size, SizeF::new(200.0, MIN_CARD.h));
        let card = RectF::new(0.0, 0.0, size.w, size.h);
        let image = image_rect(card, 4000, 40, 1.0);
        assert_eq!(image.w, 200.0);
        assert!((image.h - 2.0).abs() < 1e-4);
    }
}
