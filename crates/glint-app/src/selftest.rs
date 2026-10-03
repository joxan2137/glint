//! `--selftest [--json]`: headless checks with no window, hook, clipboard or registry: monitors, a real capture of
//! every monitor, tone-map timing, encode/decode, OCR of rendered text, settings round trip in a temporary data
//! directory, and every preview kind (overlay and editor kinds in child processes, so an unfinished crate shows up as
//! a failed check instead of taking the self-test down).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use glint_core::settings::HdrSettings;
use glint_core::{HdrImage, Image, ImageFormat, RectI, Settings, ThemeMode, ToneMapMode, ToneMapParams, encode, f16, tonemap};
use glint_ui::{Color, Gfx, OffscreenSpec, RectF, SizeF, TextStyle, Theme, Weight, render_offscreen};
use serde_json::json;

use crate::preview::{APP_KINDS, EDITOR_KINDS, OVERLAY_KINDS, render_app_preview};

struct Check {
    name: String,
    ok: bool,
    detail: String,
    ms: f64,
}

fn timed(name: &str, f: impl FnOnce() -> Result<String>) -> Check {
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let (ok, detail) = match result {
        Ok(Ok(detail)) => (true, detail),
        Ok(Err(error)) => (false, format!("{error:#}")),
        Err(panic) => (false, format!("panicked: {}", panic_message(&panic))),
    };
    Check { name: name.to_string(), ok, detail, ms }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

pub fn output_dir() -> PathBuf {
    std::env::temp_dir().join("glint-selftest")
}

/// Runs every check, prints a table (or JSON) and returns the process exit code.
pub fn run(json_output: bool) -> i32 {
    let out = output_dir();
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {error}", out.display());
        return 2;
    }
    let gfx = match Gfx::new() {
        Ok(gfx) => gfx,
        Err(error) => {
            eprintln!("graphics unavailable: {error:#}");
            return 2;
        }
    };
    let mut checks = vec![timed("monitors", monitors)];
    checks.extend(captures(&out));
    checks.push(timed("tonemap 3840x2160", tone_map));
    checks.push(timed("encode round trip", encode_round_trip));
    checks.push(timed("ocr", || ocr(&gfx)));
    checks.push(timed("settings round trip", settings_round_trip));
    for kind in APP_KINDS {
        let path = out.join(format!("{kind}.png"));
        checks.push(timed(&format!("preview {kind}"), || {
            let image = render_app_preview(&gfx, kind, ThemeMode::Dark, 1.0, None)?;
            encode::save(&image, &path, ImageFormat::Png)?;
            Ok(format!("{}×{} → {}", image.width, image.height, path.display()))
        }));
    }
    checks.extend(child_previews(&out));
    report(&checks, &out, json_output);
    if checks.iter().all(|c| c.ok) { 0 } else { 1 }
}

