use anyhow::{Result, ensure};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WPARAM},
        System::{DataExchange::COPYDATASTRUCT, Threading::CreateMutexW},
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE,
            SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_COPYDATA,
        },
    },
    core::{PCWSTR, w},
};

pub const IPC_WINDOW_CLASS: &str = "Glint.Ipc";
const IPC_TAG: usize = 0x474c4e54;
const MAX_BYTES: usize = 1024 * 1024;

pub struct InstanceGuard(HANDLE);
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
pub enum InstanceRole {
    Primary(InstanceGuard),
    Secondary,
}

pub fn acquire_single_instance() -> InstanceRole {
    unsafe {
        match CreateMutexW(None, false, w!("Local\\Glint.SingleInstance")) {
            Ok(handle) => {
                let guard = InstanceGuard(handle);
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    InstanceRole::Secondary
                } else {
                    InstanceRole::Primary(guard)
                }
            }
            Err(error) => {
                log::error!("Single-instance mutex failed: {error}");
                InstanceRole::Secondary
            }
        }
    }
}

pub fn forward_to_primary(args: &[String]) -> Result<bool> {
    let class = crate::util::wide(IPC_WINDOW_CLASS);
    let find = || unsafe { FindWindowExW(Some(HWND_MESSAGE), None, PCWSTR(class.as_ptr()), None) };
    let Some(window) = (0..20).find_map(|attempt| {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        find().ok()
    }) else {
        return Ok(false);
    };
    let mut primary_pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut primary_pid));
        let _ = AllowSetForegroundWindow(primary_pid);
    }
    let bytes = serde_json::to_vec(args)?;
    ensure!(bytes.len() <= MAX_BYTES, "IPC arguments exceed 1 MiB");
    let data = COPYDATASTRUCT {
        dwData: IPC_TAG,
        cbData: bytes.len() as u32,
        lpData: bytes.as_ptr().cast_mut().cast(),
    };
    let mut response = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            window,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM((&data as *const COPYDATASTRUCT) as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            2000,
            Some(&mut response),
        )
    };
    ensure!(sent.0 != 0, "Primary instance did not respond to IPC");
    Ok(response != 0)
}

/// Accept only the LPARAM supplied by Windows for the current WM_COPYDATA callback.
pub fn parse_copydata(lparam: LPARAM) -> Option<Vec<String>> {
    if lparam.0 == 0 {
        return None;
    }
    let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
    if data.dwData != IPC_TAG
        || data.cbData == 0
        || data.cbData as usize > MAX_BYTES
        || data.lpData.is_null()
    {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data.lpData.cast(), data.cbData as usize) };
    serde_json::from_slice(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipc_validation() {
        let bytes = br#"["--snip","text"]"#;
        let mut data = COPYDATASTRUCT {
            dwData: IPC_TAG,
            cbData: bytes.len() as u32,
            lpData: bytes.as_ptr().cast_mut().cast(),
        };
        assert_eq!(
            parse_copydata(LPARAM((&data as *const COPYDATASTRUCT) as isize)).unwrap(),
            ["--snip", "text"]
        );
        data.dwData = 0;
        assert!(parse_copydata(LPARAM((&data as *const COPYDATASTRUCT) as isize)).is_none());
        assert!(parse_copydata(LPARAM(0)).is_none());
    }
}
