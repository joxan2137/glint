//! Native windows: DirectComposition flip-model swapchains, WM_POINTER input, DPI handling, custom title bars.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result};
use glint_core::{PointF, PointI, RectF, RectI, SizeF, ThemeMode};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, ID2D1Bitmap1, ID2D1Image,
};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual2, IDCompositionVisual3};
use windows::Win32::Graphics::Dwm::DwmDefWindowProc;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISurface, IDXGISwapChain2,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::Ime::{CFS_POINT, COMPOSITIONFORM, ImmGetContext, ImmReleaseContext, ImmSetCompositionWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Input::Pointer::{
    GetPointerInfo, GetPointerInfoHistory, GetPointerPenInfo, GetPointerPenInfoHistory, POINTER_FLAG_FIFTHBUTTON,
    POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_FOURTHBUTTON, POINTER_FLAG_SECONDBUTTON, POINTER_FLAG_THIRDBUTTON,
    POINTER_INFO, POINTER_PEN_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, DefWindowProcW, DestroyWindow, GetClientRect, GetSystemMetrics, GetWindowRect,
    HTCAPTION, HTCLIENT, HTTOP, HTTOPLEFT, HTTOPRIGHT, HTTRANSPARENT, IsZoomed, LWA_ALPHA, MA_NOACTIVATE,
    MINMAXINFO, NCCALCSIZE_PARAMS, PA_NOACTIVATE, PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED,
    PEN_MASK_PRESSURE, PT_MOUSE, PT_PEN, PT_TOUCH, PT_TOUCHPAD, SM_CXDOUBLECLK, SM_CXPADDEDBORDER, SM_CYDOUBLECLK,
    SM_CYFRAME, SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetCursor, SetLayeredWindowAttributes,
    SetWindowPos, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_CHAR, WM_CLOSE,
    WM_DESTROY, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_MOUSEACTIVATE, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT,
    WM_POINTERACTIVATE, WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERHWHEEL, WM_POINTERLEAVE, WM_POINTERUP,
    WM_POINTERUPDATE, WM_POINTERWHEEL, WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_THICKFRAME, WS_MAXIMIZEBOX,
};
use windows::core::{HSTRING, Interface, PCWSTR, w};

use crate::anim::{self, precise_time};
use crate::app::{App, WM_APP_CLOSE, WindowId};
use crate::cursor::Cursor;
use crate::event::{Buttons, Event, HitArea, Key, KeyEvent, Modifiers, MouseButton, PointerEvent, PointerKind, PointerSample, WheelEvent};
use crate::gfx::is_device_lost;
use crate::painter::Painter;
use crate::theme::Theme;
use crate::view::{Ctx, CtxOutput, View, WindowOp};
use crate::widgets::Tooltip;
use crate::win;

pub(crate) const WINDOW_CLASS: PCWSTR = w!("GlintUi.Window");
const WM_MOUSELEAVE: u32 = 0x02A3;
const WORK_AREA_MARGIN: f32 = 8.0;

#[derive(Clone, Debug, PartialEq)]
pub enum WindowKind {
    /// Borderless topmost window exactly covering a monitor rect (physical px). No taskbar button.
    Overlay { rect_px: RectI },
    /// Resizable app window with Mica, system theme title bar and rounded corners.
    Normal {
        /// Client size in DIP.
        size: SizeF,
        min_size: SizeF,
        /// Center the window in this physical-px rect (usually a monitor work area).
        center_in_px: Option<RectI>,
        /// Extend content into the title bar; the view reports caption regions via `View::hit_test` and keeps the
        /// system caption buttons (top-right) clear.
        unified_title_bar: bool,
        mica: bool,
        resizable: bool,
    },
    /// Borderless per-pixel transparent window at a physical-px origin with a DIP size.
    Popup { origin_px: PointI, size: SizeF, topmost: bool, activate: bool, click_through: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowSpec {
    pub kind: WindowKind,
    pub title: String,
    /// `System` follows the app appearance (`App::set_appearance`).
    pub theme: ThemeMode,
    /// Show after the first frame; false keeps the window hidden until `Ctx::show_window`.
    pub visible: bool,
    pub exclude_from_capture: bool,
}

impl WindowSpec {
    /// Always dark, activated on show.
    pub fn overlay(rect_px: RectI) -> Self {
        Self {
            kind: WindowKind::Overlay { rect_px },
            title: "Glint".into(),
            theme: ThemeMode::Dark,
            visible: true,
            exclude_from_capture: false,
        }
    }

    pub fn normal(title: &str, size: SizeF) -> Self {
        Self {
            kind: WindowKind::Normal {
                size,
                min_size: SizeF::new(320.0, 200.0),
                center_in_px: None,
                unified_title_bar: false,
                mica: true,
                resizable: true,
            },
            title: title.into(),
            theme: ThemeMode::System,
            visible: true,
            exclude_from_capture: false,
        }
    }

    /// Topmost, non-activating by default.
    pub fn popup(origin_px: PointI, size: SizeF) -> Self {
        Self {
            kind: WindowKind::Popup { origin_px, size, topmost: true, activate: false, click_through: false },
            title: "Glint".into(),
            theme: ThemeMode::System,
            visible: true,
            exclude_from_capture: false,
        }
    }

    pub fn theme(mut self, theme: ThemeMode) -> Self {
        self.theme = theme;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn exclude_from_capture(mut self) -> Self {
        self.exclude_from_capture = true;
        self
    }

    pub fn min_size(mut self, min: SizeF) -> Self {
        if let WindowKind::Normal { min_size, .. } = &mut self.kind {
            *min_size = min;
        }
        self
    }

    pub fn centered_in(mut self, rect_px: RectI) -> Self {
        if let WindowKind::Normal { center_in_px, .. } = &mut self.kind {
            *center_in_px = Some(rect_px);
        }
        self
    }

    pub fn unified_title_bar(mut self) -> Self {
        if let WindowKind::Normal { unified_title_bar, .. } = &mut self.kind {
            *unified_title_bar = true;
        }
        self
    }

    pub fn mica(mut self, enabled: bool) -> Self {
        if let WindowKind::Normal { mica, .. } = &mut self.kind {
            *mica = enabled;
        }
        self
    }

    pub fn fixed_size(mut self) -> Self {
        if let WindowKind::Normal { resizable, .. } = &mut self.kind {
            *resizable = false;
        }
        self
    }

    pub fn topmost(mut self, on: bool) -> Self {
        if let WindowKind::Popup { topmost, .. } = &mut self.kind {
            *topmost = on;
        }
        self
    }

    /// Popups: take focus when shown and clicked.
    pub fn activating(mut self) -> Self {
        if let WindowKind::Popup { activate, .. } = &mut self.kind {
            *activate = true;
        }
        self
    }

    /// Popups: mouse input passes through to whatever is below.
    pub fn click_through(mut self) -> Self {
        if let WindowKind::Popup { click_through, .. } = &mut self.kind {
            *click_through = true;
        }
        self
    }

    fn unified(&self) -> bool {
        matches!(self.kind, WindowKind::Normal { unified_title_bar: true, .. })
    }

    fn activates(&self) -> bool {
        match self.kind {
            WindowKind::Popup { activate, .. } => activate,
            _ => true,
        }
    }

    fn click_through_enabled(&self) -> bool {
        matches!(self.kind, WindowKind::Popup { click_through: true, .. })
    }
}

struct Surface {
    generation: u64,
    swapchain: IDXGISwapChain2,
    target: Option<ID2D1Bitmap1>,
    waitable: HANDLE,
    width: u32,
    height: u32,
    _dcomp_target: IDCompositionTarget,
    visual: IDCompositionVisual2,
}

impl Drop for Surface {
    fn drop(&mut self) {
        if !self.waitable.is_invalid() {
            // SAFETY: we own the waitable handle returned by GetFrameLatencyWaitableObject.
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.waitable) };
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ClickTracker {
    time: f64,
    screen: PointI,
    button: Option<MouseButton>,
    count: u32,
}

pub(crate) struct WindowState {
    pub(crate) id: WindowId,
    hwnd: Cell<isize>,
    pub(crate) spec: WindowSpec,
    view: RefCell<Box<dyn View>>,
    surface: RefCell<Option<Surface>>,
    dpi: Cell<u32>,
    client_px: Cell<(u32, u32)>,
    theme: RefCell<Theme>,
    pub(crate) needs_frame: Cell<bool>,
    presented: Cell<bool>,
    show_pending: Cell<Option<bool>>,
    hidden: Cell<bool>,
    cursor: Cell<Cursor>,
    hovering: Cell<bool>,
    buttons_down: Cell<bool>,
    releasing_capture: Cell<bool>,
    tooltip: RefCell<Tooltip>,
    clicks: Cell<ClickTracker>,
    high_surrogate: Cell<Option<u16>>,
    opacity: Cell<f32>,
    hit_polling: Cell<bool>,
    passthrough: Cell<bool>,
}

impl WindowState {
    pub(crate) fn hwnd(&self) -> HWND {
        HWND(self.hwnd.get() as *mut _)
    }

    pub(crate) fn set_hwnd(&self, hwnd: HWND) {
        self.hwnd.set(hwnd.0 as isize);
    }

    pub(crate) fn wants_render(&self) -> bool {
        self.needs_frame.get() && (!self.hidden.get() || self.show_pending.get().is_some())
    }

    fn scale(&self) -> f32 {
        self.dpi.get() as f32 / 96.0
    }

    fn size_dip(&self) -> SizeF {
        let (w, h) = self.client_px.get();
        SizeF::new(w as f32 / self.scale(), h as f32 / self.scale())
    }

    fn ctx(&self, app: &App, time: f64) -> Ctx {
        Ctx::new(
            app.gfx(),
            Some(app.clone()),
            Some(self.id),
            self.hwnd.get(),
            self.size_dip(),
            self.scale(),
            self.theme.borrow().clone(),
            time,
        )
    }
}

fn style_for(spec: &WindowSpec) -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
    match &spec.kind {
        WindowKind::Overlay { .. } => (WS_POPUP, WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP),
        WindowKind::Normal { resizable, .. } => {
            let style = if *resizable { WS_OVERLAPPEDWINDOW } else { WS_OVERLAPPEDWINDOW & !(WS_THICKFRAME | WS_MAXIMIZEBOX) };
            (style, WS_EX_APPWINDOW | WS_EX_NOREDIRECTIONBITMAP)
        }
        WindowKind::Popup { topmost, activate, click_through, .. } => {
            let mut ex = WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED;
            if *topmost {
                ex |= WS_EX_TOPMOST;
            }
            if !*activate {
                ex |= WS_EX_NOACTIVATE;
            }
            if *click_through {
                ex |= WS_EX_TRANSPARENT;
            }
            (WS_POPUP, ex)
        }
    }
}

fn frame_metrics(style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, dpi: u32) -> RECT {
    let mut r = RECT::default();
    // SAFETY: out-pointer valid.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi) };
    r
}

/// Outer window rect of `size` (clamped between `min` and the work area minus `margin`) centered in `area`, with
/// its top-left kept inside the area even when `min` does not fit.
pub(crate) fn fit_in_work_area(size: (i32, i32), min: (i32, i32), area: RectI, margin: i32) -> RectI {
    let fit = |want: i32, min: i32, available: i32| want.min(available - 2 * margin).max(min);
    let w = fit(size.0, min.0, area.w);
    let h = fit(size.1, min.1, area.h);
    let x = (area.x + (area.w - w) / 2).max(area.x);
    let y = (area.y + (area.h - h) / 2).max(area.y);
    RectI::new(x, y, w, h)
}

fn initial_rect(spec: &WindowSpec, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE) -> (RectI, u32) {
    match &spec.kind {
        WindowKind::Overlay { rect_px } => (*rect_px, win::dpi_for_rect(*rect_px)),
        WindowKind::Popup { origin_px, size, .. } => {
            let dpi = win::dpi_at_point(*origin_px);
            let scale = dpi as f32 / 96.0;
            let rect = RectI::new(origin_px.x, origin_px.y, (size.w * scale).round() as i32, (size.h * scale).round() as i32);
            (rect, dpi)
        }
        WindowKind::Normal { size, min_size, center_in_px, unified_title_bar, .. } => {
            let dpi = center_in_px.map(win::dpi_for_rect).unwrap_or_else(|| win::dpi_at_point(PointI::new(0, 0)));
            let scale = dpi as f32 / 96.0;
            let frame = frame_metrics(style, ex, dpi);
            let top = if *unified_title_bar { 0 } else { -frame.top };
            let extra_w = frame.right - frame.left;
            let extra_h = top + frame.bottom;
            let w = (size.w * scale).round() as i32 + extra_w;
            let h = (size.h * scale).round() as i32 + extra_h;
            match center_in_px {
                Some(area) => {
                    let min = (
                        (min_size.w * scale).round() as i32 + extra_w,
                        (min_size.h * scale).round() as i32 + extra_h,
                    );
                    let margin = (WORK_AREA_MARGIN * scale).round() as i32;
                    (fit_in_work_area((w, h), min, *area, margin), dpi)
                }
                None => (RectI::new(CW_USEDEFAULT, CW_USEDEFAULT, w, h), dpi),
            }
        }
    }
}

pub(crate) fn open(app: &App, spec: WindowSpec, view: Box<dyn View>) -> Result<WindowId> {
    let id = WindowId(app.next_id());
    let theme = app.theme_for(spec.theme);
    let (style, ex) = style_for(&spec);
    let (rect, dpi) = initial_rect(&spec, style, ex);
    let state = Rc::new(WindowState {
        id,
        hwnd: Cell::new(0),
        spec: spec.clone(),
        view: RefCell::new(view),
        surface: RefCell::new(None),
        dpi: Cell::new(dpi),
        client_px: Cell::new((0, 0)),
        theme: RefCell::new(theme.clone()),
        needs_frame: Cell::new(true),
        presented: Cell::new(false),
        show_pending: Cell::new(spec.visible.then_some(spec.activates())),
        hidden: Cell::new(true),
        cursor: Cell::new(Cursor::Arrow),
        hovering: Cell::new(false),
        buttons_down: Cell::new(false),
        releasing_capture: Cell::new(false),
        tooltip: RefCell::new(Tooltip::new()),
        clicks: Cell::new(ClickTracker::default()),
        high_surrogate: Cell::new(None),
        opacity: Cell::new(1.0),
        hit_polling: Cell::new(false),
        passthrough: Cell::new(spec.click_through_enabled()),
    });
    *app.0.creating.borrow_mut() = Some(state.clone());
    // SAFETY: creates a window of our registered class; the state is registered by the first message.
    let created = unsafe {
        let instance = GetModuleHandleW(None)?;
        CreateWindowExW(ex, WINDOW_CLASS, &HSTRING::from(spec.title.as_str()), style, rect.x, rect.y, rect.w, rect.h, None, None, Some(instance.into()), None)
    };
    app.0.creating.borrow_mut().take();
    let hwnd = created.context("CreateWindowExW")?;
    if state.hwnd().is_invalid() {
        state.set_hwnd(hwnd);
        app.0.windows.borrow_mut().push(state.clone());
    }
    let raw = hwnd.0 as isize;
    state.dpi.set(win::dpi_for_window(raw));
    match &spec.kind {
        WindowKind::Normal { mica, unified_title_bar, .. } => {
            win::set_dark_mode(raw, theme.is_dark());
            win::set_corner_preference(raw, win::CornerPreference::Round);
            if *mica || *unified_title_bar {
                win::extend_frame_into_client(raw);
            }
            if *mica {
                win::set_mica(raw, true);
            }
            if *unified_title_bar {
                // SAFETY: recalculates the frame of our own window.
                let _ = unsafe { SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
            }
        }
        WindowKind::Overlay { .. } | WindowKind::Popup { .. } => {
            win::set_corner_preference(raw, win::CornerPreference::Square);
            win::disable_transitions(raw);
        }
    }
    if matches!(spec.kind, WindowKind::Popup { .. }) {
        // SAFETY: layered attributes on our own window; fully opaque, content comes from DirectComposition.
        let _ = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) };
    }
    if spec.exclude_from_capture {
        win::set_exclude_from_capture(raw, true);
    }
    update_client_size(&state);
    app.request_frame(&state);
    Ok(id)
}

fn update_client_size(win: &WindowState) -> bool {
    let mut r = RECT::default();
    // SAFETY: out-pointer valid.
    let _ = unsafe { GetClientRect(win.hwnd(), &mut r) };
    let size = ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32);
    let changed = size != win.client_px.get();
    win.client_px.set(size);
    changed
}

fn create_surface(app: &App, win: &WindowState, width: u32, height: u32) -> Result<Surface> {
    let gfx = app.gfx();
    let devices = gfx.devices();
    let dcomp = gfx.dcomp()?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
    };
    // SAFETY: COM calls on live devices; the swapchain is bound to our window through DirectComposition.
    unsafe {
        let swapchain: IDXGISwapChain2 = devices.dxgi_factory.CreateSwapChainForComposition(&devices.d3d, &desc, None)?.cast()?;
        swapchain.SetMaximumFrameLatency(2)?;
        let waitable = swapchain.GetFrameLatencyWaitableObject();
        let target = dcomp.CreateTargetForHwnd(win.hwnd(), true)?;
        let visual = dcomp.CreateVisual()?;
        visual.SetContent(&swapchain)?;
        if let Ok(v3) = visual.cast::<IDCompositionVisual3>() {
            let _ = v3.SetOpacity2(win.opacity.get());
        }
        target.SetRoot(&visual)?;
        dcomp.Commit()?;
        Ok(Surface { generation: gfx.generation(), swapchain, target: None, waitable, width, height, _dcomp_target: target, visual })
    }
}

fn surface_target(app: &App, surface: &mut Surface, dpi: f32) -> Result<ID2D1Bitmap1> {
    if let Some(target) = &surface.target {
        return Ok(target.clone());
    }
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: dpi,
        dpiY: dpi,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    // SAFETY: wraps the swapchain's current back buffer.
    let target = unsafe {
        let buffer: IDXGISurface = surface.swapchain.GetBuffer(0)?;
        app.gfx().context().CreateBitmapFromDxgiSurface(&buffer, Some(&props))?
    };
    surface.target = Some(target.clone());
    Ok(target)
}

fn ensure_surface(app: &App, win: &WindowState) -> Result<bool> {
    let (width, height) = win.client_px.get();
    if width == 0 || height == 0 {
        return Ok(false);
    }
    let generation = app.gfx().generation();
    let mut slot = win.surface.borrow_mut();
    let stale = slot.as_ref().is_some_and(|s| s.generation != generation);
    if slot.is_none() || stale {
        *slot = None;
        *slot = Some(create_surface(app, win, width, height)?);
    }
    let surface = slot.as_mut().expect("surface exists");
    if surface.width != width || surface.height != height {
        surface.target = None;
        // SAFETY: the context no longer references the old back buffer.
        unsafe {
            app.gfx().context().SetTarget(None::<&ID2D1Image>);
            surface.swapchain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0),
            )?;
        }
        surface.width = width;
        surface.height = height;
    }
    Ok(true)
}