fn report(checks: &[Check], out: &Path, json_output: bool) {
    let passed = checks.iter().filter(|c| c.ok).count();
    if json_output {
        let value = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "passed": passed == checks.len(),
            "summary": format!("{passed}/{} checks passed", checks.len()),
            "output_dir": out.display().to_string(),
            "checks": checks.iter().map(|c| json!({ "name": c.name, "ok": c.ok, "detail": c.detail, "ms": (c.ms * 10.0).round() / 10.0 })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
        return;
    }
    println!("Glint {} self-test", env!("CARGO_PKG_VERSION"));
    for check in checks {
        let status = if check.ok { "PASS" } else { "FAIL" };
        println!("  {status}  {:<26} {:>8.1} ms  {}", check.name, check.ms, check.detail);
    }
    println!("{passed}/{} checks passed; images in {}", checks.len(), out.display());
}

fn monitors() -> Result<String> {
    let monitors = glint_capture::monitors()?;
    ensure!(!monitors.is_empty(), "no monitors");
    let lines: Vec<String> = monitors
        .iter()
        .map(|m| {
            let hdr = m.hdr.map_or("SDR".to_string(), |h| format!("HDR, SDR white {:.0} nits, peak {:.0}", h.sdr_white_nits, h.max_nits));
            format!("{} {}×{} @{:.0}% {hdr}", m.friendly_name, m.rect.w, m.rect.h, m.scale() * 100.0)
        })
        .collect();
    Ok(format!("{}: {}", monitors.len(), lines.join("; ")))
}

/// Share of pixels brighter than near-black, so a broken capture (all zeros) fails.
fn lit_fraction(image: &Image) -> f64 {
    let lit = image.data.chunks_exact(4).filter(|p| p[0].max(p[1]).max(p[2]) > 12).count();
    lit as f64 / (image.width as f64 * image.height as f64).max(1.0)
}

fn captures(out: &Path) -> Vec<Check> {
    let started = Instant::now();
    let captured = match glint_capture::capture_all_reported(&HdrSettings::default()) {
        Ok(captured) => captured,
        Err(error) => {
            return vec![Check { name: "capture".into(), ok: false, detail: format!("{error:#}"), ms: started.elapsed().as_secs_f64() * 1000.0 }];
        }
    };
    captured
        .iter()
        .enumerate()
        .map(|(i, (capture, report))| {
            let path = out.join(format!("monitor-{i}.png"));
            let saved = encode::save(&capture.sdr, &path, ImageFormat::Png);
            let lit = lit_fraction(&capture.sdr);
            let hdr = capture.hdr_stats.map_or(String::new(), |s| format!(", HDR peak {:.2}× SDR white", s.peak));
            let fallback = report.fallback_reason.as_deref().map_or(String::new(), |r| format!(" ({r})"));
            let ok = lit > 0.002 && saved.is_ok() && capture.sdr.width as i32 == capture.monitor.rect.w;
            Check {
                name: format!("capture monitor {i}"),
                ok,
                detail: format!(
                    "{} {}×{} via {}{fallback}: grab {:.1} ms, tone map {:.1} ms; {:.0}% lit{hdr} → {}",
                    capture.monitor.friendly_name,
                    capture.sdr.width,
                    capture.sdr.height,
                    report.path,
                    report.grab.as_secs_f64() * 1000.0,
                    report.tonemap.as_secs_f64() * 1000.0,
                    lit * 100.0,
                    path.display()
                ),
                ms: report.total.as_secs_f64() * 1000.0,
            }
        })
        .collect()
}

fn srgb_to_linear(code: u8) -> f32 {
    let c = code as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// 3840×2160 HDR test card: an SDR grey ramp on the left (one column per 8-bit code), highlights up to 6× SDR white
/// on the right.
fn hdr_test_card(width: u32, height: u32, sdr_white_nits: f32) -> HdrImage {
    let white = sdr_white_nits / 80.0;
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let value = if x < width / 2 {
                srgb_to_linear((x % 256) as u8) * white
            } else {
                let t = (x - width / 2) as f32 / (width / 2) as f32;
                let v = (y as f32 / height as f32) * 0.5 + 0.5;
                white * (0.2 + 5.8 * t * v)
            };
            let tint = [value, value * 0.92, value * 0.85];
            data.extend(tint.iter().map(|c| f16::from_f32(*c)));
            data.push(f16::from_f32(1.0));
        }
    }
    HdrImage { width, height, data, sdr_white_nits, display_peak_nits: 1000.0 }
}

fn tone_map() -> Result<String> {
    let card = hdr_test_card(3840, 2160, 280.0);
    let started = Instant::now();
    let image = tonemap::tonemap(&card, &ToneMapParams::default());
    let full_ms = started.elapsed().as_secs_f64() * 1000.0;
    ensure!(image.width == 3840 && image.height == 2160, "unexpected output size");
    let highlight = image.pixel(3839, 2159);
    ensure!(highlight[2] >= 250 && highlight[2] > highlight[0], "highlights lost: {highlight:?}");
    let ramp = RectI::new(0, 0, 256, 4);
    let exact = tonemap::tonemap_region(&card, ramp, &ToneMapParams { mode: ToneMapMode::Auto, exposure_stops: 0.0 });
    let mismatches = (0..256u32).filter(|&x| exact.pixel(x, 0)[2] != x as u8).count();
    ensure!(mismatches == 0, "{mismatches} of 256 SDR grey codes changed in a SDR-only region");
    Ok(format!("{full_ms:.1} ms for the full frame (DESIGN budget 40 ms in release); SDR ramp bit-exact"))
}

