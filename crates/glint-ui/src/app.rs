//! Single-threaded event loop: windows, app events, timers and compositor-paced frames.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use glint_core::ThemeMode;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WAIT_FAILED, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::UI::Input::Pointer::EnableMouseInPointer;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, KillTimer, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, PostMessageW, QS_ALLINPUT,
    RegisterClassExW, SetTimer, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_QUIT, WM_SETTINGCHANGE, WM_TIMER,
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{PCSTR, w};

use crate::anim::{self, precise_time};
use crate::color::Color;
use crate::gfx::Gfx;
use crate::theme::{self, Theme};
use crate::view::{Ctx, View};
use crate::window::{self, WindowSpec, WindowState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(pub(crate) u64);

impl TimerId {
    pub const NONE: TimerId = TimerId(0);
}

pub(crate) const WM_APP_WAKE: u32 = WM_APP + 1;
pub(crate) const WM_APP_CLOSE: u32 = WM_APP + 2;
const MODAL_TIMER_ID: usize = 1;
const MODAL_FRAME_MS: u32 = 8;
const MODAL_MAX_SLEEP_MS: u32 = 60_000;

enum TimerAction {
    View { window: WindowId, token: u64 },
    Repaint(WindowId),
    PollHitRegion(WindowId),
    Callback(Box<dyn FnOnce(&App)>),
}

struct Timer {
    id: TimerId,
    deadline: f64,
    action: TimerAction,
}

struct Remote {
    queue: Mutex<VecDeque<Box<dyn Any + Send>>>,
    wake_window: AtomicIsize,
    wake_event: isize,
    alive: AtomicBool,
}

impl Drop for Remote {
    fn drop(&mut self) {
        // SAFETY: the event handle is owned by this struct and closed exactly once.
        let _ = unsafe { CloseHandle(HANDLE(self.wake_event as *mut _)) };
    }
}

fn modal_sleep_ms(seconds: f64) -> u32 {
    ((seconds * 1000.0).ceil().max(0.0) as u32).clamp(MODAL_FRAME_MS, MODAL_MAX_SLEEP_MS)
}

/// Thread-safe handle for posting events to the UI thread from anywhere.
#[derive(Clone)]
pub struct AppProxy(Arc<Remote>);

impl AppProxy {
    /// Queues `event` for the handler registered with `App::on_event::<E>`. Returns false once the loop has
    /// exited.
    pub fn post<E: Any + Send>(&self, event: E) -> bool {
        if !self.0.alive.load(Ordering::Acquire) {
            return false;
        }
        if let Ok(mut queue) = self.0.queue.lock() {
            queue.push_back(Box::new(event));
        }
        // SAFETY: the event handle stays valid while `Remote` lives; posting to a destroyed window just fails.
        unsafe {
            let _ = SetEvent(HANDLE(self.0.wake_event as *mut _));
            let wake = self.0.wake_window.load(Ordering::Acquire);
            if wake != 0 {
                let _ = PostMessageW(Some(HWND(wake as *mut _)), WM_APP_WAKE, WPARAM(0), LPARAM(0));
            }
        }
        true
    }
}

type Handler = Rc<RefCell<dyn FnMut(&App, Box<dyn Any>)>>;
type ClockWait = unsafe extern "system" fn(u32, *const HANDLE, u32) -> u32;

pub(crate) struct AppShared {
    pub(crate) gfx: Rc<Gfx>,
    pub(crate) windows: RefCell<Vec<Rc<WindowState>>>,
    pub(crate) creating: RefCell<Option<Rc<WindowState>>>,
    handlers: RefCell<HashMap<TypeId, Handler>>,
    local: RefCell<VecDeque<Box<dyn Any>>>,
    remote: Arc<Remote>,
    timers: RefCell<Vec<Timer>>,
    next_id: Cell<u64>,
    quit: Cell<bool>,
    quit_when_no_windows: Cell<bool>,
    appearance: Cell<(ThemeMode, bool)>,
    system_dark: Cell<bool>,
    system_accent: Cell<Option<Color>>,
    wake_hwnd: Cell<isize>,
    listener_hwnd: Cell<isize>,
    clock_wait: Cell<Option<ClockWait>>,
    dispatch_depth: Cell<u32>,
    modal_wake_at: Cell<Option<f64>>,
    tick_depth: Cell<u32>,
}

/// Handle to the UI-thread application. Cheap to clone; only usable on the thread running `run`.
#[derive(Clone)]
pub struct App(pub(crate) Rc<AppShared>);

thread_local! {
    static CURRENT: RefCell<Option<Weak<AppShared>>> = const { RefCell::new(None) };
}

pub(crate) fn current_app() -> Option<App> {
    CURRENT.with(|c| c.borrow().as_ref().and_then(Weak::upgrade)).map(App)
}

fn load_compositor_clock() -> Option<ClockWait> {
    // SAFETY: looks up an optional export; the signature matches the documented API.
    unsafe {
        let module = GetModuleHandleW(w!("dcomp.dll")).or_else(|_| LoadLibraryW(w!("dcomp.dll"))).ok()?;
        let proc = GetProcAddress(module, PCSTR(c"DCompositionWaitForCompositorClock".as_ptr().cast()))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, ClockWait>(proc))
    }
}

