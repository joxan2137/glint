//! Opening sessions: on windows created for the session, or on pooled windows created hidden in advance (swapchain
//! ready, caches warm) so a hotkey only resets state, paints the pending frame and shows.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use glint_core::{CaptureMode, Image, MonitorCapture, MonitorInfo, PointF, RectF, SizeF};
use glint_ui::widgets::Badge;
use glint_ui::{App, Color, Gfx, OffscreenSpec, TextStyle, Theme, Weight, WindowId, WindowSpec, render_offscreen};

use crate::chrome;
use crate::state::Session;
use crate::view::{OverlayView, Shared, arm_close_fallback};
use crate::{OverlayOutcome, OverlayPrefs, OverlayRequest, sys};

type Done = Box<dyn FnOnce(&App, OverlayOutcome, OverlayPrefs)>;

fn ms_since(at: Instant) -> f64 {
    at.elapsed().as_secs_f64() * 1000.0
}

/// A running overlay. Cheap to clone; every call is a no-op once the session has ended.
#[derive(Clone)]
pub struct OverlaySession {
    shared: Weak<RefCell<Session>>,
}

impl OverlaySession {
    /// Hands over the frozen desktop: each window swaps it in under its dim (same pixels, so nothing jumps); an ending
    /// that waited for pixels (a finished selection, a picked color) completes now.
    pub fn set_captures(&self, app: &App, captures: Vec<MonitorCapture>) {
        let Some(shared) = self.shared.upgrade() else { return };
        let mut s = shared.borrow_mut();
        if s.done.is_none() {
            return;
        }
        let count = captures.len();
        s.attach(captures);
        if let Some(at) = s.requested_at {
            log::info!("overlay: {count} capture(s) delivered {:.1} ms after the request", ms_since(at));
        }
        arm_close_fallback(app, &shared, &mut s);
        for id in &s.window_ids {
            app.request_paint(*id);
        }
    }

    /// One monitor's final SDR image ahead of the complete captures (`glint_capture::capture_all_streaming`): that
    /// monitor freezes at once; `set_captures` still completes the session.
    pub fn show_frozen(&self, app: &App, monitor: &MonitorInfo, sdr: Image) {
        let Some(shared) = self.shared.upgrade() else { return };
        let mut s = shared.borrow_mut();
        if s.done.is_none() || s.captured {
            return;
        }
        s.attach_early(monitor, sdr);
        if let Some(at) = s.requested_at {
            log::info!("overlay: {} frozen {:.1} ms after the request", monitor.device_name, ms_since(at));
        }
        for id in &s.window_ids {
            app.request_paint(*id);
        }
    }

    /// Ends the session with `Cancelled` (e.g. the capture failed); a finished selection still waiting is dropped.
    pub fn cancel(&self, app: &App) {
        let Some(shared) = self.shared.upgrade() else { return };
        let mut s = shared.borrow_mut();
        s.end(crate::state::Finish::Cancelled);
        arm_close_fallback(app, &shared, &mut s);
        for id in &s.window_ids {
            app.request_paint(*id);
        }
    }

    /// True until `done` has been called.
    pub fn is_open(&self) -> bool {
        self.shared.upgrade().is_some_and(|s| s.borrow().done.is_some())
    }

    /// The captures were handed over (or the session ended).
    pub fn has_captures(&self) -> bool {
        self.shared.upgrade().is_none_or(|s| s.borrow().captured)
    }
}

fn new_session(request: OverlayRequest, done: Done) -> Result<Shared> {
    let OverlayRequest {
        monitors,
        captures,
        windows,
        mode,
        video,
        show_magnifier,
        delay_secs,
        hdr,
        system_audio,
        microphone,
        requested_at,
    } = request;
    let monitors = if monitors.is_empty() { captures.iter().map(|c| c.monitor.clone()).collect() } else { monitors };
    ensure!(!monitors.is_empty(), "no monitors to cover");
    let prefs = OverlayPrefs { mode, video, delay_secs, show_magnifier, system_audio, microphone };
    let mut session = Session::new(monitors, windows, hdr, prefs, sys::cursor_pos());
    session.requested_at = requested_at;
    if !captures.is_empty() {
        session.attach(captures);
    }
    session.done = Some(done);
    Ok(Rc::new(RefCell::new(session)))
}

/// Toolbar monitor last, so the window that should have the keyboard is activated last.
fn show_order(shared: &Shared) -> Vec<usize> {
    let s = shared.borrow();
    let toolbar = s.toolbar_monitor;
    (0..s.monitors.len()).filter(|&i| i != toolbar).chain([toolbar]).collect()
}

fn enlist(shared: &Shared, id: WindowId) {
    let mut s = shared.borrow_mut();
    s.window_ids.push(id);
    s.open_windows += 1;
}

