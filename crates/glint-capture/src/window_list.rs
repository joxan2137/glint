use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Once;

use glint_core::{PointI, RectI, WindowInfo};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetLayeredWindowAttributes, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, WINDOW_EX_STYLE, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
};
use windows_core::BOOL;

use crate::wide;

const SHELL_DESKTOP_CLASSES: [&str; 2] = ["Progman", "WorkerW"];
const CLASS_NAME_CAPACITY: usize = 256;

/// Visible top-level windows, topmost first, with `z_order` set to the index in the result.
pub fn windows_snapshot(exclude_pid: Option<u32>) -> Vec<WindowInfo> {
    top_level_windows()
        .into_iter()
        .filter_map(|hwnd| window_info(hwnd, exclude_pid))
        .enumerate()
        .map(|(z_order, window)| WindowInfo { z_order: z_order as u32, ..window })
        .collect()
}

/// The foreground window if it passes the same filters as the snapshot; `z_order` is 0.
pub fn foreground_window() -> Option<WindowInfo> {
    // SAFETY: GetForegroundWindow has no preconditions.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }
    window_info(hwnd, None)
}

/// First window of a z-ordered list whose rect contains `p`.
pub fn window_at(windows: &[WindowInfo], p: PointI) -> Option<&WindowInfo> {
    windows.iter().find(|window| window.rect.contains(p))
}

/// Idempotent; ignores the access-denied error that means the awareness was already set (manifest or earlier call).
pub fn enable_per_monitor_dpi_awareness() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: plain process-wide setting without pointers.
        if let Err(error) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
            log::debug!("SetProcessDpiAwarenessContext: {error}");
        }
    });
}

fn top_level_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the address of the Vec passed below, alive for the whole enumeration.
        let handles = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        handles.push(hwnd);
        BOOL(1)
    }
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only touches the Vec behind the LPARAM, which outlives the call.
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut handles as *mut Vec<HWND> as isize));
    }
    handles
}

fn window_info(hwnd: HWND, exclude_pid: Option<u32>) -> Option<WindowInfo> {
    // SAFETY: both calls only read window state.
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() || is_cloaked(hwnd) {
        return None;
    }
    let process_id = process_id(hwnd);
    if exclude_pid == Some(process_id) {
        return None;
    }
    let class_name = class_name(hwnd);
    if SHELL_DESKTOP_CLASSES.contains(&class_name.as_str()) {
        return None;
    }
    let rect = frame_bounds(hwnd).filter(|rect| !rect.is_empty())?;
    let ex_style = extended_style(hwnd);
    if ex_style.contains(WS_EX_LAYERED) && is_fully_transparent(hwnd) {
        return None;
    }
    let title = title(hwnd);
    if ex_style.contains(WS_EX_TOOLWINDOW) && title.is_empty() {
        return None;
    }
    Some(WindowInfo { hwnd: hwnd.0 as isize, title, class_name, process_id, rect, z_order: 0 })
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: the out buffer is a u32 as DWMWA_CLOAKED requires.
    let result = unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast::<c_void>(), size_of::<u32>() as u32)
    };
    result.is_ok() && cloaked != 0
}

fn frame_bounds(hwnd: HWND) -> Option<RectI> {
    let mut rect = RECT::default();
    // SAFETY: the out buffer is a RECT as DWMWA_EXTENDED_FRAME_BOUNDS requires.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast::<c_void>(),
            size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    Some(RectI::from_ltrb(rect.left, rect.top, rect.right, rect.bottom))
}

fn is_fully_transparent(hwnd: HWND) -> bool {
    let mut alpha = 0u8;
    let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
    let mut color_key = COLORREF(0);
    // SAFETY: all out pointers are valid; the call fails (and we keep the window) for UpdateLayeredWindow windows.
    let queried = unsafe { GetLayeredWindowAttributes(hwnd, Some(&mut color_key), Some(&mut alpha), Some(&mut flags)) };
    queried.is_ok() && flags.contains(LWA_ALPHA) && alpha == 0
}

fn extended_style(hwnd: HWND) -> WINDOW_EX_STYLE {
    // SAFETY: GetWindowLongPtrW only reads window state.
    WINDOW_EX_STYLE(unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32)
}

fn process_id(hwnd: HWND) -> u32 {
    let mut process_id = 0u32;
    // SAFETY: the out pointer is valid for the call.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
    process_id
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; CLASS_NAME_CAPACITY];
    // SAFETY: the buffer slice is passed with its real length.
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

fn title(hwnd: HWND) -> String {
    // SAFETY: both calls only read window text into the buffer passed with its real length.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    wide::to_string(&buffer[..copied.max(0) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(hwnd: isize, rect: RectI, z_order: u32) -> WindowInfo {
        WindowInfo { hwnd, title: String::new(), class_name: String::new(), process_id: 1, rect, z_order }
    }

    #[test]
    fn window_at_returns_topmost_hit() {
        let windows = [window(1, RectI::new(100, 100, 200, 200), 0), window(2, RectI::new(0, 0, 500, 500), 1)];
        assert_eq!(window_at(&windows, PointI::new(150, 150)).map(|w| w.hwnd), Some(1));
        assert_eq!(window_at(&windows, PointI::new(10, 10)).map(|w| w.hwnd), Some(2));
        assert_eq!(window_at(&windows, PointI::new(600, 600)).map(|w| w.hwnd), None);
    }

    #[test]
    fn window_at_excludes_far_edge() {
        let windows = [window(1, RectI::new(0, 0, 10, 10), 0)];
        assert!(window_at(&windows, PointI::new(9, 9)).is_some());
        assert!(window_at(&windows, PointI::new(10, 5)).is_none());
    }
}