extern "system" fn wake_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(app) = current_app() {
        match msg {
            WM_APP_WAKE => {
                if app.0.dispatch_depth.get() > 0 {
                    app.schedule_modal_wake();
                }
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == MODAL_TIMER_ID => {
                app.disarm_modal_timer();
                app.tick();
                app.schedule_modal_wake();
                return LRESULT(0);
            }
            WM_SETTINGCHANGE => {
                app.refresh_system_settings();
                return LRESULT(0);
            }
            _ => {}
        }
    }
    // SAFETY: default handling for our helper windows.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn register_classes() -> Result<()> {
    // SAFETY: class registration with static names and valid procedures; re-registration fails harmlessly.
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let wake = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wake_proc),
            hInstance: instance,
            lpszClassName: w!("GlintUi.Wake"),
            ..Default::default()
        };
        RegisterClassExW(&wake);
        let main = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window::window_proc),
            hInstance: instance,
            lpszClassName: window::WINDOW_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&main);
    }
    Ok(())
}

fn create_helper_window(message_only: bool) -> Result<HWND> {
    // SAFETY: creates a hidden helper window of our registered class.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let parent = message_only.then_some(HWND_MESSAGE);
        let ex = if message_only { WINDOW_EX_STYLE(0) } else { WS_EX_TOOLWINDOW };
        Ok(CreateWindowExW(ex, w!("GlintUi.Wake"), w!("Glint"), WS_POPUP, 0, 0, 0, 0, parent, None, Some(instance.into()), None)?)
    }
}

/// Runs the UI event loop on the current thread until `App::quit` (or the last window closes when
/// `set_quit_when_no_windows(true)`). `setup` opens the first windows and registers event handlers.
pub fn run(setup: impl FnOnce(&App) -> Result<()>) -> Result<()> {
    // SAFETY: process-wide input setting; fails harmlessly if already enabled.
    let _ = unsafe { EnableMouseInPointer(true) };
    register_classes()?;
    let gfx = Gfx::new()?;
    // SAFETY: unnamed auto-reset event owned by `Remote`.
    let wake_event = unsafe { CreateEventW(None, false, false, None)? };
    let remote = Arc::new(Remote {
        queue: Mutex::new(VecDeque::new()),
        wake_window: AtomicIsize::new(0),
        wake_event: wake_event.0 as isize,
        alive: AtomicBool::new(true),
    });
    let shared = Rc::new(AppShared {
        gfx,
        windows: RefCell::default(),
        creating: RefCell::default(),
        handlers: RefCell::default(),
        local: RefCell::default(),
        remote: remote.clone(),
        timers: RefCell::default(),
        next_id: Cell::new(1),
        quit: Cell::new(false),
        quit_when_no_windows: Cell::new(false),
        appearance: Cell::new((ThemeMode::System, false)),
        system_dark: Cell::new(theme::system_prefers_dark()),
        system_accent: Cell::new(theme::system_accent()),
        wake_hwnd: Cell::new(0),
        listener_hwnd: Cell::new(0),
        clock_wait: Cell::new(load_compositor_clock()),
        dispatch_depth: Cell::new(0),
        modal_wake_at: Cell::new(None),
        tick_depth: Cell::new(0),
    });
    anim::set_reduced_motion(!theme::system_animations_enabled());
    CURRENT.with(|c| *c.borrow_mut() = Some(Rc::downgrade(&shared)));
    let app = App(shared);
    let wake = create_helper_window(true).context("creating wake window")?;
    let listener = create_helper_window(false).context("creating settings listener window")?;
    app.0.wake_hwnd.set(wake.0 as isize);
    app.0.listener_hwnd.set(listener.0 as isize);
    remote.wake_window.store(wake.0 as isize, Ordering::Release);

    let result = setup(&app).and_then(|()| app.main_loop());

    remote.alive.store(false, Ordering::Release);
    remote.wake_window.store(0, Ordering::Release);
    app.destroy_all_windows();
    // SAFETY: destroying our own helper windows.
    unsafe {
        let _ = DestroyWindow(wake);
        let _ = DestroyWindow(listener);
    }
    CURRENT.with(|c| *c.borrow_mut() = None);
    result
}

