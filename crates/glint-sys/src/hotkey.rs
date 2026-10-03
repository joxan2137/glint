use anyhow::{Context, Result};
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::{
            GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY,
            THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_TIME_CRITICAL,
        },
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
            VIRTUAL_KEY,
        },
        WindowsAndMessaging::*,
    },
};

const MARKER: usize = 0x474c4e54;
const REINSTALL: u32 = WM_APP + 0x47;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    Snip,
    Record,
    Text,
    WindowToClipboard,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HookConfig {
    pub win_shift_s: bool,
    pub print_screen: bool,
    pub win_shift_r: bool,
    pub win_shift_t: bool,
    pub alt_print_screen: bool,
}
impl From<&glint_core::settings::HotkeySettings> for HookConfig {
    fn from(value: &glint_core::settings::HotkeySettings) -> Self {
        Self {
            win_shift_s: value.win_shift_s,
            print_screen: value.print_screen,
            win_shift_r: value.win_shift_r,
            win_shift_t: value.win_shift_s,
            alt_print_screen: value.alt_print_screen,
        }
    }
}
impl HookConfig {
    fn bits(self) -> u8 {
        self.win_shift_s as u8
            | (self.print_screen as u8) << 1
            | (self.win_shift_r as u8) << 2
            | (self.win_shift_t as u8) << 3
            | (self.alt_print_screen as u8) << 4
    }
    fn from_bits(bits: u8) -> Self {
        Self {
            win_shift_s: bits & 1 != 0,
            print_screen: bits & 2 != 0,
            win_shift_r: bits & 4 != 0,
            win_shift_t: bits & 8 != 0,
            alt_print_screen: bits & 16 != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub swallow: bool,
    pub action: Option<HotkeyAction>,
    pub inject_mask: bool,
}

pub struct HookState {
    pub config: HookConfig,
    down: [bool; 256],
    swallowed: [bool; 256],
}
impl HookState {
    pub fn new(config: HookConfig) -> Self {
        Self {
            config,
            down: [false; 256],
            swallowed: [false; 256],
        }
    }
    /// Clears modifiers that are physically up although we never saw their key-up
    /// (missed while the hook was removed, or while an elevated window had focus).
    pub fn release_stale_modifiers(&mut self, current_vk: u32, is_down: impl Fn(u32) -> bool) {
        const MODIFIERS: [u32; 11] = [0x5b, 0x5c, 0x10, 0xa0, 0xa1, 0x11, 0xa2, 0xa3, 0x12, 0xa4, 0xa5];
        for vk in MODIFIERS {
            if vk != current_vk && self.down[vk as usize] && !is_down(vk) {
                self.down[vk as usize] = false;
            }
        }
    }

    pub fn forget_swallowed(&mut self, vk: u32) {
        if let Some(swallowed) = self.swallowed.get_mut(vk as usize) {
            *swallowed = false;
        }
    }

    pub fn reset(&mut self) {
        self.down = [false; 256];
        self.swallowed = [false; 256];
    }

    pub fn on_key(&mut self, vk: u32, is_up: bool) -> Decision {
        let key = vk as usize;
        if key >= self.down.len() {
            return Decision::default();
        }
        let repeated = self.down[key];
        self.down[key] = !is_up;
        if self.swallowed[key] {
            if is_up {
                self.swallowed[key] = false;
            }
            return Decision {
                swallow: true,
                ..Decision::default()
            };
        }
        if is_up || repeated {
            return Decision::default();
        }
        let win = self.down[0x5b] || self.down[0x5c];
        let shift = self.down[0x10] || self.down[0xa0] || self.down[0xa1];
        let ctrl = self.down[0x11] || self.down[0xa2] || self.down[0xa3];
        let alt = self.down[0x12] || self.down[0xa4] || self.down[0xa5];
        let action = if ctrl {
            None
        } else if win && shift && !alt {
            match vk {
                0x53 if self.config.win_shift_s => Some(HotkeyAction::Snip),
                0x52 if self.config.win_shift_r => Some(HotkeyAction::Record),
                0x54 if self.config.win_shift_t => Some(HotkeyAction::Text),
                _ => None,
            }
        } else if vk == 0x2c && !win && !shift {
            if alt && self.config.alt_print_screen {
                Some(HotkeyAction::WindowToClipboard)
            } else if !alt && self.config.print_screen {
                Some(HotkeyAction::Snip)
            } else {
                None
            }
        } else {
            None
        };
        self.swallowed[key] = action.is_some();
        Decision {
            swallow: action.is_some(),
            action,
            inject_mask: action.is_some() && (win || alt),
        }
    }
}

struct HookContext {
    state: HookState,
    config: Arc<AtomicU8>,
    actions: mpsc::Sender<HotkeyAction>,
}
thread_local! { static CONTEXT: RefCell<Option<HookContext>> = const { RefCell::new(None) }; }

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if event.dwExtraInfo != MARKER {
            let decision = CONTEXT.with(|context| {
                let mut context = context.borrow_mut();
                let Some(context) = context.as_mut() else {
                    return Decision::default();
                };
                context.state.config =
                    HookConfig::from_bits(context.config.load(Ordering::Relaxed));
                context.state.release_stale_modifiers(event.vkCode, |vk| unsafe {
                    GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0
                });
                let decision = context
                    .state
                    .on_key(event.vkCode, event.flags.contains(LLKHF_UP));
                if let Some(action) = decision.action
                    && context.actions.send(action).is_err()
                {
                    context.state.forget_swallowed(event.vkCode);
                    return Decision::default();
                }
                decision
            });
            if decision.inject_mask {
                inject_mask();
            }
            if decision.swallow {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn inject_mask() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0xe8),
                dwFlags: flags,
                dwExtraInfo: MARKER,
                ..Default::default()
            },
        },
    };
    let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
    unsafe {
        let _ = SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Keeps the shortcut responsive while a game saturates the CPU.
fn raise_current_thread_priority(priority: THREAD_PRIORITY) {
    if let Err(error) = unsafe { SetThreadPriority(GetCurrentThread(), priority) } {
        log::warn!("Could not raise hotkey thread priority: {error}");
    }
}

fn install_hook() -> windows::core::Result<HHOOK> {
    let module = unsafe { GetModuleHandleW(None)? };
    unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_proc),
            Some(HINSTANCE(module.0)),
            0,
        )
    }
}

