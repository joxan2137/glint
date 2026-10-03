//! Deterministic offscreen renders of overlay states (no window): a synthetic desktop unless real captures are given.

use std::cell::RefCell;
use std::f32::consts::TAU;
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use glint_core::settings::HdrSettings;
use glint_core::{CaptureMode, Image, MonitorCapture, MonitorInfo, PointF, RectF, RectI, SizeF, ThemeMode, WindowInfo};
use glint_ui::{
    Bitmap, Brush, Color, Gfx, Icon, Interpolation, OffscreenSpec, Painter, PathBuilder, Shadow, StrokeStyle, TextStyle, Theme,
    Weight, render_offscreen, render_view_offscreen,
};

use crate::OverlayPrefs;
use crate::countdown::{self, CountdownView};
use crate::freeform::Lasso;
use crate::geometry::{Selection, Space};
use crate::state::{Armed, Frozen, Gesture, Session};
use crate::view::OverlayView;

pub const KINDS: [&str; 9] = [
    "overlay",
    "overlay-edge",
    "overlay-window",
    "overlay-full",
    "overlay-freeform",
    "overlay-video",
    "overlay-color",
    "overlay-menu",
    "countdown",
];

const DESKTOP: SizeF = SizeF::new(1440.0, 900.0);
const WARM_TIME: f64 = 1000.0;
const SETTLED_TIME: f64 = WARM_TIME + 5.0;

struct Desktop {
    frozen: Frozen,
    windows: Vec<WindowInfo>,
    size: SizeF,
}

pub fn render(gfx: &Gfx, kind: &str, theme: ThemeMode, scale: f32, captures: Option<&[MonitorCapture]>) -> Result<Image> {
    let gfx = gfx.shared().context("render_preview needs a Gfx created by Gfx::new")?;
    if !KINDS.contains(&kind) {
        bail!("unknown overlay preview `{kind}`; expected one of {}", KINDS.join(", "));
    }
    let desktop = match captures {
        Some(captures) => live_desktop(captures, scale)?,
        None => synthetic_desktop(&gfx, scale)?,
    };
    if kind == "countdown" {
        return render_countdown(&gfx, &desktop, theme, scale);
    }
    render_overlay(&gfx, desktop, kind, scale)
}

fn at(size: SizeF, fx: f32, fy: f32) -> PointF {
    PointF::new(size.w * fx, size.h * fy)
}

fn render_overlay(gfx: &Rc<Gfx>, desktop: Desktop, kind: &str, scale: f32) -> Result<Image> {
    let size = desktop.size;
    let monitor = desktop.frozen.info.rect;
    let space = Space::new(monitor, scale);
    let px = |p: PointF| space.to_px(p);
    let mode = match kind {
        "overlay-window" => CaptureMode::Window,
        "overlay-full" => CaptureMode::FullScreen,
        "overlay-freeform" => CaptureMode::Freeform,
        "overlay-color" => CaptureMode::ColorPicker,
        _ => CaptureMode::Rectangle,
    };
    let prefs = OverlayPrefs {
        mode,
        video: kind == "overlay-video",
        delay_secs: if kind == "overlay-menu" { 3 } else { 0 },
        show_magnifier: true,
        system_audio: true,
        microphone: false,
    };
    let cursor = match kind {
        "overlay" => at(size, 0.68, 0.6),
        "overlay-edge" => PointF::new(size.w - 0.1, size.h - 0.1),
        "overlay-window" => at(size, 0.56, 0.5),
        "overlay-full" => at(size, 0.5, 0.62),
        "overlay-color" => at(size, 0.22, 0.36),
        "overlay-menu" => at(size, 0.52, 0.2),
        _ => at(size, 0.5, 0.5),
    };
    let mut session = Session::new(vec![desktop.frozen], desktop.windows, HdrSettings::default(), prefs, Some(px(cursor)));
    match kind {
        "overlay" | "overlay-edge" => {
            let anchor = if kind == "overlay" { at(size, 0.39, 0.33) } else { at(size, 0.74, 0.71) };
            let mut selection = Selection::begin(monitor, px(anchor));
            selection.drag_to(px(cursor), false, false);
            session.gesture = Gesture::Select { selection, monitor: 0 };
        }
        "overlay-freeform" => {
            let center = at(size, 0.3, 0.37);
            let mut points = (0..=88).map(|i| {
                let t = i as f32 / 100.0 * TAU;
                let r = size.h * (0.2 + 0.025 * (3.0 * t).sin() + 0.015 * (5.0 * t + 1.0).cos());
                PointF::new(center.x + r * 1.25 * t.cos(), center.y + r * t.sin())
            });
            let first = points.next().unwrap_or(center);
            let mut lasso = Lasso::begin(monitor, space.to_px_f(first));
            let mut last = first;
            for p in points {
                lasso.add(space.to_px_f(p));
                last = p;
            }
            session.cursor = Some(px(last));
            session.gesture = Gesture::Lasso { lasso, monitor: 0 };
        }
        "overlay-video" => {
            let rect = RectF::from_points(at(size, 0.22, 0.24), at(size, 0.7, 0.66));
            let rect_px = RectI::from_points(px(PointF::new(rect.x, rect.y)), px(PointF::new(rect.right(), rect.bottom())));
            session.armed = Some(Armed { monitor: 0, rect: rect_px });
            session.cursor = Some(px(at(size, 0.3, 0.5)));
        }
        _ => {}
    }

    let shared = Rc::new(RefCell::new(session));
    let mut view = OverlayView::new(shared, 0, &Theme::dark());
    let spec = OffscreenSpec::pixels(monitor.w as u32, monitor.h as u32, scale, Theme::dark());
    render_view_offscreen(gfx, &mut view, &spec.clone().time(WARM_TIME))?;
    match kind {
        "overlay-window" => {
            if let Some(modes) = view.toolbar.segmented_mut("mode") {
                view.preview.tooltip = modes.item_rect(1).map(|r| (r, "Window".to_string(), Some("W".to_string())));
            }
        }
        "overlay-menu" => {
            let anchor = view.toolbar.item("delay").map(|item| item.rect()).unwrap_or_default();
            if let Some(button) = view.toolbar.button_mut("delay") {
                button.force_state(true, false);
            }
            view.menu.force_open(gfx, anchor, size, Some(2));
        }
        "overlay-video" => view.record_bar.force_hover_record(),
        _ => {}
    }
    render_view_offscreen(gfx, &mut view, &spec.time(SETTLED_TIME))
}