impl App {
    pub fn gfx(&self) -> Rc<Gfx> {
        self.0.gfx.clone()
    }

    pub fn proxy(&self) -> AppProxy {
        AppProxy(self.0.remote.clone())
    }

    pub(crate) fn next_id(&self) -> u64 {
        let id = self.0.next_id.get();
        self.0.next_id.set(id + 1);
        id
    }

    /// Registers the handler for app events of type `E` (posted with `App::post`, `Ctx::post` or `AppProxy::post`).
    /// A later registration for the same type replaces the earlier one.
    pub fn on_event<E: Any>(&self, mut handler: impl FnMut(&App, E) + 'static) {
        let erased: Handler = Rc::new(RefCell::new(move |app: &App, event: Box<dyn Any>| {
            if let Ok(event) = event.downcast::<E>() {
                handler(app, *event);
            }
        }));
        self.0.handlers.borrow_mut().insert(TypeId::of::<E>(), erased);
    }

    /// Queues an event on the UI thread; delivered on the next loop turn, never re-entrantly.
    pub fn post<E: Any>(&self, event: E) {
        self.0.local.borrow_mut().push_back(Box::new(event));
        // SAFETY: the event handle lives as long as the app.
        let _ = unsafe { SetEvent(HANDLE(self.0.remote.wake_event as *mut _)) };
        if self.0.dispatch_depth.get() > 0 {
            self.schedule_modal_wake();
        }
    }

    /// Creates a window. It becomes visible after its first frame is presented (unless the spec is hidden).
    pub fn open(&self, spec: WindowSpec, view: impl View) -> Result<WindowId> {
        window::open(self, spec, Box::new(view))
    }

    /// Closes a window (deferred to the next loop turn so it never re-enters a running view).
    pub fn close(&self, id: WindowId) {
        if let Some(win) = self.window(id) {
            // SAFETY: posting to our own window.
            let _ = unsafe { PostMessageW(Some(win.hwnd()), WM_APP_CLOSE, WPARAM(0), LPARAM(0)) };
        }
    }

    pub fn request_paint(&self, id: WindowId) {
        if let Some(win) = self.window(id) {
            self.request_frame(&win);
        }
    }

    pub fn window_ids(&self) -> Vec<WindowId> {
        self.0.windows.borrow().iter().map(|w| w.id).collect()
    }

    pub fn hwnd(&self, id: WindowId) -> Option<isize> {
        self.window(id).map(|w| w.hwnd().0 as isize)
    }

    /// Runs `f` on the window's view if it is a `V` and not currently busy; window operations requested through
    /// the context are applied afterwards.
    pub fn with_view<V: View, R>(&self, id: WindowId, f: impl FnOnce(&mut V, &mut Ctx) -> R) -> Option<R> {
        let win = self.window(id)?;
        window::with_view(self, &win, f)
    }

