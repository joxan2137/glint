use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Error, Result, anyhow, bail};
use glint_core::settings::HdrSettings;
use glint_core::tonemap::{ToneMapParams, analyze, tonemap_with_stats};
use glint_core::{HdrImage, MonitorCapture, MonitorInfo};

use crate::duplication;
use crate::dxgi::{self, DxgiOutput};
use crate::gdi;
use crate::gpu::{DesktopFrame, Fp16Frame, Gpu};
use crate::graphics_capture;
use crate::monitors::monitors;

const DEFAULT_SDR_WHITE_NITS: f32 = 80.0;
const DEFAULT_DISPLAY_PEAK_NITS: f32 = 1000.0;

/// How a monitor image is grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMethod {
    /// DXGI desktop duplication; waits up to 30 ms for a frame, so a static desktop yields nothing.
    Dda,
    /// Windows.Graphics.Capture, one frame; works on a static desktop.
    Wgc,
    /// GDI BitBlt; SDR only.
    Gdi,
}

impl CaptureMethod {
    /// The order every normal capture tries.
    pub const DEFAULT_ORDER: [CaptureMethod; 3] = [CaptureMethod::Dda, CaptureMethod::Wgc, CaptureMethod::Gdi];
}

impl fmt::Display for CaptureMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CaptureMethod::Dda => "DDA",
            CaptureMethod::Wgc => "WGC",
            CaptureMethod::Gdi => "GDI",
        })
    }
}

impl FromStr for CaptureMethod {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text.to_ascii_lowercase().as_str() {
            "dda" => Ok(CaptureMethod::Dda),
            "wgc" => Ok(CaptureMethod::Wgc),
            "gdi" => Ok(CaptureMethod::Gdi),
            other => bail!("unknown capture method {other:?}, expected dda, wgc or gdi"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePath {
    DdaFp16,
    DdaBgra,
    WgcFp16,
    WgcBgra,
    Gdi,
}

impl fmt::Display for CapturePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CapturePath::DdaFp16 => "DDA FP16",
            CapturePath::DdaBgra => "DDA BGRA",
            CapturePath::WgcFp16 => "WGC FP16",
            CapturePath::WgcBgra => "WGC BGRA",
            CapturePath::Gdi => "GDI",
        })
    }
}

#[derive(Clone, Debug)]
pub struct CaptureReport {
    pub path: CapturePath,
    /// Why the methods tried before `path` were abandoned, e.g. "DDA: no frame with a desktop image within 30 ms".
    pub fallback_reason: Option<String>,
    /// Device setup, capture, frame acquire and CPU readback (or the GDI blit).
    pub grab: Duration,
    /// Stats plus tone mapping; zero for SDR frames.
    pub tonemap: Duration,
    pub total: Duration,
}

/// Captures every monitor in parallel (one thread each), in `monitors()` order.
/// Monitors that fail on every method are logged and skipped; all failing is an error.
pub fn capture_all(hdr: &HdrSettings) -> Result<Vec<MonitorCapture>> {
    Ok(capture_all_reported(hdr)?.into_iter().map(|(capture, _)| capture).collect())
}

pub fn capture_all_reported(hdr: &HdrSettings) -> Result<Vec<(MonitorCapture, CaptureReport)>> {
    capture_all_via(hdr, &CaptureMethod::DEFAULT_ORDER)
}

/// Like `capture_all_reported` with an explicit method order, e.g. `[Wgc, Gdi]` to simulate a static desktop.
pub fn capture_all_via(hdr: &HdrSettings, order: &[CaptureMethod]) -> Result<Vec<(MonitorCapture, CaptureReport)>> {
    let monitors = monitors()?;
    let results: Vec<Result<(MonitorCapture, CaptureReport)>> = std::thread::scope(|scope| {
        let workers: Vec<_> =
            monitors.iter().map(|monitor| scope.spawn(move || capture_monitor_via(monitor, hdr, order))).collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap_or_else(|_| Err(anyhow!("capture thread panicked"))))
            .collect()
    });
    let mut captures = Vec::new();
    for (monitor, result) in monitors.iter().zip(results) {
        match result {
            Ok(capture) => captures.push(capture),
            Err(error) => log::error!("capture of {} failed: {error:#}", monitor.device_name),
        }
    }
    if captures.is_empty() {
        bail!("no monitor could be captured");
    }
    Ok(captures)
}

pub fn capture_monitor(monitor: &MonitorInfo, hdr: &HdrSettings) -> Result<MonitorCapture> {
    Ok(capture_monitor_reported(monitor, hdr)?.0)
}

/// Desktop duplication, then Windows.Graphics.Capture, then GDI BitBlt. FP16 frames are tone mapped.
pub fn capture_monitor_reported(monitor: &MonitorInfo, hdr: &HdrSettings) -> Result<(MonitorCapture, CaptureReport)> {
    capture_monitor_via(monitor, hdr, &CaptureMethod::DEFAULT_ORDER)
}

/// Forces Windows.Graphics.Capture; mainly useful to cross-check the other methods.
pub fn capture_monitor_wgc(monitor: &MonitorInfo, hdr: &HdrSettings) -> Result<MonitorCapture> {
    Ok(capture_monitor_via(monitor, hdr, &[CaptureMethod::Wgc])?.0)
}

