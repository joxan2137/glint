use anyhow::{Context, Result};
use glint_core::Settings;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use windows::{
    Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    core::PCWSTR,
};

pub fn settings_path() -> PathBuf {
    std::env::var_os("GLINT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Glint")
        })
        .join("settings.json")
}

pub fn load_settings() -> Settings {
    load_from(&settings_path())
}

fn load_from(path: &Path) -> Settings {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(settings) => settings,
            Err(error) => {
                log::warn!("Invalid settings: {error}");
                let backup = path.with_extension("json.bak");
                if let Err(error) = replace_file(path, &backup) {
                    log::warn!("Could not back up settings: {error}");
                }
                Settings::default()
            }
        },
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Could not read settings: {error}");
            }
            Settings::default()
        }
    }
}

pub fn save_settings(settings: &Settings) -> Result<()> {
    save_to(&settings_path(), settings)
}

fn replace_file(from: &Path, to: &Path) -> Result<()> {
    let from = crate::util::wide(from);
    let to = crate::util::wide(to);
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

fn save_to(path: &Path, settings: &Settings) -> Result<()> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(path.parent().context("settings path has no parent")?)?;
    let temporary = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(settings)?)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_roundtrip_replace_and_corruption_backup() {
        let dir = std::env::temp_dir().join(format!("glint-settings-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut settings = Settings::default();
        save_to(&path, &settings).unwrap();
        settings.general.launch_at_login = false;
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path), settings);
        std::fs::write(&path, "broken").unwrap();
        assert_eq!(load_from(&path), Settings::default());
        assert_eq!(
            std::fs::read(path.with_extension("json.bak")).unwrap(),
            b"broken"
        );
        std::fs::remove_file(path.with_extension("json.bak")).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
