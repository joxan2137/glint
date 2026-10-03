//! Raw Win32 windows the resident instance needs besides glint-ui's: the message-only `Glint.Ipc` window that
//! receives a second instance's arguments, and a hidden top-level window for the tray callback, `TaskbarCreated`,
//! session unlock, theme and display changes. glint-ui's loop dispatches their messages; they only post events, except
//! for the end of the Windows session, which runs its handler synchronously because nothing posted runs after it.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use glint_core::PointI;
use glint_sys::instance::{IPC_WINDOW_CLASS, parse_copydata};
use glint_sys::tray::taskbar_created_message;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::UI::Shell::{NIN_SELECT, NINF_KEY};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, RegisterClassExW, WINDOW_EX_STYLE, WM_APP,
    WM_CONTEXTMENU, WM_COPYDATA, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_QUERYENDSESSION, WM_SETTINGCHANGE,
    WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP, WTS_SESSION_UNLOCK,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Tray icon callback message (`Tray::add`).
pub const TRAY_CALLBACK: u32 = WM_APP + 0x51;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

#[derive(Clone, Debug, PartialEq)]
pub enum HostMessage {
    /// A second instance forwarded its arguments.
    Forwarded(Vec<String>),
    /// Left click (or Enter) on the tray icon.
    TrayActivate,
    /// Right click on the tray icon at this screen point.
    TrayMenu(PointI),
    /// Explorer restarted.
    TaskbarCreated,
    SessionUnlocked,
    /// Light/dark or accent change.
    ThemeChanged,
    /// Monitors, resolution or DPI changed.
    DisplayChanged,
}

type Sink = Box<dyn Fn(HostMessage) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static END_SESSION: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Runs `handler` on the UI thread, inside `WM_ENDSESSION`, when Windows logs off or shuts down.
pub fn set_end_session_handler(handler: impl Fn() + 'static) {
    END_SESSION.with(|slot| *slot.borrow_mut() = Some(Box::new(handler)));
}

fn end_session() {
    END_SESSION.with(|slot| {
        if let Ok(handler) = slot.try_borrow()
            && let Some(handler) = handler.as_ref()
        {
            handler();
        }
    });
}

fn emit(message: HostMessage) {
    if let Some(sink) = SINK.get() {
        sink(message);
    }
}

fn low_word(value: usize) -> u32 {
    (value & 0xFFFF) as u32
}

fn signed_low(value: usize) -> i32 {
    (value & 0xFFFF) as u16 as i16 as i32
}

fn signed_high(value: usize) -> i32 {
    ((value >> 16) & 0xFFFF) as u16 as i16 as i32
}

/// WM_SETTINGCHANGE with "ImmersiveColorSet": light/dark or accent changed.
fn is_color_set_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: for WM_SETTINGCHANGE a non-null lParam points to a NUL-terminated UTF-16 string owned by the sender for
    // the duration of the call.
    unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.is_ok_and(|area| area == "ImmersiveColorSet")
}

extern "system" fn ipc_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        return match parse_copydata(lparam) {
            Some(args) => {
                emit(HostMessage::Forwarded(args));
                LRESULT(1)
            }
            None => LRESULT(0),
        };
    }
    // SAFETY: default processing for our own window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

extern "system" fn notify_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed);
    match msg {
        TRAY_CALLBACK => {
            match low_word(lparam.0 as usize) {
                NIN_SELECT | NIN_KEYSELECT => emit(HostMessage::TrayActivate),
                WM_CONTEXTMENU => {
                    let anchor = wparam.0;
                    emit(HostMessage::TrayMenu(PointI::new(signed_low(anchor), signed_high(anchor))));
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_WTSSESSION_CHANGE => {
            if wparam.0 as u32 == WTS_SESSION_UNLOCK {
                emit(HostMessage::SessionUnlocked);
            }
            return LRESULT(0);
        }
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                end_session();
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE if is_color_set_change(lparam) => emit(HostMessage::ThemeChanged),
        WM_DISPLAYCHANGE | WM_DPICHANGED => emit(HostMessage::DisplayChanged),
        _ if taskbar_created != 0 && msg == taskbar_created => {
            emit(HostMessage::TaskbarCreated);
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default processing for our own window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn register_class(name: &HSTRING, proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT) -> Result<()> {
    // SAFETY: registers a class with a static procedure; a repeated registration fails harmlessly.
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
    Ok(())
}

/// The two host windows; destroyed (and session notifications unregistered) on drop.
pub struct HostWindows {
    ipc: HWND,
    notify: HWND,
    session_registered: bool,
}

impl HostWindows {
    /// Creates both windows on the calling (UI) thread. `sink` receives every `HostMessage`.
    pub fn create(sink: impl Fn(HostMessage) + Send + Sync + 'static) -> Result<Self> {
        let _ = SINK.set(Box::new(sink));
        TASKBAR_CREATED.store(taskbar_created_message(), Ordering::Relaxed);
        let ipc_class = HSTRING::from(IPC_WINDOW_CLASS);
        let notify_class = HSTRING::from("Glint.Host");
        register_class(&ipc_class, ipc_proc)?;
        register_class(&notify_class, notify_proc)?;
        // SAFETY: creates our own hidden windows of the classes registered above.
        let (ipc, notify) = unsafe {
            let instance = GetModuleHandleW(None)?;
            let ipc = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &ipc_class,
                w!("Glint"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
            .context("creating the IPC window")?;
            let notify = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                &notify_class,
                w!("Glint"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("creating the notification window")?;
            (ipc, notify)
        };
        // SAFETY: registers our own window for session change messages; unregistered in Drop.
        let session_registered = unsafe { WTSRegisterSessionNotification(notify, NOTIFY_FOR_THIS_SESSION) }.is_ok();
        if !session_registered {
            log::warn!("WTSRegisterSessionNotification failed; the hook watchdog still reinstalls every 10 minutes");
        }
        Ok(Self { ipc, notify, session_registered })
    }

    /// The hidden top-level window: tray owner, clipboard owner and dialog parent.
    pub fn notify_hwnd(&self) -> HWND {
        self.notify
    }
}

impl Drop for HostWindows {
    fn drop(&mut self) {
        // SAFETY: tearing down our own windows and registration.
        unsafe {
            if self.session_registered {
                let _ = WTSUnRegisterSessionNotification(self.notify);
            }
            let _ = DestroyWindow(self.ipc);
            let _ = DestroyWindow(self.notify);
        }
    }
}
