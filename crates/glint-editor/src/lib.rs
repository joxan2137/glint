//! The markup editor window (DESIGN §8).

mod chrome;
mod color_picker;
mod crop;
mod editor;
mod history;
mod hit;
mod ink;
mod math;
mod model;
mod ocr_text;
mod pixels;
mod preview;
mod render;
mod text_edit;
mod tools;
mod viewport;
mod worker;

use std::path::PathBuf;
use std::rc::Rc;

use glint_core::{CaptureMode, HdrImage, Image, MonitorInfo, PointI, RectI, Settings, SizeF, ThemeMode, ToneMapParams};
use glint_ui::{App, Gfx, WindowId, WindowSpec};

pub use preview::KINDS as PREVIEW_KINDS;

pub struct EditorDoc {
    /// Base SDR image as captured (already tone-mapped).
    pub image: Image,
    /// Raw scRGB pixels when the capture came from an HDR monitor; enables the HDR panel.
    pub hdr: Option<HdrImage>,
    pub tone_map: ToneMapParams,
    /// Monitor to center the window on; None = monitor under the cursor.
    pub monitor: Option<MonitorInfo>,
    /// Where the capture was auto-saved, if it was. Ctrl+S overwrites it.
    pub saved_path: Option<PathBuf>,
}

pub type NewSnipFn = Rc<dyn Fn(&App, CaptureMode, u32)>;
pub type SavedFn = Rc<dyn Fn(&App, PathBuf)>;

/// Requests the editor sends back to the app.
pub struct EditorHost {
    /// "New" button / Ctrl+N: start a new snip with this mode and delay (seconds).
    pub new_snip: NewSnipFn,
    /// The editor exported or saved an image; the app updates its "last capture" (thumbnail drag source etc.).
    pub saved: SavedFn,
}

/// Smallest editor window (DIP).
pub const MIN_WINDOW: SizeF = SizeF::new(760.0, 520.0);

/// Opens a new editor window. Settings are a snapshot (save dir, format, theme).
pub fn open_editor(app: &App, doc: EditorDoc, settings: &Settings, host: EditorHost) -> anyhow::Result<WindowId> {
    worker::install_handler(app);
    let (work_px, dpi) = match &doc.monitor {
        Some(m) => (m.work_rect, m.dpi),
        None => cursor_work_area(),
    };
    let scale = (dpi as f32 / 96.0).max(0.5);
    let work_dip = SizeF::new(work_px.w as f32 / scale, work_px.h as f32 / scale);
    let image_px = SizeF::new(doc.image.width as f32, doc.image.height as f32);
    let size = window_size(image_px, scale, work_dip);
    let title = doc
        .saved_path
        .as_deref()
        .and_then(|p| p.file_stem())
        .map_or_else(|| "Glint".to_string(), |s| format!("{} – Glint", s.to_string_lossy()));
    let spec = WindowSpec::normal(&title, size).unified_title_bar().centered_in(work_px).min_size(MIN_WINDOW);
    app.open(spec, editor::EditorView::new(doc, settings, Some(host)))
}

fn cursor_work_area() -> (RectI, u32) {
    let mut point = windows::Win32::Foundation::POINT::default();
    // SAFETY: writes the cursor position into a valid local.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) };
    let at = PointI::new(point.x, point.y);
    let work = glint_ui::win::work_area_at_point(at).unwrap_or(RectI::new(0, 0, 1920, 1040));
    (work, glint_ui::win::dpi_at_point(at))
}

/// Client size for an image of `image_px` on a monitor at `scale`: the image at 100 % plus chrome, shrunk to fit
/// 80 % of the work area (keeping the image's aspect), never below `MIN_WINDOW`.
pub fn window_size(image_px: SizeF, scale: f32, work_dip: SizeF) -> SizeF {
    let chrome_w = 2.0 * chrome::FIT_SIDE;
    let chrome_h = chrome::FIT_TOP + chrome::FIT_BOTTOM;
    let image = SizeF::new(image_px.w / scale, image_px.h / scale);
    let max = SizeF::new(work_dip.w * 0.8, work_dip.h * 0.8);
    let fit = ((max.w - chrome_w) / image.w.max(1.0)).min((max.h - chrome_h) / image.h.max(1.0)).clamp(0.05, 1.0);
    let w = (image.w * fit + chrome_w).max(MIN_WINDOW.w).min(work_dip.w.max(MIN_WINDOW.w));
    let h = (image.h * fit + chrome_h).max(MIN_WINDOW.h).min(work_dip.h.max(MIN_WINDOW.h));
    SizeF::new(w.round(), h.round())
}

/// Offscreen render for visual checks: `editor`, `editor-pen`, `editor-shapes`, `editor-text`, `editor-crop`,
/// `editor-hdr`, `editor-redact`, `editor-select` (plus `editor-ocr`, `editor-picker`, `editor-menu`; see
/// `PREVIEW_KINDS`). Uses `image` when given, else a synthetic screenshot.
pub fn render_preview(
    gfx: &Gfx,
    kind: &str,
    theme: ThemeMode,
    scale: f32,
    image: Option<&Image>,
) -> anyhow::Result<Image> {
    preview::render(gfx, kind, theme, scale, image)
}

/// Full-resolution export of the `editor` preview document (optionally cropped), to compare with its canvas.
pub fn render_preview_export(gfx: &Gfx, image: Option<&Image>, cropped: bool) -> anyhow::Result<Image> {
    preview::export(gfx, image, cropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_fits_small_images_at_actual_size() {
        let size = window_size(SizeF::new(1000.0, 600.0), 1.0, SizeF::new(2560.0, 1400.0));
        assert_eq!(size, SizeF::new(1048.0, 788.0));
    }

    #[test]
    fn window_shrinks_large_images_into_eighty_percent() {
        let work = SizeF::new(1920.0, 1040.0);
        let size = window_size(SizeF::new(3840.0, 2160.0), 1.0, work);
        assert!(size.w <= work.w * 0.8 + 0.5 && size.h <= work.h * 0.8 + 0.5);
        let image_w = size.w - 2.0 * chrome::FIT_SIDE;
        let image_h = size.h - chrome::FIT_TOP - chrome::FIT_BOTTOM;
        assert!((image_w / image_h - 16.0 / 9.0).abs() < 0.02, "aspect kept: {size:?}");
    }

    #[test]
    fn window_never_below_minimum_and_respects_scale() {
        let tiny = window_size(SizeF::new(40.0, 30.0), 2.0, SizeF::new(1280.0, 700.0));
        assert_eq!(tiny, MIN_WINDOW);
        let hidpi = window_size(SizeF::new(2000.0, 1200.0), 2.0, SizeF::new(1920.0, 1040.0));
        assert_eq!(hidpi, SizeF::new(1048.0, 788.0));
    }
}
