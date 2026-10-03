//! Headless verification for glint-capture (DESIGN.md section 11): prints monitors, windows and capture timings,
//! writes `monitor-N.png` (tone mapped) and `monitor-N.json` per monitor. Shows no window, installs no hook,
//! touches neither clipboard nor registry.
//!
//! cargo run -p glint-capture --release --example probe -- --out <dir> [--runs N] [--warm] [--order dda,wgc,gdi]
//!     [--compare-gdi] [--wgc] [--cpu-tonemap]
//!     [--burn-cpu N] [--verbose]

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glint_capture::{
    CaptureMethod, CaptureReport, capture_all_streaming, capture_all_via, capture_monitor_gdi, capture_monitor_wgc,
};
use glint_core::settings::HdrSettings;
use glint_core::{Image, ImageFormat, MonitorCapture, MonitorInfo, WindowInfo};
use serde_json::{Value, json};

const WINDOWS_SHOWN: usize = 15;
const TITLE_WIDTH: usize = 48;
const DEFAULT_RUNS: usize = 2;

struct Args {
    out: PathBuf,
    runs: usize,
    warm: bool,
    order: Option<Vec<CaptureMethod>>,
    compare_gdi: bool,
    cpu_tonemap: bool,
    burn_cpu: usize,
    wgc: bool,
    verbose: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        out: std::env::temp_dir().join("glint-probe"),
        runs: DEFAULT_RUNS,
        warm: false,
        order: None,
        compare_gdi: false,
        cpu_tonemap: false,
        burn_cpu: 0,
        wgc: false,
        verbose: false,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--out" => args.out = iter.next().context("--out needs a directory")?.into(),
            "--runs" => args.runs = iter.next().context("--runs needs a number")?.parse()?,
            "--warm" => args.warm = true,
            "--order" => {
                let list = iter.next().context("--order needs a comma separated list")?;
                args.order = Some(list.split(',').map(str::parse).collect::<Result<_>>()?);
            }
            "--compare-gdi" => args.compare_gdi = true,
            "--wgc" => args.wgc = true,
            "--cpu-tonemap" => args.cpu_tonemap = true,
            "--burn-cpu" => args.burn_cpu = iter.next().context("--burn-cpu needs a thread count")?.parse()?,
            "--verbose" => args.verbose = true,
            other => bail!("unknown argument {other}"),
        }
    }
    args.runs = args.runs.max(1);
    Ok(args)
}

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn main() -> Result<()> {
    let args = parse_args()?;
    log::set_logger(&StderrLogger).ok();
    log::set_max_level(if args.verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info });
    glint_capture::enable_per_monitor_dpi_awareness();
    glint_capture::set_gpu_tonemap(!args.cpu_tonemap);
    burn_cpu(args.burn_cpu);
    std::fs::create_dir_all(&args.out).with_context(|| format!("create {}", args.out.display()))?;

    let monitors = glint_capture::monitors()?;
    print_monitors(&monitors);
    print_windows();

    if args.warm {
        let started = Instant::now();
        let devices = glint_capture::warm_up()?;
        println!("warm_up: {devices} idle device(s) in {:.1} ms", ms(started.elapsed()));
    }

    let settings = HdrSettings::default();
    let mut captures = Vec::new();
    for run in 1..=args.runs {
        let started = Instant::now();
        let sdr_ready: Mutex<Vec<(usize, f64)>> = Mutex::new(Vec::new());
        let on_sdr = |index: usize, _: &MonitorInfo, _: &Image| {
            sdr_ready.lock().unwrap().push((index, ms(started.elapsed())));
        };
        captures = match &args.order {
            Some(order) => capture_all_via(&settings, order)?,
            None => capture_all_streaming(&settings, &on_sdr)?,
        };
        let wall = ms(started.elapsed());
        let sdr_ready = sdr_ready.into_inner().unwrap();
        let per_monitor: Vec<String> = captures
            .iter()
            .enumerate()
            .map(|(index, (capture, report))| {
                let ready = sdr_ready.iter().find(|(i, _)| *i == index).map_or(String::new(), |(_, t)| format!(", sdr at {t:.0}"));
                format!("{} {} {:.0} ms{ready}", capture.monitor.device_name, report.path, ms(report.total))
            })
            .collect();
        println!("capture_all run {run}: {wall:.1} ms wall [{}]", per_monitor.join(" | "));
    }

    println!();
    for (index, (capture, report)) in captures.iter().enumerate() {
        report_capture(index, capture, report, &args)?;
    }
    println!("\noutput: {}", args.out.display());
    Ok(())
}

