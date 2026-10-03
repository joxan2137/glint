//! Draws the Glint app icon with glint-ui offscreen and writes `res/glint.ico` (PNG entries at every Windows size)
//! plus `res/glint-256.png`. No window is created.
//!
//! cargo run -p glint-app --example make_icon [-- --out <dir>]

use std::path::PathBuf;

use anyhow::{Context, Result};
use glint_app::art::render_app_icon;
use glint_app::ico::{IcoEntry, ico_bytes};
use glint_core::ImageFormat;
use glint_core::encode::{encode_png, save};
use glint_ui::Gfx;

const SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

fn output_dir() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("res"))
}

fn main() -> Result<()> {
    let out = output_dir();
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let gfx = Gfx::new()?;
    let mut entries = Vec::new();
    for size in SIZES {
        let image = render_app_icon(&gfx, size)?;
        if size == 256 {
            save(&image, &out.join("glint-256.png"), ImageFormat::Png)?;
        }
        entries.push(IcoEntry { size, png: encode_png(&image)? });
    }
    let ico = out.join("glint.ico");
    std::fs::write(&ico, ico_bytes(&entries)).with_context(|| format!("writing {}", ico.display()))?;
    println!("wrote {} and glint-256.png", ico.display());
    Ok(())
}