pub struct KeyboardHook {
    config: Arc<AtomicU8>,
    thread_id: u32,
    hook_thread: Option<JoinHandle<()>>,
    dispatcher: Option<JoinHandle<()>>,
}
impl KeyboardHook {
    pub fn start(
        cfg: HookConfig,
        on_action: impl Fn(HotkeyAction) + Send + 'static,
    ) -> Result<Self> {
        let config = Arc::new(AtomicU8::new(cfg.bits()));
        let hook_config = config.clone();
        let (actions, receiver) = mpsc::channel();
        let (ready, started) = mpsc::sync_channel(1);
        let dispatcher = thread::Builder::new()
            .name("glint-hotkey-actions".into())
            .spawn(move || {
                raise_current_thread_priority(THREAD_PRIORITY_HIGHEST);
                while let Ok(action) = receiver.recv() {
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_action(action)));
                    if outcome.is_err() {
                        log::error!("Hotkey action {action:?} panicked");
                    }
                }
            })?;
        let hook_thread = match thread::Builder::new()
            .name("glint-keyboard-hook".into())
            .spawn(move || {
                raise_current_thread_priority(THREAD_PRIORITY_TIME_CRITICAL);
                let thread_id = unsafe { GetCurrentThreadId() };
                let mut message = MSG::default();
                unsafe {
                    let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                }
                CONTEXT.with(|context| {
                    *context.borrow_mut() = Some(HookContext {
                        state: HookState::new(cfg),
                        config: hook_config,
                        actions,
                    })
                });
                let mut hook = match install_hook() {
                    Ok(hook) => hook,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        CONTEXT.with(|c| c.borrow_mut().take());
                        return;
                    }
                };
                let timer = unsafe { SetTimer(None, 0, 600_000, None) };
                if timer == 0 {
                    let error = windows::core::Error::from_thread();
                    unsafe {
                        let _ = UnhookWindowsHookEx(hook);
                    }
                    let _ = ready.send(Err(error));
                    CONTEXT.with(|c| c.borrow_mut().take());
                    return;
                }
                let _ = ready.send(Ok(thread_id));
                loop {
                    let status = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
                    if status <= 0 {
                        break;
                    }
                    if (message.message == WM_TIMER && message.wParam.0 == timer)
                        || message.message == REINSTALL
                    {
                        match install_hook() {
                            Ok(replacement) => {
                                unsafe {
                                    let _ = UnhookWindowsHookEx(hook);
                                }
                                hook = replacement;
                                CONTEXT.with(|context| {
                                    if let Some(context) = context.borrow_mut().as_mut() {
                                        context.state.reset();
                                    }
                                });
                            }
                            Err(error) => log::error!("Keyboard hook watchdog: {error}"),
                        }
                    }
                }
                unsafe {
                    let _ = UnhookWindowsHookEx(hook);
                    let _ = KillTimer(None, timer);
                }
                CONTEXT.with(|c| c.borrow_mut().take());
            }) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = dispatcher.join();
                return Err(error.into());
            }
        };
        match started
            .recv()
            .context("Hook thread exited during startup")?
        {
            Ok(thread_id) => Ok(Self {
                config,
                thread_id,
                hook_thread: Some(hook_thread),
                dispatcher: Some(dispatcher),
            }),
            Err(error) => {
                let _ = hook_thread.join();
                let _ = dispatcher.join();
                Err(error.into())
            }
        }
    }
    pub fn set_config(&self, cfg: HookConfig) {
        self.config.store(cfg.bits(), Ordering::Relaxed);
    }
    /// Call from the app's WTS_SESSION_UNLOCK handler.
    pub fn on_session_unlock(&self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, REINSTALL, WPARAM(0), LPARAM(0));
        }
    }
}
impl Drop for KeyboardHook {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.hook_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.dispatcher.take()
            && thread.thread().id() != thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn enabled() -> HookState {
        HookState::new(HookConfig::from(
            &glint_core::settings::HotkeySettings::default(),
        ))
    }
    #[test]
    fn win_shift_s_complete_sequence() {
        let mut state = enabled();
        assert_eq!(state.on_key(0x5b, false), Decision::default());
        assert_eq!(state.on_key(0xa0, false), Decision::default());
        assert_eq!(
            state.on_key(0x53, false),
            Decision {
                swallow: true,
                action: Some(HotkeyAction::Snip),
                inject_mask: true
            }
        );
        assert_eq!(
            state.on_key(0x53, false),
            Decision {
                swallow: true,
                ..Decision::default()
            }
        );
        assert_eq!(
            state.on_key(0x53, true),
            Decision {
                swallow: true,
                ..Decision::default()
            }
        );
        assert_eq!(state.on_key(0xa0, true), Decision::default());
        assert_eq!(state.on_key(0x5b, true), Decision::default());
    }
    #[test]
    fn print_screen_and_alt() {
        for (alt, expected) in [
            (false, HotkeyAction::Snip),
            (true, HotkeyAction::WindowToClipboard),
        ] {
            let mut state = enabled();
            if alt {
                state.on_key(0xa4, false);
            }
            assert_eq!(state.on_key(0x2c, false).action, Some(expected));
            assert!(state.on_key(0x2c, false).swallow);
            assert!(state.on_key(0x2c, true).swallow);
        }
    }
    #[test]
    fn disabled_and_unowned_sequences_pass_through() {
        for keys in [
            vec![0x53],
            vec![0xa2, 0x5b, 0xa0, 0x53],
            vec![0xa4, 0x5b, 0xa0, 0x53],
        ] {
            let mut state = enabled();
            for key in &keys {
                assert_eq!(state.on_key(*key, false), Decision::default());
            }
            for key in keys.iter().rev() {
                assert_eq!(state.on_key(*key, true), Decision::default());
            }
        }
        let mut state = HookState::new(HookConfig::default());
        for key in [0x5b, 0xa0, 0x53, 0x52, 0x54, 0x2c] {
            assert_eq!(state.on_key(key, false), Decision::default());
            assert_eq!(state.on_key(key, true), Decision::default());
        }
    }
    #[test]
    fn right_modifiers_and_release_after_config_change() {
        let mut state = enabled();
        state.on_key(0x5c, false);
        state.on_key(0xa1, false);
        assert_eq!(state.on_key(0x52, false).action, Some(HotkeyAction::Record));
        state.config = HookConfig::default();
        assert!(state.on_key(0x52, true).swallow);
        state.config = HookConfig::from(&glint_core::settings::HotkeySettings::default());
        assert_eq!(state.on_key(0x54, false).action, Some(HotkeyAction::Text));
        state.on_key(0x5c, true);
        state.on_key(0xa1, true);
        assert!(state.on_key(0x54, true).swallow);
    }