    /// Calls `f` once after `delay`.
    pub fn set_timer(&self, delay: Duration, f: impl FnOnce(&App) + 'static) -> TimerId {
        self.add_timer(delay, TimerAction::Callback(Box::new(f)))
    }

    pub(crate) fn set_view_timer(&self, window: WindowId, delay: Duration, token: u64) -> TimerId {
        self.add_timer(delay, TimerAction::View { window, token })
    }

    pub(crate) fn set_repaint_timer(&self, window: WindowId, delay: Duration) -> TimerId {
        self.add_timer(delay, TimerAction::Repaint(window))
    }

    pub(crate) fn set_hit_poll_timer(&self, window: WindowId, delay: Duration) -> TimerId {
        self.add_timer(delay, TimerAction::PollHitRegion(window))
    }

    fn add_timer(&self, delay: Duration, action: TimerAction) -> TimerId {
        let id = TimerId(self.next_id());
        let deadline = precise_time() + delay.as_secs_f64();
        self.0.timers.borrow_mut().push(Timer { id, deadline, action });
        if self.0.dispatch_depth.get() > 0 {
            self.schedule_modal_wake();
        }
        id
    }

    pub fn cancel_timer(&self, id: TimerId) {
        self.0.timers.borrow_mut().retain(|t| t.id != id);
    }

    pub fn quit(&self) {
        self.0.quit.set(true);
        // SAFETY: waking our own loop.
        let _ = unsafe { SetEvent(HANDLE(self.0.remote.wake_event as *mut _)) };
    }

    /// Quit automatically when the last window closes (default false: resident apps keep running).
    pub fn set_quit_when_no_windows(&self, quit: bool) {
        self.0.quit_when_no_windows.set(quit);
    }

    /// App-wide appearance: windows whose spec theme is `ThemeMode::System` follow this mode.
    pub fn set_appearance(&self, mode: ThemeMode, use_system_accent: bool) {
        self.0.appearance.set((mode, use_system_accent));
        self.apply_theme_to_windows();
    }

    pub fn system_prefers_dark(&self) -> bool {
        self.0.system_dark.get()
    }

    /// The theme a window with `mode` gets right now.
    pub fn theme_for(&self, mode: ThemeMode) -> Theme {
        let (app_mode, system_accent) = self.0.appearance.get();
        let effective = if mode == ThemeMode::System { app_mode } else { mode };
        let theme = Theme::resolve(effective, self.0.system_dark.get());
        match (system_accent, self.0.system_accent.get()) {
            (true, Some(accent)) => theme.with_accent(accent),
            _ => theme,
        }
    }

    pub(crate) fn refresh_system_settings(&self) {
        anim::set_reduced_motion(!theme::system_animations_enabled());
        let dark = theme::system_prefers_dark();
        let accent = theme::system_accent();
        if dark != self.0.system_dark.get() || accent != self.0.system_accent.get() {
            self.0.system_dark.set(dark);
            self.0.system_accent.set(accent);
            self.apply_theme_to_windows();
        }
    }

    fn apply_theme_to_windows(&self) {
        let windows: Vec<_> = self.0.windows.borrow().clone();
        for win in windows {
            window::apply_theme(self, &win, self.theme_for(win.spec.theme));
        }
    }

    pub(crate) fn window(&self, id: WindowId) -> Option<Rc<WindowState>> {
        self.0.windows.borrow().iter().find(|w| w.id == id).cloned()
    }

    pub(crate) fn window_for_hwnd(&self, hwnd: HWND) -> Option<Rc<WindowState>> {
        if let Some(win) = self.0.windows.borrow().iter().find(|w| w.hwnd() == hwnd) {
            return Some(win.clone());
        }
        let creating = self.0.creating.borrow().clone()?;
        if creating.hwnd().is_invalid() {
            creating.set_hwnd(hwnd);
            self.0.windows.borrow_mut().push(creating.clone());
            return Some(creating);
        }
        None
    }

    pub(crate) fn remove_window(&self, id: WindowId) {
        self.0.windows.borrow_mut().retain(|w| w.id != id);
        self.0.timers.borrow_mut().retain(|t| match t.action {
            TimerAction::View { window, .. } | TimerAction::Repaint(window) | TimerAction::PollHitRegion(window) => {
                window != id
            }
            TimerAction::Callback(_) => true,
        });
        if self.0.quit_when_no_windows.get() && self.0.windows.borrow().is_empty() {
            self.quit();
        }
    }

