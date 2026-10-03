//! `editor.json` beside the app settings: the last tool and every tool's options. Reading is forgiving: unknown
//! keys are ignored, a missing or malformed field falls back to that tool's default, and a corrupt file yields
//! nothing (defaults), never an error dialog.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use glint_ui::Color;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::tools::{Tool, ToolOptions};

const VERSION: u64 = 1;

/// `%APPDATA%\Glint\editor.json` (or beside the settings file when `GLINT_DATA_DIR` moves it).
pub fn path() -> PathBuf {
    glint_sys::settings_store::settings_path().with_file_name("editor.json")
}

fn hex(color: Color) -> String {
    let [r, g, b, a] = color.to_rgba8();
    format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
}

fn options_json(o: &ToolOptions) -> Value {
    json!({
        "color": hex(o.color),
        "custom": o.custom.map(hex),
        "width": o.width,
        "opacity": o.opacity,
        "dash": o.dash,
        "pressure": o.pressure,
        "smoothing": o.smoothing,
        "shape": o.shape,
        "filled": o.filled,
        "fill_opacity": o.fill_opacity,
        "corner_radius": o.corner_radius,
        "line_caps": o.line_caps,
        "arrow_caps": o.arrow_caps,
        "head_scale": o.head_scale,
        "text_size": o.text_size,
        "redact": o.redact,
    })
}

pub fn encode(tool: Tool, options: &[ToolOptions; 8]) -> Vec<u8> {
    let tools: Map<String, Value> = Tool::ALL.iter().map(|t| (t.name().to_string(), options_json(&options[t.index()]))).collect();
    let document = json!({ "version": VERSION, "tool": tool.name(), "tools": tools });
    serde_json::to_vec_pretty(&document).unwrap_or_default()
}

fn field<T: DeserializeOwned>(map: &Map<String, Value>, key: &str) -> Option<T> {
    map.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
}

fn color_field(map: &Map<String, Value>, key: &str) -> Option<Color> {
    field::<String>(map, key).and_then(|s| Color::hex(&s))
}

fn options_from(tool: Tool, map: &Map<String, Value>) -> ToolOptions {
    let mut o = ToolOptions::defaults(tool);
    if let Some(v) = color_field(map, "color") {
        o.color = v;
    }
    o.custom = color_field(map, "custom").or(o.custom);
    macro_rules! read {
        ($($name:ident),* $(,)?) => {
            $(if let Some(v) = field(map, stringify!($name)) { o.$name = v; })*
        };
    }
    read!(width, opacity, dash, pressure, smoothing, shape, filled, fill_opacity, corner_radius, line_caps, arrow_caps);
    read!(head_scale, text_size, redact);
    o.sanitized()
}

/// Parses `editor.json`; None when it is not a JSON object.
pub fn decode(bytes: &[u8]) -> Option<(Tool, [ToolOptions; 8])> {
    let Value::Object(root) = serde_json::from_slice::<Value>(bytes).ok()? else { return None };
    let tool = field::<String>(&root, "tool").and_then(|n| Tool::from_name(&n)).unwrap_or(Tool::Pen);
    let tool = if tool == Tool::Crop { Tool::Select } else { tool };
    let empty = Map::new();
    let tools = root.get("tools").and_then(Value::as_object).unwrap_or(&empty);
    let options = Tool::ALL.map(|t| match tools.get(t.name()).and_then(Value::as_object) {
        Some(map) => options_from(t, map),
        None => ToolOptions::defaults(t),
    });
    Some((tool, options))
}

/// The stored preferences, or None when the file is missing or unreadable (logged).
pub fn load(path: &Path) -> Option<(Tool, [ToolOptions; 8])> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!("reading {}: {e}", path.display());
            return None;
        }
    };
    let decoded = decode(&bytes);
    if decoded.is_none() {
        log::warn!("ignoring corrupt editor preferences in {}", path.display());
    }
    decoded
}

