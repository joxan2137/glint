//! One overlay window per monitor: the live desktop (until the frozen pixels arrive) or the frozen image under the
//! dim, selection visuals, magnifier and controls. A view either lives for one session (`open_overlay`) or is pooled:
//! created hidden ahead of time, attached to a session on show and hidden again when the session ends.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glint_core::{CaptureMode, PointF, PointI, RectF, RectI};
use glint_ui::widgets::{Badge, Menu, Response, Toolbar, ToolbarAction, Tooltip};
use glint_ui::{
    Animated, App, Backdrop, Bitmap, Color, Ctx, Cursor, Event, Icon, Interpolation, Key, KeyEvent, LineCap,
    MouseButton, Motion, Painter, PathBuilder, PointerEvent, Shadow, StrokeStyle, TextStyle, Tween, View, Weight,
};

use crate::chrome::{self, MAGNIFIER_ITEM, MODE_SEGMENTS, RecordAction, RecordBar, TOOLBAR_TOP};
use crate::freeform::{Lasso, smooth_path};
use crate::geometry::{Selection, Space, magnifier_center, place_near};
use crate::magnifier::{self, Loupe};
use crate::state::{DELAYS, Finish, Frozen, Gesture, Session};
use crate::sys;

pub type Shared = Rc<RefCell<Session>>;

const HIGHLIGHT_RADIUS: f32 = 8.0;
const EDGE_MARGIN: f32 = 8.0;
/// Windows still in a session this long after it ended (e.g. one that cannot render its fade) are ended directly.
const CLOSE_FALLBACK: Duration = Duration::from_millis(400);
const CLOSE_RETRY: Duration = Duration::from_millis(200);
const CLOSE_ATTEMPTS: u32 = 3;

/// Window-level extras an offscreen preview paints itself (tooltips normally belong to the window).
#[derive(Clone, Debug, Default)]
pub struct PreviewExtras {
    pub tooltip: Option<(RectF, String, Option<String>)>,
}

/// What the dim leaves clear on this monitor.
enum Focus {
    Dimmed,
    Rect { dip: RectF, px: RectI },
    Lasso(Vec<PointF>),
    Window,
    Display,
}

pub struct OverlayView {
    shared: Option<Shared>,
    index: usize,
    /// Pooled: hidden instead of closed when the session ends.
    reusable: bool,
    bitmap: Option<Rc<Bitmap>>,
    backdrop: Option<Rc<Bitmap>>,
    /// 0 = controls on solid glass (no frozen pixels yet), 1 = frosted over the frozen image.
    frosted: Animated<f32>,
    pub(crate) toolbar: Toolbar,
    pub(crate) menu: Menu,
    pub(crate) record_bar: RecordBar,
    highlight: Animated<RectF>,
    highlight_opacity: Animated<f32>,
    highlight_shown: bool,
    lit: Animated<f32>,
    fade: Animated<f32>,
    closing: bool,
    first_frame: bool,
    shown_logged: bool,
    pub(crate) preview: PreviewExtras,
}

fn broadcast(app: &App, session: &Session) {
    for id in &session.window_ids {
        app.request_paint(*id);
    }
}

/// Once per session, after it ended: ends whatever windows are still in it after `CLOSE_FALLBACK`.
pub fn arm_close_fallback(app: &App, shared: &Shared, s: &mut Session) {
    if s.close_fallback_armed || !s.is_closing() {
        return;
    }
    s.close_fallback_armed = true;
    let shared = shared.clone();
    app.set_timer(CLOSE_FALLBACK, move |app| force_close(app, shared, CLOSE_ATTEMPTS));
}

fn force_close(app: &App, shared: Shared, attempts_left: u32) {
    if shared.borrow().done.is_none() {
        return;
    }
    let ids = shared.borrow().window_ids.clone();
    for id in ids {
        app.with_view(id, |view: &mut OverlayView, cx| view.force_end(cx, &shared));
    }
    if shared.borrow().done.is_none() {
        return;
    }
    if attempts_left == 0 {
        log::warn!("overlay: windows did not leave the session; reporting the outcome anyway");
        deliver(app, &shared);
        return;
    }
    app.set_timer(CLOSE_RETRY, move |app| force_close(app, shared, attempts_left - 1));
}

/// Hands the outcome to `done` (exactly once: later calls find it taken).
fn deliver(app: &App, shared: &Shared) {
    let mut s = shared.borrow_mut();
    s.end(Finish::Cancelled);
    let Some(done) = s.done.take() else { return };
    let finish = s.finish.clone().unwrap_or(Finish::Cancelled);
    let frozen: Vec<Frozen> = s.frozen.iter_mut().filter_map(Option::take).collect();
    let monitors = s.monitors.clone();
    let (hdr, prefs) = (s.hdr.clone(), s.prefs.clone());
    drop(s);
    let gfx = app.gfx();
    app.set_timer(Duration::ZERO, move |app| {
        let outcome = finish.resolve(&monitors, &frozen, &hdr, &prefs, &gfx);
        done(app, outcome, prefs);
    });
}

