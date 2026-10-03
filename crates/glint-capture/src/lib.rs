//! Monitors with HDR state, one-shot desktop capture (DXGI duplication, Windows.Graphics.Capture, GDI) and the
//! window list.

mod capture;
mod display_config;
mod duplication;
mod dxgi;
mod gdi;
mod gpu;
mod graphics_capture;
mod monitors;
mod orient;
mod wide;
mod window_list;

pub use capture::{
    CaptureMethod, CapturePath, CaptureReport, capture_all, capture_all_reported, capture_all_via, capture_monitor,
    capture_monitor_gdi, capture_monitor_reported, capture_monitor_via, capture_monitor_wgc,
};
pub use gpu::{release_warm_devices, warm_up};
pub use monitors::monitors;
pub use window_list::{enable_per_monitor_dpi_awareness, foreground_window, window_at, windows_snapshot};
