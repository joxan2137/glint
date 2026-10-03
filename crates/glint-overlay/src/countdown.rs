//! The delay countdown pill: a small glass capsule at the top center of a monitor, excluded from capture.

use std::time::Duration;

use glint_core::{MonitorInfo, PointF, PointI, RectF, SizeF};
use glint_ui::anim::precise_time;
use glint_ui::widgets::{Countdown, IconButton, Presence, Response};
use glint_ui::{App, Ctx, Event, Icon, Painter, View, WindowSpec};

const PILL: SizeF = SizeF::new(140.0, 80.0);
/// Distance of the pill's top edge from the monitor's top edge, like the overlay toolbar.
const PILL_TOP: f32 = 24.0;
/// Transparent margins that hold the glass shadow; clicks there pass through (`interactive_region`).
const MARGIN_TOP: f32 = 24.0;
const MARGIN_SIDE: f32 = 48.0;
const MARGIN_BOTTOM: f32 = 64.0;
/// Window top below the monitor top, in DIP.
pub const WINDOW_TOP: f32 = PILL_TOP - MARGIN_TOP;
const TICK: u64 = 1;
const CLOSE: u64 = 2;

pub type CountdownDone = Box<dyn FnOnce(&App, bool)>;

pub fn window_size() -> SizeF {
    SizeF::new(PILL.w + 2.0 * MARGIN_SIDE, PILL.h + MARGIN_TOP + MARGIN_BOTTOM)
}

pub struct CountdownView {
    total: u32,
    remaining: u32,
    started: Option<f64>,
    digits: Countdown,
    close: IconButton,
    presence: Presence,
    done: Option<CountdownDone>,
}

impl CountdownView {
    pub fn new(secs: u32, done: Option<CountdownDone>) -> Self {
        let mut digits = Countdown::new();
        digits.set(secs);
        let mut presence = Presence::new(false);
        presence.set_shown(true);
        Self {
            total: secs,
            remaining: secs,
            started: None,
            digits,
            close: IconButton::new(Icon::X).tooltip("Cancel", None),
            presence,
            done,
        }
    }

    pub fn pill_rect() -> RectF {
        RectF::new(MARGIN_SIDE, MARGIN_TOP, PILL.w, PILL.h)
    }

    fn schedule_tick(&self, cx: &mut Ctx) {
        let Some(started) = self.started else { return };
        let elapsed = self.total - self.remaining + 1;
        let due = started + elapsed as f64 - precise_time();
        cx.set_timer(Duration::from_secs_f64(due.max(0.0)), TICK);
    }

    fn finish(&mut self, cx: &mut Ctx, completed: bool) {
        if let (Some(done), Some(app)) = (self.done.take(), cx.app()) {
            app.set_timer(Duration::ZERO, move |app| done(app, completed));
        }
        if completed {
            cx.close();
        } else {
            self.presence.set_shown(false);
            cx.request_paint();
            cx.set_timer(Duration::from_millis(130), CLOSE);
        }
    }
}

impl View for CountdownView {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        match event {
            Event::Shown => {
                self.started = Some(precise_time());
                self.schedule_tick(cx);
                false
            }
            Event::Timer(TICK) if self.done.is_some() => {
                self.remaining = self.remaining.saturating_sub(1);
                if self.remaining == 0 {
                    self.finish(cx, true);
                } else {
                    self.digits.set(self.remaining);
                    cx.request_paint();
                    self.schedule_tick(cx);
                }
                true
            }
            Event::Timer(CLOSE) => {
                cx.close();
                true
            }
            Event::Closed => {
                if let (Some(done), Some(app)) = (self.done.take(), cx.app()) {
                    app.set_timer(Duration::ZERO, move |app| done(app, false));
                }
                true
            }
            _ if self.done.is_some() => match self.close.event(cx, event) {
                Response::Action(()) => {
                    self.finish(cx, false);
                    true
                }
                response => response.consumed(),
            },
            _ => false,
        }
    }

    fn interactive_region(&self) -> Option<Vec<RectF>> {
        Some(vec![Self::pill_rect()])
    }

    fn paint(&mut self, _cx: &mut Ctx, p: &mut Painter) {
        let pill = Self::pill_rect();
        let center_y = pill.center().y;
        let digit_center = PointF::new(pill.x + 56.0, center_y);
        self.close.set_rect(RectF::new(pill.right() - 50.0, center_y - 16.0, 32.0, 32.0));
        let text = p.theme().text;
        let separator = p.theme().separator;
        let (digits, close) = (&self.digits, &mut self.close);
        self.presence.paint(p, PointF::new(pill.center().x, pill.y), |p| {
            p.glass(pill, PILL.h / 2.0, None);
            p.clip_round_rect(pill, PILL.h / 2.0, |p| digits.paint(p, digit_center, text));
            let x = p.snap(pill.right() - 62.0);
            p.fill_rect(RectF::new(x, p.snap(center_y - 14.0), 1.0, 28.0), separator);
            close.paint(p);
        });
    }
}

/// Opens the pill on `monitor`; `done(app, completed)` runs once (false when cancelled or closed early).
pub fn show(app: &App, monitor: &MonitorInfo, secs: u32, done: impl FnOnce(&App, bool) + 'static) -> anyhow::Result<()> {
    if secs == 0 {
        app.set_timer(Duration::ZERO, move |app| done(app, true));
        return Ok(());
    }
    let size = window_size();
    let scale = monitor.scale();
    let width_px = (size.w * scale).round() as i32;
    let top_px = (WINDOW_TOP * scale).round() as i32;
    let origin = PointI::new(monitor.rect.x + (monitor.rect.w - width_px) / 2, monitor.rect.y + top_px);
    let spec = WindowSpec::popup(origin, size).exclude_from_capture();
    app.open(spec, CountdownView::new(secs, Some(Box::new(done))))?;
    Ok(())
}
