//! After-capture decisions (DESIGN §7) as pure functions: what to do with a finished snip given the settings, the
//! snip outline, which overlay choices to remember, and small text formatters.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use glint_core::settings::{AfterCaptureSettings, Outline};
use glint_core::{CaptureMode, Image, ImageFormat, Settings};
use glint_overlay::OverlayPrefs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    /// A selection made in the overlay.
    Overlay,
    /// Alt+Print Screen: the foreground window straight to the clipboard.
    WindowShortcut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Thumbnail,
    Editor,
    Nothing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelinePlan {
    pub outline: Option<Outline>,
    pub copy: bool,
    pub save: Option<ImageFormat>,
    pub sound: bool,
    pub presentation: Presentation,
    /// The thumbnail needs a file to drag even though nothing is auto-saved: write a PNG to %TEMP%.
    pub temp_drag_file: bool,
}

pub fn plan(settings: &AfterCaptureSettings, source: CaptureSource) -> PipelinePlan {
    let presentation = match source {
        CaptureSource::WindowShortcut if settings.show_thumbnail || settings.open_editor => Presentation::Thumbnail,
        CaptureSource::Overlay if settings.open_editor => Presentation::Editor,
        _ if settings.show_thumbnail => Presentation::Thumbnail,
        _ => Presentation::Nothing,
    };
    let save = settings.auto_save.then_some(settings.format);
    PipelinePlan {
        outline: settings.outline.filter(|o| o.width_px > 0 && o.color[3] > 0),
        copy: settings.copy_to_clipboard || source == CaptureSource::WindowShortcut,
        save,
        sound: settings.play_sound,
        presentation,
        temp_drag_file: presentation == Presentation::Thumbnail && save.is_none(),
    }
}

/// Picks an unused `prefix <timestamp>.ext` name in `dir` and claims it with an empty placeholder file, so two
/// captures in the same second never get the same path (the writer later replaces the placeholder).
pub fn reserve_unique_path(dir: &Path, prefix: &str, ext: &str) -> Result<PathBuf> {
    for _ in 0..1000 {
        let path = glint_sys::paths::unique_path(dir, prefix, ext)?;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).with_context(|| format!("reserving {}", path.display())),
        }
    }
    bail!("no free file name for {prefix} in {}", dir.display())
}

pub fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
    }
}

/// Blends `outline` (straight-alpha BGRA) over a band `width_px` wide along the image's edges.
pub fn draw_outline(image: &mut Image, outline: &Outline) {
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 {
        return;
    }
    let band = (outline.width_px as usize).min(w.div_ceil(2)).min(h.div_ceil(2));
    let [ob, og, or, oa] = outline.color.map(|c| c as f32 / 255.0);
    for y in 0..h {
        let edge_row = y < band || y >= h - band;
        for x in 0..w {
            if !(edge_row || x < band || x >= w - band) {
                continue;
            }
            let i = (y * w + x) * 4;
            let below = &mut image.data[i..i + 4];
            let ba = below[3] as f32 / 255.0;
            let alpha = oa + ba * (1.0 - oa);
            if alpha <= 0.0 {
                continue;
            }
            let blend = |top: f32, under: u8| ((top * oa + under as f32 / 255.0 * ba * (1.0 - oa)) / alpha * 255.0).round() as u8;
            below[0] = blend(ob, below[0]);
            below[1] = blend(og, below[1]);
            below[2] = blend(or, below[2]);
            below[3] = (alpha * 255.0).round() as u8;
        }
    }
}

/// Persists the overlay's choices; returns true when anything changed. Text and color modes are one-off tools, so
/// they never become the default snip mode.
pub fn remember_overlay_choices(settings: &mut Settings, prefs: &OverlayPrefs) -> bool {
    let before = settings.clone();
    if !matches!(prefs.mode, CaptureMode::Text | CaptureMode::ColorPicker) {
        settings.capture.last_mode = prefs.mode;
    }
    settings.capture.delay_secs = prefs.delay_secs;
    settings.capture.show_magnifier = prefs.show_magnifier;
    settings.record.system_audio = prefs.system_audio;
    settings.record.microphone = prefs.microphone;
    *settings != before
}

/// The mode a plain snip starts in.
pub fn snip_mode(settings: &Settings) -> CaptureMode {
    match settings.capture.last_mode {
        CaptureMode::Text | CaptureMode::ColorPicker => CaptureMode::Rectangle,
        mode => mode,
    }
}

pub fn hex_color(bgra: [u8; 4]) -> String {
    format!("#{:02X}{:02X}{:02X}", bgra[2], bgra[1], bgra[0])
}

/// First non-empty line, trimmed and shortened for a toast.
pub fn first_line(text: &str, max_chars: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    if line.chars().count() <= max_chars {
        return Some(line.to_string());
    }
    let mut short: String = line.chars().take(max_chars.saturating_sub(1)).collect();
    short.push('…');
    Some(short)
}