fn ms_since(at: Instant) -> f64 {
    at.elapsed().as_secs_f64() * 1000.0
}

/// Everything another monitor's window needs to repaint for.
#[derive(Clone, Debug, PartialEq)]
struct Visible {
    mode: CaptureMode,
    video: bool,
    delay: u32,
    magnifier: bool,
    audio: (bool, bool),
    toolbar_monitor: usize,
    cursor_monitor: Option<usize>,
    hover: Option<RectI>,
    ctrl: bool,
    armed: Option<RectI>,
    dragging: bool,
    ended: bool,
    closing: bool,
}

impl Visible {
    fn of(s: &Session) -> Self {
        Self {
            mode: s.prefs.mode,
            video: s.prefs.video,
            delay: s.prefs.delay_secs,
            magnifier: s.prefs.show_magnifier,
            audio: (s.prefs.microphone, s.prefs.system_audio),
            toolbar_monitor: s.toolbar_monitor,
            cursor_monitor: s.cursor.and_then(|c| s.monitor_under(c)),
            hover: s.cursor.and_then(|c| s.click_target(c)),
            ctrl: s.ctrl,
            armed: s.armed.map(|a| a.rect),
            dragging: !matches!(s.gesture, Gesture::Idle | Gesture::Click),
            ended: s.finish.is_some(),
            closing: s.is_closing(),
        }
    }
}

impl OverlayView {
    /// A pooled view waiting (hidden) for its next session.
    pub fn idle() -> Self {
        Self {
            shared: None,
            index: 0,
            reusable: true,
            bitmap: None,
            backdrop: None,
            frosted: Animated::fade(0.0),
            toolbar: chrome::capture_toolbar(CaptureMode::Rectangle, false),
            menu: chrome::options_menu(),
            record_bar: RecordBar::new(),
            highlight: Animated::snappy(RectF::default()),
            highlight_opacity: Animated::fade(0.0),
            highlight_shown: false,
            lit: Animated::fade(0.0),
            fade: Animated::with_motion(1.0, Motion::Tween(Tween::FADE_OUT)),
            closing: false,
            first_frame: false,
            shown_logged: true,
            preview: PreviewExtras::default(),
        }
    }

    /// A view that belongs to one session and closes its window when the session ends.
    pub fn attached(shared: Shared, index: usize) -> Self {
        let mut view = Self { reusable: false, ..Self::idle() };
        view.reset(shared, index);
        view
    }

    pub fn is_idle(&self) -> bool {
        self.shared.is_none()
    }

    fn reset(&mut self, shared: Shared, index: usize) {
        let prefs = shared.borrow().prefs.clone();
        let reusable = self.reusable;
        *self = Self { reusable, index, shared: Some(shared), first_frame: true, shown_logged: false, ..Self::idle() };
        self.toolbar = chrome::capture_toolbar(prefs.mode, prefs.video);
        self.toolbar.snap_visible(false);
        self.record_bar.set_audio(prefs.microphone, prefs.system_audio);
    }

    /// Pooled windows: joins `shared` as monitor `index`, moves above popups created since (thumbnail, toasts) and
    /// shows the window (its first frame is the pending one).
    pub fn begin(&mut self, cx: &mut Ctx, shared: Shared, index: usize, activate: bool) {
        self.reset(shared, index);
        cx.set_window_opacity(1.0);
        cx.set_topmost(true);
        cx.show_window(activate);
        cx.request_paint();
    }

    /// Leaves the session (once): the last window to leave hands the outcome to `done`.
    fn leave(&mut self, cx: &mut Ctx) {
        let Some(shared) = self.shared.take() else { return };
        self.bitmap = None;
        self.backdrop = None;
        {
            let mut s = shared.borrow_mut();
            s.end(Finish::Cancelled);
            s.open_windows = s.open_windows.saturating_sub(1);
            if s.open_windows > 0 {
                if let Some(app) = cx.app() {
                    arm_close_fallback(app, &shared, &mut s);
                    broadcast(app, &s);
                }
                return;
            }
        }
        if let Some(app) = cx.app() {
            deliver(app, &shared);
        }
    }

    /// The fallback timer: ends this window's part in `shared` without waiting for its fade.
    fn force_end(&mut self, cx: &mut Ctx, shared: &Shared) {
        if !self.shared.as_ref().is_some_and(|mine| Rc::ptr_eq(mine, shared)) {
            return;
        }
        if self.reusable {
            cx.hide_window();
        } else {
            cx.close();
        }
        self.leave(cx);
    }

    fn space(&self, s: &Session, scale: f32) -> Space {
        Space::new(s.monitor_rects[self.index], scale)
    }

    fn over_chrome(&self, pos: PointF) -> bool {
        self.toolbar.contains(pos) || self.record_bar.contains(pos) || (self.menu.is_open() && self.menu.frame().contains(pos))
    }