/// Forces the GDI path (SDR only); mainly useful to cross-check the other methods.
pub fn capture_monitor_gdi(monitor: &MonitorInfo) -> Result<MonitorCapture> {
    Ok(capture_monitor_via(monitor, &HdrSettings::default(), &[CaptureMethod::Gdi])?.0)
}

/// Tries `order` until one method yields a frame. All GPU objects are released before the tone map runs.
pub fn capture_monitor_via(
    monitor: &MonitorInfo,
    hdr: &HdrSettings,
    order: &[CaptureMethod],
) -> Result<(MonitorCapture, CaptureReport)> {
    let started = Instant::now();
    let mut abandoned = Vec::new();
    let mut gpu: Option<Result<(DxgiOutput, Gpu)>> = None;
    for &method in order {
        let attempt = match method {
            CaptureMethod::Gdi => gdi::grab(monitor.rect).map(DesktopFrame::Bgra),
            CaptureMethod::Dda | CaptureMethod::Wgc => match gpu.get_or_insert_with(|| open_gpu(monitor)) {
                Ok((target, gpu)) if method == CaptureMethod::Dda => duplication::grab(monitor, &target.output, gpu),
                Ok((_, gpu)) => graphics_capture::grab(monitor, gpu),
                Err(error) => Err(anyhow!("GPU setup: {error:#}")),
            },
        };
        match attempt {
            Ok(frame) => {
                drop(gpu);
                return Ok(finish(monitor, hdr, frame, method, started, abandoned));
            }
            Err(error) => {
                log::info!("{}: {method} yielded no frame ({error:#})", monitor.device_name);
                abandoned.push(format!("{method}: {error:#}"));
            }
        }
    }
    bail!("every capture method failed for {}: {}", monitor.device_name, abandoned.join("; "))
}

fn open_gpu(monitor: &MonitorInfo) -> Result<(DxgiOutput, Gpu)> {
    let target = dxgi::find_output(monitor.handle)?;
    let gpu = Gpu::for_adapter(&target.adapter)?;
    log::debug!("{}: using a {} D3D11 device", monitor.device_name, if gpu.is_warm() { "warm" } else { "fresh" });
    Ok((target, gpu))
}

fn finish(
    monitor: &MonitorInfo,
    hdr: &HdrSettings,
    frame: DesktopFrame,
    method: CaptureMethod,
    started: Instant,
    abandoned: Vec<String>,
) -> (MonitorCapture, CaptureReport) {
    let grab = started.elapsed();
    let (path, capture, tonemap) = match (frame, method) {
        (DesktopFrame::Bgra(sdr), method) => (
            match method {
                CaptureMethod::Dda => CapturePath::DdaBgra,
                CaptureMethod::Wgc => CapturePath::WgcBgra,
                CaptureMethod::Gdi => CapturePath::Gdi,
            },
            MonitorCapture { monitor: monitor.clone(), sdr, hdr: None, hdr_stats: None },
            Duration::ZERO,
        ),
        (DesktopFrame::Fp16(frame), method) => {
            let tonemap_started = Instant::now();
            let image = hdr_image(monitor, frame);
            let params = ToneMapParams { mode: hdr.mode, exposure_stops: hdr.exposure_stops };
            let stats = analyze(&image);
            let sdr = tonemap_with_stats(&image, &params, &stats);
            let capture =
                MonitorCapture { monitor: monitor.clone(), sdr, hdr: Some(image), hdr_stats: Some(stats) };
            let path = if method == CaptureMethod::Dda { CapturePath::DdaFp16 } else { CapturePath::WgcFp16 };
            (path, capture, tonemap_started.elapsed())
        }
    };
    let total = started.elapsed();
    log::info!("{}: captured via {path} in {total:?} (grab {grab:?}, tone map {tonemap:?})", monitor.device_name);
    let fallback_reason = (!abandoned.is_empty()).then(|| abandoned.join("; "));
    (capture, CaptureReport { path, fallback_reason, grab, tonemap, total })
}

fn hdr_image(monitor: &MonitorInfo, frame: Fp16Frame) -> HdrImage {
    let sdr_white_nits = monitor.hdr.map_or(DEFAULT_SDR_WHITE_NITS, |hdr| hdr.sdr_white_nits);
    let display_peak_nits = monitor
        .hdr
        .map(|hdr| hdr.max_nits)
        .filter(|nits| *nits > 0.0)
        .unwrap_or(DEFAULT_DISPLAY_PEAK_NITS);
    HdrImage { width: frame.width, height: frame.height, data: frame.data, sdr_white_nits, display_peak_nits }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_parse_case_insensitively() {
        assert_eq!("WGC".parse::<CaptureMethod>().unwrap(), CaptureMethod::Wgc);
        assert_eq!("gdi".parse::<CaptureMethod>().unwrap(), CaptureMethod::Gdi);
        assert!("bitblt".parse::<CaptureMethod>().is_err());
    }

    #[test]
    fn default_order_is_dda_wgc_gdi() {
        assert_eq!(CaptureMethod::DEFAULT_ORDER, [CaptureMethod::Dda, CaptureMethod::Wgc, CaptureMethod::Gdi]);
    }
}
