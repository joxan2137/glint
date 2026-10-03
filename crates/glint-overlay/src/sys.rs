//! Cursor position helpers (physical px, virtual desktop).

use glint_core::PointI;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

pub fn cursor_pos() -> Option<PointI> {
    let mut p = POINT::default();
    // SAFETY: valid out-pointer.
    unsafe { GetCursorPos(&mut p) }.ok().map(|()| PointI::new(p.x, p.y))
}

/// Moves the cursor by `dx`, `dy` physical pixels (arrow-key nudging).
pub fn nudge_cursor(dx: i32, dy: i32) {
    if let Some(p) = cursor_pos() {
        // SAFETY: plain call; Windows clamps the position to the desktop.
        let _ = unsafe { SetCursorPos(p.x + dx, p.y + dy) };
    }
}