    #[test]
    fn disabled_combos_and_extra_modifiers() {
        let combos = [
            vec![0x5b, 0xa0, 0x53],
            vec![0x5c, 0xa1, 0x52],
            vec![0x5b, 0xa1, 0x54],
            vec![0x2c],
            vec![0xa4, 0x2c],
        ];
        for keys in combos {
            let mut state = HookState::new(HookConfig::default());
            for key in &keys {
                assert_eq!(state.on_key(*key, false), Decision::default());
                assert_eq!(state.on_key(*key, false), Decision::default());
            }
            for key in keys.iter().rev() {
                assert_eq!(state.on_key(*key, true), Decision::default());
            }
        }
        for modifier in [0xa2, 0xa3, 0xa0, 0xa1, 0x5b, 0x5c] {
            let mut state = enabled();
            state.on_key(modifier, false);
            assert_eq!(state.on_key(0x2c, false), Decision::default());
        }
    }

    #[test]
    fn both_shift_keys_and_key_already_down() {
        let mut state = enabled();
        state.on_key(0x5b, false);
        state.on_key(0xa0, false);
        state.on_key(0xa1, false);
        state.on_key(0xa0, true);
        assert_eq!(state.on_key(0x53, false).action, Some(HotkeyAction::Snip));
        let mut state = enabled();
        state.on_key(0x53, false);
        state.on_key(0x5b, false);
        state.on_key(0xa0, false);
        assert_eq!(state.on_key(0x53, false), Decision::default());
        assert_eq!(state.on_key(0x53, true), Decision::default());
        assert_eq!(state.on_key(0x53, false).action, Some(HotkeyAction::Snip));
    }
}