fn render_countdown(gfx: &Rc<Gfx>, desktop: &Desktop, theme: ThemeMode, scale: f32) -> Result<Image> {
    let theme = Theme::resolve(theme, glint_ui::theme::system_prefers_dark());
    let size = countdown::window_size();
    let background = Bitmap::from_shared(desktop.frozen.sdr.clone());
    let behind = RectF::new(-(desktop.size.w - size.w) / 2.0, -countdown::WINDOW_TOP, desktop.size.w, desktop.size.h);
    let mut view = CountdownView::new(3, None);
    let mut frame = |time: f64| {
        let spec = OffscreenSpec::new(size, scale, theme.clone()).time(time);
        render_offscreen(gfx, &spec, |cx, p| {
            p.bitmap(&background, behind, None, 1.0, Interpolation::Linear);
            glint_ui::View::paint(&mut view, cx, p);
        })
    };
    frame(WARM_TIME)?;
    frame(SETTLED_TIME)
}

fn live_desktop(captures: &[MonitorCapture], scale: f32) -> Result<Desktop> {
    let capture = captures.iter().find(|c| c.monitor.primary).or(captures.first()).context("no captures")?;
    let windows = glint_capture::windows_snapshot(Some(std::process::id()));
    let rect = capture.monitor.rect;
    Ok(Desktop {
        frozen: Frozen { info: capture.monitor.clone(), sdr: Rc::new(capture.sdr.clone()), hdr: capture.hdr.clone() },
        windows,
        size: SizeF::new(rect.w as f32 / scale, rect.h as f32 / scale),
    })
}

struct FakeWindow {
    rect: RectF,
    title: &'static str,
}

const PHOTOS: FakeWindow = FakeWindow { rect: RectF::new(96.0, 120.0, 700.0, 470.0), title: "Photos — Lake Tahoe.heic" };
const TERMINAL: FakeWindow = FakeWindow { rect: RectF::new(1000.0, 88.0, 380.0, 220.0), title: "Terminal" };
const DOCUMENT: FakeWindow = FakeWindow { rect: RectF::new(620.0, 236.0, 600.0, 430.0), title: "Release notes.txt — Notepad" };
const TASKBAR: RectF = RectF::new(0.0, 852.0, 1440.0, 48.0);

