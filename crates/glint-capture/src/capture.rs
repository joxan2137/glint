use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Error, Result, anyhow, bail};
use glint_core::settings::HdrSettings;
use glint_core::{Image, MonitorCapture, MonitorInfo};
use windows::Win32::System::Threading::THREAD_PRIORITY_ABOVE_NORMAL;

use crate::duplication;
use crate::dxgi::{self, DxgiOutput};
use crate::gdi;
use crate::gpu::Gpu;
use crate::graphics_capture;
use crate::monitors::monitors;
use crate::pipeline::{Grabbed, StageTimings};
use crate::threads;

/// How a monitor image is grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMethod {
    /// DXGI desktop duplication; waits up to 30 ms for a frame, so a static desktop yields nothing.
    Dda,
    /// Windows.Graphics.Capture, one frame; works on a static desktop and keeps HDR intact.
    Wgc,
    /// GDI BitBlt; SDR only, fast.
    Gdi,
}

impl CaptureMethod {
    /// The order a normal capture tries: SDR monitors fall back to GDI (exact for SDR, fastest), HDR monitors to
    /// Windows.Graphics.Capture (GDI would clip the highlights) and only then to GDI.
    pub fn default_order(monitor: &MonitorInfo) -> &'static [CaptureMethod] {
        if monitor.hdr.is_some() {
            &[CaptureMethod::Dda, CaptureMethod::Wgc, CaptureMethod::Gdi]
        } else {
            &[CaptureMethod::Dda, CaptureMethod::Gdi]
        }
    }
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
    /// Everything except the tone map.
    pub grab: Duration,
    /// Statistics, curve and tone map orchestration; zero for SDR frames.
    pub tonemap: Duration,
    pub total: Duration,
    pub stages: StageTimings,
}

/// Called on a capture thread with (monitor index in `monitors()` order, monitor, image) as soon as a monitor's
/// tone-mapped image exists, before its raw HDR data has finished copying.
pub type SdrReady<'a> = &'a (dyn Fn(usize, &MonitorInfo, &Image) + Sync);

/// Captures every monitor in parallel (one thread each, slightly above normal priority), in `monitors()` order.
/// Monitors that fail on every method are logged and skipped; all failing is an error.
pub fn capture_all(hdr: &HdrSettings) -> Result<Vec<MonitorCapture>> {
    Ok(capture_all_reported(hdr)?.into_iter().map(|(capture, _)| capture).collect())
}

pub fn capture_all_reported(hdr: &HdrSettings) -> Result<Vec<(MonitorCapture, CaptureReport)>> {
    capture_all_streaming(hdr, &|_, _, _| {})
}

/// Like `capture_all_reported`, but `on_sdr` fires per monitor as soon as its `sdr` image is ready.
pub fn capture_all_streaming(hdr: &HdrSettings, on_sdr: SdrReady) -> Result<Vec<(MonitorCapture, CaptureReport)>> {
    capture_all_impl(hdr, None, on_sdr)
}

/// Like `capture_all_reported` with one explicit method order for every monitor, e.g. `[Wgc, Gdi]`.
pub fn capture_all_via(hdr: &HdrSettings, order: &[CaptureMethod]) -> Result<Vec<(MonitorCapture, CaptureReport)>> {
    capture_all_impl(hdr, Some(order), &|_, _, _| {})
}

