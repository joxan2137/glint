//! Headless recording check (DESIGN §11). Shows no window.
//!
//! cargo run -p glint-record --release --example rec -- --seconds 3 --out <file.mp4>
//!     [--monitor N] [--region x,y,w,h] [--fps 60] [--no-audio] [--mic] [--pause-at 1] [--thumb <file.png>]
//!     [--clip] [--exposure <stops>] [--no-cursor] [--discard]

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use glint_core::{RectI, ToneMapMode, ToneMapParams};
use glint_record::{RecordConfig, Recorder, RecorderStatus};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

const PAUSE_LENGTH: Duration = Duration::from_millis(1500);
const START_TIMEOUT: Duration = Duration::from_secs(20);

struct Args {
    seconds: f64,
    out: PathBuf,
    monitor: Option<usize>,
    region: Option<RectI>,
    fps: u32,
    system_audio: bool,
    microphone: bool,
    pause_at: Option<f64>,
    thumb: Option<PathBuf>,
    tonemap: ToneMapParams,
    include_cursor: bool,
    discard: bool,
}

impl Args {
    fn parse() -> anyhow::Result<Self> {
        let mut args = Args {
            seconds: 3.0,
            out: std::env::temp_dir().join("glint-rec").join("rec.mp4"),
            monitor: None,
            region: None,
            fps: 30,
            system_audio: true,
            microphone: false,
            pause_at: None,
            thumb: None,
            tonemap: ToneMapParams::default(),
            include_cursor: true,
            discard: false,
        };
        let mut raw = std::env::args().skip(1);
        while let Some(flag) = raw.next() {
            let mut value = || raw.next().with_context(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--seconds" => args.seconds = value()?.parse()?,
                "--out" => args.out = value()?.into(),
                "--monitor" => args.monitor = Some(value()?.parse()?),
                "--region" => args.region = Some(parse_region(&value()?)?),
                "--fps" => args.fps = value()?.parse()?,
                "--no-audio" => args.system_audio = false,
                "--mic" => args.microphone = true,
                "--pause-at" => args.pause_at = Some(value()?.parse()?),
                "--thumb" => args.thumb = Some(value()?.into()),
                "--clip" => args.tonemap.mode = ToneMapMode::Clip,
                "--exposure" => args.tonemap.exposure_stops = value()?.parse()?,
                "--no-cursor" => args.include_cursor = false,
                "--discard" => args.discard = true,
                other => bail!("unknown argument {other}"),
            }
        }
        Ok(args)
    }
}

fn parse_region(text: &str) -> anyhow::Result<RectI> {
    let parts: Vec<i32> = text.split(',').map(|part| part.trim().parse()).collect::<Result<_, _>>()?;
    let [x, y, w, h] = parts[..] else { bail!("--region expects x,y,w,h") };
    Ok(RectI::new(x, y, w, h))
}