/// Renders one frame if the window is ready; keeps `needs_frame` set while anything animates.
pub(crate) fn render(app: &App, win: &Rc<WindowState>, time: f64) {
    if let Err(error) = try_render(app, win, time) {
        let gfx = app.gfx();
        let lost = error.downcast_ref::<windows::core::Error>().is_some_and(|e| is_device_lost(e.code())) || gfx.device_lost();
        if lost {
            if let Err(e) = gfx.recover() {
                log::error!("device recovery failed: {e:#}");
            }
            win.needs_frame.set(true);
        } else {
            log::error!("render failed: {error:#}");
            win.needs_frame.set(false);
        }
    }
}

fn try_render(app: &App, win: &Rc<WindowState>, time: f64) -> Result<()> {
    if !ensure_surface(app, win)? {
        win.needs_frame.set(false);
        return Ok(());
    }
    {
        let surface = win.surface.borrow();
        let surface = surface.as_ref().expect("surface exists");
        // SAFETY: polling our own waitable handle.
        if !surface.waitable.is_invalid() && unsafe { WaitForSingleObjectEx(surface.waitable, 0, false) } != WAIT_OBJECT_0 {
            return Ok(());
        }
    }
    let Ok(mut view) = win.view.try_borrow_mut() else { return Ok(()) };
    let gfx = app.gfx();
    let context = gfx.context();
    let dpi = win.dpi.get() as f32;
    let target = {
        let mut surface = win.surface.borrow_mut();
        surface_target(app, surface.as_mut().expect("surface exists"), dpi)?
    };
    anim::set_clock(time);
    anim::take_motion_pending();
    let mut cx = win.ctx(app, time);
    let theme = win.theme.borrow().clone();
    // SAFETY: drawing on the shared context between BeginDraw/EndDraw on this thread.
    let end = unsafe {
        context.SetTarget(&target);
        context.SetDpi(dpi, dpi);
        context.BeginDraw();
        context.Clear(None);
        {
            let mut painter = Painter::new(&gfx, context.clone(), win.scale(), &theme, win.size_dip());
            view.paint(&mut cx, &mut painter);
            win.tooltip.borrow_mut().paint(&mut painter);
        }
        let end = context.EndDraw(None, None);
        context.SetTarget(None::<&ID2D1Image>);
        end
    };
    drop(view);
    let animating = anim::take_motion_pending() || cx.out.animate;
    let output = std::mem::take(&mut cx.out);
    end?;
    {
        let surface = win.surface.borrow();
        let surface = surface.as_ref().expect("surface exists");
        // SAFETY: presenting our own swapchain.
        unsafe { surface.swapchain.Present(1, DXGI_PRESENT(0)).ok()? };
    }
    win.needs_frame.set(animating);
    if !win.presented.replace(true) {
        let dcomp = gfx.dcomp()?;
        // SAFETY: committing and waiting on our own composition device.
        unsafe {
            dcomp.Commit()?;
            let _ = dcomp.WaitForCommitCompletion();
        }
    }
    apply_output(app, win, output);
    if let Some(activate) = win.show_pending.take() {
        show(win, activate);
        dispatch(app, win, &Event::Shown);
    }
    ensure_hit_polling(app, win);
    Ok(())
}

