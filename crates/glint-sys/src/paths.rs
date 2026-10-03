use anyhow::{Context, Result};
use glint_core::Settings;
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::{
            SystemInformation::GetLocalTime,
            Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
        },
        UI::Shell::{FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    },
    core::GUID,
};

fn known_folder(id: &GUID) -> Result<PathBuf> {
    Ok(crate::util::take_path(unsafe {
        SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?
    }))
}

pub fn screenshots_dir(settings: &Settings) -> Result<PathBuf> {
    let path = match &settings.after_capture.save_dir {
        Some(path) => path.clone(),
        None => known_folder(&FOLDERID_Pictures)?.join("Screenshots"),
    };
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn recordings_dir(settings: &Settings) -> Result<PathBuf> {
    let path = match &settings.record.save_dir {
        Some(path) => path.clone(),
        None => known_folder(&FOLDERID_Videos)?.join("Screen Recordings"),
    };
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn local_data_dir() -> Result<PathBuf> {
    let path =
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("Glint");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn timestamped_name(prefix: &str, ext: &str, time: SystemTime) -> String {
    let ticks = match time.duration_since(UNIX_EPOCH) {
        Ok(d) => 116_444_736_000_000_000u64
            .saturating_add((d.as_nanos() / 100).min(u64::MAX as u128) as u64),
        Err(e) => 116_444_736_000_000_000u64
            .saturating_sub((e.duration().as_nanos() / 100).min(u64::MAX as u128) as u64),
    };
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&file_time, &mut utc).is_err()
            || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err()
        {
            local = GetLocalTime();
        }
    }
    format!(
        "{prefix} {:04}-{:02}-{:02} {:02}{:02}{:02}.{}",
        local.wYear,
        local.wMonth,
        local.wDay,
        local.wHour,
        local.wMinute,
        local.wSecond,
        ext.trim_start_matches('.')
    )
}

/// Chooses an unused filename; the caller is responsible for reserving it if needed.
pub fn unique_path(dir: &Path, prefix: &str, ext: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = timestamped_name(prefix, ext, SystemTime::now());
    let path = dir.join(&name);
    if !path.exists() {
        return Ok(path);
    }
    let stem = path
        .file_stem()
        .context("filename has no stem")?
        .to_string_lossy();
    for index in 2u64.. {
        let candidate = dir.join(format!("{stem} ({index}).{}", ext.trim_start_matches('.')));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    unreachable!()
}