fn main() -> anyhow::Result<()> {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    log::set_logger(&StderrLogger).ok();
    log::set_max_level(if std::env::var_os("GLINT_LOG_DEBUG").is_some() {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
    let args = Args::parse()?;

    let monitors = monitors::enumerate()?;
    for (index, monitor) in monitors.iter().enumerate() {
        println!(
            "monitor {index}: {} ({}) {:?} dpi {} primary {} hdr {:?}",
            monitor.friendly_name, monitor.device_name, monitor.rect, monitor.dpi, monitor.primary, monitor.hdr
        );
    }
    let monitor = match args.monitor {
        Some(index) => monitors.get(index).cloned().with_context(|| format!("no monitor {index}"))?,
        None => monitors.iter().find(|m| m.primary).or(monitors.first()).cloned().context("no monitors")?,
    };
    let region = args.region.unwrap_or(RectI::new(0, 0, monitor.rect.w, monitor.rect.h));

    let launched = Instant::now();
    let recorder = Recorder::start(RecordConfig {
        monitor,
        region,
        fps: args.fps,
        system_audio: args.system_audio,
        microphone: args.microphone,
        include_cursor: args.include_cursor,
        output: args.out.clone(),
        tonemap: args.tonemap,
    })?;
    println!("start() returned after {:?}", launched.elapsed());
    while recorder.status() == RecorderStatus::Starting {
        if launched.elapsed() > START_TIMEOUT {
            bail!("recording did not start within {START_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(5));
    }
    println!("started after {:?}", launched.elapsed());

    let target = Duration::from_secs_f64(args.seconds);
    let pause_at = args.pause_at.map(Duration::from_secs_f64);
    let mut paused_once = false;
    let mut loudest = 0.0f32;
    let mut slowest_call = Duration::ZERO;
    loop {
        let call = Instant::now();
        let elapsed = recorder.elapsed();
        loudest = loudest.max(recorder.audio_level());
        let status = recorder.status();
        slowest_call = slowest_call.max(call.elapsed());
        if let RecorderStatus::Failed(message) = status {
            bail!("recording failed: {message}");
        }
        if let Some(pause_at) = pause_at
            && !paused_once
            && elapsed >= pause_at
        {
            recorder.pause();
            println!("paused at {elapsed:?} for {PAUSE_LENGTH:?}");
            thread::sleep(PAUSE_LENGTH);
            println!("elapsed while paused: {:?}", recorder.elapsed());
            recorder.resume();
            paused_once = true;
        }
        if elapsed >= target {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    if args.discard {
        recorder.discard();
        thread::sleep(Duration::from_secs(1));
        println!("discarded; file still exists: {}", args.out.exists());
        return Ok(());
    }
    let stopping = Instant::now();
    let info = recorder.stop()?;
    println!("stop took {:?}; slowest status call {slowest_call:?}; loudest level {loudest:.3}", stopping.elapsed());
    println!(
        "wrote {} ({} bytes): {}x{}, {:?}",
        info.path.display(),
        info.bytes,
        info.width,
        info.height,
        info.duration
    );
    for warning in &info.warnings {
        println!("warning: {warning}");
    }
    if let (Some(path), Some(thumbnail)) = (args.thumb, info.thumbnail) {
        write_png(&path, &thumbnail)?;
        println!("thumbnail {}", path.display());
    }
    Ok(())
}

fn write_png(path: &std::path::Path, image: &glint_core::Image) -> anyhow::Result<()> {
    let rgba: Vec<u8> = image.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    let mut encoder =
        png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) && record.level() <= log::max_level() {
            eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

/// Minimal monitor enumeration (glint-capture owns the real one): rects, DPI, HDR state and SDR white level.
mod monitors {
    use glint_core::{HdrInfo, MonitorInfo, RectI};
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
        GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, LUID, RECT};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_OUTPUT_DESC1, IDXGIFactory1, IDXGIOutput6};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
    use windows::core::{BOOL, Interface};

    struct DisplayTarget {
        gdi_name: String,
        friendly_name: String,
        sdr_white_nits: Option<f32>,
    }

    pub fn enumerate() -> anyhow::Result<Vec<MonitorInfo>> {
        let mut handles: Vec<HMONITOR> = Vec::new();
        unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
            unsafe { (*(data.0 as *mut Vec<HMONITOR>)).push(monitor) };
            true.into()
        }
        unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut handles as *mut _ as isize)) }.ok()?;
        let outputs = dxgi_outputs();
        let targets = display_targets();
        handles.into_iter().map(|handle| describe(handle, &outputs, &targets)).collect()
    }

    fn describe(
        handle: HMONITOR,
        outputs: &[DXGI_OUTPUT_DESC1],
        targets: &[DisplayTarget],
    ) -> anyhow::Result<MonitorInfo> {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        unsafe { GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.ok()?;
        let device_name = utf16(&info.szDevice);
        let (mut dpi, mut dpi_y) = (96, 96);
        let _ = unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) };
        let target = targets.iter().find(|target| target.gdi_name == device_name);
        let hdr = outputs
            .iter()
            .find(|output| output.Monitor == handle && output.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
            .map(|output| HdrInfo {
                sdr_white_nits: target.and_then(|target| target.sdr_white_nits).unwrap_or(80.0),
                max_nits: output.MaxLuminance,
                max_full_frame_nits: output.MaxFullFrameLuminance,
                min_nits: output.MinLuminance,
            });
        Ok(MonitorInfo {
            handle: handle.0 as isize,
            friendly_name: target
                .map(|target| target.friendly_name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| device_name.clone()),
            device_name,
            rect: rect(info.monitorInfo.rcMonitor),
            work_rect: rect(info.monitorInfo.rcWork),
            dpi,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            hdr,
        })
    }

    fn rect(r: RECT) -> RectI {
        RectI::from_ltrb(r.left, r.top, r.right, r.bottom)
    }

    fn utf16(text: &[u16]) -> String {
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        String::from_utf16_lossy(&text[..end])
    }

    fn dxgi_outputs() -> Vec<DXGI_OUTPUT_DESC1> {
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
            return Vec::new();
        };
        let adapters: Vec<_> = (0..).map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok()).collect();
        adapters
            .iter()
            .flat_map(|adapter| (0..).map_while(move |index| unsafe { adapter.EnumOutputs(index) }.ok()))
            .filter_map(|output| unsafe { output.cast::<IDXGIOutput6>().ok()?.GetDesc1().ok() })
            .collect()
    }

    fn header<T>(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
        DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size_of::<T>() as u32, adapterId: adapter, id }
    }

    fn display_targets() -> Vec<DisplayTarget> {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) }
            != ERROR_SUCCESS
        {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let queried = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if queried != ERROR_SUCCESS {
            return Vec::new();
        }
        paths.truncate(path_count as usize);
        paths
            .iter()
            .map(|path| {
                let source = &path.sourceInfo;
                let target = &path.targetInfo;
                let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                    header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
                        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                        source.adapterId,
                        source.id,
                    ),
                    ..Default::default()
                };
                let mut target_name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                    header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
                        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                        target.adapterId,
                        target.id,
                    ),
                    ..Default::default()
                };
                let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                    header: header::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(
                        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                        target.adapterId,
                        target.id,
                    ),
                    ..Default::default()
                };
                unsafe {
                    DisplayConfigGetDeviceInfo(&mut source_name.header);
                    DisplayConfigGetDeviceInfo(&mut target_name.header);
                }
                let white_ok = unsafe { DisplayConfigGetDeviceInfo(&mut white.header) } == 0;
                DisplayTarget {
                    gdi_name: utf16(&source_name.viewGdiDeviceName),
                    friendly_name: utf16(&target_name.monitorFriendlyDeviceName),
                    sdr_white_nits: white_ok.then(|| white.SDRWhiteLevel as f32 / 1000.0 * 80.0),
                }
            })
            .collect()
    }
}
