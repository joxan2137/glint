//! Offscreen renders of every overlay state, dark, at scale 1 and 2. No window is created.
//!
//! cargo run -p glint-overlay --release --example preview -- --out <dir> [--live]
//! `--live` freezes the real desktop with `glint_capture::capture_all` (reads the screen only).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use glint_core::ThemeMode;
use glint_core::settings::HdrSettings;
use glint_ui::Gfx;

fn main() -> Result<()> {
    glint_ui::enable_per_monitor_dpi_awareness();
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("glint-overlay-preview"));
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let captures = if args.iter().any(|a| a == "--live") {
        let started = Instant::now();
        let captures = glint_capture::capture_all(&HdrSettings::default())?;
        println!("captured {} monitor(s) in {:.0} ms", captures.len(), started.elapsed().as_secs_f64() * 1000.0);
        Some(captures)
    } else {
        None
    };
    let gfx = Gfx::new()?;
    for scale in [1.0f32, 2.0] {
        for kind in glint_overlay::PREVIEW_KINDS {
            let started = Instant::now();
            let image = glint_overlay::render_preview(&gfx, kind, ThemeMode::Dark, scale, captures.as_deref())?;
            let path = out.join(format!("{kind}@{scale}x.png"));
            std::fs::write(&path, glint_core::encode::encode_png(&image)?)?;
            println!("  {} ({}x{}, {:.0} ms)", path.display(), image.width, image.height, started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    Ok(())
}
