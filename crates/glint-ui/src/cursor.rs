//! Standard cursors plus a crisp generated crosshair, cached per DPI.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP,
    HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, HCURSOR, ICONINFO, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, IDC_NO,
    IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT, LoadCursorW,
};
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    #[default]
    Arrow,
    Hand,
    IBeam,
    /// Glint's crisp 1-physical-px crosshair with a white halo and an open center (hotspot exact).
    Crosshair,
    SystemCrosshair,
    Move,
    ResizeNS,
    ResizeEW,
    ResizeNWSE,
    ResizeNESW,
    NotAllowed,
    Wait,
    Progress,
    Hidden,
}

thread_local! {
    static CROSSHAIRS: RefCell<HashMap<u32, isize>> = RefCell::new(HashMap::new());
}

fn system(id: PCWSTR) -> HCURSOR {
    // SAFETY: loading a predefined system cursor.
    unsafe { LoadCursorW(None, id) }.unwrap_or_default()
}

impl Cursor {
    /// The cursor handle for a window at `dpi`. `None` for `Hidden`.
    pub fn handle(self, dpi: u32) -> Option<HCURSOR> {
        Some(match self {
            Cursor::Arrow => system(IDC_ARROW),
            Cursor::Hand => system(IDC_HAND),
            Cursor::IBeam => system(IDC_IBEAM),
            Cursor::Crosshair => crosshair(dpi),
            Cursor::SystemCrosshair => system(IDC_CROSS),
            Cursor::Move => system(IDC_SIZEALL),
            Cursor::ResizeNS => system(IDC_SIZENS),
            Cursor::ResizeEW => system(IDC_SIZEWE),
            Cursor::ResizeNWSE => system(IDC_SIZENWSE),
            Cursor::ResizeNESW => system(IDC_SIZENESW),
            Cursor::NotAllowed => system(IDC_NO),
            Cursor::Wait => system(IDC_WAIT),
            Cursor::Progress => system(IDC_APPSTARTING),
            Cursor::Hidden => return None,
        })
    }
}

/// Geometry of the crosshair at a given scale, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrosshairMetrics {
    /// Square image size (odd).
    pub size: u32,
    /// Hotspot and center pixel.
    pub center: u32,
    /// Distance from the center to where each arm starts.
    pub gap: u32,
}

impl CrosshairMetrics {
    pub fn for_dpi(dpi: u32) -> Self {
        let scale = dpi.max(96) as f32 / 96.0;
        let gap = (3.0 * scale).round() as u32;
        let arm = (10.0 * scale).round() as u32;
        let center = gap + arm + 1;
        Self { size: 2 * center + 1, center, gap }
    }
}

/// BGRA pixels (straight alpha) of the crosshair.
pub fn crosshair_pixels(metrics: CrosshairMetrics) -> Vec<u8> {
    const LINE: [u8; 4] = [0x1C, 0x1C, 0x1C, 0xFF];
    const HALO: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xE0];
    let n = metrics.size as i32;
    let c = metrics.center as i32;
    let gap = metrics.gap as i32;
    let mut pixels = vec![0u8; (n * n * 4) as usize];
    let on_line = |x: i32, y: i32| {
        let (dx, dy) = ((x - c).abs(), (y - c).abs());
        let reach = c - 1;
        (dy == 0 && dx > gap && dx <= reach) || (dx == 0 && dy > gap && dy <= reach)
    };
    for y in 0..n {
        for x in 0..n {
            let i = ((y * n + x) * 4) as usize;
            if on_line(x, y) {
                pixels[i..i + 4].copy_from_slice(&LINE);
            } else if (-1..=1).any(|oy| (-1..=1).any(|ox| (ox == 0 || oy == 0) && on_line(x + ox, y + oy))) {
                pixels[i..i + 4].copy_from_slice(&HALO);
            }
        }
    }
    pixels
}

fn crosshair(dpi: u32) -> HCURSOR {
    let cached = CROSSHAIRS.with(|c| c.borrow().get(&dpi).copied());
    if let Some(handle) = cached {
        return HCURSOR(handle as *mut _);
    }
    let handle = create_crosshair(dpi).unwrap_or_else(|| system(IDC_CROSS));
    CROSSHAIRS.with(|c| c.borrow_mut().insert(dpi, handle.0 as isize));
    handle
}

fn create_crosshair(dpi: u32) -> Option<HCURSOR> {
    let metrics = CrosshairMetrics::for_dpi(dpi);
    let pixels = crosshair_pixels(metrics);
    let n = metrics.size as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: n,
            biHeight: -n,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: the DIB section owns `n * n * 4` bytes at `bits`; the mask is a blank 1-bpp bitmap; both GDI objects
    // are deleted after CreateIconIndirect copies them.
    unsafe {
        let color: HBITMAP = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
        let mask_bytes = vec![0u8; ((n + 15) / 16 * 2 * n) as usize];
        let mask = CreateBitmap(n, n, 1, 1, Some(mask_bytes.as_ptr().cast()));
        let icon = ICONINFO {
            fIcon: false.into(),
            xHotspot: metrics.center,
            yHotspot: metrics.center,
            hbmMask: mask,
            hbmColor: color,
        };
        let cursor = CreateIconIndirect(&icon).ok();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        cursor.map(|c| HCURSOR(c.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crosshair_is_odd_centered_and_open_in_the_middle() {
        for dpi in [96, 120, 144, 192] {
            let m = CrosshairMetrics::for_dpi(dpi);
            assert_eq!(m.size % 2, 1);
            assert_eq!(m.center * 2 + 1, m.size);
            let px = crosshair_pixels(m);
            let at = |x: u32, y: u32| px[((y * m.size + x) * 4 + 3) as usize];
            assert_eq!(at(m.center, m.center), 0, "center pixel stays open");
            assert_eq!(at(m.center, 1), 0xFF, "vertical arm reaches the top minus halo");
            assert_eq!(at(m.center + 1, 1), 0xE0, "halo beside the arm");
            assert_eq!(at(0, 0), 0);
        }
    }
}