fn handle(shared: &Shared, opened: &str, started: Instant) -> Result<OverlaySession> {
    let count = shared.borrow().open_windows;
    if count == 0 {
        shared.borrow_mut().done = None;
        bail!("no overlay window could be opened");
    }
    log::info!("overlay: {count} {opened} window(s) set up in {:.1} ms", ms_since(started));
    Ok(OverlaySession { shared: Rc::downgrade(shared) })
}

/// Opens one overlay window per monitor for this session only (they close when it ends). See `OverlayPool::open`.
pub fn open_fresh(app: &App, request: OverlayRequest, done: Done) -> Result<OverlaySession> {
    let started = Instant::now();
    let shared = new_session(request, done)?;
    for i in show_order(&shared) {
        let rect = shared.borrow().monitor_rects[i];
        let view = OverlayView::attached(shared.clone(), i);
        match app.open(WindowSpec::overlay(rect).exclude_from_capture(), view) {
            Ok(id) => enlist(&shared, id),
            Err(e) => log::error!("overlay window for monitor {i}: {e:#}"),
        }
    }
    handle(&shared, "new", started)
}

/// Hidden overlay windows, one per monitor, kept ready (swapchain created, toolbar caches warm) so that opening an
/// overlay only resets state, paints and shows. Rebuild it when the monitor layout or DPI changes.
pub struct OverlayPool {
    slots: Vec<(MonitorInfo, WindowId)>,
}

impl OverlayPool {
    pub fn create(app: &App, monitors: &[MonitorInfo]) -> Result<OverlayPool> {
        let started = Instant::now();
        ensure!(!monitors.is_empty(), "no monitors to cover");
        let scales: Vec<f32> = monitors.iter().map(MonitorInfo::scale).collect();
        warm_up(&app.gfx(), &scales);
        let mut pool = OverlayPool { slots: Vec::new() };
        for monitor in monitors {
            let spec = WindowSpec::overlay(monitor.rect).hidden().exclude_from_capture();
            let id = match app.open(spec, OverlayView::idle()).with_context(|| format!("overlay window for {}", monitor.device_name)) {
                Ok(id) => id,
                Err(error) => {
                    pool.destroy(app);
                    return Err(error);
                }
            };
            app.with_view(id, |_: &mut OverlayView, cx| cx.render_hidden());
            pool.slots.push((monitor.clone(), id));
        }
        log::info!("overlay pool: {} hidden window(s) ready in {:.1} ms", pool.slots.len(), ms_since(started));
        Ok(pool)
    }

    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.slots.iter().map(|(m, _)| m.clone()).collect()
    }

    /// Same monitors in the same order, at the same rects and DPI.
    pub fn covers(&self, monitors: &[MonitorInfo]) -> bool {
        self.slots.len() == monitors.len()
            && self.slots.iter().zip(monitors).all(|((mine, _), other)| mine.rect == other.rect && mine.dpi == other.dpi)
    }

    /// Every window still exists and is not in a session.
    pub fn is_ready(&self, app: &App) -> bool {
        self.slots.iter().all(|(_, id)| app.with_view(*id, |view: &mut OverlayView, _| view.is_idle()).unwrap_or(false))
    }

    /// Opens a session on the pooled windows; falls back to new windows when the pool does not fit the request.
    pub fn open(
        &self,
        app: &App,
        request: OverlayRequest,
        done: impl FnOnce(&App, OverlayOutcome, OverlayPrefs) + 'static,
    ) -> Result<OverlaySession> {
        let done: Done = Box::new(done);
        if !self.covers(&request.monitors) || !self.is_ready(app) {
            log::info!("overlay pool unavailable for this request; opening new windows");
            return open_fresh(app, request, done);
        }
        let started = Instant::now();
        let shared = new_session(request, done)?;
        let toolbar = shared.borrow().toolbar_monitor;
        for i in show_order(&shared) {
            let id = self.slots[i].1;
            let joined = app.with_view(id, |view: &mut OverlayView, cx| view.begin(cx, shared.clone(), i, i == toolbar));
            match joined {
                Some(()) => enlist(&shared, id),
                None => log::error!("pooled overlay window for monitor {i} is unavailable"),
            }
        }
        handle(&shared, "pooled", started)
    }

    pub fn destroy(&self, app: &App) {
        for (_, id) in &self.slots {
            app.close(*id);
        }
    }
}