    fn cursor_for(&self, s: &Session, pos: Option<PointF>) -> Cursor {
        if s.is_waiting() {
            return Cursor::Progress;
        }
        if self.menu.is_open() || pos.is_some_and(|p| self.over_chrome(p)) {
            return Cursor::Arrow;
        }
        if s.space && matches!(s.gesture, Gesture::Select { .. }) {
            return Cursor::Move;
        }
        match s.prefs.mode {
            CaptureMode::Window | CaptureMode::FullScreen => Cursor::Arrow,
            _ => Cursor::Crosshair,
        }
    }

    fn route(&mut self, cx: &mut Ctx, s: &mut Session, event: &Event) -> bool {
        if self.menu.is_open() {
            match self.menu.event(cx, event) {
                Response::Action(i) => {
                    self.menu_chosen(s, i);
                    return true;
                }
                Response::Consumed => return true,
                Response::Ignored => {}
            }
        }
        match self.record_bar.event(cx, event) {
            Response::Action(action) => {
                record_chosen(s, action);
                return true;
            }
            Response::Consumed => return true,
            Response::Ignored => {}
        }
        match self.toolbar.event(cx, event) {
            Response::Action(action) => {
                self.toolbar_chosen(cx, s, action);
                return true;
            }
            Response::Consumed => return true,
            Response::Ignored => {}
        }
        match event {
            Event::KeyDown(k) => self.key_down(cx, s, k),
            Event::KeyUp(k) => key_up(s, k),
            Event::PointerDown(e) => self.pointer_down(cx, s, e),
            Event::PointerMove(e) => self.pointer_move(cx, s, e),
            Event::PointerUp(e) => pointer_up(s, e),
            Event::PointerCancel => {
                s.gesture = Gesture::Idle;
                true
            }
            Event::PointerLeave => {
                cx.request_paint();
                false
            }
            _ => false,
        }
    }

    /// While the outcome waits for the captures only cancelling is possible.
    fn route_waiting(&mut self, cx: &mut Ctx, s: &mut Session, event: &Event) -> bool {
        let cancel = matches!(event, Event::KeyDown(k) if k.key == Key::Escape)
            || matches!(event, Event::PointerDown(e) if e.button == Some(MouseButton::Right));
        if cancel {
            s.end(Finish::Cancelled);
        }
        if event.pointer().is_some() {
            cx.set_cursor(Cursor::Progress);
        }
        true
    }

    fn toolbar_chosen(&mut self, cx: &mut Ctx, s: &mut Session, action: ToolbarAction) {
        match action {
            ToolbarAction::Selected("mode", i) => s.set_mode(MODE_SEGMENTS[i]),
            ToolbarAction::Selected("media", i) => s.set_video(i == 1),
            ToolbarAction::Clicked("text") => s.set_mode(CaptureMode::Text),
            ToolbarAction::Clicked("color") => s.set_mode(CaptureMode::ColorPicker),
            ToolbarAction::Clicked("close") => s.end(Finish::Cancelled),
            ToolbarAction::Clicked("delay") => {
                if let Some(anchor) = self.toolbar.item("delay").map(|item| item.rect()) {
                    cx.hide_tooltip();
                    self.menu.open(cx.gfx(), anchor, cx.size());
                }
            }
            _ => {}
        }
        cx.request_paint();
    }

    fn menu_chosen(&mut self, s: &mut Session, index: usize) {
        match DELAYS.get(index) {
            Some(&secs) => s.choose_delay(secs),
            None if index == MAGNIFIER_ITEM => s.prefs.show_magnifier = !s.prefs.show_magnifier,
            None => {}
        }
    }

    fn key_down(&mut self, cx: &mut Ctx, s: &mut Session, k: &KeyEvent) -> bool {
        let gesturing = matches!(s.gesture, Gesture::Select { .. } | Gesture::Lasso { .. });
        let step = if k.mods.shift { 10 } else { 1 };
        match k.key {
            Key::Escape => s.end(Finish::Cancelled),
            Key::Enter => {
                if !k.repeat {
                    press_enter(s);
                }
            }
            Key::Space if gesturing => s.space = true,
            Key::Space if !k.repeat => s.toggle_rectangle_window(),
            Key::Control => s.ctrl = true,
            Key::Shift => refresh_square(s, true),
            Key::Left => sys::nudge_cursor(-step, 0),
            Key::Right => sys::nudge_cursor(step, 0),
            Key::Up => sys::nudge_cursor(0, -step),
            Key::Down => sys::nudge_cursor(0, step),
            Key::Char(c) if !k.repeat && !k.mods.ctrl && !k.mods.alt && !k.mods.win => match c {
                'R' => s.set_mode(CaptureMode::Rectangle),
                'W' => s.set_mode(CaptureMode::Window),
                'F' => s.set_mode(CaptureMode::FullScreen),
                'L' => s.set_mode(CaptureMode::Freeform),
                'T' => s.set_mode(CaptureMode::Text),
                'C' => s.set_mode(CaptureMode::ColorPicker),
                'V' => s.set_video(!s.prefs.video),
                'M' => s.prefs.show_magnifier = !s.prefs.show_magnifier,
                _ => return false,
            },
            _ => return false,
        }
        let pos = s.cursor.map(|c| self.space(s, cx.scale()).to_dip(c));
        cx.set_cursor(self.cursor_for(s, pos));
        cx.request_paint();
        true
    }

