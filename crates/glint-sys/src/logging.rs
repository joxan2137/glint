use anyhow::{Result, anyhow};
use std::{
    backtrace::Backtrace,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use windows::Win32::System::SystemInformation::GetLocalTime;

struct FileLogger {
    path: PathBuf,
    file: Mutex<Option<File>>,
}
impl FileLogger {
    fn write(&self, line: &str) {
        let Ok(mut slot) = self.file.lock() else {
            return;
        };
        let oversized = slot
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.len() + line.len() as u64 > 1_048_576);
        if oversized {
            slot.take();
            let mut rotated = self.path.as_os_str().to_owned();
            rotated.push(".1");
            let rotated = PathBuf::from(rotated);
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(&self.path, rotated);
        }
        if slot.is_none() {
            *slot = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(file) = slot.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}
impl log::Log for FileLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        let time = unsafe { GetLocalTime() };
        self.write(&format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {:5} {}: {}\n",
            time.wYear,
            time.wMonth,
            time.wDay,
            time.wHour,
            time.wMinute,
            time.wSecond,
            time.wMilliseconds,
            record.level(),
            record.target(),
            record.args()
        ));
    }
    fn flush(&self) {
        if let Ok(mut slot) = self.file.lock()
            && let Some(file) = slot.as_mut()
        {
            let _ = file.flush();
        }
    }
}

pub fn init_logging(path: &Path) -> Result<()> {
    static LOGGER: OnceLock<FileLogger> = OnceLock::new();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    LOGGER
        .set(FileLogger {
            path: path.into(),
            file: Mutex::new(Some(file)),
        })
        .map_err(|_| anyhow!("Logging is already initialized"))?;
    log::set_logger(LOGGER.get().expect("logger initialized"))
        .map_err(|e| anyhow!("Could not register logger: {e}"))?;
    log::set_max_level(log::LevelFilter::Info);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("Panic: {info}\n{}", Backtrace::force_capture());
        previous(info);
    }));
    Ok(())
}
