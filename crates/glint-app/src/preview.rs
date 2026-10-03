//! `--preview <kind>`: offscreen renders of every view for visual review (no window, hook, clipboard or registry).
//! Popups are rendered exactly as their windows draw them, then composited onto a synthetic desktop for context.

use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use glint_core::settings::HdrSettings;
use glint_core::{HdrInfo, MonitorCapture, RectI, Settings};
use glint_ui::{
    Bitmap, Brush, Color, Gfx, Image, Interpolation, OffscreenSpec, Painter, PathBuilder, PointF, RectF, Shadow, SizeF,
    TextStyle, Theme, ThemeMode, View, Weight, render_offscreen, render_view_offscreen,
};

use crate::art::{render_app_icon, render_tray_glyph};
use crate::cli::PreviewArgs;
use crate::hud::{BorderView, HudPhase, HudReadout, HudView, border_rect_px};
use crate::popup::corner_layout;
use crate::settings_model::{DisplayLine, InstallState, SettingsEnv};
use crate::settings_view::{SettingsView, WINDOW_SIZE};
use crate::thumbnail::{ThumbnailContent, ThumbnailView, card_size};
use crate::toast::{Tint, Toast, ToastIcon, ToastView};

pub const APP_KINDS: [&str; 12] = [
    "thumbnail",
    "thumbnail-hover",
    "thumbnail-video",
    "toast",
    "toast-color",
    "settings",
    "settings-light",
    "hud",
    "hud-paused",
    "border",
    "tray-icon",
    "app-icon",
];

pub use glint_editor::PREVIEW_KINDS as EDITOR_KINDS;
pub use glint_overlay::PREVIEW_KINDS as OVERLAY_KINDS;

pub const ICON_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

pub fn theme_for(mode: ThemeMode) -> Theme {
    if mode == ThemeMode::Light { Theme::light() } else { Theme::dark() }
}

/// Renders `args.kind` (Glint's own views, or the overlay and editor crates' previews).
pub fn render(gfx: &Rc<Gfx>, args: &PreviewArgs) -> Result<Image> {
    let kind = args.kind.as_str();
    if OVERLAY_KINDS.contains(&kind) {
        let captures = if args.live { Some(live_captures()?) } else { None };
        return glint_overlay::render_preview(gfx, kind, args.theme, args.scale, captures.as_deref());
    }
    if EDITOR_KINDS.contains(&kind) {
        let image = if args.live { Some(live_screenshot()?) } else { None };
        return glint_editor::render_preview(gfx, kind, args.theme, args.scale, image.as_ref());
    }
    let screenshot = if args.live && kind.starts_with("thumbnail") { Some(live_screenshot()?) } else { None };
    render_app_preview(gfx, kind, args.theme, args.scale, screenshot)
}

fn live_captures() -> Result<Vec<MonitorCapture>> {
    glint_capture::capture_all(&HdrSettings::default()).context("capturing the screen for --live")
}

/// A real capture of the primary (or first) monitor.
fn live_screenshot() -> Result<Image> {
    let captures = live_captures()?;
    let capture = captures.iter().find(|c| c.monitor.primary).or(captures.first()).context("no monitor captured")?;
    Ok(capture.sdr.clone())
}

/// Renders one of Glint's own preview kinds; `screenshot` replaces the synthetic thumbnail content.
pub fn render_app_preview(gfx: &Rc<Gfx>, kind: &str, theme: ThemeMode, scale: f32, screenshot: Option<Image>) -> Result<Image> {
    let theme_value = theme_for(theme);
    match kind {
        "thumbnail" | "thumbnail-hover" | "thumbnail-video" => thumbnail(gfx, kind, &theme_value, scale, screenshot),
        "toast" | "toast-color" => toast(gfx, kind, &theme_value, scale),
        "settings" => settings(gfx, &theme_value, scale),
        "settings-light" => settings(gfx, &Theme::light(), scale),
        "hud" | "hud-paused" => hud(gfx, kind == "hud-paused", &theme_value, scale),
        "border" => border(gfx, &theme_value, scale),
        "app-icon" => app_icon_sheet(gfx, &theme_value, scale),
        "tray-icon" => tray_icon_sheet(gfx, &theme_value, scale),
        other => bail!("unknown preview kind `{other}`"),
    }
}

