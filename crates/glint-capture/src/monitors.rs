use std::mem::size_of;

use anyhow::{Result, bail};
use glint_core::{HdrInfo, MonitorInfo, RectI};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows_core::BOOL;

use crate::display_config::{self, DisplayTarget};
use crate::dxgi::{self, DxgiOutput, OutputColor};
use crate::wide;

const DEFAULT_DPI: u32 = 96;
const SDR_REFERENCE_WHITE_NITS: f32 = 80.0;

pub fn monitors() -> Result<Vec<MonitorInfo>> {
    let handles = enum_display_monitors();
    if handles.is_empty() {
        bail!("EnumDisplayMonitors returned no monitors");
    }
    let targets = display_config::active_targets();
    let outputs = dxgi::enumerate_outputs().unwrap_or_else(|error| {
        log::warn!("DXGI enumeration failed, HDR state unavailable: {error:#}");
        Vec::new()
    });
    let monitors: Vec<MonitorInfo> =
        handles.into_iter().filter_map(|handle| describe(handle, &targets, &outputs)).collect();
    if monitors.is_empty() {
        bail!("GetMonitorInfoW failed for every monitor");
    }
    Ok(monitors)
}

fn describe(handle: HMONITOR, targets: &[DisplayTarget], outputs: &[DxgiOutput]) -> Option<MonitorInfo> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: MONITORINFOEXW starts with MONITORINFO and cbSize announces the extended size.
    if !unsafe { GetMonitorInfoW(handle, (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>()) }.as_bool() {
        return None;
    }
    let device_name = wide::to_string(&info.szDevice);
    let target = targets.iter().find(|t| t.gdi_device_name == device_name);
    let color = outputs.iter().find(|o| o.desc.Monitor == handle).and_then(|o| dxgi::output_color(&o.output));
    let sdr_white_nits = target.and_then(|t| t.sdr_white_nits).unwrap_or(SDR_REFERENCE_WHITE_NITS);

    log::info!(
        "{device_name}: dxgi_hdr={:?} sdr_white={sdr_white_nits:.0} nits advanced_color={:?}",
        color.map(|c| c.hdr_active),
        target.and_then(|t| t.advanced_color)
    );

    Some(MonitorInfo {
        handle: handle.0 as isize,
        friendly_name: target.and_then(|t| t.friendly_name.clone()).unwrap_or_else(|| device_name.clone()),
        device_name,
        rect: rect_i(info.monitorInfo.rcMonitor),
        work_rect: rect_i(info.monitorInfo.rcWork),
        dpi: effective_dpi(handle),
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        hdr: color.filter(|c| c.hdr_active).map(|c| hdr_info(c, sdr_white_nits)),
    })
}

fn hdr_info(color: OutputColor, sdr_white_nits: f32) -> HdrInfo {
    HdrInfo {
        sdr_white_nits,
        max_nits: color.max_nits,
        max_full_frame_nits: color.max_full_frame_nits,
        min_nits: color.min_nits,
    }
}

fn rect_i(rect: RECT) -> RectI {
    RectI::from_ltrb(rect.left, rect.top, rect.right, rect.bottom)
}

fn effective_dpi(handle: HMONITOR) -> u32 {
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: both out pointers are valid for the call.
    match unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } {
        Ok(()) if dpi_x > 0 => dpi_x,
        _ => DEFAULT_DPI,
    }
}

fn enum_display_monitors() -> Vec<HMONITOR> {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the address of the Vec passed below, alive for the whole enumeration.
        let handles = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        handles.push(monitor);
        BOOL(1)
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only touches the Vec behind the LPARAM, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut handles as *mut Vec<HMONITOR> as isize));
    }
    handles
}