/// Spins `threads` normal-priority threads for the rest of the process to imitate a game saturating the CPU.
fn burn_cpu(threads: usize) {
    for _ in 0..threads {
        std::thread::spawn(|| {
            let mut value = 1u64;
            loop {
                value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407));
            }
        });
    }
}

fn print_monitors(monitors: &[MonitorInfo]) {
    println!("Monitors ({})", monitors.len());
    for (index, monitor) in monitors.iter().enumerate() {
        let r = monitor.rect;
        println!(
            "  [{index}] {} \"{}\" rect {}x{} at ({},{}) dpi {} ({:.0}%){}",
            monitor.device_name,
            monitor.friendly_name,
            r.w,
            r.h,
            r.x,
            r.y,
            monitor.dpi,
            monitor.scale() * 100.0,
            if monitor.primary { " primary" } else { "" }
        );
        match monitor.hdr {
            Some(hdr) => println!(
                "      HDR on: sdr white {:.0} nits, peak {:.0}, full-frame {:.0}, min {:.4}",
                hdr.sdr_white_nits, hdr.max_nits, hdr.max_full_frame_nits, hdr.min_nits
            ),
            None => println!("      HDR off (SDR)"),
        }
    }
    println!();
}

fn print_windows() {
    let windows = glint_capture::windows_snapshot(Some(std::process::id()));
    println!("Windows: {} in snapshot, top {}", windows.len(), WINDOWS_SHOWN.min(windows.len()));
    for window in windows.iter().take(WINDOWS_SHOWN) {
        print_window(window);
    }
    if let Some(foreground) = glint_capture::foreground_window() {
        println!("  foreground: hwnd {:#x} \"{}\"", foreground.hwnd, truncate(&foreground.title, TITLE_WIDTH));
    }
    println!();
}

fn print_window(window: &WindowInfo) {
    let r = window.rect;
    println!(
        "  z{:<3} pid {:<6} {:<28} {:>5}x{:<5} at ({},{})  \"{}\"",
        window.z_order,
        window.process_id,
        truncate(&window.class_name, 28),
        r.w,
        r.h,
        r.x,
        r.y,
        truncate(&window.title, TITLE_WIDTH)
    );
}

fn report_capture(index: usize, capture: &MonitorCapture, report: &CaptureReport, args: &Args) -> Result<()> {
    let monitor = &capture.monitor;
    println!(
        "monitor-{index} {}: path {}  {}x{}  grab {:.1} ms, tone map {:.1} ms, total {:.1} ms",
        monitor.device_name,
        report.path,
        capture.sdr.width,
        capture.sdr.height,
        ms(report.grab),
        ms(report.tonemap),
        ms(report.total)
    );
    let stages = &report.stages;
    println!(
        "  stages: setup {:.1} ms, acquire {:.1}, copy+map {:.1}, convert {:.1}, tone map {:.1}",
        ms(stages.setup),
        ms(stages.acquire),
        ms(stages.copy_map),
        ms(stages.convert),
        ms(stages.tonemap)
    );
    if let Some(reason) = &report.fallback_reason {
        println!("  earlier paths abandoned: {reason}");
    }
    let summary = summarize(&capture.sdr);
    println!(
        "  sdr: mean rgb ({:.1}, {:.1}, {:.1}), black pixels {:.2}%",
        summary.mean_rgb[0],
        summary.mean_rgb[1],
        summary.mean_rgb[2],
        summary.black_fraction * 100.0
    );
    if let Some(stats) = &capture.hdr_stats {
        println!(
            "  hdr: peak {:.2}x sdr white, max {:.2}x, hdr pixels {:.3}%, out of gamut {:.3}%, has_hdr_content {}",
            stats.peak,
            stats.max,
            stats.hdr_fraction * 100.0,
            stats.out_of_gamut_fraction * 100.0,
            stats.has_hdr_content()
        );
    }
    if args.compare_gdi {
        compare_with_gdi(capture)?;
    }
    if args.wgc {
        compare_with_wgc(index, capture, args)?;
    }

    let png = args.out.join(format!("monitor-{index}.png"));
    glint_core::encode::save(&capture.sdr, &png, ImageFormat::Png)?;
    let json_path = args.out.join(format!("monitor-{index}.json"));
    std::fs::write(&json_path, serde_json::to_string_pretty(&monitor_json(capture, report, &summary))?)?;
    println!("  wrote {} and {}", png.display(), json_path.display());
    Ok(())
}

