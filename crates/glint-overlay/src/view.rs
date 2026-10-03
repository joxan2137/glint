//! One overlay window per monitor: frozen image, dim, selection visuals, magnifier and controls.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use glint_core::{CaptureMode, PointF, PointI, RectF, RectI};
use glint_ui::widgets::{Badge, Menu, Response, Toolbar, ToolbarAction, Tooltip};
use glint_ui::{
    Animated, App, Backdrop, Bitmap, Color, Ctx, Cursor, Event, Gfx, Icon, Interpolation, Key, KeyEvent, MouseButton,
    Motion, Painter, PathBuilder, PointerEvent, StrokeStyle, Theme, Tween, View,
};

use crate::chrome::{self, MAGNIFIER_ITEM, MODE_SEGMENTS, RecordAction, RecordBar, TOOLBAR_TOP};
use crate::freeform::{Lasso, smooth_path};
use crate::geometry::{Selection, Space, magnifier_center, place_near};
use crate::magnifier::{self, Loupe};
use crate::state::{DELAYS, Finish, Gesture, Session};
use crate::sys;

pub type Shared = Rc<RefCell<Session>>;

const HIGHLIGHT_RADIUS: f32 = 8.0;
const EDGE_MARGIN: f32 = 8.0;
/// Windows still open this long after the session ended (e.g. one that cannot render its fade) are closed directly.
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
    shared: Shared,
    index: usize,
    bitmap: Rc<Bitmap>,
    backdrop: Rc<Bitmap>,
    pub(crate) toolbar: Toolbar,
    pub(crate) menu: Menu,
    pub(crate) record_bar: RecordBar,
    highlight: Animated<RectF>,
    highlight_opacity: Animated<f32>,
    highlight_shown: bool,
    lit: Animated<f32>,
    fade: Animated<f32>,
    closing: bool,
    pub(crate) preview: PreviewExtras,
}

fn broadcast(cx: &Ctx, session: &Session) {
    if let Some(app) = cx.app() {
        for id in &session.window_ids {
            app.request_paint(*id);
        }
    }
}

/// Once per session, after it ended: closes whatever overlay windows are still open after `CLOSE_FALLBACK`.
fn arm_close_fallback(cx: &Ctx, shared: &Shared, s: &mut Session) {
    let Some(app) = cx.app() else { return };
    if s.close_fallback_armed || !s.is_closing() {
        return;
    }
    s.close_fallback_armed = true;
    let shared = shared.clone();
    app.set_timer(CLOSE_FALLBACK, move |app| force_close(app, shared, CLOSE_ATTEMPTS));
}

fn force_close(app: &App, shared: Shared, attempts_left: u32) {
    let open: Vec<_> = shared.borrow().window_ids.iter().copied().filter(|id| app.hwnd(*id).is_some()).collect();
    if open.is_empty() || attempts_left == 0 {
        if !open.is_empty() {
            log::warn!("overlay: {} window(s) did not close; reporting the outcome anyway", open.len());
        }
        deliver(app, &shared);
        return;
    }
    for id in open {
        app.close(id);
    }
    app.set_timer(CLOSE_RETRY, move |app| force_close(app, shared, attempts_left - 1));
}

/// Hands the outcome to `done` (exactly once: later calls find it taken).
fn deliver(app: &App, shared: &Shared) {
    let mut s = shared.borrow_mut();
    s.end(Finish::Cancelled);
    let Some(done) = s.done.take() else { return };
    let finish = s.finish.clone().unwrap_or(Finish::Cancelled);
    let frozen = std::mem::take(&mut s.frozen);
    let (hdr, prefs) = (s.hdr.clone(), s.prefs.clone());
    drop(s);
    let gfx = app.gfx();
    app.set_timer(Duration::ZERO, move |app| {
        let outcome = finish.resolve(&frozen, &hdr, &prefs, &gfx);
        done(app, outcome, prefs);
    });
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
            closing: s.is_closing(),
        }
    }
}

impl OverlayView {
    pub fn new(shared: Shared, index: usize, theme: &Theme) -> Self {
        let (bitmap, scale, prefs) = {
            let s = shared.borrow();
            let frozen = &s.frozen[index];
            (Bitmap::from_shared(frozen.sdr.clone()), frozen.info.scale(), s.prefs.clone())
        };
        let backdrop = bitmap.glass_backdrop(theme, scale);
        let mut toolbar = chrome::capture_toolbar(prefs.mode, prefs.video);
        toolbar.snap_visible(false);
        let mut record_bar = RecordBar::new();
        record_bar.set_audio(prefs.microphone, prefs.system_audio);
        Self {
            shared,
            index,
            bitmap,
            backdrop,
            toolbar,
            menu: chrome::options_menu(),
            record_bar,
            highlight: Animated::snappy(RectF::default()),
            highlight_opacity: Animated::fade(0.0),
            highlight_shown: false,
            lit: Animated::fade(0.0),
            fade: Animated::with_motion(1.0, Motion::Tween(Tween::FADE_OUT)),
            closing: false,
            preview: PreviewExtras::default(),
        }
    }

