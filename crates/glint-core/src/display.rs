use crate::geom::RectI;
use crate::image::{HdrImage, Image};
use crate::tonemap::HdrStats;

/// HDR / advanced-color state of a monitor while HDR is on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrInfo {
    /// Windows "SDR content brightness" (DISPLAYCONFIG_SDR_WHITE_LEVEL), in nits.
    pub sdr_white_nits: f32,
    /// IDXGIOutput6::GetDesc1 MaxLuminance, in nits.
    pub max_nits: f32,
    /// IDXGIOutput6::GetDesc1 MaxFullFrameLuminance, in nits.
    pub max_full_frame_nits: f32,
    /// IDXGIOutput6::GetDesc1 MinLuminance, in nits.
    pub min_nits: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    /// HMONITOR as isize. Valid for the current session only.
    pub handle: isize,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device_name: String,
    /// Human name, e.g. "DELL U2723QE". Falls back to device_name.
    pub friendly_name: String,
    /// Monitor rect in physical pixels, virtual-desktop coordinates.
    pub rect: RectI,
    /// Work area (monitor minus taskbar) in physical pixels, virtual-desktop coordinates.
    pub work_rect: RectI,
    /// Effective DPI; 96 = 100 % scale.
    pub dpi: u32,
    pub primary: bool,
    /// Some while the monitor runs in HDR (advanced color) mode.
    pub hdr: Option<HdrInfo>,
}

impl MonitorInfo {
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
}

/// A visible top-level window at the moment of capture.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    /// HWND as isize.
    pub hwnd: isize,
    pub title: String,
    pub class_name: String,
    pub process_id: u32,
    /// DWMWA_EXTENDED_FRAME_BOUNDS in physical pixels, virtual-desktop coordinates (no invisible resize borders).
    pub rect: RectI,
    /// 0 = topmost.
    pub z_order: u32,
}

/// One frozen monitor image.
#[derive(Clone, Debug)]
pub struct MonitorCapture {
    pub monitor: MonitorInfo,
    /// Tone-mapped (or plain SDR) image of the whole monitor, monitor.rect size.
    pub sdr: Image,
    /// Raw scRGB pixels when the monitor was in HDR mode, same size as `sdr`.
    pub hdr: Option<HdrImage>,
    /// Stats of `hdr` over the whole monitor.
    pub hdr_stats: Option<HdrStats>,
}