    pub(crate) fn request_frame(&self, win: &WindowState) {
        win.needs_frame.set(true);
        if self.0.dispatch_depth.get() > 0 {
            self.schedule_modal_wake();
        }
    }

    /// Runs user code (window procedures, timer callbacks, event handlers). If that code enters a nested modal
    /// loop (menus, file dialogs, drag and drop, window moves) the main loop cannot run, so a wake timer on the
    /// helper window keeps frames, timers and posted events flowing until the modal loop ends.
    pub(crate) fn callout<R>(&self, f: impl FnOnce() -> R) -> R {
        self.0.dispatch_depth.set(self.0.dispatch_depth.get() + 1);
        self.schedule_modal_wake();
        let result = f();
        self.0.dispatch_depth.set(self.0.dispatch_depth.get() - 1);
        result
    }

    /// Milliseconds until the loop has work: a frame, a queued event or the next timer.
    fn next_work_ms(&self) -> Option<u32> {
        let events_waiting = !self.0.local.borrow().is_empty()
            || self.0.remote.queue.lock().map(|q| !q.is_empty()).unwrap_or(false);
        if events_waiting || self.frames_pending() {
            return Some(MODAL_FRAME_MS);
        }
        let deadline = self.0.timers.borrow().iter().map(|t| t.deadline).fold(f64::INFINITY, f64::min);
        deadline.is_finite().then(|| modal_sleep_ms(deadline - precise_time()))
    }

    /// Arms (or shortens) the modal keep-alive timer when there is pending work.
    pub(crate) fn schedule_modal_wake(&self) {
        let Some(ms) = self.next_work_ms() else { return };
        let wake_at = precise_time() + ms as f64 / 1000.0;
        if self.0.modal_wake_at.get().is_some_and(|armed| armed <= wake_at + 0.002) {
            return;
        }
        self.0.modal_wake_at.set(Some(wake_at));
        // SAFETY: (re)arms a timer on our own wake window.
        unsafe { SetTimer(Some(HWND(self.0.wake_hwnd.get() as *mut _)), MODAL_TIMER_ID, ms, None) };
    }

    /// Keeps frames flowing at full rate (window move/size loops).
    pub(crate) fn arm_modal_timer(&self) {
        self.0.modal_wake_at.set(Some(precise_time() + MODAL_FRAME_MS as f64 / 1000.0));
        // SAFETY: timer on our own wake window.
        unsafe { SetTimer(Some(HWND(self.0.wake_hwnd.get() as *mut _)), MODAL_TIMER_ID, MODAL_FRAME_MS, None) };
    }

    fn disarm_modal_timer(&self) {
        if self.0.modal_wake_at.take().is_some() {
            // SAFETY: killing our own timer.
            let _ = unsafe { KillTimer(Some(HWND(self.0.wake_hwnd.get() as *mut _)), MODAL_TIMER_ID) };
        }
    }

    fn frames_pending(&self) -> bool {
        self.0.windows.borrow().iter().any(|w| w.wants_render())
    }

    fn main_loop(&self) -> Result<()> {
        loop {
            if !self.pump_messages() {
                break;
            }
            self.disarm_modal_timer();
            self.tick();
            self.disarm_modal_timer();
            if self.0.quit.get() {
                break;
            }
            if self.frames_pending() {
                self.wait_for_compositor();
            } else {
                self.wait_idle();
            }
        }
        Ok(())
    }