/// `0:07`, `12:34`, `1:02:03`.
pub fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs_f64().round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// HUD clock: `00:12`, `12:34`, `1:02:03`.
pub fn format_clock(duration: Duration) -> String {
    let total = duration.as_secs();
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(mode: CaptureMode) -> OverlayPrefs {
        OverlayPrefs { mode, video: false, delay_secs: 5, show_magnifier: false, system_audio: false, microphone: true }
    }

    #[test]
    fn default_settings_copy_save_and_show_the_thumbnail() {
        let plan = plan(&AfterCaptureSettings::default(), CaptureSource::Overlay);
        assert!(plan.copy && !plan.sound && plan.outline.is_none());
        assert_eq!(plan.save, Some(ImageFormat::Png));
        assert_eq!(plan.presentation, Presentation::Thumbnail);
        assert!(!plan.temp_drag_file);
    }

    #[test]
    fn editor_wins_over_thumbnail_and_unsaved_thumbnails_get_a_drag_file() {
        let mut settings = AfterCaptureSettings { open_editor: true, auto_save: false, ..Default::default() };
        assert_eq!(plan(&settings, CaptureSource::Overlay).presentation, Presentation::Editor);
        let window = plan(&settings, CaptureSource::WindowShortcut);
        assert_eq!(window.presentation, Presentation::Thumbnail);
        assert!(window.temp_drag_file);
        settings.open_editor = false;
        settings.show_thumbnail = false;
        assert_eq!(plan(&settings, CaptureSource::Overlay).presentation, Presentation::Nothing);
    }

    #[test]
    fn window_shortcut_always_copies_and_jpeg_is_respected() {
        let settings = AfterCaptureSettings { copy_to_clipboard: false, format: ImageFormat::Jpeg, ..Default::default() };
        assert!(!plan(&settings, CaptureSource::Overlay).copy);
        let window = plan(&settings, CaptureSource::WindowShortcut);
        assert!(window.copy);
        assert_eq!(window.save, Some(ImageFormat::Jpeg));
        assert_eq!(extension(ImageFormat::Jpeg), "jpg");
    }

    #[test]
    fn invisible_outlines_are_dropped() {
        let settings = AfterCaptureSettings { outline: Some(Outline { color: [0, 0, 255, 0], width_px: 2 }), ..Default::default() };
        assert!(plan(&settings, CaptureSource::Overlay).outline.is_none());
    }

    #[test]
    fn outline_paints_only_the_edge_band() {
        let mut image = Image::from_bgra(6, 5, [10u8, 20, 30, 255].repeat(30));
        draw_outline(&mut image, &Outline { color: [0, 0, 255, 255], width_px: 2 });
        assert_eq!(image.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(image.pixel(1, 2), [0, 0, 255, 255]);
        assert_eq!(image.pixel(4, 3), [0, 0, 255, 255]);
        assert_eq!(image.pixel(2, 2), [10, 20, 30, 255], "interior untouched");
    }

    #[test]
    fn outline_blends_translucent_colors_and_fills_transparent_pixels() {
        let mut image = Image::from_bgra(3, 3, [0u8, 0, 0, 255].repeat(8).into_iter().chain([0, 0, 0, 0]).collect());
        draw_outline(&mut image, &Outline { color: [255, 255, 255, 128], width_px: 1 });
        let [b, g, r, a] = image.pixel(0, 0);
        assert_eq!(a, 255);
        assert!((127..=129).contains(&b) && b == g && g == r);
        assert_eq!(image.pixel(2, 2), [255, 255, 255, 128], "transparent corner takes the outline color");
    }

    #[test]
    fn overlay_choices_are_remembered_except_one_off_modes() {
        let mut settings = Settings::default();
        assert!(remember_overlay_choices(&mut settings, &prefs(CaptureMode::Window)));
        assert_eq!(settings.capture.last_mode, CaptureMode::Window);
        assert_eq!((settings.capture.delay_secs, settings.capture.show_magnifier), (5, false));
        assert!(settings.record.microphone && !settings.record.system_audio);
        assert!(!remember_overlay_choices(&mut settings, &prefs(CaptureMode::Text)));
        assert_eq!(settings.capture.last_mode, CaptureMode::Window);
        settings.capture.last_mode = CaptureMode::ColorPicker;
        assert_eq!(snip_mode(&settings), CaptureMode::Rectangle);
    }

    #[test]
    fn reserved_paths_are_distinct_within_one_second() {
        let dir = std::env::temp_dir().join(format!("glint-reserve-test-{}", std::process::id()));
        let first = reserve_unique_path(&dir, "Screenshot", "png").unwrap();
        let second = reserve_unique_path(&dir, "Screenshot", "png").unwrap();
        assert_ne!(first, second);
        assert!(first.exists() && second.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn formatters() {
        assert_eq!(hex_color([0x33, 0x22, 0x11, 255]), "#112233");
        assert_eq!(first_line("\n  Hello world  \nsecond", 40).as_deref(), Some("Hello world"));
        assert_eq!(first_line("abcdefghij", 5).as_deref(), Some("abcd…"));
        assert_eq!(first_line(" \n ", 5), None);
        assert_eq!(format_duration(Duration::from_millis(7400)), "0:07");
        assert_eq!(format_duration(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(format_clock(Duration::from_millis(12_900)), "00:12");
        assert_eq!(format_clock(Duration::from_secs(754)), "12:34");
    }
}
