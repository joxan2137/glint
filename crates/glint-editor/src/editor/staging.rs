//! Direct state setup for offscreen previews (no input events, no animation in flight).

use glint_core::{HdrStats, PointF, RectF, SizeF};
use glint_ui::Gfx;

use super::{CropSession, EditorView, OcrState, TextEditing};
use crate::chrome::ToastKind;
use crate::model::Document;
use crate::ocr_text::RecognizedText;
use crate::tools::{Tool, ToolOptions};

impl EditorView {
    pub(crate) fn stage_layout(&mut self, gfx: &Gfx, size: SizeF, scale: f32) {
        self.prepare(gfx, size, scale, None);
    }

    pub(crate) fn stage_image_scale(&mut self, scale: f32) {
        self.image_scale = scale;
    }

    pub(crate) fn doc_mut(&mut self) -> &mut Document {
        &mut self.doc
    }

    pub(crate) fn stage_tool(&mut self, tool: Tool, options: impl FnOnce(&mut ToolOptions)) {
        self.tool = tool;
        options(&mut self.options[tool.index()]);
    }

    pub(crate) fn stage_selection(&mut self, id: Option<u64>) {
        self.selection = id;
        self.handles.snap(if id.is_some() { 1.0 } else { 0.0 });
    }

    pub(crate) fn stage_editing(&mut self, id: u64, caret: usize) {
        self.editing = Some(TextEditing { id, caret });
        self.caret_on = true;
    }

    pub(crate) fn stage_crop(&mut self, rect: RectF, dragging: bool) {
        let mut session = CropSession {
            rect,
            shown: glint_ui::Animated::new(rect),
            grid: glint_ui::Animated::fade(0.0),
            previous_tool: self.tool,
        };
        session.grid.snap(if dragging { 1.0 } else { 0.0 });
        self.crop = Some(session);
        self.tool = Tool::Crop;
    }

    pub(crate) fn stage_ocr(&mut self, text: RecognizedText, selection: Option<(usize, usize)>) {
        self.ocr = OcrState::Ready { text, selection };
    }

    pub(crate) fn stage_hdr(&mut self, stats: HdrStats) {
        self.hdr_stats = Some(stats);
    }

    pub(crate) fn stage_history(&mut self, before: &Document) {
        self.history.record(before);
    }

    pub(crate) fn stage_hover(&mut self, pos: PointF) {
        self.hover = Some(pos);
    }

    pub(crate) fn stage_toast(&mut self, text: &str, kind: ToastKind) {
        self.chrome.show_toast(text, kind);
        self.chrome.snap_toast();
    }

    pub(crate) fn stage_menu(&mut self, gfx: &Gfx, highlighted: Option<usize>) {
        self.chrome.force_menu(gfx, highlighted);
    }

    pub(crate) fn stage_hdr_popover(&mut self) {
        self.chrome.force_hdr_popover();
    }

    pub(crate) fn stage_picker(&mut self) {
        self.chrome.force_picker();
    }

    pub(crate) fn stage_refit(&mut self) {
        self.refit(false);
    }
}