    fn space(&self, s: &Session, scale: f32) -> Space {
        Space::new(s.monitor_rects[self.index], scale)
    }

    fn over_chrome(&self, pos: PointF) -> bool {
        self.toolbar.contains(pos) || self.record_bar.contains(pos) || (self.menu.is_open() && self.menu.frame().contains(pos))
    }

    fn cursor_for(&self, s: &Session, pos: Option<PointF>) -> Cursor {
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

    fn on_closed(&mut self, cx: &mut Ctx) {
        {
            let mut s = self.shared.borrow_mut();
            s.end(Finish::Cancelled);
            s.open_windows = s.open_windows.saturating_sub(1);
            if s.open_windows > 0 {
                arm_close_fallback(cx, &self.shared, &mut s);
                broadcast(cx, &s);
                return;
            }
        }
        if let Some(app) = cx.app() {
            deliver(app, &self.shared);
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
    fn prepare(&mut self, gfx: &Gfx, bounds: RectF, space: &Space, s: &Session) {
        let here = s.toolbar_monitor == self.index;
        self.toolbar.layout_centered(gfx, bounds.w / 2.0, TOOLBAR_TOP);
        chrome::sync_toolbar(&mut self.toolbar, s.prefs.mode, s.prefs.video, self.menu.is_open());
        let show_toolbar = here && !s.is_dragging() && !s.is_closing();
        self.toolbar.set_visible(show_toolbar);
        if !show_toolbar && self.menu.is_open() {
            self.menu.close();
        }
        chrome::sync_menu(&mut self.menu, s.prefs.delay_secs, s.prefs.show_magnifier);

        self.record_bar.set_audio(s.prefs.microphone, s.prefs.system_audio);
        match s.armed.filter(|a| a.monitor == self.index && !s.is_dragging() && !s.is_closing()) {
            Some(armed) => {
                self.record_bar.layout(gfx, space.rect_to_dip(armed.rect), bounds.inset(EDGE_MARGIN));
                self.record_bar.set_visible(true);
            }
            None => self.record_bar.set_visible(false),
        }

        let hover = (s.armed.is_none() && !s.is_closing())
            .then(|| s.cursor.and_then(|c| s.click_target(c)))
            .flatten();
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
        if !wanted || s.is_closing() || self.menu.is_open() {
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

    fn paint_undimmed(&self, p: &mut Painter, bounds: RectF) {
        p.bitmap(&self.bitmap, bounds, None, 1.0, Interpolation::Nearest);
    }

    fn paint_selection(&self, p: &mut Painter, rect: RectF) {
        let px = p.px();
        p.stroke_rect(rect.inset(-px * 1.5), Color::rgba(0.0, 0.0, 0.0, 0.25), px);
        p.stroke_rect(rect.inset(-px * 0.5), Color::WHITE, px);
    }

    fn paint_size_badge(&self, p: &mut Painter, rect: RectF, px: RectI, bounds: RectF, avoid: Option<RectF>) {
        let badge = Badge::new(&format!("{} × {}", px.w, px.h));
        let size = badge.size(p.gfx());
        let avoid = avoid.or_else(|| self.record_bar.is_visible().then(|| self.record_bar.rect()));
        let spot = place_near(rect, size, EDGE_MARGIN, bounds.inset(EDGE_MARGIN), avoid);
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
            let dashed = StrokeStyle { cap: glint_ui::LineCap::Round, ..StrokeStyle::dashed(&[0.5, 3.0]) };
            p.line(*last, *first, Color::rgba(1.0, 1.0, 1.0, 0.7), 1.5, &dashed);
        }
    }

    fn paint_display_hint(&self, p: &mut Painter, s: &Session, bounds: RectF, opacity: f32) {
        let monitor = &s.frozen[self.index].info;
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

    fn paint_frame(&mut self, cx: &mut Ctx, p: &mut Painter) {
        let shared = self.shared.clone();
        let s = shared.borrow();
        if s.frozen.len() <= self.index {
            cx.close();
            return;
        }
        let bounds = p.bounds();
        let space = self.space(&s, p.scale());
        self.prepare(p.gfx(), bounds, &space, &s);
        let theme = p.theme().clone();
        let dim = theme.overlay_dim;
        self.paint_undimmed(p, bounds);

        let magnifier = self.magnifier_spot(&s, &space, bounds);
        let avoid = magnifier.map(|(_, center)| magnifier::extent(center));
        let mut backdrop_dim = dim;
        match self.focus(&s, &space) {
            Focus::Dimmed => p.fill_rect(bounds, dim),
            Focus::Rect { dip, px } => {
                p.fill_rect(RectF::from_ltrb(bounds.x, bounds.y, bounds.right(), dip.y), dim);
                p.fill_rect(RectF::from_ltrb(bounds.x, dip.bottom(), bounds.right(), bounds.bottom()), dim);
                p.fill_rect(RectF::from_ltrb(bounds.x, dip.y, dip.x, dip.bottom()), dim);
                p.fill_rect(RectF::from_ltrb(dip.right(), dip.y, bounds.right(), dip.bottom()), dim);
                if px.w > 0 && px.h > 0 {
                    self.paint_selection(p, dip);
                    self.paint_size_badge(p, dip, px, bounds, avoid);
                }
            }
            Focus::Lasso(points) => {
                p.fill_rect(bounds, dim);
                let mut closed = PathBuilder::new();
                smooth_path(&mut closed, &points, true);
                if let Ok(path) = closed.build(p.gfx()) {
                    p.clip_path(&path, |p| self.paint_undimmed(p, bounds));
                }
                self.paint_lasso(p, &points);
            }
            Focus::Window => {
                p.fill_rect(bounds, dim);
                let opacity = self.highlight_opacity.get();
                if opacity > 0.001 {
                    let rect = p.snap_rect(self.highlight.get());
                    p.layer(opacity, |p| {
                        p.clip_round_rect(rect, HIGHLIGHT_RADIUS, |p| self.paint_undimmed(p, bounds));
                        p.fill_round_rect(rect, HIGHLIGHT_RADIUS, theme.accent.with_alpha(0.08));
                        p.stroke_round_rect(rect.inset(1.0), HIGHLIGHT_RADIUS - 1.0, theme.accent, 2.0);
                    });
                }
            }
            Focus::Display => {
                let lit = self.lit.get().clamp(0.0, 1.0);
                backdrop_dim = dim.multiply_alpha(1.0 - lit);
                p.fill_rect(bounds, backdrop_dim);
                if lit > 0.001 {
                    p.stroke_rect(bounds.inset(1.0), theme.accent.with_alpha(lit), 2.0);
                    self.paint_display_hint(p, &s, bounds, lit);
                }
            }
        }

        if let Some((pixel, center)) = magnifier {
            let image = &s.frozen[self.index].sdr;
            Loupe { source: &self.bitmap, image, pixel }.paint(p, center);
        }

        let backdrop = Backdrop::new(&self.backdrop, bounds).dimmed(backdrop_dim);
        self.record_bar.paint(p, Some(&backdrop));
        self.toolbar.paint(p, Some(&backdrop));
        self.menu.paint(p, Some(&backdrop));
        if let Some((anchor, text, shortcut)) = &self.preview.tooltip {
            Tooltip::paint_bubble(p, *anchor, text, shortcut.as_deref(), 1.0, bounds.size());
        }

        let cursor_dip = s.cursor.map(|c| space.to_dip(c));
        cx.set_cursor(self.cursor_for(&s, cursor_dip));
        if self.closing {
            let opacity = self.fade.get();
            cx.set_window_opacity(opacity);
            if self.fade.is_animating() {
                cx.animate();
            } else {
                cx.close();
            }
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
        CaptureMode::ColorPicker => {
            if let Some(bgra) = s.pixel_at(cursor) {
                s.end(Finish::Color(bgra));
            }
        }
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
                if let Some(bgra) = s.pixel_at(e.screen_px) {
                    s.end(Finish::Color(bgra));
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
        match event {
            Event::Closed => {
                self.on_closed(cx);
                return true;
            }
            Event::CloseRequested => {
                let mut s = self.shared.borrow_mut();
                s.end(Finish::Cancelled);
                arm_close_fallback(cx, &self.shared, &mut s);
                broadcast(cx, &s);
                return true;
            }
            Event::Shown => {
                if self.shared.borrow().toolbar_monitor == self.index {
                    cx.activate();
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
        let shared = self.shared.clone();
        let mut s = shared.borrow_mut();
        if s.is_closing() {
            return true;
        }
        let before = Visible::of(&s);
        let handled = self.route(cx, &mut s, event);
        if Visible::of(&s) != before {
            arm_close_fallback(cx, &shared, &mut s);
            broadcast(cx, &s);
        }
        handled
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.paint_frame(cx, p);
    }
}