    fn pump_messages(&self) -> bool {
        let wake = HWND(self.0.wake_hwnd.get() as *mut _);
        let mut msg = MSG::default();
        // SAFETY: standard message pump on this thread.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    self.0.quit.set(true);
                    return false;
                }
                if msg.hwnd == wake && (msg.message == WM_TIMER || msg.message == WM_APP_WAKE) {
                    continue;
                }
                let _ = TranslateMessage(&msg);
                self.callout(|| DispatchMessageW(&msg));
            }
        }
        true
    }

    fn wait_for_compositor(&self) {
        let event = HANDLE(self.0.remote.wake_event as *mut _);
        if let Some(wait) = self.0.clock_wait.get() {
            // SAFETY: one valid handle, bounded timeout (two frames at 60 Hz so a stalled clock never hitches long).
            let result = unsafe { wait(1, &event, 34) };
            if result != WAIT_FAILED.0 {
                return;
            }
            self.0.clock_wait.set(None);
        }
        // SAFETY: blocks until the next DWM composition.
        if unsafe { DwmFlush() }.is_err() {
            std::thread::sleep(Duration::from_millis(8));
        }
    }

    fn wait_idle(&self) {
        let timeout = self
            .0
            .timers
            .borrow()
            .iter()
            .map(|t| t.deadline)
            .fold(f64::INFINITY, f64::min);
        let millis = if timeout.is_finite() {
            ((timeout - precise_time()) * 1000.0).ceil().clamp(0.0, u32::MAX as f64 - 1.0) as u32
        } else {
            u32::MAX
        };
        if !self.0.local.borrow().is_empty() {
            return;
        }
        let event = HANDLE(self.0.remote.wake_event as *mut _);
        // SAFETY: waits on our event and the message queue.
        unsafe { MsgWaitForMultipleObjectsEx(Some(&[event]), millis, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
    }

    /// Fires due timers, delivers app events and renders windows that asked for a frame. Re-entrant from nested
    /// modal loops (file dialogs, window drags); busy views and handlers are skipped until the next turn.
    pub(crate) fn tick(&self) {
        let depth = self.0.tick_depth.get();
        if depth >= 3 {
            return;
        }
        self.0.tick_depth.set(depth + 1);
        let now = precise_time();
        anim::set_clock(now);
        self.fire_timers(now);
        self.deliver_events();
        let windows: Vec<_> = self.0.windows.borrow().iter().filter(|w| w.wants_render()).cloned().collect();
        let frame_time = precise_time();
        for win in windows {
            window::render(self, &win, frame_time);
        }
        self.0.tick_depth.set(depth);
    }

    fn fire_timers(&self, now: f64) {
        let due: Vec<Timer> = {
            let mut timers = self.0.timers.borrow_mut();
            let (due, pending): (Vec<_>, Vec<_>) = timers.drain(..).partition(|t| t.deadline <= now);
            *timers = pending;
            due
        };
        for timer in due {
            match timer.action {
                TimerAction::View { window, token } => {
                    if let Some(win) = self.window(window) {
                        self.callout(|| window::dispatch(self, &win, &crate::event::Event::Timer(token)));
                    }
                }
                TimerAction::Repaint(window) => self.request_paint(window),
                TimerAction::PollHitRegion(window) => {
                    if let Some(win) = self.window(window) {
                        window::poll_hit_region(self, &win);
                    }
                }
                TimerAction::Callback(f) => self.callout(|| f(self)),
            }
        }
    }

    fn deliver_events(&self) {
        if let Ok(mut remote) = self.0.remote.queue.lock() {
            let mut local = self.0.local.borrow_mut();
            while let Some(event) = remote.pop_front() {
                local.push_back(event as Box<dyn Any>);
            }
        }
        for _ in 0..1024 {
            let Some(event) = self.0.local.borrow_mut().pop_front() else { break };
            let type_id = (*event).type_id();
            let handler = self.0.handlers.borrow().get(&type_id).cloned();
            match handler {
                Some(handler) => match handler.try_borrow_mut() {
                    Ok(mut h) => self.callout(|| h(self, event)),
                    Err(_) => {
                        self.0.local.borrow_mut().push_back(event);
                        break;
                    }
                },
                None => log::warn!("no handler registered for posted app event {type_id:?}"),
            }
        }
    }

    fn destroy_all_windows(&self) {
        let windows: Vec<_> = self.0.windows.borrow().clone();
        for win in windows {
            // SAFETY: destroying our own window.
            let _ = unsafe { DestroyWindow(win.hwnd()) };
        }
        self.0.windows.borrow_mut().clear();
        self.0.timers.borrow_mut().clear();
    }
}
