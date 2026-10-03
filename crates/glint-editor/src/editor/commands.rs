//! Export and everything built on it (copy, save, share, OCR, auto-copy), HDR re-tone-mapping, worker messages.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::ensure;
use glint_core::{CaptureMode, Image, ImageFormat, PointF};
use glint_ui::{Ctx, Gfx};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

use super::{EditorView, Gesture, OcrState, TIMER_AUTOCOPY};
use crate::chrome::ToastKind;
use crate::ocr_text::RecognizedText;
use crate::render::{self, Layer, Scene};
use crate::tools;
use crate::worker::{self, Message, Poster, Retoner};

/// Share files older than this are removed when an editor opens or closes.
const SHARE_RETENTION: Duration = Duration::from_secs(30 * 60);

fn share_root() -> PathBuf {
    std::env::temp_dir().join("Glint").join("Share")
}

/// A fresh folder per share, so the file keeps its friendly name and never collides with another share.
fn unique_share_dir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    share_root().join(format!("{}-{nanos}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
}

/// Deletes share folders older than `max_age` on a background thread; errors are ignored.
fn prune_shares(max_age: Duration) {
    let spawned = std::thread::Builder::new().name("glint-share-cleanup".into()).spawn(move || {
        let Ok(entries) = std::fs::read_dir(share_root()) else { return };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > max_age);
            if old {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("spawning share cleanup: {e}");
    }
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
    }
}

/// The format a path's extension asks for, else `fallback`.
pub fn format_for(path: &Path, fallback: ImageFormat) -> ImageFormat {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("jpg" | "jpeg") => ImageFormat::Jpeg,
        Some("png") => ImageFormat::Png,
        _ => fallback,
    }
}

