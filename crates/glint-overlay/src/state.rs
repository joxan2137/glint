//! State shared by every overlay window of one session: the monitors, the frozen desktop once it arrives, the
//! user's choices, the gesture in progress and how the session ends.

use std::rc::Rc;
use std::time::Instant;

use glint_core::settings::HdrSettings;
use glint_core::{CaptureMode, HdrImage, Image, MonitorCapture, MonitorInfo, PointF, PointI, RectI, WindowInfo};
use glint_ui::{App, Gfx, WindowId};

use crate::freeform::{Lasso, apply_mask, lasso_mask};
use crate::geometry::{Selection, hovered_window, largest_overlap, monitor_at, virtual_bounds};
use crate::snip::{Source, snip_from};
use crate::{OverlayOutcome, OverlayPrefs, RecordRegion};

pub type DoneFn = Box<dyn FnOnce(&App, OverlayOutcome, OverlayPrefs)>;

pub const DELAYS: [u32; 4] = [0, 3, 5, 10];

/// One monitor's frozen capture as the overlay keeps it.
pub struct Frozen {
    pub info: MonitorInfo,
    pub sdr: Rc<Image>,
    pub hdr: Option<HdrImage>,
}

impl From<MonitorCapture> for Frozen {
    fn from(capture: MonitorCapture) -> Self {
        Self { info: capture.monitor, sdr: Rc::new(capture.sdr), hdr: capture.hdr }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Gesture {
    Idle,
    /// Primary button down in a click-to-capture mode (window, full screen, color).
    Click,
    Select { selection: Selection, monitor: usize },
    Lasso { lasso: Lasso, monitor: usize },
}

/// A video region chosen and waiting for Record (virtual-desktop px, inside one monitor).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Armed {
    pub monitor: usize,
    pub rect: RectI,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Finish {
    Cancelled,
    Region { rect: RectI, mode: CaptureMode, lasso: Option<Vec<PointF>> },
    /// Color mode: the pixel at this desktop position.
    ColorAt(PointI),
    Record(Armed),
    Delay(u32),
}

impl Finish {
    /// Endings that are built from the frozen pixels (they wait for the captures).
    pub fn needs_pixels(&self) -> bool {
        matches!(self, Finish::Region { .. } | Finish::ColorAt(_))
    }
}

pub fn supports_video(mode: CaptureMode) -> bool {
    matches!(mode, CaptureMode::Rectangle | CaptureMode::Window | CaptureMode::FullScreen)
}

/// Mode and video flag made consistent: video only with rectangle, window or full screen.
pub fn normalized(prefs: OverlayPrefs) -> OverlayPrefs {
    let mut prefs = prefs;
    if prefs.video && !supports_video(prefs.mode) {
        prefs.mode = CaptureMode::Rectangle;
    }
    prefs
}

/// BGRA of the frozen pixel at a desktop position.
pub fn frozen_pixel<'a>(frozen: impl IntoIterator<Item = &'a Frozen>, p: PointI) -> Option<[u8; 4]> {
    let f = frozen.into_iter().find(|f| f.info.rect.contains(p))?;
    let (x, y) = ((p.x - f.info.rect.x) as u32, (p.y - f.info.rect.y) as u32);
    (x < f.sdr.width && y < f.sdr.height).then(|| f.sdr.pixel(x, y))
}

pub struct Session {
    pub monitors: Vec<MonitorInfo>,
    pub monitor_rects: Vec<RectI>,
    /// Frozen pixels per monitor: `None` until the captures arrive (or when a monitor could not be captured).
    pub frozen: Vec<Option<Frozen>>,
    /// The captures were delivered; whatever could be captured is in `frozen`.
    pub captured: bool,
    /// When the user asked for the overlay (latency logs).
    pub requested_at: Option<Instant>,
    pub windows: Vec<WindowInfo>,
    pub desktop: RectI,
    pub hdr: HdrSettings,
    pub prefs: OverlayPrefs,
    pub cursor: Option<PointI>,
    pub toolbar_monitor: usize,
    pub gesture: Gesture,
    pub armed: Option<Armed>,
    pub ctrl: bool,
    pub space: bool,
    pub finish: Option<Finish>,
    /// The timer that force-ends windows that never finished their fade is set.
    pub close_fallback_armed: bool,
    pub window_ids: Vec<WindowId>,
    pub open_windows: usize,
    pub done: Option<DoneFn>,
}

impl Session {
    pub fn new(monitors: Vec<MonitorInfo>, windows: Vec<WindowInfo>, hdr: HdrSettings, prefs: OverlayPrefs, cursor: Option<PointI>) -> Self {
        let monitor_rects: Vec<RectI> = monitors.iter().map(|m| m.rect).collect();
        let toolbar_monitor = cursor
            .and_then(|c| monitor_at(&monitor_rects, c))
            .or_else(|| monitors.iter().position(|m| m.primary))
            .unwrap_or(0);
        Self {
            desktop: virtual_bounds(monitor_rects.iter().copied()),
            frozen: monitors.iter().map(|_| None).collect(),
            monitor_rects,
            monitors,
            captured: false,
            requested_at: None,
            windows,
            hdr,
            prefs: normalized(prefs),
            cursor,
            toolbar_monitor,
            gesture: Gesture::Idle,
            armed: None,
            ctrl: false,
            space: false,
            finish: None,
            close_fallback_armed: false,
            window_ids: Vec::new(),
            open_windows: 0,
            done: None,
        }
    }