fn synthetic_desktop(gfx: &Rc<Gfx>, scale: f32) -> Result<Desktop> {
    let spec = OffscreenSpec::new(DESKTOP, scale, Theme::dark()).background(Color::BLACK);
    let mut image = render_offscreen(gfx, &spec, |_, p| paint_desktop(p))?;
    image.data.chunks_exact_mut(4).for_each(|px| px[3] = 255);
    let rect = RectI::new(0, 0, image.width as i32, image.height as i32);
    let to_px = |r: RectF| r.scale(scale).round_out();
    let windows = [(TASKBAR, "Taskbar"), (DOCUMENT.rect, DOCUMENT.title), (TERMINAL.rect, TERMINAL.title), (PHOTOS.rect, PHOTOS.title)]
        .iter()
        .enumerate()
        .map(|(z, (r, title))| WindowInfo {
            hwnd: z as isize + 1,
            title: title.to_string(),
            class_name: "Preview".into(),
            process_id: 0,
            rect: to_px(*r),
            z_order: z as u32,
        })
        .collect();
    let info = MonitorInfo {
        handle: 1,
        device_name: "\\\\.\\DISPLAY1".into(),
        friendly_name: "Studio Display".into(),
        rect,
        work_rect: RectI::new(0, 0, rect.w, (TASKBAR.y * scale) as i32),
        dpi: (96.0 * scale).round() as u32,
        primary: true,
        hdr: None,
    };
    Ok(Desktop { frozen: Frozen { info, sdr: Rc::new(image), hdr: None }, windows, size: DESKTOP })
}

fn hex(text: &str) -> Color {
    Color::hex(text).unwrap_or(Color::BLACK)
}

fn paint_desktop(p: &mut Painter) {
    let full = RectF::new(0.0, 0.0, DESKTOP.w, DESKTOP.h);
    p.fill_rect(
        full,
        Brush::linear(
            PointF::new(0.0, 0.0),
            PointF::new(DESKTOP.w * 0.7, DESKTOP.h),
            &[(0.0, hex("#0B1D3A")), (0.45, hex("#2C3E8F")), (0.8, hex("#8E4BA8")), (1.0, hex("#F08A5D"))],
        ),
    );
    for (color, x, y, r) in [("#5EC8F2", 0.12, 0.86, 260.0), ("#F5B971", 0.9, 0.12, 200.0), ("#6A5ACD", 0.55, 0.05, 180.0)] {
        p.fill_circle(PointF::new(DESKTOP.w * x, DESKTOP.h * y), r, hex(color).with_alpha(0.35));
    }
    paint_window(p, &PHOTOS, true, paint_photo);
    paint_window(p, &TERMINAL, true, paint_terminal);
    paint_window(p, &DOCUMENT, false, paint_document);
    paint_taskbar(p);
}

fn paint_window(p: &mut Painter, window: &FakeWindow, dark: bool, content: fn(&mut Painter, RectF)) {
    let r = window.rect;
    let (chrome, title) = if dark { (hex("#202124"), Color::rgba(1.0, 1.0, 1.0, 0.85)) } else { (hex("#F3F3F3"), Color::rgba(0.0, 0.0, 0.0, 0.85)) };
    p.shadow(r, 8.0, &Shadow::new(16.0, 48.0, Color::rgba(0.0, 0.0, 0.0, 0.45)));
    p.clip_round_rect(r, 8.0, |p| {
        p.fill_rect(r, chrome);
        p.text(window.title, &TextStyle::caption(), title, RectF::new(r.x + 14.0, r.y, r.w - 160.0, 32.0));
        for (i, icon) in [Icon::Minus, Icon::Square, Icon::X].iter().enumerate() {
            p.icon_with_stroke(*icon, PointF::new(r.right() - 115.0 + i as f32 * 46.0, r.y + 16.0), 10.0, title, 1.4);
        }
        content(p, RectF::new(r.x, r.y + 32.0, r.w, r.h - 32.0));
    });
    p.hairline_round_rect(r, 8.0, Color::rgba(1.0, 1.0, 1.0, if dark { 0.10 } else { 0.0 }), true);
    p.hairline_round_rect(r, 8.0, Color::rgba(0.0, 0.0, 0.0, 0.35), false);
}

fn paint_photo(p: &mut Painter, r: RectF) {
    p.fill_rect(r, Brush::vertical(r, hex("#3A7BD5"), hex("#F9D29D")));
    p.fill_circle(PointF::new(r.x + r.w * 0.72, r.y + r.h * 0.34), 46.0, hex("#FFE29A"));
    p.fill_circle(PointF::new(r.x + r.w * 0.72, r.y + r.h * 0.34), 70.0, hex("#FFE29A").with_alpha(0.25));
    let ridge = |p: &mut Painter, points: &[(f32, f32)], color: Color| {
        let mut path = PathBuilder::new();
        path.move_to(PointF::new(r.x, r.bottom()));
        for (fx, fy) in points {
            path.line_to(PointF::new(r.x + r.w * fx, r.y + r.h * fy));
        }
        path.line_to(PointF::new(r.right(), r.bottom()));
        path.close();
        if let Ok(path) = path.build(p.gfx()) {
            p.fill_path(&path, color);
        }
    };
    ridge(p, &[(0.0, 0.62), (0.18, 0.38), (0.32, 0.55), (0.5, 0.3), (0.68, 0.58), (0.85, 0.42), (1.0, 0.6)], hex("#5B6C9A"));
    ridge(p, &[(0.0, 0.72), (0.22, 0.56), (0.4, 0.7), (0.62, 0.52), (0.8, 0.68), (1.0, 0.62)], hex("#2F4858"));
    let lake = RectF::new(r.x, r.y + r.h * 0.78, r.w, r.h * 0.22);
    p.fill_rect(lake, Brush::vertical(lake, hex("#1D6A96"), hex("#0B3954")));
    for i in 0..6 {
        let y = lake.y + 10.0 + i as f32 * 14.0;
        p.fill_rect(RectF::new(r.x + r.w * 0.6 - i as f32 * 6.0, y, 60.0 + i as f32 * 12.0, 2.0), hex("#FFE29A").with_alpha(0.5));
    }
}

