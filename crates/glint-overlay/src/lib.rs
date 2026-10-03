//! The frozen-screen selection overlay (DESIGN §6) and the delay countdown pill.

mod chrome;
mod countdown;
mod freeform;
mod geometry;
mod magnifier;
mod preview;
mod snip;
mod state;
mod sys;
mod view;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{bail, ensure};
use glint_core::settings::HdrSettings;
use glint_core::{CaptureMode, HdrImage, HdrStats, Image, MonitorCapture, MonitorInfo, RectI, ThemeMode, WindowInfo};
use glint_ui::{App, Gfx, WindowSpec};

pub use preview::KINDS as PREVIEW_KINDS;

use crate::snip::Source;
use crate::state::{Frozen, Session};
use crate::view::OverlayView;

pub struct OverlayRequest {
    pub captures: Vec<MonitorCapture>,
    pub windows: Vec<WindowInfo>,
    pub mode: CaptureMode,
    /// Start in Video mode (Win+Shift+R).
    pub video: bool,
    pub show_magnifier: bool,
    pub delay_secs: u32,
    pub hdr: HdrSettings,
    pub system_audio: bool,
    pub microphone: bool,
}

/// A finished image capture.
pub struct Snip {
    /// Final SDR pixels (re-tone-mapped for the region per DESIGN §3.6), alpha 0 outside a freeform lasso.
    pub image: Image,
    /// Raw scRGB pixels of the same rect when the source monitor was HDR (lets the editor re-tone-map).
    pub hdr: Option<HdrImage>,
    pub hdr_stats: Option<HdrStats>,
    /// Captured rect in physical pixels, virtual-desktop coordinates.
    pub rect_px: RectI,
    /// Monitor that holds most of the rect (thumbnail and editor placement).
    pub monitor: MonitorInfo,
    pub mode: CaptureMode,
}

pub struct RecordRegion {
    pub monitor: MonitorInfo,
    /// Physical pixels relative to monitor.rect origin.
    pub region: RectI,
    pub system_audio: bool,
    pub microphone: bool,
}

pub enum OverlayOutcome {
    Cancelled,
    Snip(Snip),
    /// Text mode: the app runs OCR on the image and copies the text.
    Text(Snip),
    Color { bgra: [u8; 4] },
    Record(RecordRegion),
    /// The user picked a delay: the app shows the countdown, captures again and reopens the overlay.
    Delay { secs: u32 },
}

/// Choices the user made in the overlay that the app persists in settings.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayPrefs {
    pub mode: CaptureMode,
    pub video: bool,
    pub delay_secs: u32,
    pub show_magnifier: bool,
    pub system_audio: bool,
    pub microphone: bool,
}

/// Opens one overlay window per monitor. `done` runs exactly once, after every overlay window has closed.
pub fn open_overlay(
    app: &App,
    request: OverlayRequest,
    done: impl FnOnce(&App, OverlayOutcome, OverlayPrefs) + 'static,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let OverlayRequest { captures, windows, mode, video, show_magnifier, delay_secs, hdr, system_audio, microphone } = request;
    ensure!(!captures.is_empty(), "no monitor captures to show");
    let frozen: Vec<Frozen> =
        captures.into_iter().map(|c| Frozen { info: c.monitor, sdr: Rc::new(c.sdr), hdr: c.hdr }).collect();
    let prefs = OverlayPrefs { mode, video, delay_secs, show_magnifier, system_audio, microphone };
    let session = Session::new(frozen, windows, hdr, prefs, sys::cursor_pos());
    let toolbar_monitor = session.toolbar_monitor;
    let rects = session.monitor_rects.clone();
    let shared = Rc::new(RefCell::new(session));
    shared.borrow_mut().done = Some(Box::new(done));
    let theme = app.theme_for(ThemeMode::Dark);
    let last_shown_gets_focus = (0..rects.len()).filter(|&i| i != toolbar_monitor).chain([toolbar_monitor]);
    for i in last_shown_gets_focus {
        let view = OverlayView::new(shared.clone(), i, &theme);
        match app.open(WindowSpec::overlay(rects[i]), view) {
            Ok(id) => {
                let mut s = shared.borrow_mut();
                s.window_ids.push(id);
                s.open_windows += 1;
            }
            Err(e) => log::error!("overlay window for monitor {i}: {e:#}"),
        }
    }
    if shared.borrow().open_windows == 0 {
        shared.borrow_mut().done = None;
        bail!("no overlay window could be opened");
    }
    log::debug!("overlay: {} window(s) opened in {:.1} ms", rects.len(), started.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}

/// Countdown pill (excluded from capture) at the top center of `monitor`. `done(app, completed)`:
/// completed = false when the user cancelled it.
pub fn show_countdown(
    app: &App,
    monitor: &MonitorInfo,
    secs: u32,
    done: impl FnOnce(&App, bool) + 'static,
) -> anyhow::Result<()> {
    countdown::show(app, monitor, secs, done)
}

/// Builds a Snip for `rect_px` (virtual desktop) from frozen captures, stitching across monitors when needed.
/// Used by the overlay and by the app for Alt+Print Screen.
pub fn snip_rect(captures: &[MonitorCapture], rect_px: RectI, hdr: &HdrSettings, mode: CaptureMode) -> Option<Snip> {
    let sources: Vec<Source> =
        captures.iter().map(|c| Source { monitor: &c.monitor, sdr: &c.sdr, hdr: c.hdr.as_ref() }).collect();
    snip::snip_from(&sources, rect_px, hdr, mode)
}

/// Offscreen render of an overlay state for visual checks: `overlay`, `overlay-window`, `overlay-full`,
/// `overlay-freeform`, `overlay-video`, `overlay-color`, `overlay-menu`, `countdown` (plus `overlay-edge`;
/// all names in `PREVIEW_KINDS`).
/// Uses `captures` when given, else a synthetic desktop.
pub fn render_preview(
    gfx: &Gfx,
    kind: &str,
    theme: ThemeMode,
    scale: f32,
    captures: Option<&[MonitorCapture]>,
) -> anyhow::Result<Image> {
    preview::render(gfx, kind, theme, scale, captures)
}