    /// The overlay monitor a capture belongs to: same rect, else same device.
    fn slot_for(&self, monitor: &MonitorInfo) -> Option<usize> {
        let slot = self
            .monitors
            .iter()
            .position(|m| m.rect == monitor.rect)
            .or_else(|| self.monitors.iter().position(|m| m.device_name == monitor.device_name));
        if slot.is_none() {
            log::warn!("overlay: capture of {} matches no overlay monitor", monitor.device_name);
        }
        slot
    }

    /// One monitor's final image ahead of the complete captures: shown, sampled by the magnifier; endings that need
    /// pixels still wait for `attach`, which brings the raw HDR data.
    pub fn attach_early(&mut self, monitor: &MonitorInfo, sdr: Image) {
        if let Some(i) = self.slot_for(monitor).filter(|&i| self.frozen[i].is_none()) {
            self.frozen[i] = Some(Frozen { info: monitor.clone(), sdr: Rc::new(sdr), hdr: None });
        }
    }

    /// Takes the frozen desktop (the image already on screen is kept: same pixels).
    pub fn attach(&mut self, captures: Vec<MonitorCapture>) {
        for capture in captures {
            let Some(i) = self.slot_for(&capture.monitor) else { continue };
            let mut frozen = Frozen::from(capture);
            if let Some(early) = &self.frozen[i] {
                frozen.sdr = early.sdr.clone();
            }
            self.frozen[i] = Some(frozen);
        }
        self.captured = true;
    }

    pub fn pixels(&self, monitor: usize) -> Option<&Frozen> {
        self.frozen.get(monitor).and_then(Option::as_ref)
    }

    pub fn set_mode(&mut self, mode: CaptureMode) {
        self.prefs.mode = mode;
        if !supports_video(mode) {
            self.prefs.video = false;
        }
        self.reset_gesture();
    }

    pub fn set_video(&mut self, video: bool) {
        self.prefs.video = video;
        self.prefs = normalized(self.prefs.clone());
        self.reset_gesture();
    }

    pub fn toggle_rectangle_window(&mut self) {
        self.set_mode(if self.prefs.mode == CaptureMode::Window { CaptureMode::Rectangle } else { CaptureMode::Window });
    }

    fn reset_gesture(&mut self) {
        self.gesture = Gesture::Idle;
        self.armed = None;
    }

    /// The first ending wins, except that a cancel replaces an ending still waiting for the captures.
    pub fn end(&mut self, finish: Finish) {
        let replaces = finish == Finish::Cancelled && self.is_waiting();
        if self.finish.is_none() || replaces {
            self.finish = Some(finish);
        }
    }