/// Renders the controls once offscreen at each monitor scale so the first overlay frame finds text layouts, fonts,
/// icon geometry and glass shadows cached.
pub fn warm_up(gfx: &Rc<Gfx>, scales: &[f32]) {
    let started = Instant::now();
    let mut done: Vec<f32> = Vec::new();
    for &scale in scales {
        if done.iter().any(|s| (s - scale).abs() < 0.001) {
            continue;
        }
        done.push(scale);
        let spec = OffscreenSpec::new(SizeF::new(640.0, 160.0), scale, Theme::dark());
        let rendered = render_offscreen(gfx, &spec, |_, p| {
            let mut toolbar = chrome::capture_toolbar(CaptureMode::Rectangle, false);
            toolbar.layout_at(p.gfx(), PointF::new(8.0, 8.0));
            toolbar.paint(p, None);
            Badge::new("1280 × 720").paint_centered(p, PointF::new(520.0, 120.0));
            let busy = TextStyle::new(12.0).weight(Weight::Semibold);
            p.text("Capturing…", &busy, Color::WHITE, RectF::new(8.0, 120.0, 200.0, 24.0));
        });
        if let Err(error) = rendered {
            log::warn!("overlay warm-up at scale {scale}: {error:#}");
        }
    }
    log::info!("overlay: controls warmed up for {} scale(s) in {:.1} ms", done.len(), ms_since(started));
}

#[cfg(test)]
mod latency {
    use glint_core::settings::HdrSettings;
    use glint_core::{Image, RectI};
    use glint_ui::View;

    use super::*;

    fn millis(f: impl FnOnce()) -> f64 {
        let started = Instant::now();
        f();
        ms_since(started)
    }

    /// Headless costs of the hotkey path; run with `cargo test -p glint-overlay --release latency -- --ignored
    /// --nocapture`. Offscreen frames include a readback, measured separately and subtracted.
    #[test]
    #[ignore]
    fn hotkey_path_costs() {
        glint_capture::enable_per_monitor_dpi_awareness();
        let mut monitors = Vec::new();
        let cold = millis(|| monitors = glint_capture::monitors().unwrap());
        let warm = millis(|| monitors = glint_capture::monitors().unwrap());
        let mut windows = Vec::new();
        let snapshot = millis(|| windows = glint_capture::windows_snapshot(Some(std::process::id())));
        println!("monitors(): cold {cold:.1} ms, warm {warm:.1} ms; windows_snapshot: {snapshot:.2} ms ({} windows)", windows.len());
        let gfx = Gfx::new().unwrap();
        let scales: Vec<f32> = monitors.iter().map(MonitorInfo::scale).collect();
        println!("warm_up (cold process): {:.1} ms", millis(|| warm_up(&gfx, &scales)));
        for (i, monitor) in monitors.iter().enumerate() {
            let rect: RectI = monitor.rect;
            let spec = OffscreenSpec::pixels(rect.w as u32, rect.h as u32, monitor.scale(), Theme::dark());
            let readback = (0..5).map(|_| millis(|| drop(render_offscreen(&gfx, &spec, |_, _| {}).unwrap()))).fold(f64::MAX, f64::min);
            let prefs = OverlayPrefs { mode: CaptureMode::Rectangle, video: false, delay_secs: 0, show_magnifier: true, system_audio: true, microphone: false };
            let shared = Rc::new(RefCell::new(Session::new(vec![monitor.clone()], windows.clone(), HdrSettings::default(), prefs, None)));
            let mut view = OverlayView::attached(shared.clone(), 0);
            let mut frame = |time: f64| {
                let mut paint = 0.0;
                let total = millis(|| {
                    let spec = spec.clone().time(time);
                    drop(render_offscreen(&gfx, &spec, |cx, p| paint = millis(|| view.paint(cx, p))).unwrap());
                });
                (paint, (total - readback).max(paint))
            };
            let first = frame(100.0);
            let second = frame(100.016);
            let toolbar = frame(100.3);
            let mut sdr = Image::new(rect.w as u32, rect.h as u32);
            sdr.data.chunks_exact_mut(4).for_each(|px| px.copy_from_slice(&[90, 80, 70, 255]));
            shared.borrow_mut().attach(vec![MonitorCapture { monitor: monitor.clone(), sdr, hdr: None, hdr_stats: None }]);
            let swap = frame(100.4);
            let frosted = frame(100.6);
            println!(
                "monitor {i} {}x{} @{:.2} (paint CPU / with GPU flush, readback {readback:.1} ms excluded): pending first frame                  {:.2}/{:.2} ms, next {:.2}/{:.2} ms, toolbar shown {:.2}/{:.2} ms; frozen swap (upload + blur) {:.2}/{:.2} ms,                  steady {:.2}/{:.2} ms",
                rect.w,
                rect.h,
                monitor.scale(),
                first.0, first.1, second.0, second.1, toolbar.0, toolbar.1, swap.0, swap.1, frosted.0, frosted.1
            );
        }
    }
}