/// Writes through a unique temporary file and an atomic rename.
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().context("preferences path has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let temporary =
        path.with_extension(format!("json.{}.{}.tmp", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
    let written = std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written.with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cap, Dash, ShapeKind};
    use crate::tools::palette;

    fn customized() -> [ToolOptions; 8] {
        let mut options = Tool::ALL.map(ToolOptions::defaults);
        let pen = &mut options[Tool::Pen.index()];
        pen.width = 5.5;
        pen.opacity = 0.6;
        pen.dash = Dash::Dotted;
        pen.pressure = false;
        pen.smoothing = 80.0;
        pen.custom = Some(Color::rgba8(90, 200, 250, 0.5));
        let shapes = &mut options[Tool::Shapes.index()];
        shapes.shape = ShapeKind::Arrow;
        shapes.arrow_caps = [Cap::Dot, Cap::Arrow];
        shapes.corner_radius = 12.0;
        shapes.fill_opacity = 0.25;
        shapes.head_scale = 1.5;
        options
    }

    #[test]
    fn round_trips_every_field() {
        let options = customized();
        let (tool, back) = decode(&encode(Tool::Shapes, &options)).unwrap();
        assert_eq!(tool, Tool::Shapes);
        for t in Tool::ALL {
            let (a, b) = (back[t.index()], options[t.index()]);
            assert_eq!(a.color.to_rgba8(), b.color.to_rgba8(), "{t:?}");
            assert_eq!(a.custom.map(|c| c.to_rgba8()), b.custom.map(|c| c.to_rgba8()));
            assert_eq!((a.width, a.opacity, a.dash, a.pressure, a.smoothing), (b.width, b.opacity, b.dash, b.pressure, b.smoothing));
            assert_eq!((a.shape, a.arrow_caps, a.line_caps, a.head_scale), (b.shape, b.arrow_caps, b.line_caps, b.head_scale));
            assert_eq!((a.corner_radius, a.fill_opacity, a.text_size, a.redact), (b.corner_radius, b.fill_opacity, b.text_size, b.redact));
        }
    }

    #[test]
    fn corrupt_files_are_ignored() {
        assert!(decode(b"").is_none());
        assert!(decode(b"{ not json").is_none());
        assert!(decode(b"[1, 2]").is_none());
    }

    #[test]
    fn bad_fields_fall_back_per_field_and_values_are_clamped() {
        let text = r##"{
            "tool": "Crop",
            "tools": {
                "Pen": { "width": "wide", "opacity": 9, "dash": "Wavy", "color": "#34C759", "smoothing": 55 },
                "Highlighter": 42,
                "Nonsense": { "width": 3 }
            },
            "extra": true
        }"##;
        let (tool, options) = decode(text.as_bytes()).unwrap();
        assert_eq!(tool, Tool::Select, "crop is never restored as the active tool");
        let pen = options[Tool::Pen.index()];
        let defaults = ToolOptions::defaults(Tool::Pen);
        assert_eq!(pen.width, defaults.width);
        assert_eq!(pen.opacity, 1.0);
        assert_eq!(pen.dash, Dash::Solid);
        assert_eq!(pen.smoothing, 55.0);
        assert_eq!(pen.color.to_rgba8(), palette(3).to_rgba8());
        assert_eq!(options[Tool::Highlighter.index()], ToolOptions::defaults(Tool::Highlighter));
    }

    #[test]
    fn writes_atomically_into_a_new_folder() {
        let dir = std::env::temp_dir().join(format!("glint-editor-prefs-test-{}", std::process::id()));
        let path = dir.join("nested").join("editor.json");
        let options = customized();
        write(&path, &encode(Tool::Pen, &options)).unwrap();
        let (tool, back) = load(&path).unwrap();
        assert_eq!(tool, Tool::Pen);
        assert_eq!(back[Tool::Pen.index()].width, 5.5);
        std::fs::write(&path, b"garbage").unwrap();
        assert!(load(&path).is_none());
        assert!(load(&dir.join("missing.json")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