fn paint_terminal(p: &mut Painter, r: RectF) {
    p.fill_rect(r, hex("#0C0C0C"));
    let style = TextStyle::new(12.0);
    let lines = [
        ("#3FB950", "PS C:\\glint> cargo test -p glint-overlay"),
        ("#C9D1D9", "   Compiling glint-overlay v0.1.0"),
        ("#C9D1D9", "    Finished test [optimized] in 4.21s"),
        ("#C9D1D9", "     Running unittests src/lib.rs"),
        ("#3FB950", "test result: ok. 31 passed; 0 failed"),
        ("#3FB950", "PS C:\\glint> _"),
    ];
    for (i, (color, line)) in lines.iter().enumerate() {
        p.text(line, &style, hex(color), RectF::new(r.x + 12.0, r.y + 10.0 + i as f32 * 20.0, r.w - 24.0, 18.0));
    }
}

fn paint_document(p: &mut Painter, r: RectF) {
    p.fill_rect(r, Color::WHITE);
    let heading = TextStyle::new(20.0).weight(Weight::Semibold);
    let body = TextStyle::body();
    let ink = Color::rgba(0.0, 0.0, 0.0, 0.86);
    p.text("Glint 1.0", &heading, ink, RectF::new(r.x + 28.0, r.y + 18.0, r.w - 56.0, 30.0));
    let lines = [
        "HDR captures now look exactly like what you saw on screen.",
        "Freeform snips keep a soft, antialiased edge.",
        "Window mode highlights the window under the pointer.",
        "Press Space while dragging to move the selection.",
        "Hold Shift for a perfect square; arrows nudge by one pixel.",
        "The magnifier shows every pixel at 8× with its hex color.",
    ];
    for (i, line) in lines.iter().enumerate() {
        p.text(line, &body, ink, RectF::new(r.x + 28.0, r.y + 64.0 + i as f32 * 26.0, r.w - 56.0, 20.0));
    }
    let chart = RectF::new(r.x + 28.0, r.y + 236.0, r.w - 56.0, 130.0);
    p.fill_rect(chart, hex("#F5F7FA"));
    for (i, (h, color)) in [(0.45, "#FF3B30"), (0.7, "#FF9500"), (0.55, "#FFCC00"), (0.9, "#34C759"), (0.62, "#007AFF"), (0.8, "#AF52DE")]
        .iter()
        .enumerate()
    {
        let bar_h = chart.h * h * 0.85;
        p.fill_round_rect(RectF::new(chart.x + 24.0 + i as f32 * 86.0, chart.bottom() - 10.0 - bar_h, 44.0, bar_h), 4.0, hex(color));
    }
    p.line(PointF::new(chart.x, chart.bottom() - 10.0), PointF::new(chart.right(), chart.bottom() - 10.0), hex("#C7CCD1"), 1.0, &StrokeStyle::default());
}

fn paint_taskbar(p: &mut Painter) {
    p.fill_rect(TASKBAR, Color::rgba(0.11, 0.11, 0.13, 0.94));
    p.fill_rect(RectF::new(0.0, TASKBAR.y, DESKTOP.w, 1.0), Color::rgba(1.0, 1.0, 1.0, 0.08));
    let colors = ["#0078D4", "#FFB900", "#E74856", "#0099BC", "#7A7574", "#10893E", "#8764B8"];
    for (i, color) in colors.iter().enumerate() {
        let c = PointF::new(DESKTOP.w / 2.0 - 120.0 + i as f32 * 40.0, TASKBAR.center().y);
        p.fill_round_rect(RectF::new(c.x - 13.0, c.y - 13.0, 26.0, 26.0), 6.0, hex(color));
    }
    p.text("14:32", &TextStyle::caption().align(glint_ui::TextAlign::Trailing), Color::WHITE, RectF::new(0.0, TASKBAR.y, DESKTOP.w - 20.0, TASKBAR.h));
}