    fn pointer_down(&mut self, cx: &mut Ctx, s: &mut Session, e: &PointerEvent) -> bool {
        match e.button {
            Some(MouseButton::Right) => s.end(Finish::Cancelled),
            Some(MouseButton::Left) => {
                let bounds = s.monitor_rects[self.index];
                let at = e.screen_px;
                s.cursor = Some(at);
                s.ctrl = e.mods.ctrl;
                s.gesture = match s.prefs.mode {
                    CaptureMode::Rectangle | CaptureMode::Text => {
                        Gesture::Select { selection: Selection::begin(bounds, at), monitor: self.index }
                    }
                    CaptureMode::Freeform => Gesture::Lasso {
                        lasso: Lasso::begin(bounds, PointF::new(at.x as f32, at.y as f32)),
                        monitor: self.index,
                    },
                    CaptureMode::Window | CaptureMode::FullScreen | CaptureMode::ColorPicker => Gesture::Click,
                };
            }
            _ => return false,
        }
        cx.request_paint();
        true
    }

    fn pointer_move(&mut self, cx: &mut Ctx, s: &mut Session, e: &PointerEvent) -> bool {
        let space = self.space(s, cx.scale());
        s.cursor = Some(e.screen_px);
        s.ctrl = e.mods.ctrl;
        let moving = s.space;
        match &mut s.gesture {
            Gesture::Select { selection, .. } => selection.drag_to(e.screen_px, e.mods.shift, moving),
            Gesture::Lasso { lasso, .. } => {
                for sample in &e.history {
                    lasso.add(space.to_px_f(sample.pos));
                }
                lasso.add(PointF::new(e.screen_px.x as f32, e.screen_px.y as f32));
            }
            Gesture::Idle | Gesture::Click => {
                if s.toolbar_monitor != self.index && s.monitor_under(e.screen_px) == Some(self.index) {
                    s.toolbar_monitor = self.index;
                }
            }
        }
        cx.set_cursor(self.cursor_for(s, Some(e.pos)));
        cx.request_paint();
        true
    }

    /// Syncs widgets and animation targets with the session; runs at the start of every paint.
    fn prepare(&mut self, p: &Painter, space: &Space, s: &Session) {
        let gfx = p.gfx();
        let bounds = p.bounds();
        if self.bitmap.is_none()
            && let Some(frozen) = s.pixels(self.index)
        {
            let bitmap = Bitmap::from_shared(frozen.sdr.clone());
            self.backdrop = Some(bitmap.glass_backdrop(p.theme(), p.scale()));
            self.bitmap = Some(bitmap);
            self.frosted.set(1.0);
        }
        let ended = s.finish.is_some();
        let here = s.toolbar_monitor == self.index;
        self.toolbar.layout_centered(gfx, bounds.w / 2.0, TOOLBAR_TOP);
        chrome::sync_toolbar(&mut self.toolbar, s.prefs.mode, s.prefs.video, self.menu.is_open());
        let show_toolbar = here && !s.is_dragging() && !ended;
        self.toolbar.set_visible(show_toolbar);
        if !show_toolbar && self.menu.is_open() {
            self.menu.close();
        }
        chrome::sync_menu(&mut self.menu, s.prefs.delay_secs, s.prefs.show_magnifier);

        self.record_bar.set_audio(s.prefs.microphone, s.prefs.system_audio);
        match s.armed.filter(|a| a.monitor == self.index && !s.is_dragging() && !ended) {
            Some(armed) => {
                self.record_bar.layout(gfx, space.rect_to_dip(armed.rect), bounds.inset(EDGE_MARGIN));
                self.record_bar.set_visible(true);
            }
            None => self.record_bar.set_visible(false),
        }

        let hover = (s.armed.is_none() && !ended).then(|| s.cursor.and_then(|c| s.click_target(c))).flatten();
        match hover.filter(|_| s.prefs.mode == CaptureMode::Window) {
            Some(rect) => {
                let target = space.rect_to_dip(rect);
                if self.highlight_shown {
                    self.highlight.set(target);
                } else {
                    self.highlight.snap(target);
                }
                self.highlight_opacity.set(1.0);
                self.highlight_shown = true;
            }
            None => {
                self.highlight_opacity.set(0.0);
                self.highlight_shown = false;
            }
        }
        let monitor = s.monitor_rects[self.index];
        let lit = s.prefs.mode == CaptureMode::FullScreen && hover.is_some_and(|r| r.intersect(&monitor) == Some(monitor));
        self.lit.set(if lit { 1.0 } else { 0.0 });

        if s.is_closing() && !self.closing {
            self.closing = true;
            self.fade.set(0.0);
        }
    }