    /// The session ended and the windows fade out.
    pub fn is_closing(&self) -> bool {
        self.finish.as_ref().is_some_and(|f| self.captured || !f.needs_pixels())
    }

    /// The user finished, but the outcome needs pixels that have not arrived yet.
    pub fn is_waiting(&self) -> bool {
        self.finish.is_some() && !self.is_closing()
    }

    pub fn monitor_under(&self, p: PointI) -> Option<usize> {
        monitor_at(&self.monitor_rects, p)
    }

    /// True while a drag is big enough to count (the toolbar steps aside).
    pub fn is_dragging(&self) -> bool {
        match &self.gesture {
            Gesture::Select { selection, .. } => selection.is_meaningful(),
            Gesture::Lasso { lasso, .. } => lasso.points().len() > 1,
            Gesture::Idle | Gesture::Click => false,
        }
    }

    /// What a click at `p` captures in window and full-screen modes.
    pub fn click_target(&self, p: PointI) -> Option<RectI> {
        let monitor = self.monitor_under(p).map(|i| self.monitor_rects[i]);
        match self.prefs.mode {
            CaptureMode::Window => hovered_window(&self.windows, self.desktop, p).or(monitor),
            CaptureMode::FullScreen if self.ctrl && !self.prefs.video => Some(self.desktop),
            _ => monitor,
        }
    }

    /// A recordable region: `rect` clipped to the monitor that holds most of it.
    pub fn arm(&mut self, rect: RectI) {
        if let Some(monitor) = largest_overlap(&self.monitor_rects, rect)
            && let Some(clipped) = rect.intersect(&self.monitor_rects[monitor])
        {
            self.armed = Some(Armed { monitor, rect: clipped });
        }
    }

    /// Ends the session with a capture of `rect` (or arms it for recording in video mode).
    pub fn complete_region(&mut self, rect: RectI, mode: CaptureMode) {
        if self.prefs.video {
            self.arm(rect);
        } else {
            self.end(Finish::Region { rect, mode, lasso: None });
        }
    }