// ---- synthetic content ------------------------------------------------------------------------------------------

/// A calm Windows-11-like wallpaper with one app window, in DIP.
fn paint_desktop(p: &mut Painter, size: SizeF, dark: bool) {
    let (a, b, c) = if dark { ("#0B1B3A", "#24346E", "#5B4BA8") } else { ("#9CC3F0", "#C8D7F5", "#E9D9F3") };
    p.fill_rect(
        RectF::new(0.0, 0.0, size.w, size.h),
        Brush::linear(
            PointF::new(0.0, 0.0),
            PointF::new(size.w, size.h),
            &[(0.0, Color::hex(a).unwrap()), (0.55, Color::hex(b).unwrap()), (1.0, Color::hex(c).unwrap())],
        ),
    );
    let glow = if dark { Color::rgba(0.55, 0.45, 1.0, 0.22) } else { Color::rgba(1.0, 1.0, 1.0, 0.45) };
    p.shadow(RectF::new(size.w * 0.55, size.h * 0.15, size.w * 0.3, size.h * 0.3), size.w * 0.15, &Shadow::new(0.0, size.w * 0.25, glow));
    let window = RectF::new(size.w * 0.06, size.h * 0.1, size.w * 0.5, size.h * 0.62);
    p.shadow(window, 8.0, &Shadow::new(10.0, 32.0, Color::rgba(0.0, 0.0, 0.0, 0.3)));
    let (body, bar, ink) = if dark {
        (Color::hex("#202020").unwrap(), Color::hex("#2B2B2B").unwrap(), Color::rgba(1.0, 1.0, 1.0, 0.16))
    } else {
        (Color::hex("#FBFBFB").unwrap(), Color::hex("#F0F0F0").unwrap(), Color::rgba(0.0, 0.0, 0.0, 0.10))
    };
    p.fill_round_rect(window, 8.0, body);
    p.clip_round_rect(window, 8.0, |p| {
        p.fill_rect(RectF::new(window.x, window.y, window.w, 30.0), bar);
        for i in 0..7 {
            let w = window.w * (0.5 + 0.06 * ((i * 5) % 7) as f32);
            p.fill_round_rect(RectF::new(window.x + 18.0, window.y + 48.0 + i as f32 * 18.0, w * 0.7, 7.0), 3.5, ink);
        }
    });
}

/// A colorful "screenshot": a photo viewer showing a mountain lake, at `w`×`h` physical pixels.
pub fn synthetic_screenshot(gfx: &Rc<Gfx>, w: u32, h: u32) -> Result<Image> {
    let spec = OffscreenSpec::pixels(w, h, 1.0, Theme::dark());
    let (wf, hf) = (w as f32, h as f32);
    render_offscreen(gfx, &spec, |_, p| {
        p.fill_rect(RectF::new(0.0, 0.0, wf, hf), Color::hex("#1F1F1F").unwrap());
        p.fill_rect(RectF::new(0.0, 0.0, wf, hf * 0.06), Color::hex("#2C2C2C").unwrap());
        let title = TextStyle::new(hf * 0.022).weight(Weight::Medium);
        p.text("Photos — Lake Tahoe.heic", &title, Color::rgba(1.0, 1.0, 1.0, 0.85), RectF::new(wf * 0.02, 0.0, wf * 0.6, hf * 0.06));
        let photo = RectF::new(wf * 0.04, hf * 0.1, wf * 0.92, hf * 0.84);
        p.clip_round_rect(photo, hf * 0.012, |p| {
            p.fill_rect(
                photo,
                Brush::linear(
                    PointF::new(0.0, photo.y),
                    PointF::new(0.0, photo.y + photo.h * 0.62),
                    &[(0.0, Color::hex("#2F6FD8").unwrap()), (0.6, Color::hex("#F6A15C").unwrap()), (1.0, Color::hex("#FFD6A0").unwrap())],
                ),
            );
            p.fill_circle(PointF::new(photo.x + photo.w * 0.7, photo.y + photo.h * 0.42), photo.h * 0.09, Color::hex("#FFF2CF").unwrap());
            let ridge = |p: &mut Painter, points: &[(f32, f32)], color: &str| {
                let mut path = PathBuilder::new();
                path.move_to(PointF::new(photo.x, photo.bottom()));
                for (x, y) in points {
                    path.line_to(PointF::new(photo.x + photo.w * x, photo.y + photo.h * y));
                }
                path.line_to(PointF::new(photo.right(), photo.bottom())).close();
                if let Ok(path) = path.build(p.gfx()) {
                    p.fill_path(&path, Color::hex(color).unwrap());
                }
            };
            ridge(p, &[(0.0, 0.62), (0.18, 0.4), (0.33, 0.55), (0.52, 0.33), (0.7, 0.52), (0.86, 0.42), (1.0, 0.58)], "#4B4E7A");
            ridge(p, &[(0.0, 0.7), (0.22, 0.56), (0.4, 0.66), (0.62, 0.5), (0.8, 0.64), (1.0, 0.6)], "#2D3253");
            p.fill_rect(
                RectF::new(photo.x, photo.y + photo.h * 0.72, photo.w, photo.h * 0.28),
                Brush::linear(
                    PointF::new(0.0, photo.y + photo.h * 0.72),
                    PointF::new(0.0, photo.bottom()),
                    &[(0.0, Color::hex("#3E7CB8").unwrap()), (1.0, Color::hex("#123B66").unwrap())],
                ),
            );
        });
    })
}