    fn focus(&self, s: &Session, space: &Space) -> Focus {
        let monitor = s.monitor_rects[self.index];
        if let Some(Finish::Region { rect, lasso, .. }) = &s.finish {
            return match lasso {
                Some(points) if rect.intersect(&monitor).is_some() => {
                    Focus::Lasso(points.iter().map(|p| space.point_to_dip(*p)).collect())
                }
                None if rect.intersect(&monitor).is_some() => Focus::Rect { dip: space.rect_to_dip(*rect), px: *rect },
                _ => Focus::Dimmed,
            };
        }
        match &s.gesture {
            Gesture::Select { selection, monitor } if *monitor == self.index => {
                let px = selection.rect();
                return Focus::Rect { dip: space.rect_to_dip(px), px };
            }
            Gesture::Lasso { lasso, monitor } if *monitor == self.index => {
                return Focus::Lasso(lasso.points().iter().map(|p| space.point_to_dip(*p)).collect());
            }
            _ => {}
        }
        if let Some(armed) = s.armed.filter(|a| a.monitor == self.index) {
            return Focus::Rect { dip: space.rect_to_dip(armed.rect), px: armed.rect };
        }
        match s.prefs.mode {
            CaptureMode::Window => Focus::Window,
            CaptureMode::FullScreen => Focus::Display,
            _ => Focus::Dimmed,
        }
    }

    /// The sampled pixel (monitor-local px) and the loupe center (DIP) when the magnifier shows on this monitor.
    fn magnifier_spot(&self, s: &Session, space: &Space, bounds: RectF) -> Option<(PointI, PointF)> {
        let wanted = match s.prefs.mode {
            CaptureMode::ColorPicker => true,
            CaptureMode::Rectangle | CaptureMode::Text | CaptureMode::Freeform => s.prefs.show_magnifier,
            CaptureMode::Window | CaptureMode::FullScreen => false,
        };
        if !wanted || s.finish.is_some() || self.menu.is_open() || self.bitmap.is_none() {
            return None;
        }
        let (pixel, anchor) = match &s.gesture {
            Gesture::Select { selection, monitor } if *monitor == self.index => (selection.corner(), Some(selection.anchor())),
            Gesture::Lasso { monitor, .. } if *monitor == self.index => (s.cursor?, None),
            Gesture::Idle | Gesture::Click if s.armed.is_none() => {
                let cursor = s.cursor?;
                (s.monitor_under(cursor) == Some(self.index)).then_some((cursor, None))?
            }
            _ => return None,
        };
        let monitor = s.monitor_rects[self.index];
        let pixel = PointI::new(pixel.x.clamp(monitor.x, monitor.right() - 1), pixel.y.clamp(monitor.y, monitor.bottom() - 1));
        let cursor = space.to_dip(pixel);
        if self.over_chrome(cursor) {
            return None;
        }
        let away = anchor.map(|a| space.to_dip(a));
        let center = magnifier_center(cursor, away, magnifier::DIAMETER, magnifier::OFFSET, magnifier::INFO_SPACE, bounds.inset(EDGE_MARGIN));
        Some((PointI::new(pixel.x - monitor.x, pixel.y - monitor.y), center))
    }

    fn paint_selection(&self, p: &mut Painter, rect: RectF) {
        let px = p.px();
        p.stroke_rect(rect.inset(-px * 1.5), Color::rgba(0.0, 0.0, 0.0, 0.25), px);
        p.stroke_rect(rect.inset(-px * 0.5), Color::WHITE, px);
    }

    fn badge_spot(&self, p: &Painter, rect: RectF, size: glint_core::SizeF, avoid: Option<RectF>) -> RectF {
        let avoid = avoid.or_else(|| self.record_bar.is_visible().then(|| self.record_bar.rect()));
        place_near(rect, size, EDGE_MARGIN, p.bounds().inset(EDGE_MARGIN), avoid)
    }

    fn paint_size_badge(&self, p: &mut Painter, rect: RectF, px: RectI, avoid: Option<RectF>) {
        let badge = Badge::new(&format!("{} × {}", px.w, px.h));
        let spot = self.badge_spot(p, rect, badge.size(p.gfx()), avoid);
        badge.paint(p, p.snap_rect(spot));
    }