fn encode_round_trip() -> Result<String> {
    let (w, h) = (257u32, 131u32);
    let data: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, (i * 13) as u8, (i * 31) as u8, (i % 251) as u8]).collect();
    let image = Image::from_bgra(w, h, data);
    let png = encode::encode_png(&image)?;
    ensure!(encode::decode(&png)? == image, "PNG round trip changed pixels");
    let jpeg = encode::encode_jpeg(&image, 90)?;
    let decoded = encode::decode(&jpeg)?;
    ensure!((decoded.width, decoded.height) == (w, h), "JPEG size changed");
    Ok(format!("PNG {} bytes lossless, JPEG {} bytes", png.len(), jpeg.len()))
}

fn ocr(gfx: &Rc<Gfx>) -> Result<String> {
    let spec = OffscreenSpec::new(SizeF::new(420.0, 90.0), 2.0, Theme::light()).background(Color::WHITE);
    let image = render_offscreen(gfx, &spec, |_, p| {
        let style = TextStyle::new(34.0).weight(Weight::Semibold);
        p.text("Glint 12345", &style, Color::BLACK, RectF::new(24.0, 0.0, 380.0, 90.0));
    })?;
    let worker = std::thread::spawn(move || glint_sys::ocr::ocr(&image));
    let result = worker.join().map_err(|_| anyhow::anyhow!("OCR thread panicked"))??;
    ensure!(result.text.contains("12345"), "recognized {:?}", result.text);
    Ok(format!("read {:?} ({})", result.text, result.language))
}

fn settings_round_trip() -> Result<String> {
    let dir = std::env::temp_dir().join(format!("glint-selftest-settings-{}", std::process::id()));
    let previous = std::env::var_os("GLINT_DATA_DIR");
    // SAFETY: the self-test is single-threaded at this point (capture and OCR workers have joined), so no other
    // thread reads the environment concurrently.
    unsafe { std::env::set_var("GLINT_DATA_DIR", &dir) };
    let result = (|| -> Result<String> {
        let mut settings = Settings::default();
        settings.capture.delay_secs = 5;
        settings.hdr.exposure_stops = -0.5;
        settings.record.fps = 60;
        settings.appearance.theme = ThemeMode::Light;
        glint_sys::settings_store::save_settings(&settings)?;
        let path = glint_sys::settings_store::settings_path();
        ensure!(path.starts_with(&dir), "settings path {} ignores GLINT_DATA_DIR", path.display());
        let loaded = glint_sys::settings_store::load_settings();
        if loaded != settings {
            bail!("loaded settings differ from saved ones");
        }
        Ok(format!("saved and reloaded {}", path.display()))
    })();
    // SAFETY: as above.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("GLINT_DATA_DIR", value),
            None => std::env::remove_var("GLINT_DATA_DIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn child_previews(out: &Path) -> Vec<Check> {
    let exe = match std::env::current_exe().context("locating glint.exe") {
        Ok(exe) => exe,
        Err(error) => return vec![Check { name: "preview children".into(), ok: false, detail: format!("{error:#}"), ms: 0.0 }],
    };
    let started = Instant::now();
    let children: Vec<_> = OVERLAY_KINDS
        .iter()
        .chain(EDITOR_KINDS.iter())
        .map(|kind| {
            let path = out.join(format!("{kind}.png"));
            let _ = std::fs::remove_file(&path);
            let child = std::process::Command::new(&exe)
                .args(["--preview", kind, "--out"])
                .arg(&path)
                .args(["--theme", "dark", "--scale", "1"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            (*kind, path, child)
        })
        .collect();
    children
        .into_iter()
        .map(|(kind, path, child)| {
            let name = format!("preview {kind}");
            let outcome = child.map_err(anyhow::Error::from).and_then(|c| Ok(c.wait_with_output()?));
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            match outcome {
                Ok(output) if output.status.success() && path.exists() => {
                    Check { name, ok: true, detail: format!("→ {}", path.display()), ms }
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let reason = stderr.lines().find(|l| l.contains("panicked") || l.contains("error") || l.contains("not yet implemented"))
                        .or_else(|| stderr.lines().next())
                        .unwrap_or("no output")
                        .trim()
                        .to_string();
                    Check { name, ok: false, detail: format!("exit {:?}: {reason}", output.status.code()), ms }
                }
                Err(error) => Check { name, ok: false, detail: format!("{error:#}"), ms },
            }
        })
        .collect()
}