fn sample_env() -> SettingsEnv {
    SettingsEnv {
        displays: vec![
            DisplayLine {
                name: "DELL U2723QE".into(),
                hdr: Some(HdrInfo { sdr_white_nits: 280.0, max_nits: 603.0, max_full_frame_nits: 400.0, min_nits: 0.1 }),
            },
            DisplayLine { name: "LG ULTRAGEAR".into(), hdr: None },
        ],
        screenshots_dir: r"C:\Users\you\Pictures\Screenshots".into(),
        recordings_dir: r"C:\Users\you\Videos\Screen Recordings".into(),
        install: InstallState::NotInstalled,
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

// ---- composition helpers ----------------------------------------------------------------------------------------

/// Renders `view` at `size` and returns it as a bitmap for compositing.
fn view_bitmap(gfx: &Rc<Gfx>, view: &mut dyn View, size: SizeF, scale: f32, theme: &Theme) -> Result<Rc<Bitmap>> {
    let spec = OffscreenSpec::new(size, scale, theme.clone());
    Ok(Bitmap::new(render_view_offscreen(gfx, view, &spec)?))
}

fn place(p: &mut Painter, bitmap: &Bitmap, origin: PointF) {
    let origin = p.snap_point(origin);
    let size = SizeF::new(bitmap.width() as f32 * p.px(), bitmap.height() as f32 * p.px());
    p.bitmap(bitmap, RectF::new(origin.x, origin.y, size.w, size.h), None, 1.0, Interpolation::Nearest);
}

fn on_desktop(gfx: &Rc<Gfx>, canvas: SizeF, scale: f32, theme: &Theme, layers: &[(Rc<Bitmap>, PointF)]) -> Result<Image> {
    let spec = OffscreenSpec::new(canvas, scale, theme.clone());
    render_offscreen(gfx, &spec, |_, p| {
        paint_desktop(p, canvas, theme.is_dark());
        for (bitmap, origin) in layers {
            place(p, bitmap, *origin);
        }
    })
}

// ---- kinds -----------------------------------------------------------------------------------------------------

fn thumbnail(gfx: &Rc<Gfx>, kind: &str, theme: &Theme, scale: f32, screenshot: Option<Image>) -> Result<Image> {
    let image = match screenshot {
        Some(image) => image,
        None => synthetic_screenshot(gfx, (1600.0 * scale) as u32, (1000.0 * scale) as u32)?,
    };
    let canvas = SizeF::new(560.0, 340.0);
    let work = RectI::new(0, 0, (canvas.w * scale).round() as i32, (canvas.h * scale).round() as i32);
    let card = card_size(image.width, image.height, scale);
    let layout = corner_layout(card, work, scale, 0);
    let content = if kind == "thumbnail-video" {
        ThumbnailContent::Video { duration: Duration::from_secs(74) }
    } else {
        ThumbnailContent::Image
    };
    let mut view = ThumbnailView::new(1, Rc::new(image), scale, content, layout.card).preview_state(kind == "thumbnail-hover");
    let bitmap = view_bitmap(gfx, &mut view, layout.size, scale, theme)?;
    let origin = PointF::new(layout.origin_px.x as f32 / scale, layout.origin_px.y as f32 / scale);
    on_desktop(gfx, canvas, scale, theme, &[(bitmap, origin)])
}

fn toast(gfx: &Rc<Gfx>, kind: &str, theme: &Theme, scale: f32) -> Result<Image> {
    let toasts = if kind == "toast-color" {
        vec![
            Toast::new(ToastIcon::Swatch([0xF6, 0x82, 0x3B, 255]), "Copied #3B82F6", None),
            Toast::new(ToastIcon::Symbol(glint_ui::Icon::ScanText, Tint::Neutral), "No text found", Some("Try a larger or sharper selection")),
        ]
    } else {
        vec![
            Toast::new(ToastIcon::App, "Glint is running", Some("Press Win + Shift + S to snip")),
            Toast::new(ToastIcon::Symbol(glint_ui::Icon::ScanText, Tint::Accent), "Text copied", Some("Quarterly revenue grew 18% year over year…")),
        ]
    };
    let canvas = SizeF::new(520.0, 260.0);
    let work = RectI::new(0, 0, (canvas.w * scale).round() as i32, (canvas.h * scale).round() as i32);
    let mut layers = Vec::new();
    let mut lift = 0;
    for (i, item) in toasts.into_iter().enumerate() {
        let card = crate::toast::card_size(gfx, &item);
        let layout = corner_layout(card, work, scale, lift);
        lift += ((layout.card.h + 12.0) * scale).round() as i32;
        let mut view = ToastView::new(i as u64, item, layout.card).settled();
        let bitmap = view_bitmap(gfx, &mut view, layout.size, scale, theme)?;
        layers.push((bitmap, PointF::new(layout.origin_px.x as f32 / scale, layout.origin_px.y as f32 / scale)));
    }
    on_desktop(gfx, canvas, scale, theme, &layers)
}

fn settings(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let mut settings = Settings::default();
    settings.hdr.exposure_stops = 0.5;
    let mut view = SettingsView::new(settings, sample_env());
    let height = view.content_height(gfx, WINDOW_SIZE.w);
    let spec = OffscreenSpec::new(SizeF::new(WINDOW_SIZE.w, height), scale, theme.clone());
    render_view_offscreen(gfx, &mut view, &spec)
}

fn hud(gfx: &Rc<Gfx>, paused: bool, theme: &Theme, scale: f32) -> Result<Image> {
    let phase = if paused { HudPhase::Paused } else { HudPhase::Recording };
    let readout = HudReadout { phase, elapsed: Duration::from_millis(83_400), level: 0.32 };
    let mut view = HudView::new(true, true).with_readout(readout);
    let size = view.window_size(gfx);
    let spec = OffscreenSpec::new(size, scale, theme.clone()).time(10.0);
    let bitmap = Bitmap::new(render_view_offscreen(gfx, &mut view, &spec)?);
    let canvas = SizeF::new(size.w + 160.0, size.h + 40.0);
    on_desktop(gfx, canvas, scale, theme, &[(bitmap, PointF::new(80.0, 0.0))])
}

fn border(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let canvas = SizeF::new(640.0, 400.0);
    let monitor = RectI::new(0, 0, (canvas.w * scale).round() as i32, (canvas.h * scale).round() as i32);
    let region = RectI::new((120.0 * scale) as i32, (90.0 * scale) as i32, (400.0 * scale) as i32, (230.0 * scale) as i32);
    let frame = border_rect_px(region, monitor, scale);
    let size = SizeF::new(frame.w as f32 / scale, frame.h as f32 / scale);
    let bitmap = view_bitmap(gfx, &mut BorderView, size, scale, theme)?;
    let mut hud = HudView::new(false, false).with_readout(HudReadout { phase: HudPhase::Recording, elapsed: Duration::from_secs(5), level: 0.0 });
    let hud_size = hud.window_size(gfx);
    let spec = OffscreenSpec::new(hud_size, scale, theme.clone()).time(10.0);
    let hud_bitmap = Bitmap::new(render_view_offscreen(gfx, &mut hud, &spec)?);
    let layers = [
        (bitmap, PointF::new(frame.x as f32 / scale, frame.y as f32 / scale)),
        (hud_bitmap, PointF::new((canvas.w - hud_size.w) / 2.0, 0.0)),
    ];
    on_desktop(gfx, canvas, scale, theme, &layers)
}

fn label(p: &mut Painter, text: &str, center_x: f32, y: f32, color: Color) {
    let style = TextStyle::caption().centered().tabular();
    p.text(text, &style, color, RectF::new(center_x - 60.0, y, 120.0, 16.0));
}

/// Draws `image` 1:1 with physical pixels at `origin` (DIP), optionally magnified with nearest-neighbour sampling.
fn draw_pixels(p: &mut Painter, image: &Rc<Bitmap>, origin: PointF, magnify: f32) {
    let size = SizeF::new(image.width() as f32 * p.px() * magnify, image.height() as f32 * p.px() * magnify);
    let origin = p.snap_point(origin);
    p.bitmap(image, RectF::new(origin.x, origin.y, size.w, size.h), None, 1.0, Interpolation::Nearest);
}

fn app_icon_sheet(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let icons: Vec<(u32, Rc<Bitmap>)> =
        ICON_SIZES.iter().map(|&s| Ok((s, Bitmap::new(render_app_icon(gfx, s)?)))).collect::<Result<_>>()?;
    let size = SizeF::new(1000.0, 620.0);
    let spec = OffscreenSpec::new(size, scale, theme.clone()).background(theme.window_background);
    render_offscreen(gfx, &spec, |_, p| {
        let text = theme.text_secondary;
        let mut x = 24.0;
        for (side, bitmap) in icons.iter().rev().take(5) {
            let dip = *side as f32 / scale;
            draw_pixels(p, bitmap, PointF::new(x, 24.0 + (256.0 / scale - dip)), 1.0);
            label(p, &format!("{side} px"), x + dip / 2.0, 36.0 + 256.0 / scale, text);
            x += dip + 32.0;
        }
        let mut x = 24.0;
        let top = 340.0;
        for (side, bitmap) in icons.iter().take(5) {
            let dip = *side as f32 / scale;
            draw_pixels(p, bitmap, PointF::new(x, top), 1.0);
            draw_pixels(p, bitmap, PointF::new(x, top + 48.0), 4.0);
            label(p, &format!("{side} px"), x + dip * 2.0, top + 60.0 + dip * 4.0, text);
            x += dip * 4.0 + 40.0;
        }
    })
}

fn tray_icon_sheet(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let sizes = [16u32, 20, 24, 32, 40];
    let size = SizeF::new(760.0, 420.0);
    let spec = OffscreenSpec::new(size, scale, theme.clone()).background(theme.window_background);
    let strips = [(Color::rgb8(0x1C, 0x1C, 0x1C), Color::WHITE), (Color::rgb8(0xEE, 0xEE, 0xEE), Color::BLACK)];
    let glyphs: Vec<Vec<Rc<Bitmap>>> = strips
        .iter()
        .map(|(_, ink)| sizes.iter().map(|&s| Ok(Bitmap::new(render_tray_glyph(gfx, s, *ink)?))).collect::<Result<_>>())
        .collect::<Result<_>>()?;
    render_offscreen(gfx, &spec, |_, p| {
        for (row, ((background, _), bitmaps)) in strips.iter().zip(&glyphs).enumerate() {
            let top = 20.0 + row as f32 * 200.0;
            p.fill_rect(RectF::new(0.0, top, size.w, 180.0), *background);
            let mut x = 24.0;
            for (side, bitmap) in sizes.iter().zip(bitmaps) {
                draw_pixels(p, bitmap, PointF::new(x, top + 16.0), 1.0);
                draw_pixels(p, bitmap, PointF::new(x, top + 48.0), (96 / side).max(2) as f32);
                let ink = if row == 0 { Color::rgba(1.0, 1.0, 1.0, 0.6) } else { Color::rgba(0.0, 0.0, 0.0, 0.55) };
                label(p, &format!("{side} px"), x + 48.0 / scale, top + 150.0, ink);
                x += 96.0 / scale + 48.0;
            }
        }
    })
}