    fn paint_lasso(&self, p: &mut Painter, points: &[PointF]) {
        if points.len() < 2 {
            return;
        }
        let mut open = PathBuilder::new();
        smooth_path(&mut open, points, false);
        let Ok(stroke) = open.build(p.gfx()) else { return };
        let round = StrokeStyle::round();
        p.stroke_path(&stroke, Color::rgba(0.0, 0.0, 0.0, 0.35), 4.0, &round);
        p.stroke_path(&stroke, Color::WHITE, 2.0, &round);
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            let dashed = StrokeStyle { cap: LineCap::Round, ..StrokeStyle::dashed(&[0.5, 3.0]) };
            p.line(*last, *first, Color::rgba(1.0, 1.0, 1.0, 0.7), 1.5, &dashed);
        }
    }

    fn paint_display_hint(&self, p: &mut Painter, s: &Session, bounds: RectF, opacity: f32) {
        let monitor = &s.monitors[self.index];
        let all = s.ctrl && s.monitor_rects.len() > 1;
        let text = if all {
            format!("All displays  {} × {}", s.desktop.w, s.desktop.h)
        } else {
            format!("{}  {} × {}", monitor.friendly_name, monitor.rect.w, monitor.rect.h)
        };
        let badge = Badge::new(&text).icon(Icon::Monitor);
        let below_toolbar = if self.toolbar.is_visible() { self.toolbar.rect().bottom() + 12.0 } else { TOOLBAR_TOP };
        let center = PointF::new(bounds.center().x, below_toolbar + 12.0);
        p.layer(opacity, |p| {
            badge.paint_centered(p, p.snap_point(center));
        });
    }

    /// Dim everywhere but `hole` (even-odd), for the live desktop where nothing undimmed can be redrawn.
    fn dim_around(&self, p: &mut Painter, dim: Color, add_hole: impl FnOnce(&mut PathBuilder)) {
        let bounds = p.bounds();
        let mut path = PathBuilder::new().even_odd();
        path.polyline(
            &[PointF::new(bounds.x, bounds.y), PointF::new(bounds.right(), bounds.y), PointF::new(bounds.right(), bounds.bottom()), PointF::new(bounds.x, bounds.bottom())],
            true,
        );
        add_hole(&mut path);
        match path.build(p.gfx()) {
            Ok(path) => p.fill_path(&path, dim),
            Err(_) => p.fill_rect(bounds, dim),
        }
    }

    fn paint_focus(&self, p: &mut Painter, s: &Session, space: &Space, avoid: Option<RectF>, time: f64) -> Color {
        let bounds = p.bounds();
        let theme = p.theme().clone();
        let dim = theme.overlay_dim;
        match self.focus(s, space) {
            Focus::Dimmed => p.fill_rect(bounds, dim),
            Focus::Rect { dip, px } => {
                p.fill_rect(RectF::from_ltrb(bounds.x, bounds.y, bounds.right(), dip.y), dim);
                p.fill_rect(RectF::from_ltrb(bounds.x, dip.bottom(), bounds.right(), bounds.bottom()), dim);
                p.fill_rect(RectF::from_ltrb(bounds.x, dip.y, dip.x, dip.bottom()), dim);
                p.fill_rect(RectF::from_ltrb(dip.right(), dip.y, bounds.right(), dip.bottom()), dim);
                if px.w > 0 && px.h > 0 {
                    self.paint_selection(p, dip);
                    if s.is_waiting() {
                        self.paint_busy_badge(p, dip, avoid, time);
                    } else if s.finish.is_none() {
                        self.paint_size_badge(p, dip, px, avoid);
                    }
                }
            }
            Focus::Lasso(points) => {
                match &self.bitmap {
                    Some(bitmap) => {
                        p.fill_rect(bounds, dim);
                        let mut closed = PathBuilder::new();
                        smooth_path(&mut closed, &points, true);
                        if let Ok(path) = closed.build(p.gfx()) {
                            p.clip_path(&path, |p| p.bitmap(bitmap, bounds, None, 1.0, Interpolation::Nearest));
                        }
                    }
                    None => self.dim_around(p, dim, |path| smooth_path(path, &points, true)),
                }
                self.paint_lasso(p, &points);
                if s.is_waiting()
                    && let Some(bbox) = crate::freeform::points_bbox(&points)
                {
                    self.paint_busy_badge(p, bbox.to_f(), avoid, time);
                }
            }
            Focus::Window => {
                let opacity = self.highlight_opacity.get();
                if opacity <= 0.001 {
                    p.fill_rect(bounds, dim);
                } else {
                    let rect = p.snap_rect(self.highlight.get());
                    self.dim_around(p, dim, |path| {
                        path.round_rect(rect, HIGHLIGHT_RADIUS);
                    });
                    p.fill_round_rect(rect, HIGHLIGHT_RADIUS, dim.multiply_alpha(1.0 - opacity));
                    p.layer(opacity, |p| {
                        p.fill_round_rect(rect, HIGHLIGHT_RADIUS, theme.accent.with_alpha(0.08));
                        p.stroke_round_rect(rect.inset(1.0), HIGHLIGHT_RADIUS - 1.0, theme.accent, 2.0);
                    });
                }
            }
            Focus::Display => {
                let lit = self.lit.get().clamp(0.0, 1.0);
                let lit_dim = dim.multiply_alpha(1.0 - lit);
                p.fill_rect(bounds, lit_dim);
                if lit > 0.001 {
                    p.stroke_rect(bounds.inset(1.0), theme.accent.with_alpha(lit), 2.0);
                    self.paint_display_hint(p, s, bounds, lit);
                }
                return lit_dim;
            }
        }
        dim
    }

    /// "Capturing…" with an activity indicator where the size badge was, while the outcome waits for the pixels.
    fn paint_busy_badge(&self, p: &mut Painter, rect: RectF, avoid: Option<RectF>, time: f64) {
        let theme = p.theme().clone();
        let style = TextStyle::new(12.0).weight(Weight::Semibold);
        let label = "Capturing…";
        let text_w = p.measure(label, &style).w;
        let size = glint_core::SizeF::new((10.0 + 14.0 + 6.0 + text_w + 12.0).ceil(), 24.0);
        let pill = p.snap_rect(self.badge_spot(p, rect, size, avoid));
        let radius = pill.h / 2.0;
        p.shadow_outside(pill, radius, &Shadow::new(2.0, 8.0, Color::rgba(0.0, 0.0, 0.0, 0.35)));
        p.fill_round_rect(pill, radius, theme.glass_fill_solid);
        p.hairline_round_rect(pill, radius, theme.hairline, true);
        p.hairline_round_rect(pill, radius, theme.outer_border, false);
        let center = PointF::new(pill.x + 10.0 + 7.0, pill.center().y);
        let head = (time * 12.0).floor() as i64;
        for spoke in 0..8i64 {
            let angle = spoke as f32 * std::f32::consts::TAU / 8.0;
            let age = (head - spoke).rem_euclid(8) as f32;
            let (sin, cos) = angle.sin_cos();
            let ink = theme.text.multiply_alpha(1.0 - age / 9.0);
            let a = PointF::new(center.x + cos * 3.2, center.y + sin * 3.2);
            let b = PointF::new(center.x + cos * 6.2, center.y + sin * 6.2);
            p.line(a, b, ink, 1.6, &StrokeStyle::round());
        }
        p.text(label, &style, theme.text, RectF::new(pill.x + 30.0, pill.y, text_w + 1.0, pill.h));
    }

    /// Toolbar, record bar and menu: solid glass over the live desktop, crossfading to frosted glass over the frozen
    /// image once it is there (same pixels, so nothing jumps).
    fn paint_chrome(&mut self, p: &mut Painter, backdrop_dim: Color) {
        let bounds = p.bounds();
        let frosted = if self.backdrop.is_some() { self.frosted.get().clamp(0.0, 1.0) } else { 0.0 };
        let backdrop = self.backdrop.clone();
        let paint = |view: &mut Self, p: &mut Painter, backdrop: Option<&Backdrop>| {
            view.record_bar.paint(p, backdrop);
            view.toolbar.paint(p, backdrop);
            view.menu.paint(p, backdrop);
        };
        if frosted < 0.999 {
            paint(self, p, None);
        }
        if frosted > 0.001
            && let Some(blurred) = &backdrop
        {
            let backdrop = Backdrop::new(blurred, bounds).dimmed(backdrop_dim);
            p.layer(frosted, |p| paint(self, p, Some(&backdrop)));
        }
    }

    fn paint_frame(&mut self, cx: &mut Ctx, p: &mut Painter) {
        let Some(shared) = self.shared.clone() else { return };
        if self.closing && !self.fade.is_animating() {
            if self.reusable {
                cx.hide_window();
                self.leave(cx);
            } else {
                cx.close();
            }
            return;
        }
        let started = Instant::now();
        let s = shared.borrow();
        let bounds = p.bounds();
        let space = self.space(&s, p.scale());
        self.prepare(p, &space, &s);
        if let Some(bitmap) = &self.bitmap {
            p.bitmap(bitmap, bounds, None, 1.0, Interpolation::Nearest);
        }
        let magnifier = self.magnifier_spot(&s, &space, bounds);
        let avoid = magnifier.map(|(_, center)| magnifier::extent(center));
        let backdrop_dim = self.paint_focus(p, &s, &space, avoid, cx.time());
        if s.is_waiting() {
            if let Some(Finish::ColorAt(at)) = s.finish
                && s.monitor_under(at) == Some(self.index)
            {
                let spot = space.to_dip(at);
                self.paint_busy_badge(p, RectF::new(spot.x, spot.y, 0.0, 0.0), None, cx.time());
            }
            cx.animate();
        }
        if let (Some((pixel, center)), Some(bitmap), Some(frozen)) = (magnifier, &self.bitmap, s.pixels(self.index)) {
            Loupe { source: bitmap, image: &frozen.sdr, pixel }.paint(p, center);
        }
        self.paint_chrome(p, backdrop_dim);
        if let Some((anchor, text, shortcut)) = &self.preview.tooltip {
            Tooltip::paint_bubble(p, *anchor, text, shortcut.as_deref(), 1.0, bounds.size());
        }
        let cursor_dip = s.cursor.map(|c| space.to_dip(c));
        cx.set_cursor(self.cursor_for(&s, cursor_dip));
        if self.closing {
            cx.set_window_opacity(self.fade.get());
            cx.animate();
        }
        if std::mem::take(&mut self.first_frame) {
            log::info!("overlay: monitor {} first frame painted in {:.1} ms", self.index, ms_since(started));
        }
    }
}