/// Runs `f` inside a COM single-threaded apartment (file dialogs need one); balanced with the caller's own.
fn with_sta<R>(f: impl FnOnce() -> R) -> R {
    // SAFETY: plain COM initialization for this thread, undone below only when it succeeded.
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = f();
    if initialized {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    result
}

impl EditorView {
    /// Base + annotations at full resolution over the crop. Images beyond the GPU's bitmap limit export just the
    /// cropped region at full resolution; a crop that is itself too large fails with an error.
    pub(crate) fn export(&mut self, gfx: &Rc<Gfx>) -> anyhow::Result<Image> {
        let (w, h) = (self.base_image.width, self.base_image.height);
        let content = self.doc.content_rect(self.image_size());
        let max_side = gfx.max_bitmap_size();
        ensure!(content.w > 0 && content.h > 0, "nothing to export");
        ensure!(
            content.w as u32 <= max_side && content.h as u32 <= max_side,
            "a {}×{} export exceeds the GPU's {max_side} px bitmap limit",
            content.w,
            content.h
        );
        self.prepare_layers(gfx, false);
        if crate::pixels::reduction(w, h, max_side) == 1 {
            let scene = self.scene().ok_or_else(|| anyhow::anyhow!("the base image is not ready"))?;
            return render::export(gfx, &self.doc, &scene);
        }
        let base = Layer::region(&self.base_image, content);
        let pixelated = self.doc.has_pixelation().then(|| Layer::region(&self.pixelated_image(), content));
        let scene = Scene { base: &base, pixelated: pixelated.as_ref(), image: (w, h), cache: &self.cache };
        render::export(gfx, &self.doc, &scene)
    }

    pub(super) fn copy(&mut self, cx: &mut Ctx) {
        if cx.hwnd() == 0 {
            return;
        }
        let copied = self.export(cx.gfx()).and_then(|image| glint_sys::clipboard::copy_image(hwnd(cx.hwnd()), &image));
        match copied {
            Ok(()) => {
                self.unsynced_changes = false;
                self.toast(cx, "Copied", ToastKind::Done);
            }
            Err(e) => {
                log::error!("copy: {e:#}");
                self.toast(cx, "Couldn't copy the image", ToastKind::Error);
            }
        }
    }

    pub(super) fn save(&mut self, cx: &mut Ctx) {
        match self.saved_path.clone() {
            Some(path) => {
                let format = format_for(&path, self.settings.after_capture.format);
                self.save_to(cx, path, format);
            }
            None => self.save_as(cx),
        }
    }

    pub(super) fn save_as(&mut self, cx: &mut Ctx) {
        let (Some(app), Some(window)) = (cx.app().cloned(), cx.window()) else { return };
        let owner = cx.hwnd();
        let format = self.saved_path.as_deref().map_or(self.settings.after_capture.format, |p| {
            format_for(p, self.settings.after_capture.format)
        });
        let dir = self
            .saved_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| glint_sys::paths::screenshots_dir(&self.settings).ok())
            .unwrap_or_else(std::env::temp_dir);
        let name = glint_sys::paths::timestamped_name("Screenshot", extension(format), SystemTime::now());
        app.set_timer(Duration::ZERO, move |app| {
            let chosen = with_sta(|| glint_sys::dialogs::save_image_dialog(hwnd(owner), &dir, &name, format));
            if let Some((path, format)) = chosen {
                app.with_view::<EditorView, _>(window, |view, cx| view.save_to(cx, path, format));
            }
        });
    }

    pub(super) fn save_to(&mut self, cx: &mut Ctx, path: PathBuf, format: ImageFormat) {
        self.ensure_worker(cx);
        let image = match self.export(cx.gfx()) {
            Ok(image) => image,
            Err(e) => {
                log::error!("export for save: {e:#}");
                self.toast(cx, "Couldn't save the image", ToastKind::Error);
                return;
            }
        };
        let Some(poster) = &self.poster else { return };
        let job = worker::track_save(self.host.as_ref().map(|h| h.saved.clone()));
        poster.spawn("glint-save", move || {
            let result = glint_core::encode::save(&image, &path, format).map(|()| path).map_err(|e| format!("{e:#}"));
            Message::Saved { job, result }
        });
    }

    pub(super) fn share(&mut self, cx: &mut Ctx) {
        self.ensure_worker(cx);
        let image = match self.export(cx.gfx()) {
            Ok(image) => image,
            Err(e) => {
                log::error!("export for share: {e:#}");
                self.toast(cx, "Couldn't share the image", ToastKind::Error);
                return;
            }
        };
        let Some(poster) = &self.poster else { return };
        let dir = unique_share_dir();
        let name = glint_sys::paths::timestamped_name("Screenshot", "png", SystemTime::now());
        poster.spawn("glint-share", move || {
            let path = dir.join(name);
            let written = std::fs::create_dir_all(&dir)
                .map_err(anyhow::Error::from)
                .and_then(|()| glint_core::encode::save(&image, &path, ImageFormat::Png));
            Message::ShareReady(written.map(|()| path).map_err(|e| format!("{e:#}")))
        });
    }

    pub(super) fn start_ocr(&mut self, cx: &mut Ctx) {
        if matches!(self.ocr, OcrState::Busy) {
            return;
        }
        if !matches!(self.ocr, OcrState::Off) {
            self.exit_ocr();
            return;
        }
        self.finish_text(cx);
        self.ensure_worker(cx);
        let image = match self.export(cx.gfx()) {
            Ok(image) => image,
            Err(e) => {
                log::error!("export for OCR: {e:#}");
                return;
            }
        };
        let Some(poster) = &self.poster else { return };
        self.ocr_job += 1;
        let job = self.ocr_job;
        poster.spawn("glint-ocr", move || Message::Ocr { job, result: glint_sys::ocr::ocr(&image).map_err(|e| format!("{e:#}")) });
        self.select(None);
        self.ocr = OcrState::Busy;
        cx.request_paint();
    }

    /// Shows recognized text over the canvas. Word rectangles come in export (crop) pixels.
    pub(crate) fn show_ocr(&mut self, result: &glint_sys::ocr::OcrResult) {
        let content = self.doc.content_rect(self.image_size());
        let lines = result.lines.iter().map(|l| l.words.iter().map(|w| (w.text.clone(), w.rect)).collect()).collect();
        let text = RecognizedText::new(lines, PointF::new(content.x as f32, content.y as f32));
        self.ocr = OcrState::Ready { text, selection: None };
    }

    pub(super) fn copy_ocr_text(&mut self, cx: &mut Ctx, all: bool) {
        let OcrState::Ready { text, selection } = &self.ocr else { return };
        let (copy, lines) = match (all, selection) {
            (false, Some((a, b))) => (text.text_of(*a, *b), text.lines_in(*a, *b)),
            _ => (text.all_text(), text.line_count),
        };
        if copy.is_empty() || cx.hwnd() == 0 {
            return;
        }
        match glint_sys::clipboard::copy_text(hwnd(cx.hwnd()), &copy) {
            Ok(()) => self.notify_copied_text(cx, lines),
            Err(e) => {
                log::error!("copy text: {e:#}");
                self.toast(cx, "Couldn't copy the text", ToastKind::Error);
            }
        }
    }

    pub(super) fn new_snip(&mut self, cx: &mut Ctx, mode: CaptureMode) {
        if let (Some(app), Some(host)) = (cx.app(), &self.host) {
            (host.new_snip)(app, mode, self.delay);
        }
    }

    pub(super) fn schedule_autocopy(&mut self, cx: &mut Ctx) {
        if self.settings.after_capture.copy_to_clipboard && cx.app().is_some() {
            self.set_view_timer(cx, TIMER_AUTOCOPY, 500);
        }
    }

    /// Snipping Tool's "automatically copy changes": once edits settle, the clipboard gets the new image.
    pub(super) fn autocopy(&mut self, cx: &mut Ctx) {
        if !matches!(self.gesture, Gesture::None) || self.editing.is_some() {
            self.schedule_autocopy(cx);
            return;
        }
        self.copy_unsynced(cx);
    }

    fn copy_unsynced(&mut self, cx: &mut Ctx) {
        if !self.unsynced_changes || cx.hwnd() == 0 {
            return;
        }
        match self.export(cx.gfx()).and_then(|image| glint_sys::clipboard::copy_image(hwnd(cx.hwnd()), &image)) {
            Ok(()) => self.unsynced_changes = false,
            Err(e) => log::warn!("auto-copy: {e:#}"),
        }
    }

    /// Asks the worker for a new base image when the tone-map parameters changed (latest request wins).
    pub(super) fn retone_if_needed(&mut self) {
        if self.doc.tone_map == self.requested_tone_map {
            return;
        }
        self.requested_tone_map = self.doc.tone_map;
        self.retone_requested += 1;
        if let Some(retoner) = &self.retoner {
            retoner.request(self.retone_requested, self.doc.tone_map);
        }
    }

    pub(super) fn replace_base(&mut self, image: Image) {
        self.base_image = Rc::new(image);
        self.base_layer = None;
        self.pixelated_image = None;
        self.pixelated_layer = None;
    }

    fn ensure_worker(&mut self, cx: &Ctx) {
        if self.poster.is_some() {
            return;
        }
        let (Some(app), Some(window)) = (cx.app(), cx.window()) else { return };
        let poster = Poster::new(app.proxy(), window);
        if let Some(hdr) = &self.hdr {
            self.retoner = Some(Retoner::start(hdr.clone(), poster.clone()));
        }
        self.poster = Some(poster);
    }

    pub(super) fn on_shown(&mut self, cx: &mut Ctx) {
        self.ensure_worker(cx);
        prune_shares(SHARE_RETENTION);
        cx.request_paint();
    }

    /// Commits typing and flushes a pending auto-copy so closing right after an edit still updates the clipboard.
    pub(super) fn on_closed(&mut self, cx: &mut Ctx) {
        self.finish_text(cx);
        if self.gesture_pointer.is_some() || !matches!(self.gesture, Gesture::None) {
            self.end_gesture(cx);
        }
        if self.cancel_view_timer(cx, TIMER_AUTOCOPY) {
            self.copy_unsynced(cx);
        }
        tools::remember(self.tool, &self.options);
        self.retoner = None;
        self.ocr_job += 1;
        if cx.hwnd() != 0 {
            glint_sys::shell::release_share_window(hwnd(cx.hwnd()));
            prune_shares(SHARE_RETENTION);
        }
    }

    pub(crate) fn on_message(&mut self, cx: &mut Ctx, message: Message) {
        match message {
            Message::HdrStats(stats) => self.hdr_stats = Some(stats),
            Message::Retoned { generation, image } => {
                if generation > self.retone_applied {
                    self.retone_applied = generation;
                    self.replace_base(image);
                    if self.retone_applied == self.retone_requested {
                        self.schedule_autocopy(cx);
                    }
                }
            }
            Message::Ocr { job, .. } if job != self.ocr_job || !matches!(self.ocr, OcrState::Busy) => {}
            Message::Ocr { result: Ok(result), .. } => {
                self.show_ocr(&result);
                if let OcrState::Ready { text, .. } = &self.ocr
                    && text.is_empty()
                {
                    self.toast(cx, "No text found", ToastKind::Info);
                }
            }
            Message::Ocr { result: Err(e), .. } => {
                log::warn!("OCR: {e}");
                self.exit_ocr();
                self.toast(cx, "Text recognition isn't available", ToastKind::Error);
            }
            Message::Saved { result: Ok(path), .. } => {
                let folder = path.parent().and_then(Path::file_name).map(|f| f.to_string_lossy().into_owned());
                self.saved_path = Some(path);
                self.toast(cx, &format!("Saved to {}", folder.as_deref().unwrap_or("disk")), ToastKind::Done);
            }
            Message::Saved { result: Err(e), .. } => {
                log::error!("save: {e}");
                self.toast(cx, "Couldn't save the image", ToastKind::Error);
            }
            Message::ShareReady(Ok(path)) => {
                if let Err(e) = glint_sys::shell::share_files(hwnd(cx.hwnd()), &[path]) {
                    log::error!("share: {e:#}");
                    self.toast(cx, "Sharing isn't available", ToastKind::Error);
                }
            }
            Message::ShareReady(Err(e)) => {
                log::error!("share export: {e}");
                self.toast(cx, "Couldn't share the image", ToastKind::Error);
            }
        }
        cx.request_paint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_follow_extensions() {
        assert_eq!(format_for(Path::new("a/Shot.JPG"), ImageFormat::Png), ImageFormat::Jpeg);
        assert_eq!(format_for(Path::new("a/Shot.png"), ImageFormat::Jpeg), ImageFormat::Png);
        assert_eq!(format_for(Path::new("a/Shot"), ImageFormat::Jpeg), ImageFormat::Jpeg);
        assert_eq!(extension(ImageFormat::Jpeg), "jpg");
    }
}
