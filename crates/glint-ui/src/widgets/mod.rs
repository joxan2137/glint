//! Retained widgets. Each is a plain struct owned by a view: lay it out (`set_rect`/`layout`), feed it events
//! (`event` returns a `Response`), and paint it. Animation state lives inside the widget.

mod badge;
mod button;
mod countdown;
mod icon_button;
mod menu;
mod popover;
mod segmented;
mod slider;
mod swatch;
mod toggle;
mod toolbar;
mod tooltip;

pub use badge::{Badge, BadgeStyle};
pub use button::{Button, ButtonStyle};
pub use countdown::Countdown;
pub use icon_button::IconButton;
pub use menu::{Menu, MenuItem};
pub use popover::{Popover, PopoverPlacement};
pub use segmented::{Segment, Segmented, SegmentedStyle};
pub use slider::Slider;
pub use swatch::ColorSwatch;
pub use toggle::Toggle;
pub use toolbar::{Toolbar, ToolbarAction, ToolbarItem, ToolbarItemKind};
pub use tooltip::Tooltip;

use glint_core::{PointF, RectF};

use crate::anim::{Animated, Motion, Spring, Tween};
use crate::event::{Event, MouseButton};
use crate::painter::Painter;
use crate::view::Ctx;

/// What a widget did with an event.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Response<T> {
    /// Not for this widget; keep routing.
    #[default]
    Ignored,
    /// Used by the widget (hover, press, drag) with no result yet.
    Consumed,
    /// The widget produced a result (click, new selection, new value).
    Action(T),
}

impl<T> Response<T> {
    pub fn consumed(&self) -> bool {
        !matches!(self, Response::Ignored)
    }

    pub fn action(self) -> Option<T> {
        match self {
            Response::Action(value) => Some(value),
            _ => None,
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Response<U> {
        match self {
            Response::Ignored => Response::Ignored,
            Response::Consumed => Response::Consumed,
            Response::Action(value) => Response::Action(f(value)),
        }
    }
}

/// Hover/press tracking with ≈100 ms animated highlight amounts.
#[derive(Clone, Debug)]
pub struct Interaction {
    pub hovered: bool,
    pub pressed: bool,
    hover: Animated<f32>,
    press: Animated<f32>,
}

impl Default for Interaction {
    fn default() -> Self {
        Self::new()
    }
}

impl Interaction {
    pub fn new() -> Self {
        Self {
            hovered: false,
            pressed: false,
            hover: Animated::with_motion(0.0, Motion::Tween(Tween::HOVER)),
            press: Animated::with_motion(0.0, Motion::Tween(Tween::HOVER)),
        }
    }

    /// 0..1 hover highlight.
    pub fn hover_amount(&self) -> f32 {
        self.hover.get().clamp(0.0, 1.0)
    }

    /// 0..1 press highlight.
    pub fn press_amount(&self) -> f32 {
        self.press.get().clamp(0.0, 1.0)
    }

    /// Sets the visual state without animation (previews, tests).
    pub fn force(&mut self, hovered: bool, pressed: bool) {
        self.hovered = hovered;
        self.pressed = pressed;
        self.hover.snap(if hovered { 1.0 } else { 0.0 });
        self.press.snap(if pressed { 1.0 } else { 0.0 });
    }

    fn sync(&mut self) {
        self.hover.set(if self.hovered { 1.0 } else { 0.0 });
        self.press.set(if self.pressed && self.hovered { 1.0 } else { 0.0 });
    }

    /// Updates from a pointer event over `bounds`; returns Action(()) on a completed primary click.
    pub fn update(&mut self, event: &Event, bounds: RectF, enabled: bool) -> Response<()> {
        let was = (self.hovered, self.pressed);
        let response = match event {
            Event::PointerMove(e) => {
                self.hovered = enabled && bounds.contains(e.pos);
                if self.pressed { Response::Consumed } else { Response::Ignored }
            }
            Event::PointerDown(e) => {
                self.hovered = enabled && bounds.contains(e.pos);
                if self.hovered && e.button == Some(MouseButton::Left) {
                    self.pressed = true;
                    Response::Consumed
                } else {
                    Response::Ignored
                }
            }
            Event::PointerUp(e) if self.pressed => {
                self.pressed = false;
                self.hovered = enabled && bounds.contains(e.pos);
                if self.hovered { Response::Action(()) } else { Response::Consumed }
            }
            Event::PointerLeave => {
                self.hovered = false;
                Response::Ignored
            }
            Event::PointerCancel => {
                self.pressed = false;
                self.hovered = false;
                Response::Ignored
            }
            _ => Response::Ignored,
        };
        if was != (self.hovered, self.pressed) {
            self.sync();
        }
        response
    }
}

/// Appear/disappear animation per DESIGN §5: appear = spring opacity 0→1, scale 0.96→1, y −6→0;
/// disappear = 120 ms fade with scale → 0.98.
#[derive(Clone, Debug)]
pub struct Presence {
    shown: bool,
    progress: Animated<f32>,
}

impl Presence {
    pub fn new(shown: bool) -> Self {
        Self { shown, progress: Animated::new(if shown { 1.0 } else { 0.0 }) }
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// True while visible or still fading out.
    pub fn is_visible(&self) -> bool {
        self.shown || self.progress.get() > 0.001
    }

    pub fn set_shown(&mut self, shown: bool) {
        if shown == self.shown {
            return;
        }
        self.shown = shown;
        if shown {
            self.progress.set_with(1.0, Motion::Spring(Spring::DEFAULT));
        } else {
            self.progress.set_with(0.0, Motion::Tween(Tween::FADE_OUT));
        }
    }

    pub fn snap(&mut self, shown: bool) {
        self.shown = shown;
        self.progress.snap(if shown { 1.0 } else { 0.0 });
    }

    /// `(opacity, scale, dy)` for the current frame.
    pub fn transform(&self) -> (f32, f32, f32) {
        let p = self.progress.get();
        if self.shown {
            (p.clamp(0.0, 1.0), 0.96 + 0.04 * p, -6.0 * (1.0 - p))
        } else {
            (p.clamp(0.0, 1.0), 0.98 + 0.02 * p, 0.0)
        }
    }

    /// Paints `f` with the presence transform around `origin` (the point that stays fixed while scaling).
    pub fn paint<R>(&self, p: &mut Painter, origin: PointF, f: impl FnOnce(&mut Painter) -> R) -> Option<R> {
        let (opacity, scale, dy) = self.transform();
        if opacity <= 0.001 {
            return None;
        }
        Some(p.layer(opacity, |p| p.translate(0.0, dy, |p| p.scale_around(scale, origin, f))))
    }
}

/// Shows the tooltip when hover starts and hides it when hover ends.
pub(crate) fn hover_tooltip(cx: &mut Ctx, interaction: &Interaction, was_hovered: bool, bounds: RectF, tooltip: Option<(&str, Option<&str>)>) {
    match (was_hovered, interaction.hovered, tooltip) {
        (false, true, Some((text, shortcut))) => cx.show_tooltip(bounds, text, shortcut),
        (true, false, Some(_)) => cx.hide_tooltip_for(bounds),
        _ => {}
    }
}
