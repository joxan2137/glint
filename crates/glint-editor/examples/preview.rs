//! Renders every editor preview kind, dark and light, at scale 1 and 2, plus full-resolution exports. No window.
//!
//! cargo run -p glint-editor --release --example preview -- --out <dir> [--image <png>]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glint_core::{Image, ThemeMode};
use glint_ui::Gfx;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn save(dir: &Path, name: &str, image: &Image) -> Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, glint_core::encode::encode_png(image)?).with_context(|| format!("writing {}", path.display()))?;
    println!("  {}", path.display());
    Ok(())
}

fn main() -> Result<()> {
    glint_ui::enable_per_monitor_dpi_awareness();
    let out = arg("--out").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("glint-editor-preview"));
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let image = match arg("--image") {
        Some(path) => Some(glint_core::encode::decode(&std::fs::read(&path).with_context(|| format!("reading {path}"))?)?),
        None => None,
    };
    let gfx = Gfx::new()?;
    for scale in [1.0f32, 2.0] {
        for (theme, name) in [(ThemeMode::Dark, "dark"), (ThemeMode::Light, "light")] {
            for kind in glint_editor::PREVIEW_KINDS {
                let started = std::time::Instant::now();
                let rendered = glint_editor::render_preview(&gfx, kind, theme, scale, image.as_ref())
                    .with_context(|| format!("rendering {kind} {name} @{scale}x"))?;
                save(&out, &format!("{kind}-{name}@{scale}x.png"), &rendered)?;
                println!("    {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    save(&out, "export.png", &glint_editor::render_preview_export(&gfx, image.as_ref(), false)?)?;
    save(&out, "export-cropped.png", &glint_editor::render_preview_export(&gfx, image.as_ref(), true)?)?;
    println!("wrote {}", out.display());
    Ok(())
}