fn show(win: &WindowState, activate: bool) {
    win.hidden.set(false);
    let hwnd = win.hwnd();
    // SAFETY: plain calls on our own window.
    unsafe {
        if activate {
            let _ = ShowWindow(hwnd, SW_SHOW);
            win::force_foreground(win.hwnd.get());
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

/// Sends an event to the view and applies its requests. Returns whether the view consumed it.
pub(crate) fn dispatch(app: &App, win: &Rc<WindowState>, event: &Event) -> bool {
    let Ok(mut view) = win.view.try_borrow_mut() else { return false };
    let now = precise_time();
    anim::set_clock(now);
    anim::take_motion_pending();
    let mut cx = win.ctx(app, now);
    let handled = view.event(&mut cx, event);
    drop(view);
    if anim::take_motion_pending() {
        cx.out.paint = true;
    }
    let output = std::mem::take(&mut cx.out);
    apply_output(app, win, output);
    handled
}

pub(crate) fn with_view<V: View, R>(app: &App, win: &Rc<WindowState>, f: impl FnOnce(&mut V, &mut Ctx) -> R) -> Option<R> {
    let mut view = win.view.try_borrow_mut().ok()?;
    let concrete = (&mut **view as &mut dyn std::any::Any).downcast_mut::<V>()?;
    let now = precise_time();
    anim::set_clock(now);
    anim::take_motion_pending();
    let mut cx = win.ctx(app, now);
    let result = f(concrete, &mut cx);
    drop(view);
    if anim::take_motion_pending() {
        cx.out.paint = true;
    }
    let output = std::mem::take(&mut cx.out);
    apply_output(app, win, output);
    Some(result)
}

pub(crate) fn apply_theme(app: &App, win: &Rc<WindowState>, theme: Theme) {
    if *win.theme.borrow() == theme {
        return;
    }
    if matches!(win.spec.kind, WindowKind::Normal { .. }) {
        win::set_dark_mode(win.hwnd.get(), theme.is_dark());
    }
    *win.theme.borrow_mut() = theme;
    dispatch(app, win, &Event::ThemeChanged);
    app.request_frame(win);
}

fn apply_output(app: &App, win: &Rc<WindowState>, output: CtxOutput) {
    if let Some(cursor) = output.cursor {
        win.cursor.set(cursor);
        if win.hovering.get() {
            set_system_cursor(win);
        }
    }
    if output.paint || output.animate {
        app.request_frame(win);
    }
    for op in output.ops {
        apply_op(app, win, op);
    }
}

fn set_system_cursor(win: &WindowState) {
    // SAFETY: SetCursor with a shared system or cached cursor handle.
    unsafe { SetCursor(win.cursor.get().handle(win.dpi.get())) };
}

fn apply_op(app: &App, win: &Rc<WindowState>, op: WindowOp) {
    let hwnd = win.hwnd();
    let raw = win.hwnd.get();
    // SAFETY: every call operates on our own window handle.
    unsafe {
        match op {
            WindowOp::Close => {
                let _ = DestroyWindow(hwnd);
            }
            WindowOp::SetRectPx(r) => {
                win::set_window_rect_px(raw, r);
            }
            WindowOp::MoveToPx(p) => {
                let _ = SetWindowPos(hwnd, None, p.x, p.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            WindowOp::ResizeDip(size) => {
                let scale = win.scale();
                let (cw, ch) = win.client_px.get();
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let extra_w = (r.right - r.left) - cw as i32;
                let extra_h = (r.bottom - r.top) - ch as i32;
                let w = (size.w * scale).round() as i32 + extra_w;
                let h = (size.h * scale).round() as i32 + extra_h;
                let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            WindowOp::Show { activate } => {
                if win.hidden.get() {
                    win.show_pending.set(Some(activate));
                    app.request_frame(win);
                } else if activate {
                    win::force_foreground(raw);
                }
            }
            WindowOp::Hide => {
                win.hidden.set(true);
                win.show_pending.set(None);
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            WindowOp::Activate => {
                win::force_foreground(raw);
            }
            WindowOp::SetOpacity(opacity) => {
                win.opacity.set(opacity);
                if let Some(surface) = win.surface.borrow().as_ref()
                    && let Ok(v3) = surface.visual.cast::<IDCompositionVisual3>()
                {
                    let _ = v3.SetOpacity2(opacity.clamp(0.0, 1.0));
                    if let Ok(dcomp) = app.gfx().dcomp() {
                        let _ = dcomp.Commit();
                    }
                }
            }
            WindowOp::SetTopmost(on) => {
                win::set_topmost(raw, on);
            }
            WindowOp::SetTitle(title) => {
                let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
            }
            WindowOp::Minimize => {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
            WindowOp::ToggleMaximize => {
                let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
            }
            WindowOp::CapturePointer => {
                SetCapture(hwnd);
            }
            WindowOp::ReleasePointer => {
                win.releasing_capture.set(true);
                let _ = ReleaseCapture();
                win.releasing_capture.set(false);
            }
            WindowOp::ShowTooltip { anchor, text, shortcut } => {
                let delay = win.tooltip.borrow_mut().request(anchor, &text, shortcut.as_deref(), win.size_dip());
                match delay {
                    Some(delay) => {
                        app.set_repaint_timer(win.id, delay);
                    }
                    None => app.request_frame(win),
                }
            }
            WindowOp::HideTooltip { anchor } => {
                let mut tooltip = win.tooltip.borrow_mut();
                if anchor.is_none_or(|a| tooltip.anchor() == a) && tooltip.dismiss() {
                    app.request_frame(win);
                }
            }
            WindowOp::SetImeCaret(caret) => {
                let scale = win.scale();
                let imc = ImmGetContext(hwnd);
                if !imc.is_invalid() {
                    let form = COMPOSITIONFORM {
                        dwStyle: CFS_POINT,
                        ptCurrentPos: POINT { x: (caret.x * scale).round() as i32, y: (caret.bottom() * scale).round() as i32 },
                        rcArea: RECT::default(),
                    };
                    let _ = ImmSetCompositionWindow(imc, &form);
                    let _ = ImmReleaseContext(hwnd, imc);
                }
            }
        }
    }
}

fn modifiers() -> Modifiers {
    // SAFETY: plain key state queries.
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    Modifiers {
        shift: down(VK_SHIFT.0),
        ctrl: down(VK_CONTROL.0),
        alt: down(VK_MENU.0),
        win: down(VK_LWIN.0) || down(VK_RWIN.0),
    }
}

fn loword(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

fn hiword(v: usize) -> u32 {
    ((v >> 16) & 0xFFFF) as u32
}

fn client_dip(win: &WindowState, screen: POINT) -> PointF {
    let mut p = screen;
    // SAFETY: converts within our own window.
    let _ = unsafe { ScreenToClient(win.hwnd(), &mut p) };
    PointF::new(p.x as f32 / win.scale(), p.y as f32 / win.scale())
}

fn pointer_kind(info: &POINTER_INFO) -> PointerKind {
    match info.pointerType {
        t if t == PT_PEN => PointerKind::Pen,
        t if t == PT_TOUCH => PointerKind::Touch,
        t if t == PT_TOUCHPAD => PointerKind::Touchpad,
        t if t == PT_MOUSE => PointerKind::Mouse,
        _ => PointerKind::Mouse,
    }
}

fn changed_button(change: i32) -> Option<MouseButton> {
    match change {
        1 | 2 => Some(MouseButton::Left),
        3 | 4 => Some(MouseButton::Right),
        5 | 6 => Some(MouseButton::Middle),
        7 | 8 => Some(MouseButton::X1),
        9 | 10 => Some(MouseButton::X2),
        _ => None,
    }
}

fn pen_pressure(pen: &POINTER_PEN_INFO) -> f32 {
    if pen.penMask & PEN_MASK_PRESSURE != 0 { (pen.pressure as f32 / 1024.0).clamp(0.0, 1.0) } else { 0.5 }
}

fn read_pointer(win: &WindowState, wparam: WPARAM) -> Option<PointerEvent> {
    let id = loword(wparam.0);
    let mut info = POINTER_INFO::default();
    // SAFETY: out-pointer valid; id comes from the message.
    unsafe { GetPointerInfo(id, &mut info) }.ok()?;
    let kind = pointer_kind(&info);
    let flags = info.pointerFlags;
    let mut buttons = Buttons {
        left: flags.contains(POINTER_FLAG_FIRSTBUTTON),
        right: flags.contains(POINTER_FLAG_SECONDBUTTON),
        middle: flags.contains(POINTER_FLAG_THIRDBUTTON),
        x1: flags.contains(POINTER_FLAG_FOURTHBUTTON),
        x2: flags.contains(POINTER_FLAG_FIFTHBUTTON),
        eraser: false,
    };
    let mut pressure = if kind == PointerKind::Mouse { 1.0 } else { 0.5 };
    let mut history = Vec::new();
    if kind == PointerKind::Pen {
        let mut pen = POINTER_PEN_INFO::default();
        // SAFETY: out-pointer valid.
        if unsafe { GetPointerPenInfo(id, &mut pen) }.is_ok() {
            pressure = pen_pressure(&pen);
            buttons.eraser = pen.penFlags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0;
            if pen.penFlags & PEN_FLAG_BARREL != 0 {
                buttons.right = true;
            }
        }
        if info.historyCount > 1 {
            let mut count = info.historyCount;
            let mut entries = vec![POINTER_PEN_INFO::default(); count as usize];
            // SAFETY: buffer holds `count` entries.
            if unsafe { GetPointerPenInfoHistory(id, &mut count, Some(entries.as_mut_ptr())) }.is_ok() {
                history = entries[1..count as usize]
                    .iter()
                    .rev()
                    .map(|e| PointerSample { pos: client_dip(win, e.pointerInfo.ptPixelLocation), pressure: pen_pressure(e) })
                    .collect();
            }
        }
    } else if info.historyCount > 1 {
        let mut count = info.historyCount;
        let mut entries = vec![POINTER_INFO::default(); count as usize];
        // SAFETY: buffer holds `count` entries.
        if unsafe { GetPointerInfoHistory(id, &mut count, Some(entries.as_mut_ptr())) }.is_ok() {
            history = entries[1..count as usize]
                .iter()
                .rev()
                .map(|e| PointerSample { pos: client_dip(win, e.ptPixelLocation), pressure })
                .collect();
        }
    }
    Some(PointerEvent {
        pos: client_dip(win, info.ptPixelLocation),
        screen_px: PointI::new(info.ptPixelLocation.x, info.ptPixelLocation.y),
        kind,
        id,
        button: changed_button(info.ButtonChangeType.0),
        buttons,
        pressure,
        mods: modifiers(),
        click_count: 0,
        history,
    })
}

fn count_clicks(win: &WindowState, event: &mut PointerEvent) {
    let now = precise_time();
    let mut tracker = win.clicks.get();
    // SAFETY: plain system metric queries.
    let (limit, slop_x, slop_y) = unsafe {
        (GetDoubleClickTime() as f64 / 1000.0, GetSystemMetrics(SM_CXDOUBLECLK) / 2, GetSystemMetrics(SM_CYDOUBLECLK) / 2)
    };
    let near = (event.screen_px.x - tracker.screen.x).abs() <= slop_x && (event.screen_px.y - tracker.screen.y).abs() <= slop_y;
    tracker.count = if tracker.button == event.button && near && now - tracker.time <= limit { tracker.count + 1 } else { 1 };
    tracker.time = now;
    tracker.screen = event.screen_px;
    tracker.button = event.button;
    win.clicks.set(tracker);
    event.click_count = tracker.count;
}

fn begin_hover(win: &WindowState) {
    if win.hovering.replace(true) {
        return;
    }
    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: win.hwnd(),
        dwHoverTime: 0,
    };
    // SAFETY: valid struct for our own window.
    let _ = unsafe { TrackMouseEvent(&mut track) };
    set_system_cursor(win);
}

fn end_hover(app: &App, win: &Rc<WindowState>) {
    if win.hovering.replace(false) {
        if win.tooltip.borrow_mut().dismiss() {
            app.request_frame(win);
        }
        dispatch(app, win, &Event::PointerLeave);
    }
}

fn wheel_event(win: &WindowState, wparam: WPARAM, lparam: LPARAM, horizontal: bool) -> Event {
    let delta = hiword(wparam.0) as u16 as i16 as f32 / 120.0;
    let screen = POINT { x: loword(lparam.0 as usize) as u16 as i16 as i32, y: hiword(lparam.0 as usize) as u16 as i16 as i32 };
    let delta = if horizontal { PointF::new(delta, 0.0) } else { PointF::new(0.0, delta) };
    let precise = (delta.x.fract() != 0.0) || (delta.y.fract() != 0.0);
    Event::Wheel(WheelEvent { pos: client_dip(win, screen), delta, precise, mods: modifiers() })
}

fn key_event(wparam: WPARAM, lparam: LPARAM) -> KeyEvent {
    let vk = wparam.0 as u16;
    KeyEvent { key: Key::from_vk(vk), vk, mods: modifiers(), repeat: (lparam.0 >> 30) & 1 == 1 }
}

fn resize_border(win: &WindowState) -> i32 {
    let dpi = win.dpi.get();
    // SAFETY: plain system metric queries.
    unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

fn handle_dpi_changed(app: &App, win: &Rc<WindowState>, wparam: WPARAM, lparam: LPARAM) {
    let new_dpi = loword(wparam.0).max(96);
    win.dpi.set(new_dpi);
    let hwnd = win.hwnd();
    // SAFETY: lparam points to the suggested RECT for WM_DPICHANGED; positioning our own window.
    unsafe {
        match &win.spec.kind {
            WindowKind::Normal { .. } => {
                let r = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            WindowKind::Overlay { rect_px } => {
                win::set_window_rect_px(win.hwnd.get(), *rect_px);
            }
            WindowKind::Popup { .. } => {
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let size = win.size_dip();
                let scale = new_dpi as f32 / 96.0;
                let (w, h) = ((size.w * scale).round() as i32, (size.h * scale).round() as i32);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }
    update_client_size(win);
    dispatch(app, win, &Event::ScaleChanged);
    dispatch(app, win, &Event::Resized);
    app.request_frame(win);
}

fn handle_message(app: &App, win: &Rc<WindowState>, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let hwnd = win.hwnd();
    match msg {
        WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP => {
            let mut event = read_pointer(win, wparam)?;
            begin_hover(win);
            let wrapped = match msg {
                WM_POINTERDOWN => {
                    count_clicks(win, &mut event);
                    if event.kind == PointerKind::Mouse {
                        // SAFETY: capture on our own window.
                        unsafe { SetCapture(hwnd) };
                    }
                    win.buttons_down.set(true);
                    if win.tooltip.borrow_mut().dismiss() {
                        app.request_frame(win);
                    }
                    Event::PointerDown(event)
                }
                WM_POINTERUP => {
                    let all_up = !event.buttons.any();
                    let wrapped = Event::PointerUp(event);
                    if all_up {
                        win.buttons_down.set(false);
                        dispatch(app, win, &wrapped);
                        win.releasing_capture.set(true);
                        // SAFETY: releasing our own capture.
                        let _ = unsafe { ReleaseCapture() };
                        win.releasing_capture.set(false);
                        return Some(LRESULT(0));
                    }
                    wrapped
                }
                _ => {
                    if !win.buttons_down.get() && win.hit_polling.get() && !in_interactive_region(win, event.pos) {
                        set_passthrough(win, true);
                    }
                    Event::PointerMove(event)
                }
            };
            dispatch(app, win, &wrapped);
            Some(LRESULT(0))
        }
        WM_POINTERLEAVE | WM_MOUSELEAVE => {
            if !win.buttons_down.get() {
                end_hover(app, win);
            }
            Some(LRESULT(0))
        }
        WM_POINTERCAPTURECHANGED => {
            if win.buttons_down.replace(false) && !win.releasing_capture.get() {
                dispatch(app, win, &Event::PointerCancel);
            }
            Some(LRESULT(0))
        }
        WM_POINTERWHEEL | WM_MOUSEWHEEL => {
            dispatch(app, win, &wheel_event(win, wparam, lparam, false));
            Some(LRESULT(0))
        }
        WM_POINTERHWHEEL | WM_MOUSEHWHEEL => {
            dispatch(app, win, &wheel_event(win, wparam, lparam, true));
            Some(LRESULT(0))
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let event = key_event(wparam, lparam);
            let enters_menu_mode = matches!(event.key, Key::Alt | Key::F(10));
            let alt_f4 = msg == WM_SYSKEYDOWN && event.key == Key::F(4) && event.mods.alt && !event.repeat;
            let handled = dispatch(app, win, &Event::KeyDown(event));
            if !handled && alt_f4 && !matches!(win.spec.kind, WindowKind::Normal { .. }) {
                dispatch(app, win, &Event::CloseRequested);
                return Some(LRESULT(0));
            }
            (handled || msg == WM_KEYDOWN || enters_menu_mode).then_some(LRESULT(0))
        }
        WM_KEYUP | WM_SYSKEYUP => {
            let event = key_event(wparam, lparam);
            let enters_menu_mode = matches!(event.key, Key::Alt | Key::F(10));
            let handled = dispatch(app, win, &Event::KeyUp(event));
            (handled || msg == WM_KEYUP || enters_menu_mode).then_some(LRESULT(0))
        }
        WM_CHAR => {
            let unit = wparam.0 as u16;
            let text = if (0xD800..0xDC00).contains(&unit) {
                win.high_surrogate.set(Some(unit));
                None
            } else if (0xDC00..0xE000).contains(&unit) {
                win.high_surrogate.take().and_then(|high| String::from_utf16(&[high, unit]).ok())
            } else if unit >= 0x20 && unit != 0x7F {
                String::from_utf16(&[unit]).ok()
            } else {
                None
            };
            if let Some(text) = text {
                dispatch(app, win, &Event::Text(text));
            }
            Some(LRESULT(0))
        }
        WM_SETFOCUS => {
            dispatch(app, win, &Event::Focus(true));
            Some(LRESULT(0))
        }
        WM_KILLFOCUS => {
            dispatch(app, win, &Event::Focus(false));
            Some(LRESULT(0))
        }
        WM_SIZE => {
            if update_client_size(win) {
                dispatch(app, win, &Event::Resized);
                app.request_frame(win);
                if !win.hidden.get() && win.client_px.get().0 > 0 {
                    render(app, win, precise_time());
                }
            }
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            handle_dpi_changed(app, win, wparam, lparam);
            Some(LRESULT(0))
        }
        WM_GETMINMAXINFO => {
            if let WindowKind::Normal { min_size, .. } = &win.spec.kind {
                let scale = win.scale();
                // SAFETY: lparam points to MINMAXINFO for this message.
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                info.ptMinTrackSize.x = (min_size.w * scale).round() as i32;
                info.ptMinTrackSize.y = (min_size.h * scale).round() as i32;
            }
            None
        }
        WM_NCCALCSIZE if wparam.0 != 0 && win.spec.unified() => {
            // SAFETY: lparam points to NCCALCSIZE_PARAMS when wparam is TRUE.
            unsafe {
                let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
                let top = params.rgrc[0].top;
                DefWindowProcW(hwnd, msg, wparam, lparam);
                params.rgrc[0].top = top;
                if IsZoomed(hwnd).as_bool() {
                    params.rgrc[0].top += resize_border(win);
                }
            }
            Some(LRESULT(0))
        }
        WM_NCHITTEST => {
            if win.spec.click_through_enabled() {
                return Some(LRESULT(HTTRANSPARENT as isize));
            }
            if matches!(win.spec.kind, WindowKind::Popup { .. }) {
                let screen = PointI::new(loword(lparam.0 as usize) as u16 as i16 as i32, hiword(lparam.0 as usize) as u16 as i16 as i32);
                let pos = client_dip(win, POINT { x: screen.x, y: screen.y });
                let outside = !win.buttons_down.get() && !in_interactive_region(win, pos);
                return Some(LRESULT(if outside { HTTRANSPARENT as isize } else { HTCLIENT as isize }));
            }
            if !win.spec.unified() {
                return None;
            }
            // SAFETY: default hit testing for borders.
            let hit = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            if hit.0 != HTCLIENT as isize {
                return Some(hit);
            }
            let screen = POINT { x: loword(lparam.0 as usize) as u16 as i16 as i32, y: hiword(lparam.0 as usize) as u16 as i16 as i32 };
            let mut client = screen;
            // SAFETY: converts within our own window.
            let _ = unsafe { ScreenToClient(hwnd, &mut client) };
            // SAFETY: plain query.
            let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
            let border = resize_border(win);
            if !maximized && client.y < border {
                let width = win.client_px.get().0 as i32;
                let area = if client.x < border * 2 {
                    HTTOPLEFT
                } else if client.x > width - border * 2 {
                    HTTOPRIGHT
                } else {
                    HTTOP
                };
                return Some(LRESULT(area as isize));
            }
            let pos = PointF::new(client.x as f32 / win.scale(), client.y as f32 / win.scale());
            let area = win.view.try_borrow().map(|v| v.hit_test(pos)).unwrap_or_default();
            Some(LRESULT(if area == HitArea::Caption { HTCAPTION } else { HTCLIENT } as isize))
        }
        WM_SETCURSOR => {
            if loword(lparam.0 as usize) == HTCLIENT {
                set_system_cursor(win);
                return Some(LRESULT(1));
            }
            None
        }
        WM_MOUSEACTIVATE if !win.spec.activates() => Some(LRESULT(MA_NOACTIVATE as isize)),
        WM_POINTERACTIVATE if !win.spec.activates() => Some(LRESULT(PA_NOACTIVATE as isize)),
        WM_ACTIVATE => None,
        WM_PAINT => {
            // BeginPaint/EndPaint clear both the update region and the internal-paint flag; ValidateRect alone
            // left hidden layered popups receiving WM_PAINT forever. Content itself comes from DirectComposition.
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: paired calls on our own window inside its WM_PAINT handler.
            unsafe {
                BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
            }
            app.request_frame(win);
            Some(LRESULT(0))
        }
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_ENTERSIZEMOVE => {
            app.arm_modal_timer();
            None
        }
        WM_SETTINGCHANGE => {
            app.refresh_system_settings();
            None
        }
        WM_CLOSE => {
            dispatch(app, win, &Event::CloseRequested);
            Some(LRESULT(0))
        }
        WM_APP_CLOSE => {
            // SAFETY: destroying our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            dispatch(app, win, &Event::Closed);
            win.surface.borrow_mut().take();
            app.remove_window(win.id);
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// True when `pos` (client DIP) is inside the view's interactive region (or the view declares none).
fn in_interactive_region(win: &WindowState, pos: PointF) -> bool {
    match win.view.try_borrow().map(|v| v.interactive_region()) {
        Ok(Some(rects)) => rects.iter().any(|r| r.contains(pos)),
        _ => true,
    }
}

/// Toggles WS_EX_TRANSPARENT on a (layered) popup so the system routes mouse input to the windows below.
fn set_passthrough(win: &WindowState, on: bool) {
    if win.passthrough.replace(on) == on {
        return;
    }
    let hwnd = win.hwnd();
    // SAFETY: style change on our own window.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let transparent = WS_EX_TRANSPARENT.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, if on { ex | transparent } else { ex & !transparent });
    }
}

/// DIP client rects to virtual-desktop pixel rects.
pub(crate) fn region_to_screen(region: &[RectF], origin: PointI, scale: f32) -> Vec<RectI> {
    region.iter().map(|r| r.scale(scale).round_out().offset(origin.x, origin.y)).collect()
}

pub(crate) fn region_contains(region: &[RectI], point: PointI) -> bool {
    region.iter().any(|r| r.contains(point))
}

/// How soon to look at the cursor again: 30 Hz over or next to the popup, slower the farther away it is (assuming
/// the cursor moves at most ~4000 px/s), never slower than 2 Hz.
pub(crate) fn hit_poll_interval(window: RectI, cursor: PointI) -> Duration {
    let dx = (window.x - cursor.x).max(cursor.x - window.right()).max(0) as f64;
    let dy = (window.y - cursor.y).max(cursor.y - window.bottom()).max(0) as f64;
    let seconds = ((dx * dx + dy * dy).sqrt() / 4000.0).clamp(1.0 / 30.0, 0.5);
    Duration::from_secs_f64(seconds)
}

/// Starts cursor polling for popups whose view declares an interactive region.
fn ensure_hit_polling(app: &App, win: &Rc<WindowState>) {
    let eligible = matches!(win.spec.kind, WindowKind::Popup { click_through: false, .. });
    if !eligible || win.hit_polling.get() || win.hidden.get() {
        return;
    }
    if win.view.try_borrow().is_ok_and(|v| v.interactive_region().is_some()) {
        poll_hit_region(app, win);
    }
}

/// One polling step: makes the popup click-through unless the cursor is over its interactive region, then
/// schedules the next step. Stops when the window hides or the view drops its region.
pub(crate) fn poll_hit_region(app: &App, win: &Rc<WindowState>) {
    win.hit_polling.set(false);
    if win.hidden.get() {
        return;
    }
    let region = match win.view.try_borrow().map(|v| v.interactive_region()) {
        Ok(Some(region)) => region,
        Ok(None) => {
            set_passthrough(win, false);
            return;
        }
        Err(_) => {
            win.hit_polling.set(true);
            app.set_hit_poll_timer(win.id, Duration::from_millis(33));
            return;
        }
    };
    let (Some(origin), Some(window_rect)) = (win::client_origin_px(win.hwnd.get()), win::window_rect_px(win.hwnd.get()))
    else {
        return;
    };
    let mut cursor = POINT::default();
    // SAFETY: out-pointer valid.
    let cursor = if unsafe { GetCursorPos(&mut cursor) }.is_ok() { PointI::new(cursor.x, cursor.y) } else { origin };
    if !win.buttons_down.get() {
        let inside = region_contains(&region_to_screen(&region, origin, win.scale()), cursor);
        set_passthrough(win, !inside);
    }
    win.hit_polling.set(true);
    app.set_hit_poll_timer(win.id, hit_poll_interval(window_rect, cursor));
}

pub(crate) extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let found = crate::app::current_app().and_then(|app| app.window_for_hwnd(hwnd).map(|w| (app, w)));
    if let Some((app, win)) = found {
        if win.spec.unified() {
            let mut result = LRESULT(0);
            // SAFETY: lets DWM handle caption button hover/press for the extended frame.
            if unsafe { DwmDefWindowProc(hwnd, msg, wparam, lparam, &mut result) }.as_bool() {
                return result;
            }
        }
        if let Some(result) = handle_message(&app, &win, msg, wparam, lparam) {
            return result;
        }
    }
    // SAFETY: default processing.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_area_fit_clamps_size_and_keeps_top_left_inside() {
        let area = RectI::new(0, 0, 1920, 1032);
        let r = fit_in_work_area((1064, 1190), (400, 300), area, 12);
        assert_eq!((r.w, r.h), (1064, 1008));
        assert_eq!(r.y, 12);
        assert_eq!(r.x, (1920 - 1064) / 2);
        let small = fit_in_work_area((800, 600), (400, 300), area, 12);
        assert_eq!(small, RectI::new(560, 216, 800, 600));
        let too_big_min = fit_in_work_area((900, 900), (1200, 1100), RectI::new(100, 50, 1000, 800), 12);
        assert_eq!((too_big_min.x, too_big_min.y), (100, 50), "min size wins but the title bar stays on screen");
        assert_eq!((too_big_min.w, too_big_min.h), (1200, 1100));
    }

    #[test]
    fn region_maps_to_screen_pixels() {
        let rects = region_to_screen(&[RectF::new(32.0, 12.0, 300.0, 44.0)], PointI::new(1000, 0), 1.5);
        assert_eq!(rects, vec![RectI::new(1048, 18, 450, 66)]);
        assert!(region_contains(&rects, PointI::new(1048, 18)));
        assert!(!region_contains(&rects, PointI::new(1047, 18)));
        assert!(!region_contains(&rects, PointI::new(1100, 84)));
        assert!(!region_contains(&[], PointI::new(0, 0)));
    }

    #[test]
    fn poll_interval_is_fast_near_and_slow_far() {
        let window = RectI::new(800, 0, 400, 100);
        assert_eq!(hit_poll_interval(window, PointI::new(900, 50)), Duration::from_secs_f64(1.0 / 30.0));
        assert_eq!(hit_poll_interval(window, PointI::new(1250, 120)), Duration::from_secs_f64(1.0 / 30.0));
        let far = hit_poll_interval(window, PointI::new(1000, 900));
        assert!(far > Duration::from_millis(150) && far <= Duration::from_millis(500));
        assert_eq!(hit_poll_interval(window, PointI::new(-3000, 3000)), Duration::from_millis(500));
    }
}
