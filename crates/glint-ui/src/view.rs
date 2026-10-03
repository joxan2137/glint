//! The `View` trait and its context.

use std::any::Any;
use std::rc::Rc;
use std::time::Duration;

use glint_core::{PointF, PointI, RectF, RectI, SizeF};

use crate::app::{App, TimerId, WindowId};
use crate::cursor::Cursor;
use crate::event::{Event, HitArea};
use crate::gfx::Gfx;
use crate::painter::Painter;
use crate::text::TextStyle;
use crate::theme::Theme;

/// A window's content: handles events and paints in DIPs.
///
/// Views are owned by their window and live on the UI thread. Call `Ctx` methods to request repaints, change the
/// cursor, operate on the window or post events to the app; window operations are applied right after the call
/// returns, so they never re-enter the view.
pub trait View: Any {
    /// Returns true when the event was consumed. Unconsumed system keys (Alt+F4, Alt+Space) reach Windows.
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        let _ = (cx, event);
        false
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter);

    /// For windows with a custom title bar: which client points drag the window.
    fn hit_test(&self, pos: PointF) -> HitArea {
        let _ = pos;
        HitArea::Client
    }

    /// Popups only: the client rects (DIP) that take mouse input. Everywhere else — shadow margins, room reserved
    /// for tooltips — clicks pass through to whatever window is below, including other processes. `None` (the
    /// default) makes the whole window interactive; `Some(vec![])` makes it fully click-through for now.
    fn interactive_region(&self) -> Option<Vec<RectF>> {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WindowOp {
    Close,
    SetRectPx(RectI),
    MoveToPx(PointI),
    ResizeDip(SizeF),
    Show { activate: bool },
    Hide,
    RenderHidden,
    Activate,
    SetOpacity(f32),
    SetTopmost(bool),
    SetTitle(String),
    Minimize,
    ToggleMaximize,
    CapturePointer,
    ReleasePointer,
    ShowTooltip { anchor: RectF, text: String, shortcut: Option<String> },
    HideTooltip { anchor: Option<RectF> },
    SetImeCaret(RectF),
}

#[derive(Default)]
pub(crate) struct CtxOutput {
    pub paint: bool,
    pub animate: bool,
    pub cursor: Option<Cursor>,
    pub ops: Vec<WindowOp>,
}

/// Per-call context handed to views.
pub struct Ctx {
    gfx: Rc<Gfx>,
    app: Option<App>,
    window: Option<WindowId>,
    hwnd: isize,
    size: SizeF,
    scale: f32,
    theme: Theme,
    time: f64,
    pub(crate) out: CtxOutput,
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        gfx: Rc<Gfx>,
        app: Option<App>,
        window: Option<WindowId>,
        hwnd: isize,
        size: SizeF,
        scale: f32,
        theme: Theme,
        time: f64,
    ) -> Self {
        Self { gfx, app, window, hwnd, size, scale, theme, time, out: CtxOutput::default() }
    }

    /// A context with no window, for offscreen rendering and tests. Window operations are ignored.
    pub fn offscreen(gfx: Rc<Gfx>, size: SizeF, scale: f32, theme: Theme, time: f64) -> Self {
        Self::new(gfx, None, None, 0, size, scale, theme, time)
    }

    pub fn gfx(&self) -> &Rc<Gfx> {
        &self.gfx
    }

    /// The running app; None offscreen.
    pub fn app(&self) -> Option<&App> {
        self.app.as_ref()
    }

    pub fn window(&self) -> Option<WindowId> {
        self.window
    }

    /// Raw HWND (0 offscreen).
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    pub fn is_offscreen(&self) -> bool {
        self.window.is_none()
    }

    /// Client size in DIP.
    pub fn size(&self) -> SizeF {
        self.size
    }

    pub fn bounds(&self) -> RectF {
        RectF::new(0.0, 0.0, self.size.w, self.size.h)
    }

    /// Physical pixels per DIP.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// One physical pixel in DIP.
    pub fn px(&self) -> f32 {
        1.0 / self.scale
    }

    pub fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Seconds on the frame clock (same value `Animated` uses).
    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn request_paint(&mut self) {
        self.out.paint = true;
    }

    /// Requests another frame after this one (continuous animations such as a pulsing dot). `Animated` values
    /// request frames automatically.
    pub fn animate(&mut self) {
        self.out.animate = true;
        self.out.paint = true;
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.out.cursor = Some(cursor);
    }

    /// Mouse capture. Primary-button presses capture automatically until all buttons are released.
    pub fn capture_pointer(&mut self) {
        self.out.ops.push(WindowOp::CapturePointer);
    }

    pub fn release_pointer(&mut self) {
        self.out.ops.push(WindowOp::ReleasePointer);
    }

    pub fn close(&mut self) {
        self.out.ops.push(WindowOp::Close);
    }

    /// Moves and resizes the window in physical pixels (virtual-desktop coordinates).
    pub fn set_window_rect_px(&mut self, rect: RectI) {
        self.out.ops.push(WindowOp::SetRectPx(rect));
    }

    pub fn move_window_px(&mut self, origin: PointI) {
        self.out.ops.push(WindowOp::MoveToPx(origin));
    }

    /// Resizes the client area to `size` DIP at the window's current scale.
    pub fn resize_window(&mut self, size: SizeF) {
        self.out.ops.push(WindowOp::ResizeDip(size));
    }

    pub fn show_window(&mut self, activate: bool) {
        self.out.ops.push(WindowOp::Show { activate });
    }

    pub fn hide_window(&mut self) {
        self.out.ops.push(WindowOp::Hide);
    }

    /// Renders one frame while the window stays hidden: creates its swapchain and warms its render target, so a later
    /// `show_window` only paints and presents (windows kept ready for instant display).
    pub fn render_hidden(&mut self) {
        self.out.ops.push(WindowOp::RenderHidden);
    }

    pub fn activate(&mut self) {
        self.out.ops.push(WindowOp::Activate);
    }

    /// Whole-window opacity through the composition visual (cheap; no repaint).
    pub fn set_window_opacity(&mut self, opacity: f32) {
        self.out.ops.push(WindowOp::SetOpacity(opacity));
    }

    pub fn set_topmost(&mut self, topmost: bool) {
        self.out.ops.push(WindowOp::SetTopmost(topmost));
    }

    pub fn set_title(&mut self, title: &str) {
        self.out.ops.push(WindowOp::SetTitle(title.to_string()));
    }

    pub fn minimize(&mut self) {
        self.out.ops.push(WindowOp::Minimize);
    }

    pub fn toggle_maximize(&mut self) {
        self.out.ops.push(WindowOp::ToggleMaximize);
    }

    /// Queues `event` for the app's handler registered with `App::on_event::<E>` (delivered after this call).
    pub fn post<E: Any + Send>(&mut self, event: E) {
        if let Some(app) = &self.app {
            app.post(event);
        }
    }

    /// Delivers `Event::Timer(token)` to this view after `delay`.
    pub fn set_timer(&mut self, delay: Duration, token: u64) -> TimerId {
        match (&self.app, self.window) {
            (Some(app), Some(window)) => app.set_view_timer(window, delay, token),
            _ => TimerId::NONE,
        }
    }

    pub fn cancel_timer(&mut self, id: TimerId) {
        if let Some(app) = &self.app {
            app.cancel_timer(id);
        }
    }

    /// Shows a tooltip near `anchor` after the 500 ms hover delay (immediately if one was just visible).
    pub fn show_tooltip(&mut self, anchor: RectF, text: &str, shortcut: Option<&str>) {
        self.out.ops.push(WindowOp::ShowTooltip {
            anchor,
            text: text.to_string(),
            shortcut: shortcut.map(str::to_string),
        });
    }

    pub fn hide_tooltip(&mut self) {
        self.out.ops.push(WindowOp::HideTooltip { anchor: None });
    }

    /// Hides the tooltip only if it belongs to `anchor` (so moving between adjacent buttons never hides the new
    /// one).
    pub fn hide_tooltip_for(&mut self, anchor: RectF) {
        self.out.ops.push(WindowOp::HideTooltip { anchor: Some(anchor) });
    }

    /// Positions the IME candidate window at the text caret (DIP rect).
    pub fn set_ime_caret(&mut self, caret: RectF) {
        self.out.ops.push(WindowOp::SetImeCaret(caret));
    }

    pub fn measure_text(&self, text: &str, style: &TextStyle) -> SizeF {
        self.gfx.measure_text(text, style)
    }

    /// Window rect in physical pixels; None offscreen.
    pub fn window_rect_px(&self) -> Option<RectI> {
        (self.hwnd != 0).then(|| crate::win::window_rect_px(self.hwnd)).flatten()
    }

    /// Client-area origin on the virtual desktop in physical pixels; None offscreen.
    pub fn client_origin_px(&self) -> Option<PointI> {
        (self.hwnd != 0).then(|| crate::win::client_origin_px(self.hwnd)).flatten()
    }

    /// Where Windows draws the caption buttons in a custom-title-bar window (DIP client coordinates).
    pub fn caption_buttons_rect(&self) -> Option<RectF> {
        if self.hwnd == 0 {
            return None;
        }
        crate::win::caption_buttons_rect_px(self.hwnd).map(|r| r.to_f().scale(1.0 / self.scale))
    }
}