    pub fn choose_delay(&mut self, secs: u32) {
        self.prefs.delay_secs = secs;
        if secs > 0 {
            self.end(Finish::Delay(secs));
        }
    }
}

impl Finish {
    /// Builds the outcome once the windows are gone (crops, region tone mapping and the lasso mask happen here).
    pub fn resolve(
        self,
        monitors: &[MonitorInfo],
        frozen: &[Frozen],
        hdr: &HdrSettings,
        prefs: &OverlayPrefs,
        gfx: &Rc<Gfx>,
    ) -> OverlayOutcome {
        match self {
            Finish::Cancelled => OverlayOutcome::Cancelled,
            Finish::ColorAt(p) => match frozen_pixel(frozen, p) {
                Some(bgra) => OverlayOutcome::Color { bgra },
                None => OverlayOutcome::Cancelled,
            },
            Finish::Delay(secs) => OverlayOutcome::Delay { secs },
            Finish::Record(armed) => {
                let monitor = monitors[armed.monitor].clone();
                let region = armed.rect.offset(-monitor.rect.x, -monitor.rect.y);
                OverlayOutcome::Record(RecordRegion { monitor, region, system_audio: prefs.system_audio, microphone: prefs.microphone })
            }
            Finish::Region { rect, mode, lasso } => {
                let sources: Vec<Source> =
                    frozen.iter().map(|f| Source { monitor: &f.info, sdr: &f.sdr, hdr: f.hdr.as_ref() }).collect();
                let Some(mut snip) = snip_from(&sources, rect, hdr, mode) else { return OverlayOutcome::Cancelled };
                if let Some(points) = lasso {
                    apply_mask(&mut snip.image, &lasso_mask(gfx, &points, snip.rect_px));
                    snip.hdr = None;
                    snip.hdr_stats = None;
                }
                if mode == CaptureMode::Text { OverlayOutcome::Text(snip) } else { OverlayOutcome::Snip(snip) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(mode: CaptureMode, video: bool) -> OverlayPrefs {
        OverlayPrefs { mode, video, delay_secs: 0, show_magnifier: true, system_audio: true, microphone: false }
    }

    fn monitor(rect: RectI, primary: bool) -> MonitorInfo {
        MonitorInfo {
            handle: rect.x as isize,
            device_name: format!("D{}", rect.x),
            friendly_name: "D".into(),
            rect,
            work_rect: rect,
            dpi: 96,
            primary,
            hdr: None,
        }
    }

    fn capture(monitor: &MonitorInfo) -> MonitorCapture {
        let mut sdr = Image::new(monitor.rect.w as u32, monitor.rect.h as u32);
        sdr.data.chunks_exact_mut(4).for_each(|px| px.copy_from_slice(&[1, 2, 3, 255]));
        MonitorCapture { monitor: monitor.clone(), sdr, hdr: None, hdr_stats: None }
    }

    fn pending_session(prefs: OverlayPrefs, cursor: Option<PointI>) -> Session {
        let monitors = vec![monitor(RectI::new(0, 0, 100, 100), false), monitor(RectI::new(100, 0, 200, 150), true)];
        Session::new(monitors, Vec::new(), HdrSettings::default(), prefs, cursor)
    }

    fn session(prefs: OverlayPrefs, cursor: Option<PointI>) -> Session {
        let mut s = pending_session(prefs, cursor);
        let captures = s.monitors.iter().map(capture).collect();
        s.attach(captures);
        s
    }

    fn pixel_at(s: &Session, p: PointI) -> Option<[u8; 4]> {
        frozen_pixel(s.frozen.iter().flatten(), p)
    }

    fn take_frozen(s: &mut Session) -> Vec<Frozen> {
        std::mem::take(&mut s.frozen).into_iter().flatten().collect()
    }

    #[test]
    fn video_is_only_kept_with_rectangle_window_or_full_screen() {
        assert_eq!(normalized(prefs(CaptureMode::Text, true)), prefs(CaptureMode::Rectangle, true));
        assert_eq!(normalized(prefs(CaptureMode::Window, true)), prefs(CaptureMode::Window, true));
        let mut s = session(prefs(CaptureMode::Window, true), None);
        s.set_mode(CaptureMode::ColorPicker);
        assert_eq!((s.prefs.mode, s.prefs.video), (CaptureMode::ColorPicker, false));
        s.set_video(true);
        assert_eq!((s.prefs.mode, s.prefs.video), (CaptureMode::Rectangle, true));
        s.toggle_rectangle_window();
        assert_eq!(s.prefs.mode, CaptureMode::Window);
        s.toggle_rectangle_window();
        assert_eq!(s.prefs.mode, CaptureMode::Rectangle);
    }

    #[test]
    fn toolbar_starts_under_the_cursor_else_on_the_primary_monitor() {
        assert_eq!(session(prefs(CaptureMode::Rectangle, false), Some(PointI::new(10, 10))).toolbar_monitor, 0);
        assert_eq!(session(prefs(CaptureMode::Rectangle, false), None).toolbar_monitor, 1);
    }

    #[test]
    fn delay_choice_ends_only_for_non_zero_delays() {
        let mut s = session(prefs(CaptureMode::Rectangle, false), None);
        s.choose_delay(0);
        assert!(!s.is_closing());
        s.choose_delay(5);
        assert_eq!(s.finish, Some(Finish::Delay(5)));
        assert_eq!(s.prefs.delay_secs, 5);
        s.end(Finish::Cancelled);
        assert_eq!(s.finish, Some(Finish::Delay(5)), "the first ending wins");
    }

    #[test]
    fn captures_arrive_later_and_map_to_their_monitors() {
        let mut s = pending_session(prefs(CaptureMode::Rectangle, false), None);
        assert!(s.pixels(0).is_none() && pixel_at(&s, PointI::new(150, 20)).is_none());
        let late = vec![capture(&s.monitors[1])];
        s.attach(late);
        assert!(s.captured);
        assert!(s.pixels(0).is_none(), "a monitor that was not captured stays live");
        assert_eq!(pixel_at(&s, PointI::new(150, 20)), Some([1, 2, 3, 255]));
    }

    #[test]
    fn early_images_show_before_the_captures_complete() {
        let mut s = pending_session(prefs(CaptureMode::Rectangle, false), None);
        let early = capture(&s.monitors[1]);
        s.attach_early(&early.monitor, early.sdr);
        assert!(s.pixels(1).is_some() && !s.captured);
        let shown = s.pixels(1).unwrap().sdr.clone();
        s.end(Finish::Region { rect: RectI::new(110, 10, 20, 20), mode: CaptureMode::Rectangle, lasso: None });
        assert!(s.is_waiting(), "region endings still wait for the raw captures");
        let captures = s.monitors.iter().map(capture).collect();
        s.attach(captures);
        assert!(Rc::ptr_eq(&s.pixels(1).unwrap().sdr, &shown), "the image on screen is kept");
        assert!(s.is_closing());
    }

    #[test]
    fn endings_that_need_pixels_wait_and_can_still_be_cancelled() {
        let mut s = pending_session(prefs(CaptureMode::Rectangle, false), None);
        s.end(Finish::Region { rect: RectI::new(10, 10, 20, 20), mode: CaptureMode::Rectangle, lasso: None });
        assert!(s.is_waiting() && !s.is_closing());
        s.end(Finish::Cancelled);
        assert_eq!(s.finish, Some(Finish::Cancelled), "cancel replaces a waiting ending");
        assert!(s.is_closing());

        let mut s = pending_session(prefs(CaptureMode::ColorPicker, false), None);
        s.end(Finish::ColorAt(PointI::new(5, 5)));
        assert!(s.is_waiting());
        let captures = s.monitors.iter().map(capture).collect();
        s.attach(captures);
        assert!(s.is_closing() && !s.is_waiting());

        let mut s = pending_session(prefs(CaptureMode::Rectangle, true), None);
        s.complete_region(RectI::new(10, 10, 50, 50), CaptureMode::Rectangle);
        let armed = s.armed.unwrap();
        s.end(Finish::Record(armed));
        assert!(s.is_closing(), "recording needs no frozen pixels");
    }

    #[test]
    fn click_targets_follow_the_mode() {
        let mut s = session(prefs(CaptureMode::FullScreen, false), None);
        assert_eq!(s.click_target(PointI::new(150, 10)), Some(RectI::new(100, 0, 200, 150)));
        s.ctrl = true;
        assert_eq!(s.click_target(PointI::new(150, 10)), Some(RectI::new(0, 0, 300, 150)));
        s.set_mode(CaptureMode::Window);
        s.windows = vec![WindowInfo {
            hwnd: 1,
            title: "w".into(),
            class_name: "c".into(),
            process_id: 1,
            rect: RectI::new(50, 20, 400, 50),
            z_order: 0,
        }];
        assert_eq!(s.click_target(PointI::new(60, 30)), Some(RectI::new(50, 20, 250, 50)));
        assert_eq!(s.click_target(PointI::new(10, 90)), Some(RectI::new(0, 0, 100, 100)), "bare desktop = its monitor");
    }

    #[test]
    fn video_regions_are_armed_inside_one_monitor_and_recorded_relative_to_it() {
        let mut s = session(prefs(CaptureMode::Rectangle, true), None);
        s.complete_region(RectI::new(80, 10, 100, 50), CaptureMode::Rectangle);
        let armed = s.armed.unwrap();
        assert_eq!(armed, Armed { monitor: 1, rect: RectI::new(100, 10, 80, 50) });
        assert!(!s.is_closing());
        let gfx = Gfx::new().unwrap();
        let frozen = take_frozen(&mut s);
        let outcome = Finish::Record(armed).resolve(&s.monitors, &frozen, &s.hdr, &s.prefs, &gfx);
        let OverlayOutcome::Record(region) = outcome else { panic!("record outcome expected") };
        assert_eq!(region.region, RectI::new(0, 10, 80, 50));
        assert!(region.system_audio && !region.microphone);
    }

    #[test]
    fn text_regions_become_text_outcomes_and_colors_come_from_the_frozen_image() {
        let mut s = session(prefs(CaptureMode::Text, false), None);
        assert_eq!(pixel_at(&s, PointI::new(150, 20)), Some([1, 2, 3, 255]));
        assert_eq!(pixel_at(&s, PointI::new(50, 120)), None);
        let gfx = Gfx::new().unwrap();
        let frozen = take_frozen(&mut s);
        let finish = Finish::Region { rect: RectI::new(10, 10, 20, 20), mode: CaptureMode::Text, lasso: None };
        let OverlayOutcome::Text(snip) = finish.resolve(&s.monitors, &frozen, &s.hdr, &s.prefs, &gfx) else {
            panic!("text outcome expected")
        };
        assert_eq!((snip.image.width, snip.image.height), (20, 20));
        let color = Finish::ColorAt(PointI::new(150, 20)).resolve(&s.monitors, &frozen, &s.hdr, &s.prefs, &gfx);
        assert!(matches!(color, OverlayOutcome::Color { bgra: [1, 2, 3, 255] }));
    }

    #[test]
    fn freeform_snips_drop_the_unmasked_hdr_crop() {
        let mut s = session(prefs(CaptureMode::Freeform, false), None);
        let mut frozen = take_frozen(&mut s);
        let (w, h) = (frozen[0].sdr.width, frozen[0].sdr.height);
        frozen[0].hdr = Some(HdrImage {
            width: w,
            height: h,
            data: vec![glint_core::f16::from_f32(1.0); (w * h * 4) as usize],
            sdr_white_nits: 80.0,
            display_peak_nits: 1000.0,
        });
        let gfx = Gfx::new().unwrap();
        let rect = RectI::new(10, 10, 40, 40);
        let plain = Finish::Region { rect, mode: CaptureMode::Rectangle, lasso: None };
        let OverlayOutcome::Snip(snip) = plain.resolve(&s.monitors, &frozen, &s.hdr, &s.prefs, &gfx) else { panic!("snip expected") };
        assert!(snip.hdr.is_some() && snip.hdr_stats.is_some());
        let mut lasso = Lasso::begin(RectI::new(0, 0, 100, 100), PointF::new(30.0, 10.0));
        for p in [(50.0, 30.0), (30.0, 50.0), (10.0, 30.0)] {
            lasso.add(PointF::new(p.0, p.1));
        }
        let freeform = Finish::Region { rect: lasso.bbox(), mode: CaptureMode::Freeform, lasso: Some(lasso.points().to_vec()) };
        let OverlayOutcome::Snip(snip) = freeform.resolve(&s.monitors, &frozen, &s.hdr, &s.prefs, &gfx) else {
            panic!("snip expected")
        };
        assert!(snip.hdr.is_none() && snip.hdr_stats.is_none(), "a re-tone-map would fill the cutout");
        assert_eq!(snip.image.pixel(0, 0)[3], 0);
    }

    #[test]
    fn freeform_outcome_is_masked_outside_the_lasso() {
        let mut s = session(prefs(CaptureMode::Freeform, false), None);
        let gfx = Gfx::new().unwrap();
        let mut lasso = Lasso::begin(RectI::new(0, 0, 100, 100), PointF::new(40.0, 20.0));
        for p in [(60.0, 40.0), (40.0, 60.0), (20.0, 40.0)] {
            lasso.add(PointF::new(p.0, p.1));
        }
        let frozen = take_frozen(&mut s);
        let finish = Finish::Region { rect: lasso.bbox(), mode: CaptureMode::Freeform, lasso: Some(lasso.points().to_vec()) };
        let OverlayOutcome::Snip(snip) = finish.resolve(&s.monitors, &frozen, &s.hdr, &s.prefs, &gfx) else { panic!("snip expected") };
        assert_eq!(snip.rect_px, RectI::new(20, 20, 41, 41));
        assert_eq!(snip.image.pixel(20, 20)[3], 255, "center is kept");
        assert_eq!(snip.image.pixel(0, 0)[3], 0, "rounded corner is cut");
    }
}