fn key_up(s: &mut Session, k: &KeyEvent) -> bool {
    match k.key {
        Key::Space => s.space = false,
        Key::Control => s.ctrl = false,
        Key::Shift => refresh_square(s, false),
        _ => return false,
    }
    true
}

fn refresh_square(s: &mut Session, square: bool) {
    if let Gesture::Select { selection, .. } = &mut s.gesture {
        selection.refresh(square);
    }
}

fn press_enter(s: &mut Session) {
    if let Some(armed) = s.armed {
        s.end(Finish::Record(armed));
        return;
    }
    let Some(cursor) = s.cursor else { return };
    match s.prefs.mode {
        CaptureMode::ColorPicker => s.end(Finish::ColorAt(cursor)),
        CaptureMode::Window | CaptureMode::FullScreen => {
            if let Some(rect) = s.click_target(cursor) {
                s.complete_region(rect, s.prefs.mode);
            }
        }
        mode => {
            if let Some(i) = s.monitor_under(cursor) {
                let kind = if mode == CaptureMode::Text { CaptureMode::Text } else { CaptureMode::FullScreen };
                s.complete_region(s.monitor_rects[i], kind);
            }
        }
    }
}

fn pointer_up(s: &mut Session, e: &PointerEvent) -> bool {
    if e.button != Some(MouseButton::Left) {
        return false;
    }
    s.ctrl = e.mods.ctrl;
    match std::mem::replace(&mut s.gesture, Gesture::Idle) {
        Gesture::Select { selection, .. } => {
            if selection.is_meaningful() {
                let mode = if s.prefs.mode == CaptureMode::Text { CaptureMode::Text } else { CaptureMode::Rectangle };
                s.complete_region(selection.rect(), mode);
            }
        }
        Gesture::Lasso { lasso, .. } => {
            if lasso.is_meaningful() {
                s.end(Finish::Region { rect: lasso.bbox(), mode: CaptureMode::Freeform, lasso: Some(lasso.points().to_vec()) });
            }
        }
        Gesture::Click => match s.prefs.mode {
            CaptureMode::ColorPicker => {
                if s.monitor_under(e.screen_px).is_some() {
                    s.end(Finish::ColorAt(e.screen_px));
                }
            }
            CaptureMode::Window | CaptureMode::FullScreen => {
                if let Some(rect) = s.click_target(e.screen_px) {
                    s.complete_region(rect, s.prefs.mode);
                }
            }
            _ => {}
        },
        Gesture::Idle => {}
    }
    true
}