fn capture_all_impl(
    hdr: &HdrSettings,
    order: Option<&[CaptureMethod]>,
    on_sdr: SdrReady,
) -> Result<Vec<(MonitorCapture, CaptureReport)>> {
    let monitors = monitors()?;
    let results: Vec<Result<(MonitorCapture, CaptureReport)>> = std::thread::scope(|scope| {
        let workers: Vec<_> = monitors
            .iter()
            .enumerate()
            .map(|(index, monitor)| {
                scope.spawn(move || {
                    threads::set_current_priority(THREAD_PRIORITY_ABOVE_NORMAL);
                    let order = order.unwrap_or_else(|| CaptureMethod::default_order(monitor));
                    capture_monitor_impl(monitor, hdr, order, &|image| on_sdr(index, monitor, image))
                })
            })
            .collect();
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

/// `CaptureMethod::default_order` for this monitor. FP16 frames are tone mapped on the GPU.
pub fn capture_monitor_reported(monitor: &MonitorInfo, hdr: &HdrSettings) -> Result<(MonitorCapture, CaptureReport)> {
    capture_monitor_via(monitor, hdr, CaptureMethod::default_order(monitor))
}

/// Forces Windows.Graphics.Capture; mainly useful to cross-check the other methods.
pub fn capture_monitor_wgc(monitor: &MonitorInfo, hdr: &HdrSettings) -> Result<MonitorCapture> {
    Ok(capture_monitor_via(monitor, hdr, &[CaptureMethod::Wgc])?.0)
}

/// Forces the GDI path (SDR only); mainly useful to cross-check the other methods.
pub fn capture_monitor_gdi(monitor: &MonitorInfo) -> Result<MonitorCapture> {
    Ok(capture_monitor_via(monitor, &HdrSettings::default(), &[CaptureMethod::Gdi])?.0)
}

/// Tries `order` until one method yields a frame. All GPU objects are released before the CPU copies finish.
pub fn capture_monitor_via(
    monitor: &MonitorInfo,
    hdr: &HdrSettings,
    order: &[CaptureMethod],
) -> Result<(MonitorCapture, CaptureReport)> {
    capture_monitor_impl(monitor, hdr, order, &|_| {})
}

fn capture_monitor_impl(
    monitor: &MonitorInfo,
    hdr: &HdrSettings,
    order: &[CaptureMethod],
    on_sdr: &dyn Fn(&Image),
) -> Result<(MonitorCapture, CaptureReport)> {
    let started = Instant::now();
    let mut clock = StageTimings::default();
    let mut abandoned = Vec::new();
    let mut gpu: Option<Result<(DxgiOutput, Gpu)>> = None;
    for &method in order {
        let attempt = match method {
            CaptureMethod::Gdi => grab_gdi(monitor, on_sdr, &mut clock),
            CaptureMethod::Dda | CaptureMethod::Wgc => match gpu.get_or_insert_with(|| open_gpu(monitor)) {
                Ok((target, gpu)) if method == CaptureMethod::Dda => {
                    duplication::grab(monitor, &target.output, gpu, hdr, on_sdr, &mut clock)
                }
                Ok((_, gpu)) => graphics_capture::grab(monitor, gpu, hdr, on_sdr, &mut clock),
                Err(error) => Err(anyhow!("GPU setup: {error:#}")),
            },
        };
        match attempt {
            Ok(grabbed) => {
                drop(gpu);
                return Ok(report(monitor, grabbed, method, started, clock, abandoned));
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

fn grab_gdi(monitor: &MonitorInfo, on_sdr: &dyn Fn(&Image), clock: &mut StageTimings) -> Result<Grabbed> {
    let started = Instant::now();
    let sdr = gdi::grab(monitor.rect)?;
    clock.copy_map += started.elapsed();
    on_sdr(&sdr);
    Ok(Grabbed { sdr, hdr: None, stats: None })
}

fn report(
    monitor: &MonitorInfo,
    grabbed: Grabbed,
    method: CaptureMethod,
    started: Instant,
    stages: StageTimings,
    abandoned: Vec<String>,
) -> (MonitorCapture, CaptureReport) {
    let path = match (method, grabbed.hdr.is_some()) {
        (CaptureMethod::Dda, true) => CapturePath::DdaFp16,
        (CaptureMethod::Dda, false) => CapturePath::DdaBgra,
        (CaptureMethod::Wgc, true) => CapturePath::WgcFp16,
        (CaptureMethod::Wgc, false) => CapturePath::WgcBgra,
        (CaptureMethod::Gdi, _) => CapturePath::Gdi,
    };
    let total = started.elapsed();
    let capture = MonitorCapture {
        monitor: monitor.clone(),
        sdr: grabbed.sdr,
        hdr: grabbed.hdr,
        hdr_stats: grabbed.stats,
    };
    let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
    log::info!(
        "{}: {path} {:.1} ms (setup {:.1}, acquire {:.1}, copy+map {:.1}, convert {:.1}, tone map {:.1})",
        monitor.device_name,
        ms(total),
        ms(stages.setup),
        ms(stages.acquire),
        ms(stages.copy_map),
        ms(stages.convert),
        ms(stages.tonemap)
    );
    let fallback_reason = (!abandoned.is_empty()).then(|| abandoned.join("; "));
    let report = CaptureReport {
        path,
        fallback_reason,
        grab: total.saturating_sub(stages.tonemap),
        tonemap: stages.tonemap,
        total,
        stages,
    };
    (capture, report)
}

#[cfg(test)]
mod tests {
    use glint_core::{HdrInfo, RectI};

    use super::*;

    fn monitor(hdr: bool) -> MonitorInfo {
        MonitorInfo {
            handle: 0,
            device_name: "test".into(),
            friendly_name: "test".into(),
            rect: RectI::new(0, 0, 100, 100),
            work_rect: RectI::new(0, 0, 100, 100),
            dpi: 96,
            primary: true,
            hdr: hdr.then_some(HdrInfo { sdr_white_nits: 200.0, max_nits: 1000.0, max_full_frame_nits: 600.0, min_nits: 0.0 }),
        }
    }

    #[test]
    fn methods_parse_case_insensitively() {
        assert_eq!("WGC".parse::<CaptureMethod>().unwrap(), CaptureMethod::Wgc);
        assert_eq!("gdi".parse::<CaptureMethod>().unwrap(), CaptureMethod::Gdi);
        assert!("bitblt".parse::<CaptureMethod>().is_err());
    }

    #[test]
    fn sdr_monitors_skip_wgc_and_hdr_monitors_keep_it() {
        assert_eq!(CaptureMethod::default_order(&monitor(false)), [CaptureMethod::Dda, CaptureMethod::Gdi]);
        assert_eq!(
            CaptureMethod::default_order(&monitor(true)),
            [CaptureMethod::Dda, CaptureMethod::Wgc, CaptureMethod::Gdi]
        );
    }
}
