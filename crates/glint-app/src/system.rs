//! Small safe wrappers around the Win32 calls the app needs beyond glint-ui and glint-sys.

use anyhow::{Context, Result, ensure};
use glint_core::{Image, MonitorInfo, PointI, RectI};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
    EnumDisplayMonitors, HBITMAP, HDC, HGDIOBJ, HMONITOR,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, GetCurrentThread, SetPriorityClass, SetThreadPriority,
    THREAD_PRIORITY, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST,
};
use windows_core::BOOL;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetCursorPos, HICON, ICONINFO, IsWindow, IsWindowVisible, MSG, PM_NOREMOVE,
    PeekMessageW, SM_CXSMICON, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SetForegroundWindow, ShowWindow,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FileOpenDialog, IFileOpenDialog, IShellItem,
    SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, w};

pub fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

pub fn cursor_pos() -> PointI {
    let mut point = POINT::default();
    // SAFETY: out-pointer to a valid POINT.
    let _ = unsafe { GetCursorPos(&mut point) };
    PointI::new(point.x, point.y)
}

/// Monitor rects from `EnumDisplayMonitors` (microseconds; no DXGI or display-config queries), to check that a
/// cached monitor list still matches the layout.
pub fn display_rects() -> Vec<RectI> {
    unsafe extern "system" fn collect(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec passed below and `rect` points to the monitor rect, both valid during the call.
        unsafe {
            let rects = &mut *(data.0 as *mut Vec<RectI>);
            let r = *rect;
            rects.push(RectI::from_ltrb(r.left, r.top, r.right, r.bottom));
        }
        true.into()
    }
    let mut rects: Vec<RectI> = Vec::new();
    // SAFETY: the callback only writes into `rects`, which outlives the enumeration.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut rects as *mut Vec<RectI> as isize)) };
    rects
}

/// Snipping must feel instant while a game saturates the CPU: the process runs above normal (idle cost is zero)
/// and the UI thread at the highest normal priority.
pub fn raise_ui_priority() {
    // SAFETY: plain calls on pseudo handles of this process and thread.
    let (process, thread) = unsafe {
        (
            SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS),
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST),
        )
    };
    log::info!("priority: process above normal {:?}, UI thread highest {:?}", process.is_ok(), thread.is_ok());
}

/// Worker threads on the capture path (they still yield to the UI thread).
pub fn raise_worker_priority() {
    set_thread_priority(THREAD_PRIORITY_ABOVE_NORMAL);
}

fn set_thread_priority(priority: THREAD_PRIORITY) {
    // SAFETY: plain call on the pseudo handle of the calling thread.
    if let Err(error) = unsafe { SetThreadPriority(GetCurrentThread(), priority) } {
        log::warn!("thread priority: {error}");
    }
}

/// The monitor that contains `point`, else the primary, else the first.
pub fn monitor_at(monitors: &[MonitorInfo], point: PointI) -> Option<&MonitorInfo> {
    monitors
        .iter()
        .find(|m| m.rect.contains(point))
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
}

/// Notification-area icon size in pixels at `dpi`.
pub fn small_icon_size(dpi: u32) -> u32 {
    // SAFETY: plain metric query.
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) };
    if size > 0 { size as u32 } else { (16 * dpi).div_ceil(96) }
}

/// True when the taskbar uses the light theme (`SystemUsesLightTheme`), which decides the tray glyph color.
pub fn taskbar_is_light() -> bool {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reads one DWORD into a buffer of the stated size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value != 0
}

/// An HICON destroyed on drop.
pub struct OwnedIcon(HICON);

impl OwnedIcon {
    /// Builds a 32-bit alpha icon from a straight-alpha BGRA image (icons use straight alpha).
    pub fn from_image(image: &Image) -> Result<Self> {
        ensure!(image.width > 0 && image.height > 0, "empty icon image");
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: image.width as i32,
            biHeight: -(image.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
        let mut bits = std::ptr::null_mut();
        // SAFETY: CreateDIBSection allocates a top-down 32-bpp section; we copy exactly width*height*4 bytes into
        // it, then hand both bitmaps to CreateIconIndirect (which copies them) and delete ours.
        unsafe {
            let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).context("CreateDIBSection")?;
            let color = GdiBitmap(color);
            ensure!(!bits.is_null(), "CreateDIBSection returned no pixels");
            std::ptr::copy_nonoverlapping(image.data.as_ptr(), bits.cast::<u8>(), image.data.len());
            let mask_bits = vec![0u8; (image.width.div_ceil(16) * 2 * image.height) as usize];
            let mask = GdiBitmap(CreateBitmap(image.width as i32, image.height as i32, 1, 1, Some(mask_bits.as_ptr().cast())));
            let icon_info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask.0, hbmColor: color.0 };
            let icon = CreateIconIndirect(&icon_info).context("CreateIconIndirect")?;
            Ok(Self(icon))
        }
    }

    pub fn handle(&self) -> HICON {
        self.0
    }
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        // SAFETY: we own the icon handle.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

struct GdiBitmap(HBITMAP);