fn record_chosen(s: &mut Session, action: RecordAction) {
    match action {
        RecordAction::Record => {
            if let Some(armed) = s.armed {
                s.end(Finish::Record(armed));
            }
        }
        RecordAction::Microphone => s.prefs.microphone = !s.prefs.microphone,
        RecordAction::SystemAudio => s.prefs.system_audio = !s.prefs.system_audio,
        RecordAction::Cancel => s.end(Finish::Cancelled),
    }
}

impl View for OverlayView {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        if let Event::Closed = event {
            self.leave(cx);
            return true;
        }
        let Some(shared) = self.shared.clone() else { return false };
        match event {
            Event::Shown => {
                let s = shared.borrow();
                if s.toolbar_monitor == self.index {
                    cx.activate();
                }
                if let Some(at) = s.requested_at.filter(|_| !std::mem::replace(&mut self.shown_logged, true)) {
                    log::info!("overlay: monitor {} on screen {:.1} ms after the request", self.index, ms_since(at));
                }
                return false;
            }
            Event::Focus(false) if self.menu.is_open() => {
                self.menu.close();
                cx.request_paint();
                return false;
            }
            _ => {}
        }
        let mut s = shared.borrow_mut();
        if let Event::CloseRequested = event {
            s.end(Finish::Cancelled);
        } else if s.is_closing() {
            return true;
        }
        let before = Visible::of(&s);
        let handled = if s.is_waiting() { self.route_waiting(cx, &mut s, event) } else { self.route(cx, &mut s, event) };
        if Visible::of(&s) != before
            && let Some(app) = cx.app()
        {
            arm_close_fallback(app, &shared, &mut s);
            broadcast(app, &s);
        }
        handled
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.paint_frame(cx, p);
    }
}
