//! State shared by every overlay window of one session: the frozen desktop, the user's choices, the gesture in
//! progress and how the session ends.

use std::rc::Rc;

use glint_core::settings::HdrSettings;
use glint_core::{CaptureMode, HdrImage, Image, MonitorInfo, PointF, PointI, RectI, WindowInfo};
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
    Color([u8; 4]),
    Record(Armed),
    Delay(u32),
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

pub struct Session {
    pub frozen: Vec<Frozen>,
    pub monitor_rects: Vec<RectI>,
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
    /// The timer that force-closes windows that never finished their fade is set.
    pub close_fallback_armed: bool,
    pub window_ids: Vec<WindowId>,
    pub open_windows: usize,
    pub done: Option<DoneFn>,
}

impl Session {
    pub fn new(frozen: Vec<Frozen>, windows: Vec<WindowInfo>, hdr: HdrSettings, prefs: OverlayPrefs, cursor: Option<PointI>) -> Self {
        let monitor_rects: Vec<RectI> = frozen.iter().map(|f| f.info.rect).collect();
        let toolbar_monitor = cursor
            .and_then(|c| monitor_at(&monitor_rects, c))
            .or_else(|| frozen.iter().position(|f| f.info.primary))
            .unwrap_or(0);
        Self {
            desktop: virtual_bounds(monitor_rects.iter().copied()),
            monitor_rects,
            frozen,
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

    pub fn end(&mut self, finish: Finish) {
        if self.finish.is_none() {
            self.finish = Some(finish);
        }
    }

    pub fn is_closing(&self) -> bool {
        self.finish.is_some()
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

    /// The frozen pixel (BGRA) at a desktop position.
    pub fn pixel_at(&self, p: PointI) -> Option<[u8; 4]> {
        let i = self.monitor_under(p)?;
        let frozen = &self.frozen[i];
        let (x, y) = (p.x - frozen.info.rect.x, p.y - frozen.info.rect.y);
        (x >= 0 && y >= 0 && (x as u32) < frozen.sdr.width && (y as u32) < frozen.sdr.height)
            .then(|| frozen.sdr.pixel(x as u32, y as u32))
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
    pub fn resolve(self, frozen: &[Frozen], hdr: &HdrSettings, prefs: &OverlayPrefs, gfx: &Rc<Gfx>) -> OverlayOutcome {
        match self {
            Finish::Cancelled => OverlayOutcome::Cancelled,
            Finish::Color(bgra) => OverlayOutcome::Color { bgra },
            Finish::Delay(secs) => OverlayOutcome::Delay { secs },
            Finish::Record(armed) => {
                let monitor = frozen[armed.monitor].info.clone();
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

    fn frozen(rect: RectI, primary: bool) -> Frozen {
        let info = MonitorInfo {
            handle: 1,
            device_name: "D".into(),
            friendly_name: "D".into(),
            rect,
            work_rect: rect,
            dpi: 96,
            primary,
            hdr: None,
        };
        let mut sdr = Image::new(rect.w as u32, rect.h as u32);
        sdr.data.chunks_exact_mut(4).for_each(|px| px.copy_from_slice(&[1, 2, 3, 255]));
        Frozen { info, sdr: Rc::new(sdr), hdr: None }
    }

    fn session(prefs: OverlayPrefs, cursor: Option<PointI>) -> Session {
        let frozen = vec![frozen(RectI::new(0, 0, 100, 100), false), frozen(RectI::new(100, 0, 200, 150), true)];
        Session::new(frozen, Vec::new(), HdrSettings::default(), prefs, cursor)
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
        let outcome = Finish::Record(armed).resolve(&s.frozen, &s.hdr, &s.prefs, &gfx);
        let OverlayOutcome::Record(region) = outcome else { panic!("record outcome expected") };
        assert_eq!(region.region, RectI::new(0, 10, 80, 50));
        assert!(region.system_audio && !region.microphone);
    }

    #[test]
    fn text_regions_become_text_outcomes_and_colors_come_from_the_frozen_image() {
        let s = session(prefs(CaptureMode::Text, false), None);
        assert_eq!(s.pixel_at(PointI::new(150, 20)), Some([1, 2, 3, 255]));
        assert_eq!(s.pixel_at(PointI::new(50, 120)), None);
        let gfx = Gfx::new().unwrap();
        let finish = Finish::Region { rect: RectI::new(10, 10, 20, 20), mode: CaptureMode::Text, lasso: None };
        let OverlayOutcome::Text(snip) = finish.resolve(&s.frozen, &s.hdr, &s.prefs, &gfx) else { panic!("text outcome expected") };
        assert_eq!((snip.image.width, snip.image.height), (20, 20));
    }

    #[test]
    fn freeform_snips_drop_the_unmasked_hdr_crop() {
        let mut s = session(prefs(CaptureMode::Freeform, false), None);
        let (w, h) = (s.frozen[0].sdr.width, s.frozen[0].sdr.height);
        s.frozen[0].hdr = Some(HdrImage {
            width: w,
            height: h,
            data: vec![glint_core::f16::from_f32(1.0); (w * h * 4) as usize],
            sdr_white_nits: 80.0,
            display_peak_nits: 1000.0,
        });
        let gfx = Gfx::new().unwrap();
        let rect = RectI::new(10, 10, 40, 40);
        let plain = Finish::Region { rect, mode: CaptureMode::Rectangle, lasso: None };
        let OverlayOutcome::Snip(snip) = plain.resolve(&s.frozen, &s.hdr, &s.prefs, &gfx) else { panic!("snip expected") };
        assert!(snip.hdr.is_some() && snip.hdr_stats.is_some());
        let mut lasso = Lasso::begin(RectI::new(0, 0, 100, 100), PointF::new(30.0, 10.0));
        for p in [(50.0, 30.0), (30.0, 50.0), (10.0, 30.0)] {
            lasso.add(PointF::new(p.0, p.1));
        }
        let freeform = Finish::Region { rect: lasso.bbox(), mode: CaptureMode::Freeform, lasso: Some(lasso.points().to_vec()) };
        let OverlayOutcome::Snip(snip) = freeform.resolve(&s.frozen, &s.hdr, &s.prefs, &gfx) else { panic!("snip expected") };
        assert!(snip.hdr.is_none() && snip.hdr_stats.is_none(), "a re-tone-map would fill the cutout");
        assert_eq!(snip.image.pixel(0, 0)[3], 0);
    }

    #[test]
    fn freeform_outcome_is_masked_outside_the_lasso() {
        let s = session(prefs(CaptureMode::Freeform, false), None);
        let gfx = Gfx::new().unwrap();
        let mut lasso = Lasso::begin(RectI::new(0, 0, 100, 100), PointF::new(40.0, 20.0));
        for p in [(60.0, 40.0), (40.0, 60.0), (20.0, 40.0)] {
            lasso.add(PointF::new(p.0, p.1));
        }
        let finish = Finish::Region { rect: lasso.bbox(), mode: CaptureMode::Freeform, lasso: Some(lasso.points().to_vec()) };
        let OverlayOutcome::Snip(snip) = finish.resolve(&s.frozen, &s.hdr, &s.prefs, &gfx) else { panic!("snip expected") };
        assert_eq!(snip.rect_px, RectI::new(20, 20, 41, 41));
        assert_eq!(snip.image.pixel(20, 20)[3], 255, "center is kept");
        assert_eq!(snip.image.pixel(0, 0)[3], 0, "rounded corner is cut");
    }
}