impl Drop for GdiBitmap {
    fn drop(&mut self) {
        // SAFETY: we own the bitmap handle.
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

fn set_transitions(window: HWND, enabled: bool) {
    let disabled = windows::core::BOOL::from(!enabled);
    // SAFETY: `disabled` lives for the call and its size is passed.
    let _ = unsafe {
        DwmSetWindowAttribute(window, DWMWA_TRANSITIONS_FORCEDISABLED, (&disabled as *const windows::core::BOOL).cast(), size_of::<windows::core::BOOL>() as u32)
    };
}

/// Hides a visible window at once (no DWM fade, so a capture right after does not see it); false if it was not
/// visible.
pub fn hide_instantly(raw: isize) -> bool {
    let window = hwnd(raw);
    // SAFETY: plain queries and calls on a window handle; stale handles fail harmlessly.
    unsafe {
        if !IsWindow(Some(window)).as_bool() || !IsWindowVisible(window).as_bool() {
            return false;
        }
        set_transitions(window, false);
        let _ = ShowWindow(window, SW_HIDE);
    }
    true
}

/// Shows a window hidden by `hide_instantly` again and restores its DWM transitions.
pub fn show_again(raw: isize, activate: bool) {
    let window = hwnd(raw);
    // SAFETY: as above.
    unsafe {
        if !IsWindow(Some(window)).as_bool() {
            return;
        }
        let _ = ShowWindow(window, if activate { SW_SHOW } else { SW_SHOWNOACTIVATE });
        if activate {
            let _ = SetForegroundWindow(window);
        }
        set_transitions(window, true);
    }
}

/// Waits up to `limit` for `workers` to finish while answering messages other threads send to this thread's
/// windows (a worker writing the clipboard may wait on our clipboard-owner window). Unfinished workers are left
/// running.
pub fn join_bounded(workers: Vec<JoinHandle<()>>, limit: Duration) {
    let deadline = Instant::now() + limit;
    let mut pending = workers;
    while !pending.is_empty() && Instant::now() < deadline {
        let mut message = MSG::default();
        // SAFETY: PM_NOREMOVE only dispatches incoming sent messages; nothing is removed from the queue.
        let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
        let (finished, running): (Vec<_>, Vec<_>) = pending.into_iter().partition(|w| w.is_finished());
        for worker in finished {
            let _ = worker.join();
        }
        pending = running;
        if !pending.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if !pending.is_empty() {
        log::warn!("{} worker(s) still running at exit", pending.len());
    }
}

/// Modal "Open image" dialog filtered to PNG and JPEG, starting in `folder`. Needs a COM STA on this thread;
/// cancel and failure return None.
pub fn pick_image(owner: isize, folder: Option<&Path>) -> Option<PathBuf> {
    // SAFETY: COM calls on a dialog object created and used on this STA thread; the returned display name is freed
    // exactly once with CoTaskMemFree.
    let show = || -> windows::core::Result<PathBuf> {
        unsafe {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_FILEMUSTEXIST)?;
            dialog.SetFileTypes(&[
                COMDLG_FILTERSPEC { pszName: w!("Images (PNG, JPEG)"), pszSpec: w!("*.png;*.jpg;*.jpeg") },
                COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
            ])?;
            dialog.SetTitle(w!("Open image in Glint"))?;
            if let Some(folder) = folder
                && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(folder.as_os_str()), None)
            {
                dialog.SetFolder(&item)?;
            }
            dialog.Show(Some(hwnd(owner)))?;
            let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = name.to_string();
            CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
            Ok(PathBuf::from(path.unwrap_or_default()))
        }
    };
    show().ok().filter(|path| !path.as_os_str().is_empty())
}

/// Lets a GUI-subsystem process print to the console it was started from (`--selftest`, `--version`).
pub fn attach_parent_console() {
    // SAFETY: no pointers; fails harmlessly when there is no parent console or stdout is already redirected.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// OLE (single-threaded apartment) for drag and drop, shell dialogs and shortcuts on this thread.
pub struct OleGuard {
    initialized: bool,
}

impl OleGuard {
    pub fn new() -> Self {
        // SAFETY: initializes OLE for the calling thread; balanced in Drop when it succeeded.
        let initialized = unsafe { OleInitialize(None) }.is_ok();
        if !initialized {
            log::warn!("OleInitialize failed; drag and drop may not work");
        }
        Self { initialized }
    }
}

impl Default for OleGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OleGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful OleInitialize on this thread.
            unsafe { OleUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_core::RectI;

    fn monitor(x: i32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            handle: x as isize,
            device_name: format!("D{x}"),
            friendly_name: format!("M{x}"),
            rect: RectI::new(x, 0, 1920, 1080),
            work_rect: RectI::new(x, 0, 1920, 1040),
            dpi: 96,
            primary,
            hdr: None,
        }
    }

    #[test]
    fn monitor_at_falls_back_to_the_primary() {
        let monitors = [monitor(-1920, false), monitor(0, true)];
        assert_eq!(monitor_at(&monitors, PointI::new(-10, 10)).unwrap().rect.x, -1920);
        assert_eq!(monitor_at(&monitors, PointI::new(5000, 10)).unwrap().rect.x, 0);
        assert!(monitor_at(&[], PointI::new(0, 0)).is_none());
    }
}