fn compare_with_gdi(capture: &MonitorCapture) -> Result<()> {
    let gdi = capture_monitor_gdi(&capture.monitor)?;
    print_diff("gdi", &capture.sdr, &gdi.sdr);
    Ok(())
}

fn compare_with_wgc(index: usize, capture: &MonitorCapture, args: &Args) -> Result<()> {
    let mut wgc = None;
    for run in 1..=args.runs {
        let started = Instant::now();
        wgc = Some(capture_monitor_wgc(&capture.monitor, &HdrSettings::default())?);
        println!("  forced WGC run {run}: {:.1} ms (includes the tone map for HDR monitors)", ms(started.elapsed()));
    }
    let wgc = wgc.context("--runs is at least 1")?;
    print_diff("wgc", &capture.sdr, &wgc.sdr);
    glint_core::encode::save(&wgc.sdr, &args.out.join(format!("monitor-{index}-wgc.png")), ImageFormat::Png)
}

fn print_diff(label: &str, reference: &Image, other: &Image) {
    if (other.width, other.height) != (reference.width, reference.height) {
        println!("  {label} compare: size differs ({}x{})", other.width, other.height);
        return;
    }
    let mut total_diff = 0u64;
    let mut differing = 0usize;
    for (a, b) in reference.data.chunks_exact(4).zip(other.data.chunks_exact(4)) {
        let diff: u32 = (0..3).map(|c| a[c].abs_diff(b[c]) as u32).sum();
        total_diff += diff as u64;
        differing += (diff > 24) as usize;
    }
    let pixels = (reference.width as usize * reference.height as usize).max(1);
    println!(
        "  {label} compare: mean abs channel diff {:.2}, pixels differing by more than 24 summed levels {:.2}%",
        total_diff as f64 / (pixels * 3) as f64,
        differing as f64 / pixels as f64 * 100.0
    );
}

struct Summary {
    mean_rgb: [f64; 3],
    black_fraction: f64,
}

fn summarize(image: &Image) -> Summary {
    let mut sums = [0u64; 3];
    let mut black = 0usize;
    for pixel in image.data.chunks_exact(4) {
        (0..3).for_each(|c| sums[c] += pixel[c] as u64);
        black += (pixel[..3] == [0, 0, 0]) as usize;
    }
    let pixels = (image.width as usize * image.height as usize).max(1) as f64;
    // BGRA storage: channel 2 is red.
    Summary {
        mean_rgb: [sums[2] as f64 / pixels, sums[1] as f64 / pixels, sums[0] as f64 / pixels],
        black_fraction: black as f64 / pixels,
    }
}

fn monitor_json(capture: &MonitorCapture, report: &CaptureReport, summary: &Summary) -> Value {
    let monitor = &capture.monitor;
    json!({
        "monitor": {
            "handle": monitor.handle,
            "device_name": monitor.device_name,
            "friendly_name": monitor.friendly_name,
            "rect": monitor.rect,
            "work_rect": monitor.work_rect,
            "dpi": monitor.dpi,
            "primary": monitor.primary,
            "hdr": monitor.hdr.map(|hdr| json!({
                "sdr_white_nits": hdr.sdr_white_nits,
                "max_nits": hdr.max_nits,
                "max_full_frame_nits": hdr.max_full_frame_nits,
                "min_nits": hdr.min_nits,
            })),
        },
        "capture": {
            "path": report.path.to_string(),
            "fallback_reason": report.fallback_reason,
            "grab_ms": ms(report.grab),
            "tonemap_ms": ms(report.tonemap),
            "total_ms": ms(report.total),
            "width": capture.sdr.width,
            "height": capture.sdr.height,
            "has_hdr_image": capture.hdr.is_some(),
        },
        "sdr_summary": { "mean_rgb": summary.mean_rgb, "black_fraction": summary.black_fraction },
        "hdr_stats": capture.hdr_stats.map(|stats| json!({
            "peak": stats.peak,
            "max": stats.max,
            "hdr_fraction": stats.hdr_fraction,
            "out_of_gamut_fraction": stats.out_of_gamut_fraction,
            "has_hdr_content": stats.has_hdr_content(),
        })),
    })
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn truncate(text: &str, width: usize) -> String {
    match text.char_indices().nth(width) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_string(),
    }
}
