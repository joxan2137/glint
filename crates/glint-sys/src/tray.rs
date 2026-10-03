use anyhow::{Result, ensure};
use glint_core::PointI;
use windows::{
    Win32::{
        Foundation::{FreeLibrary, HWND, LPARAM, WPARAM},
        System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{PCSTR, PCWSTR, w},
};

pub struct Tray {
    data: NOTIFYICONDATAW,
}
fn tooltip_buffer(text: &str) -> [u16; 128] {
    let mut result = [0; 128];
    for (destination, source) in result[..127].iter_mut().zip(text.encode_utf16()) {
        *destination = source;
    }
    if (0xd800..=0xdbff).contains(&result[126]) {
        result[126] = 0;
    }
    result
}
impl Tray {
    pub fn add(hwnd: HWND, callback_msg: u32, icon: HICON, tooltip: &str) -> Result<Self> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: callback_msg,
            hIcon: icon,
            szTip: tooltip_buffer(tooltip),
            ..Default::default()
        };
        ensure!(
            unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool(),
            "Could not add tray icon"
        );
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
            anyhow::bail!("Could not set tray icon version");
        }
        Ok(Self { data })
    }
    /// Re-adds the icon after Explorer restarts (the "TaskbarCreated" message).
    pub fn recreate(&mut self) -> Result<()> {
        self.data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        ensure!(
            unsafe { Shell_NotifyIconW(NIM_ADD, &self.data) }.as_bool(),
            "Could not re-add tray icon"
        );
        self.data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        ensure!(
            unsafe { Shell_NotifyIconW(NIM_SETVERSION, &self.data) }.as_bool(),
            "Could not set tray icon version"
        );
        Ok(())
    }

    pub fn set_tooltip(&mut self, tooltip: &str) -> Result<()> {
        self.data.szTip = tooltip_buffer(tooltip);
        self.modify(NIF_TIP | NIF_SHOWTIP)
    }
    pub fn set_icon(&mut self, icon: HICON) -> Result<()> {
        self.data.hIcon = icon;
        self.modify(NIF_ICON)
    }
    fn modify(&mut self, flags: NOTIFY_ICON_DATA_FLAGS) -> Result<()> {
        self.data.uFlags = flags;
        ensure!(
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data) }.as_bool(),
            "Could not update tray icon"
        );
        Ok(())
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
        }
    }
}

pub fn taskbar_created_message() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}

#[derive(Clone, Debug, Default)]
pub struct MenuItem {
    pub id: u32,
    pub label: String,
    pub checked: bool,
    pub enabled: bool,
    pub separator: bool,
    pub submenu: Vec<MenuItem>,
}
impl MenuItem {
    pub fn new(id: u32, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            enabled: true,
            ..Default::default()
        }
    }
    pub fn separator() -> Self {
        Self {
            separator: true,
            ..Default::default()
        }
    }
    pub fn submenu(label: impl Into<String>, items: Vec<Self>) -> Self {
        Self {
            label: label.into(),
            submenu: items,
            enabled: true,
            ..Default::default()
        }
    }
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}
struct Menu(HMENU);
impl Drop for Menu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}
fn create_menu(items: &[MenuItem]) -> windows::core::Result<Menu> {
    let menu = Menu(unsafe { CreatePopupMenu()? });
    for item in items {
        let mut flags = if item.separator {
            MF_SEPARATOR
        } else {
            MF_STRING
        };
        if item.checked {
            flags |= MF_CHECKED;
        }
        if !item.enabled {
            flags |= MF_GRAYED;
        }
        let child = if item.submenu.is_empty() {
            None
        } else {
            Some(create_menu(&item.submenu)?)
        };
        let id = if let Some(child) = &child {
            flags |= MF_POPUP;
            child.0.0 as usize
        } else {
            item.id as usize
        };
        let label = crate::util::wide(&item.label);
        unsafe {
            AppendMenuW(menu.0, flags, id, PCWSTR(label.as_ptr()))?;
        }
        if let Some(child) = child {
            std::mem::forget(child);
        }
    }
    Ok(menu)
}

pub fn show_context_menu(hwnd: HWND, items: &[MenuItem], at: PointI) -> Option<u32> {
    let menu = create_menu(items).ok()?;
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenuEx(
            menu.0,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            at.x,
            at.y,
            hwnd,
            None,
        )
        .0 as u32;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        (id != 0).then_some(id)
    }
}

pub fn set_dark_menus(dark: bool) {
    type SetAppMode = unsafe extern "system" fn(i32) -> i32;
    type FlushMenuThemes = unsafe extern "system" fn();
    static FUNCTIONS: std::sync::OnceLock<Option<(SetAppMode, FlushMenuThemes)>> =
        std::sync::OnceLock::new();
    let functions = FUNCTIONS.get_or_init(|| unsafe {
        let Ok(module) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
        else {
            return None;
        };
        if let (Some(set), Some(flush)) = (
            GetProcAddress(module, PCSTR(135usize as *const u8)),
            GetProcAddress(module, PCSTR(136usize as *const u8)),
        ) {
            let set: SetAppMode = std::mem::transmute(set);
            let flush: FlushMenuThemes = std::mem::transmute(flush);
            // Keep the module resident with its cached function pointers and app mode.
            return Some((set, flush));
        }
        let _ = FreeLibrary(module);
        None
    });
    if let Some((set, flush)) = functions {
        unsafe {
            set(if dark { 2 } else { 3 });
            flush();
        }
    }
}
