//! The resident instance: tray, hotkeys, capture flows, the after-capture pipeline, thumbnail, toasts, settings
//! window and the recording HUD. All state lives in one `Rc<RefCell<State>>` on the UI thread; captures, encoding,
//! OCR and finalizing recordings run on worker threads that post results back through the `AppProxy`. Borrows of the
//! state are kept short and never span a modal call (menus, dialogs, drag and drop) or a window creation.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use glint_core::encode;
use glint_core::settings::HdrSettings;
use glint_core::{CaptureMode, HdrImage, Image, ImageFormat, MonitorCapture, MonitorInfo, Settings, ToneMapParams, WindowInfo};
use glint_editor::{EditorDoc, EditorHost};
use glint_overlay::{OverlayOutcome, OverlayPrefs, OverlayRequest, RecordRegion, Snip};
use glint_record::{RecordConfig, Recorder, RecorderStatus, RecordingInfo};
use glint_sys::hotkey::{HookConfig, HotkeyAction, KeyboardHook};
use glint_sys::{clipboard, dialogs, install, paths, settings_store, shell, sound};
use glint_ui::{App, AppProxy, PointI, RectI, SizeF, TimerId, WindowId, WindowSpec};

use crate::cli::{self, Command, Greeting};
use crate::host::{HostMessage, HostWindows};
use crate::hud::{BorderView, HudCommand, HudPhase, HudReadout, HudView, border_rect_px};
use crate::pipeline::{
    CaptureSource, Presentation, draw_outline, extension, first_line, hex_color, plan, remember_overlay_choices,
    reserve_unique_path, snip_mode,
};
use crate::popup::{PopupLayout, corner_layout, occupied_height_px};
use crate::settings_model::{
    DisplayLine, FolderKind, InstallState, SettingsEnv, SettingsRequest, effects,
};
use crate::settings_view::{MIN_SIZE, SettingsMessage, SettingsView, WINDOW_SIZE};
use crate::system::{cursor_pos, hide_instantly, hwnd, join_bounded, monitor_at, show_again};
use crate::thumbnail::{ThumbnailAction, ThumbnailContent, ThumbnailEvent, ThumbnailView, card_size};
use crate::toast::{Tint, Toast, ToastClosed, ToastIcon, ToastView};
use crate::tray::{TrayCommand, TrayIcon, command_for, menu_items};

const RECORD_COUNTDOWN_SECS: u32 = 3;
const HUD_REFRESH: Duration = Duration::from_millis(33);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
const TRAY_RETRY: Duration = Duration::from_secs(2);
const TRAY_ATTEMPTS: u32 = 60;
/// Time for DWM to take hidden editor windows off the screen before capturing.
const EDITOR_HIDE_SETTLE: Duration = Duration::from_millis(120);
const QUIT_WORKER_WAIT: Duration = Duration::from_secs(5);
const END_SESSION_WORKER_WAIT: Duration = Duration::from_secs(3);

/// What a capture is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Overlay { mode: CaptureMode, video: bool },
    /// Alt+Print Screen.
    ActiveWindow,
}

/// Frozen screen state from a worker thread.
pub struct Frozen {
    captures: Vec<MonitorCapture>,
    windows: Vec<WindowInfo>,
    foreground: Option<WindowInfo>,
}

pub enum CountdownPurpose {
    Recapture(Intent),
    Record(RecordRegion),
}

/// Everything posted to the UI thread's main handler.
pub enum AppEvent {
    Host(HostMessage),
    Hotkey(HotkeyAction),
    Captured { intent: Intent, result: Result<Frozen> },
    OverlayClosed { outcome: OverlayOutcome, prefs: OverlayPrefs },
    CountdownDone { purpose: CountdownPurpose, completed: bool },
    PipelineDone { errors: Vec<String> },
    TextRecognized(Result<String>),
    RecordingFinished { monitor: MonitorInfo, result: Result<RecordingInfo> },
    NewSnip { mode: CaptureMode, delay_secs: u32 },
    /// The editor opened for capture `id` saved or exported to `path`.
    EditorSaved { id: u64, path: PathBuf },
    Copied { id: u64, result: Result<()> },
    /// `--edit` / "Open image…": the file was read and decoded on a worker.
    ImageLoaded { path: PathBuf, result: Result<Image> },
    SavedAs { id: u64, result: Result<PathBuf> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Idle,
    Capturing,
    Overlay,
    Countdown,
}

enum Captured {
    Image { hdr: Option<HdrImage> },
    Video { path: PathBuf },
}

struct LastCapture {
    id: u64,
    image: Rc<Image>,
    kind: Captured,
    monitor: MonitorInfo,
    saved: Option<PathBuf>,
    drag_file: Option<PathBuf>,
}

struct OpenPopup {
    id: u64,
    window: WindowId,
    layout: PopupLayout,
    scale: f32,
    /// Work area the popup sits in.
    work: RectI,
}

struct ActiveRecording {
    recorder: Recorder,
    hud: Option<WindowId>,
    border: Option<WindowId>,
    monitor: MonitorInfo,
}

struct State {
    settings: Settings,
    hook: Option<KeyboardHook>,
    tray: TrayIcon,
    host: Option<HostWindows>,
    owner: isize,
    busy: Busy,
    next_id: u64,
    last: Option<LastCapture>,
    thumbnail: Option<OpenPopup>,
    toast: Option<OpenPopup>,
    settings_window: Option<WindowId>,
    recording: Option<ActiveRecording>,
    save_timer: Option<TimerId>,
    relaunch: Option<PathBuf>,
    editors: Vec<WindowId>,
    /// Editor windows hidden while a snip started from the editor is in progress (raw HWNDs).
    hidden_editors: Vec<isize>,
    /// Workers whose result must not be lost at exit (files, clipboard, recording finalize).
    workers: Vec<JoinHandle<()>>,
}

/// What `main` does after the loop ends.
pub enum Exit {
    Done,
    /// `--install` from the settings window: start the installed copy once this process released the mutex.
    Relaunch(PathBuf),
}

#[derive(Clone)]
struct Glint {
    state: Rc<RefCell<State>>,
    proxy: AppProxy,
}

/// Runs the resident instance until the user quits. `initial` is the command that started it.
pub fn run(initial: Command) -> Result<Exit> {
    let _ole = crate::system::OleGuard::new();
    let settings = settings_store::load_settings();
    let relaunch: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let relaunch_out = relaunch.clone();
    glint_ui::run(move |app: &App| {
        app.set_quit_when_no_windows(false);
        app.set_appearance(settings.appearance.theme, settings.appearance.system_accent);
        let proxy = app.proxy();
        let sink = proxy.clone();
        let host = HostWindows::create(move |message| {
            sink.post(AppEvent::Host(message));
        })?;
        let owner = host.notify_hwnd().0 as isize;
        let glint = Glint {
            state: Rc::new(RefCell::new(State {
                settings,
                hook: None,
                tray: TrayIcon::new(owner),
                host: Some(host),
                owner,
                busy: Busy::Idle,
                next_id: 1,
                last: None,
                thumbnail: None,
                toast: None,
                settings_window: None,
                recording: None,
                save_timer: None,
                relaunch: None,
                editors: Vec::new(),
                hidden_editors: Vec::new(),
                workers: Vec::new(),
            })),
            proxy,
        };
        glint.register(app, relaunch.clone());
        let hook_ok = glint.start_hook();
        glint.show_tray_with_retry(app, TRAY_ATTEMPTS);
        warm_up_capture();
        cleanup_drag_files();
        glint.run_command(app, initial, hook_ok);
        Ok(())
    })?;
    let relaunch = relaunch_out.borrow_mut().take();
    Ok(relaunch.map_or(Exit::Done, Exit::Relaunch))
}

fn warm_up_capture() {
    let spawned = std::thread::Builder::new().name("glint-warm-up".into()).spawn(|| match glint_capture::warm_up() {
        Ok(devices) => log::info!("capture warmed up ({devices} devices)"),
        Err(error) => log::warn!("capture warm-up failed: {error:#}"),
    });
    if let Err(error) = spawned {
        log::warn!("could not start the warm-up thread: {error}");
    }
}

fn drag_dir() -> PathBuf {
    std::env::temp_dir().join("Glint")
}

fn cleanup_drag_files() {
    if let Ok(entries) = std::fs::read_dir(drag_dir()) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn error_text(error: &anyhow::Error) -> String {
    first_line(&format!("{error:#}"), 90).unwrap_or_else(|| "Unknown error".into())
}

fn monitor_under_cursor() -> Option<MonitorInfo> {
    let monitors = glint_capture::monitors().map_err(|e| log::warn!("monitors: {e:#}")).ok()?;
    monitor_at(&monitors, cursor_pos()).cloned()
}

fn spawn_worker(name: &str, work: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(work).map_err(|error| log::error!("could not start {name}: {error}")).ok()
}

/// A claimed PNG path in %TEMP%\Glint for dragging a capture that was not saved.
fn reserve_drag_file() -> Option<PathBuf> {
    std::fs::create_dir_all(drag_dir())
        .map_err(anyhow::Error::from)
        .and_then(|()| reserve_unique_path(&drag_dir(), "Screenshot", "png"))
        .map_err(|error| log::warn!("drag file: {error:#}"))
        .ok()
}

fn exe_for_login() -> PathBuf {
    if install::is_installed()
        && let Ok(path) = install::installed_exe_path()
    {
        return path;
    }
    std::env::current_exe().unwrap_or_default()
}

fn settings_env(settings: &Settings) -> SettingsEnv {
    let displays = glint_capture::monitors()
        .map(|monitors| monitors.into_iter().map(|m| DisplayLine { name: m.friendly_name, hdr: m.hdr }).collect())
        .unwrap_or_default();
    let dir = |result: Result<PathBuf>| result.map(|p| p.display().to_string()).unwrap_or_else(|e| format!("Unavailable ({e})"));
    SettingsEnv {
        displays,
        screenshots_dir: dir(paths::screenshots_dir(settings)),
        recordings_dir: dir(paths::recordings_dir(settings)),
        install: match install::installed_exe_path() {
            Ok(path) if install::is_installed() => InstallState::Installed { path: path.display().to_string() },
            _ => InstallState::NotInstalled,
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

impl Glint {
    fn register(&self, app: &App, relaunch: Rc<RefCell<Option<PathBuf>>>) {
        let glint = self.clone();
        app.on_event(move |app: &App, event: AppEvent| glint.on_event(app, event));
        let glint = self.clone();
        app.on_event(move |app: &App, event: ThumbnailEvent| glint.on_thumbnail(app, event));
        let glint = self.clone();
        app.on_event(move |_: &App, ToastClosed(id): ToastClosed| {
            let mut state = glint.state.borrow_mut();
            if state.toast.as_ref().is_some_and(|t| t.id == id) {
                state.toast = None;
            }
        });
        let glint = self.clone();
        app.on_event(move |app: &App, command: HudCommand| glint.on_hud(app, command));
        let glint = self.clone();
        app.on_event(move |app: &App, message: SettingsMessage| glint.on_settings(app, message));
        let glint = self.clone();
        app.on_event(move |app: &App, QuitRequest: QuitRequest| {
            *relaunch.borrow_mut() = glint.state.borrow_mut().relaunch.take();
            app.quit();
        });
        let state = Rc::downgrade(&self.state);
        let proxy = self.proxy.clone();
        crate::host::set_end_session_handler(move || {
            if let Some(state) = state.upgrade() {
                Glint { state, proxy: proxy.clone() }.end_session();
            }
        });
    }

    /// Runs `work` on a named thread whose completion `quit` waits for (bounded).
    fn spawn_tracked(&self, name: &str, work: impl FnOnce() + Send + 'static) -> bool {
        let Some(handle) = spawn_worker(name, work) else { return false };
        let mut state = self.state.borrow_mut();
        state.workers.retain(|w| !w.is_finished());
        state.workers.push(handle);
        true
    }

    fn next_id(&self) -> u64 {
        let mut state = self.state.borrow_mut();
        state.next_id += 1;
        state.next_id
    }

    fn settings(&self) -> Settings {
        self.state.borrow().settings.clone()
    }

    fn owner(&self) -> isize {
        self.state.borrow().owner
    }

    // ---- startup and commands -------------------------------------------------------------------------------

    fn start_hook(&self) -> bool {
        let config = HookConfig::from(&self.state.borrow().settings.hotkeys);
        let proxy = self.proxy.clone();
        match KeyboardHook::start(config, move |action| {
            proxy.post(AppEvent::Hotkey(action));
        }) {
            Ok(hook) => {
                self.state.borrow_mut().hook = Some(hook);
                true
            }
            Err(error) => {
                log::error!("keyboard hook failed: {error:#}");
                false
            }
        }
    }

    fn tray_dpi(&self) -> u32 {
        glint_ui::win::dpi_for_window(self.owner())
    }

    fn show_tray(&self, app: &App) -> bool {
        let dpi = self.tray_dpi();
        let gfx = app.gfx();
        let shown = self.state.borrow_mut().tray.show(&gfx, dpi);
        shown.map_err(|error| log::warn!("tray icon: {error:#}")).is_ok()
    }

    /// Explorer may not be ready at logon: retry every 2 s for up to 2 minutes (TaskbarCreated also re-adds it).
    fn show_tray_with_retry(&self, app: &App, attempts_left: u32) {
        if self.show_tray(app) || attempts_left == 0 {
            return;
        }
        let glint = self.clone();
        app.set_timer(TRAY_RETRY, move |app| {
            if !glint.state.borrow().tray.is_shown() {
                glint.show_tray_with_retry(app, attempts_left - 1);
            }
        });
    }

    fn run_command(&self, app: &App, command: Command, hook_ok: bool) {
        log::info!("command {command:?}");
        match command {
            Command::Start(Greeting::Silent) => {}
            Command::Start(greeting) => {
                let title = if greeting == Greeting::Installed { "Glint is installed" } else { "Glint is running" };
                let detail = if hook_ok { "Press Win + Shift + S to snip" } else { "Use the tray icon to snip" };
                self.toast(app, Toast::new(ToastIcon::App, title, Some(detail)));
            }
            Command::Snip(mode) => {
                let mode = mode.unwrap_or_else(|| snip_mode(&self.state.borrow().settings));
                self.capture(app, Intent::Overlay { mode, video: false });
            }
            Command::Record => self.record_shortcut(app),
            Command::Settings => self.open_settings(app),
            Command::Edit(path) => self.open_image(app, path),
            Command::Quit => self.quit(),
            Command::Uninstall => self.uninstall(app),
            other => log::info!("ignoring {other:?} in the resident instance"),
        }
    }

    fn on_event(&self, app: &App, event: AppEvent) {
        match event {
            AppEvent::Host(message) => self.on_host(app, message),
            AppEvent::Hotkey(action) => self.on_hotkey(app, action),
            AppEvent::Captured { intent, result } => self.on_captured(app, intent, result),
            AppEvent::OverlayClosed { outcome, prefs } => self.on_overlay_closed(app, outcome, prefs),
            AppEvent::CountdownDone { purpose, completed } => self.on_countdown(app, purpose, completed),
            AppEvent::PipelineDone { errors } => {
                if let Some(error) = errors.first() {
                    self.toast(app, Toast::new(ToastIcon::Symbol(glint_ui::Icon::Info, Tint::Destructive), "Couldn't finish the snip", Some(error)));
                }
            }
            AppEvent::TextRecognized(result) => self.on_text(app, result),
            AppEvent::RecordingFinished { monitor, result } => self.on_recording_finished(app, monitor, result),
            AppEvent::NewSnip { mode, delay_secs } => self.new_snip_from_editor(app, mode, delay_secs),
            AppEvent::EditorSaved { id, path } => {
                if let Some(last) = self.state.borrow_mut().last.as_mut().filter(|l| l.id == id) {
                    last.saved = Some(path);
                }
            }
            AppEvent::ImageLoaded { path, result } => match result {
                Ok(image) => self.edit_image(app, image, path),
                Err(error) => self.error_toast(app, "Couldn't open the image", &error),
            },
            AppEvent::Copied { id, result } => match result {
                Ok(()) => self.with_thumbnail(app, id, |view, cx| view.flash_copied(cx)),
                Err(error) => self.error_toast(app, "Couldn't copy", &error),
            },
            AppEvent::SavedAs { id, result } => {
                self.with_thumbnail(app, id, |view, cx| view.set_held(cx, false));
                match result {
                    Ok(path) => {
                        if let Some(last) = self.state.borrow_mut().last.as_mut().filter(|l| l.id == id) {
                            last.saved = Some(path);
                        }
                    }
                    Err(error) => self.error_toast(app, "Couldn't save", &error),
                }
            }
        }
    }

    fn on_host(&self, app: &App, message: HostMessage) {
        match message {
            HostMessage::Forwarded(args) => {
                let parsed = cli::parse(&args);
                for ignored in &parsed.ignored {
                    log::info!("ignoring forwarded argument {ignored:?}");
                }
                let hook_ok = self.state.borrow().hook.is_some();
                self.run_command(app, parsed.command, hook_ok);
            }
            HostMessage::TrayActivate => {
                let mode = snip_mode(&self.state.borrow().settings);
                self.capture(app, Intent::Overlay { mode, video: false });
            }
            HostMessage::TrayMenu(at) => self.tray_menu(app, at),
            HostMessage::TaskbarCreated => {
                self.show_tray(app);
            }
            HostMessage::SessionUnlocked => {
                if let Some(hook) = &self.state.borrow().hook {
                    hook.on_session_unlock();
                }
            }
            HostMessage::ThemeChanged => self.refresh_tray(app),
            HostMessage::DisplayChanged => {
                self.refresh_tray(app);
                warm_up_capture();
            }
        }
    }

    fn refresh_tray(&self, app: &App) {
        let dpi = self.tray_dpi();
        let gfx = app.gfx();
        if let Err(error) = self.state.borrow_mut().tray.refresh(&gfx, dpi) {
            log::warn!("tray icon refresh: {error:#}");
        }
    }

    fn tray_menu(&self, app: &App, at: PointI) {
        let dark = {
            let state = self.state.borrow();
            match state.settings.appearance.theme {
                glint_core::ThemeMode::Dark => true,
                glint_core::ThemeMode::Light => false,
                glint_core::ThemeMode::System => app.system_prefers_dark(),
            }
        };
        glint_sys::tray::set_dark_menus(dark);
        let recording = self.state.borrow().recording.is_some();
        let owner = hwnd(self.owner());
        let Some(command) = glint_sys::tray::show_context_menu(owner, &menu_items(recording), at).and_then(command_for) else {
            return;
        };
        match command {
            TrayCommand::Snip(mode) => {
                self.capture(app, Intent::Overlay { mode, video: false });
            }
            TrayCommand::Record => self.record_shortcut(app),
            TrayCommand::StopRecording => self.stop_recording(app),
            TrayCommand::Text => {
                self.capture(app, Intent::Overlay { mode: CaptureMode::Text, video: false });
            }
            TrayCommand::OpenImage => {
                let folder = paths::screenshots_dir(&self.settings()).ok();
                if let Some(path) = crate::system::pick_image(self.owner(), folder.as_deref()) {
                    self.open_image(app, path);
                }
            }
            TrayCommand::OpenScreenshots => {
                let opened = paths::screenshots_dir(&self.settings()).and_then(|dir| shell::open_path(&dir));
                if let Err(error) = opened {
                    self.error_toast(app, "Couldn't open the folder", &error);
                }
            }
            TrayCommand::Settings => self.open_settings(app),
            TrayCommand::Quit => self.quit(),
        }
    }

    fn on_hotkey(&self, app: &App, action: HotkeyAction) {
        log::info!("hotkey {action:?}");
        let mode = snip_mode(&self.state.borrow().settings);
        match action {
            HotkeyAction::Snip => {
                self.capture(app, Intent::Overlay { mode, video: false });
            }
            HotkeyAction::Text => {
                self.capture(app, Intent::Overlay { mode: CaptureMode::Text, video: false });
            }
            HotkeyAction::Record => self.record_shortcut(app),
            HotkeyAction::WindowToClipboard => {
                self.capture(app, Intent::ActiveWindow);
            }
        }
    }

    fn record_shortcut(&self, app: &App) {
        if self.state.borrow().recording.is_some() {
            self.stop_recording(app);
        } else {
            let mode = snip_mode(&self.state.borrow().settings);
            self.capture(app, Intent::Overlay { mode, video: true });
        }
    }

    // ---- capture ------------------------------------------------------------------------------------------------

    /// Freezes every monitor on a worker thread, then opens the overlay (or finishes Alt+Print Screen). Returns
    /// false when it did not start. While recording, the overlay opens for photos only.
    fn capture(&self, app: &App, intent: Intent) -> bool {
        let (hdr, intent) = {
            let mut state = self.state.borrow_mut();
            if state.busy != Busy::Idle {
                log::info!("capture ignored: {:?} in progress", state.busy);
                return false;
            }
            state.busy = Busy::Capturing;
            let intent = match intent {
                Intent::Overlay { mode, video: true } if state.recording.is_some() => Intent::Overlay { mode, video: false },
                other => other,
            };
            (state.settings.hdr.clone(), intent)
        };
        let proxy = self.proxy.clone();
        let started = spawn_worker("glint-capture", move || {
            let result = std::panic::catch_unwind(|| freeze(&hdr, intent))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("the capture thread panicked")));
            proxy.post(AppEvent::Captured { intent, result });
        });
        if started.is_none() {
            self.state.borrow_mut().busy = Busy::Idle;
            let detail = Some("Windows could not start a capture thread");
            self.toast(app, Toast::new(ToastIcon::Symbol(glint_ui::Icon::Info, Tint::Destructive), "Couldn't capture the screen", detail));
            return false;
        }
        true
    }

    // ---- snips started from the editor ---------------------------------------------------------------------

    /// Editor "New" / Ctrl+N: hide the editors so they are not in the capture, let DWM settle, then capture (or
    /// count down). They come back when the overlay or countdown ends.
    fn new_snip_from_editor(&self, app: &App, mode: CaptureMode, delay_secs: u32) {
        if self.state.borrow().busy != Busy::Idle {
            return;
        }
        let intent = Intent::Overlay { mode, video: false };
        let glint = self.clone();
        let start = move |app: &App| {
            let started = if delay_secs > 0 { glint.delay(app, delay_secs, intent) } else { glint.capture(app, intent) };
            if !started {
                glint.restore_editors(true);
            }
        };
        if self.hide_editors(app) {
            app.set_timer(EDITOR_HIDE_SETTLE, start);
        } else {
            start(app);
        }
    }

    fn hide_editors(&self, app: &App) -> bool {
        let mut state = self.state.borrow_mut();
        state.editors.retain(|id| app.hwnd(*id).is_some());
        let windows: Vec<isize> = state.editors.iter().filter_map(|id| app.hwnd(*id)).collect();
        for window in windows {
            if hide_instantly(window) {
                state.hidden_editors.push(window);
            }
        }
        !state.hidden_editors.is_empty()
    }

    /// Shows editors hidden by `new_snip_from_editor`; `activate` brings the last one back to the front.
    fn restore_editors(&self, activate: bool) {
        let hidden = std::mem::take(&mut self.state.borrow_mut().hidden_editors);
        let count = hidden.len();
        for (i, window) in hidden.into_iter().enumerate() {
            show_again(window, activate && i + 1 == count);
        }
    }

    fn on_captured(&self, app: &App, intent: Intent, result: Result<Frozen>) {
        let frozen = match result {
            Ok(frozen) => frozen,
            Err(error) => {
                self.state.borrow_mut().busy = Busy::Idle;
                self.restore_editors(true);
                self.error_toast(app, "Couldn't capture the screen", &error);
                return;
            }
        };
        let settings = self.settings();
        match intent {
            Intent::ActiveWindow => {
                self.state.borrow_mut().busy = Busy::Idle;
                self.restore_editors(false);
                let snip = frozen
                    .foreground
                    .and_then(|window| glint_overlay::snip_rect(&frozen.captures, window.rect, &settings.hdr, CaptureMode::Window));
                match snip {
                    Some(snip) => self.finish_snip(app, snip, CaptureSource::WindowShortcut),
                    None => self.toast(app, Toast::new(ToastIcon::Symbol(glint_ui::Icon::AppWindow, Tint::Neutral), "No window to capture", None)),
                }
            }
            Intent::Overlay { mode, video } => {
                let request = OverlayRequest {
                    captures: frozen.captures,
                    windows: frozen.windows,
                    mode,
                    video,
                    show_magnifier: settings.capture.show_magnifier,
                    delay_secs: settings.capture.delay_secs,
                    hdr: settings.hdr.clone(),
                    system_audio: settings.record.system_audio,
                    microphone: settings.record.microphone,
                };
                self.state.borrow_mut().busy = Busy::Overlay;
                let opened = glint_overlay::open_overlay(app, request, |app: &App, outcome, prefs| {
                    app.post(AppEvent::OverlayClosed { outcome, prefs });
                });
                if let Err(error) = opened {
                    self.state.borrow_mut().busy = Busy::Idle;
                    self.restore_editors(true);
                    self.error_toast(app, "Couldn't open the snip overlay", &error);
                }
            }
        }
    }

    fn on_overlay_closed(&self, app: &App, outcome: OverlayOutcome, prefs: OverlayPrefs) {
        let remembered = {
            let mut state = self.state.borrow_mut();
            state.busy = Busy::Idle;
            remember_overlay_choices(&mut state.settings, &prefs)
        };
        if remembered {
            self.schedule_save(app);
            self.refresh_settings_window(app);
        }
        self.restore_editors(matches!(outcome, OverlayOutcome::Cancelled));
        log::info!("overlay closed: {}", outcome_label(&outcome));
        match outcome {
            OverlayOutcome::Cancelled => {}
            OverlayOutcome::Snip(snip) => self.finish_snip(app, snip, CaptureSource::Overlay),
            OverlayOutcome::Text(snip) => self.recognize_text(snip.image),
            OverlayOutcome::Color { bgra } => {
                let hex = hex_color(bgra);
                match clipboard::copy_text(hwnd(self.owner()), &hex) {
                    Ok(()) => self.toast(app, Toast::new(ToastIcon::Swatch(bgra), &format!("Copied {hex}"), None)),
                    Err(error) => self.error_toast(app, "Couldn't copy the color", &error),
                }
            }
            OverlayOutcome::Record(region) => {
                if self.refuse_second_recording(app) {
                    return;
                }
                let monitor = region.monitor.clone();
                self.countdown(app, &monitor, RECORD_COUNTDOWN_SECS, CountdownPurpose::Record(region));
            }
            OverlayOutcome::Delay { secs } => {
                self.delay(app, secs, Intent::Overlay { mode: prefs.mode, video: prefs.video });
            }
        }
    }

    /// One recording at a time: a second one is refused with a toast.
    fn refuse_second_recording(&self, app: &App) -> bool {
        if self.state.borrow().recording.is_none() {
            return false;
        }
        let icon = ToastIcon::Symbol(glint_ui::Icon::Video, Tint::Neutral);
        self.toast(app, Toast::new(icon, "Already recording", Some("Press Win + Shift + R to stop")));
        true
    }

    fn delay(&self, app: &App, secs: u32, intent: Intent) -> bool {
        match monitor_under_cursor() {
            Some(monitor) => self.countdown(app, &monitor, secs, CountdownPurpose::Recapture(intent)),
            None => self.capture(app, intent),
        }
    }

    fn countdown(&self, app: &App, monitor: &MonitorInfo, secs: u32, purpose: CountdownPurpose) -> bool {
        {
            let mut state = self.state.borrow_mut();
            if state.busy != Busy::Idle {
                return false;
            }
            state.busy = Busy::Countdown;
        }
        let shown = glint_overlay::show_countdown(app, monitor, secs, move |app: &App, completed| {
            app.post(AppEvent::CountdownDone { purpose, completed });
        });
        if let Err(error) = shown {
            log::warn!("countdown: {error:#}");
            self.state.borrow_mut().busy = Busy::Idle;
            self.error_toast(app, "Couldn't show the countdown", &error);
            return false;
        }
        true
    }

    fn on_countdown(&self, app: &App, purpose: CountdownPurpose, completed: bool) {
        self.state.borrow_mut().busy = Busy::Idle;
        if !completed {
            self.restore_editors(true);
            return;
        }
        match purpose {
            CountdownPurpose::Recapture(intent) => {
                if !self.capture(app, intent) {
                    self.restore_editors(true);
                }
            }
            CountdownPurpose::Record(region) => {
                if !self.refuse_second_recording(app) {
                    self.start_recording(app, region);
                }
            }
        }
    }

    // ---- after capture ---------------------------------------------------------------------------------------

    /// DESIGN §7: outline → clipboard → auto-save → sound → thumbnail or editor. Clipboard and encoding run on a
    /// worker; the thumbnail appears at once and knows where the file will be.
    fn finish_snip(&self, app: &App, snip: Snip, source: CaptureSource) {
        let settings = self.settings();
        let plan = plan(&settings.after_capture, source);
        let mut image = snip.image;
        if let Some(outline) = &plan.outline {
            draw_outline(&mut image, outline);
        }
        let id = self.next_id();
        let save = plan.save.and_then(|format| {
            let dir = self.screenshot_folder(app, &settings)?;
            reserve_unique_path(&dir, "Screenshot", extension(format))
                .map(|path| (path, format))
                .map_err(|error| self.error_toast(app, "Couldn't save the screenshot", &error))
                .ok()
        });
        let drag_file = plan.temp_drag_file.then(reserve_drag_file).flatten();
        let worker_image = image.clone();
        let owner = self.owner();
        let copy = plan.copy;
        let worker_save = save.clone();
        let worker_drag = drag_file.clone();
        let proxy = self.proxy.clone();
        self.spawn_tracked("glint-after-capture", move || {
            let mut errors = Vec::new();
            if copy && let Err(error) = clipboard::copy_image(hwnd(owner), &worker_image) {
                errors.push(format!("Clipboard: {}", error_text(&error)));
            }
            if let Some((path, format)) = &worker_save
                && let Err(error) = encode::save(&worker_image, path, *format)
            {
                let _ = std::fs::remove_file(path);
                errors.push(format!("Saving {}: {}", path.display(), error_text(&error)));
            }
            if let Some(path) = &worker_drag
                && let Err(error) = encode::save(&worker_image, path, ImageFormat::Png)
            {
                log::warn!("drag file: {error:#}");
            }
            proxy.post(AppEvent::PipelineDone { errors });
        });
        if plan.sound {
            sound::play_shutter();
        }
        let image = Rc::new(image);
        self.state.borrow_mut().last = Some(LastCapture {
            id,
            image,
            kind: Captured::Image { hdr: snip.hdr },
            monitor: snip.monitor,
            saved: save.map(|(path, _)| path),
            drag_file,
        });
        match plan.presentation {
            Presentation::Thumbnail => self.show_thumbnail(app),
            Presentation::Editor => self.open_editor(app),
            Presentation::Nothing => {}
        }
    }

    /// The configured screenshots folder; if it cannot be used, says so and falls back to Pictures\Screenshots.
    fn screenshot_folder(&self, app: &App, settings: &Settings) -> Option<PathBuf> {
        let error = match paths::screenshots_dir(settings) {
            Ok(dir) => return Some(dir),
            Err(error) => error,
        };
        let mut default = settings.clone();
        default.after_capture.save_dir = None;
        match paths::screenshots_dir(&default) {
            Ok(dir) => {
                log::warn!("save folder unavailable, using {}: {error:#}", dir.display());
                let detail = format!("Saved to {} instead", dir.display());
                let icon = ToastIcon::Symbol(glint_ui::Icon::Folder, Tint::Destructive);
                self.toast(app, Toast::new(icon, "Your screenshots folder is unavailable", Some(&detail)));
                Some(dir)
            }
            Err(fallback) => {
                self.error_toast(app, "Couldn't save the screenshot", &error.context(fallback));
                None
            }
        }
    }

    fn recognize_text(&self, image: Image) {
        let proxy = self.proxy.clone();
        spawn_worker("glint-ocr", move || {
            let result = glint_sys::ocr::ocr(&image).map(|r| r.text);
            proxy.post(AppEvent::TextRecognized(result));
        });
    }

    fn on_text(&self, app: &App, result: Result<String>) {
        match result {
            Ok(text) if !text.trim().is_empty() => match clipboard::copy_text(hwnd(self.owner()), &text) {
                Ok(()) => {
                    let line = first_line(&text, 60);
                    self.toast(app, Toast::new(ToastIcon::Symbol(glint_ui::Icon::ScanText, Tint::Accent), "Text copied", line.as_deref()));
                }
                Err(error) => self.error_toast(app, "Couldn't copy the text", &error),
            },
            Ok(_) => self.toast(
                app,
                Toast::new(ToastIcon::Symbol(glint_ui::Icon::ScanText, Tint::Neutral), "No text found", Some("Try a larger or sharper selection")),
            ),
            Err(error) => self.error_toast(app, "Couldn't read text", &error),
        }
    }

    fn show_thumbnail(&self, app: &App) {
        let (id, image, monitor, content) = {
            let state = self.state.borrow();
            let Some(last) = &state.last else { return };
            let content = match &last.kind {
                Captured::Image { .. } => ThumbnailContent::Image,
                Captured::Video { .. } => ThumbnailContent::Video { duration: Duration::ZERO },
            };
            (last.id, last.image.clone(), last.monitor.clone(), content)
        };
        self.open_thumbnail(app, id, image, monitor, content);
    }

    fn open_thumbnail(&self, app: &App, id: u64, image: Rc<Image>, monitor: MonitorInfo, content: ThumbnailContent) {
        let scale = monitor.scale();
        let card = card_size(image.width, image.height, scale);
        let layout = corner_layout(card, monitor.work_rect, scale, 0);
        if let Some(old) = self.state.borrow_mut().thumbnail.take() {
            app.close(old.window);
        }
        let view = ThumbnailView::new(id, image, scale, content, layout.card);
        match app.open(WindowSpec::popup(layout.origin_px, layout.size).exclude_from_capture(), view) {
            Ok(window) => {
                self.state.borrow_mut().thumbnail = Some(OpenPopup { id, window, layout, scale, work: monitor.work_rect });
                self.lift_toast_above_thumbnail(app);
            }
            Err(error) => log::error!("thumbnail window: {error:#}"),
        }
    }

    /// A toast already showing in the same corner moves up so the new thumbnail does not cover it.
    fn lift_toast_above_thumbnail(&self, app: &App) {
        let (window, layout) = {
            let state = self.state.borrow();
            let (Some(thumb), Some(toast)) = (&state.thumbnail, &state.toast) else { return };
            if toast.work != thumb.work {
                return;
            }
            let lift = occupied_height_px(&thumb.layout, thumb.scale);
            (toast.window, corner_layout(toast.layout.card.size(), toast.work, toast.scale, lift))
        };
        app.with_view(window, |_: &mut ToastView, cx| cx.move_window_px(layout.origin_px));
        if let Some(toast) = self.state.borrow_mut().toast.as_mut() {
            toast.layout = layout;
        }
    }

    fn with_thumbnail(&self, app: &App, id: u64, f: impl FnOnce(&mut ThumbnailView, &mut glint_ui::Ctx)) {
        let window = self.state.borrow().thumbnail.as_ref().filter(|t| t.id == id).map(|t| t.window);
        if let Some(window) = window {
            app.with_view(window, f);
        }
    }

    fn on_thumbnail(&self, app: &App, event: ThumbnailEvent) {
        let current = self.state.borrow().last.as_ref().is_some_and(|l| l.id == event.id);
        if event.action == ThumbnailAction::Closed {
            let mut state = self.state.borrow_mut();
            if state.thumbnail.as_ref().is_some_and(|t| t.id == event.id) {
                state.thumbnail = None;
            }
            return;
        }
        if !current {
            return;
        }
        match event.action {
            ThumbnailAction::Open | ThumbnailAction::Edit => {
                let video = match &self.state.borrow().last.as_ref().map(|l| &l.kind) {
                    Some(Captured::Video { path }) => Some(path.clone()),
                    _ => None,
                };
                self.with_thumbnail(app, event.id, |view, cx| view.dismiss(cx));
                match video {
                    Some(path) => {
                        if let Err(error) = shell::open_path(&path) {
                            self.error_toast(app, "Couldn't open the recording", &error);
                        }
                    }
                    None => self.open_editor(app),
                }
            }
            ThumbnailAction::Copy => {
                let Some(image) = self.state.borrow().last.as_ref().map(|l| (*l.image).clone()) else { return };
                let owner = self.owner();
                let proxy = self.proxy.clone();
                let id = event.id;
                self.spawn_tracked("glint-copy", move || {
                    let result = clipboard::copy_image(hwnd(owner), &image);
                    proxy.post(AppEvent::Copied { id, result });
                });
            }
            ThumbnailAction::Save => self.save_as(app, event.id),
            ThumbnailAction::Drag => self.drag_last(app, event.id),
            ThumbnailAction::Closed => {}
        }
    }

    fn save_as(&self, app: &App, id: u64) {
        let settings = self.settings();
        let Some(image) = self.state.borrow().last.as_ref().map(|l| (*l.image).clone()) else { return };
        let dir = paths::screenshots_dir(&settings).unwrap_or_else(|_| std::env::temp_dir());
        let name = paths::timestamped_name("Screenshot", extension(settings.after_capture.format), std::time::SystemTime::now());
        self.with_thumbnail(app, id, |view, cx| view.set_held(cx, true));
        let chosen = dialogs::save_image_dialog(hwnd(self.owner()), &dir, &name, settings.after_capture.format);
        let Some((path, format)) = chosen else {
            self.with_thumbnail(app, id, |view, cx| view.set_held(cx, false));
            return;
        };
        let proxy = self.proxy.clone();
        self.spawn_tracked("glint-save", move || {
            let result = encode::save(&image, &path, format).map(|()| path);
            proxy.post(AppEvent::SavedAs { id, result });
        });
    }

    /// OLE file drag of the saved file (or a PNG in %TEMP% when nothing was saved). Blocks in a modal loop.
    fn drag_last(&self, app: &App, id: u64) {
        let (file, image) = {
            let state = self.state.borrow();
            let Some(last) = &state.last else { return };
            let file = match &last.kind {
                Captured::Video { path } => Some(path.clone()),
                Captured::Image { .. } => last.saved.clone().filter(|p| p.exists()).or_else(|| last.drag_file.clone().filter(|p| p.exists())),
            };
            (file, last.image.clone())
        };
        let file = match file {
            Some(file) => file,
            None => {
                let written = reserve_drag_file()
                    .context("no temporary file for the drag")
                    .and_then(|path| encode::save(&image, &path, ImageFormat::Png).map(|()| path));
                let path = match written {
                    Ok(path) => path,
                    Err(error) => {
                        self.error_toast(app, "Couldn't prepare the file", &error);
                        return;
                    }
                };
                if let Some(last) = self.state.borrow_mut().last.as_mut() {
                    last.drag_file = Some(path.clone());
                }
                path
            }
        };
        let source = self.state.borrow().thumbnail.as_ref().filter(|t| t.id == id).and_then(|t| app.hwnd(t.window));
        let source = source.unwrap_or_else(|| self.owner());
        self.with_thumbnail(app, id, |view, cx| view.set_held(cx, true));
        if let Err(error) = shell::drag_files(hwnd(source), &[file]) {
            log::warn!("drag: {error:#}");
        }
        self.with_thumbnail(app, id, |view, cx| view.set_held(cx, false));
    }

    /// Reads and decodes a PNG/JPEG on a worker, then opens it in the editor (`ImageLoaded`).
    fn open_image(&self, app: &App, path: PathBuf) {
        let proxy = self.proxy.clone();
        let started = spawn_worker("glint-open-image", move || {
            let result = std::fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| encode::decode(&bytes))
                .with_context(|| path.display().to_string());
            proxy.post(AppEvent::ImageLoaded { path, result });
        });
        if started.is_none() {
            let icon = ToastIcon::Symbol(glint_ui::Icon::Info, Tint::Destructive);
            self.toast(app, Toast::new(icon, "Couldn't open the image", None));
        }
    }

    /// An image file in the editor: Ctrl+S writes back to it; it never becomes the thumbnail's capture.
    fn edit_image(&self, app: &App, image: Image, path: PathBuf) {
        let settings = self.settings();
        let doc = EditorDoc {
            image,
            hdr: None,
            tone_map: ToneMapParams { mode: settings.hdr.mode, exposure_stops: settings.hdr.exposure_stops },
            monitor: None,
            saved_path: Some(path),
        };
        let id = self.next_id();
        self.show_editor(app, id, doc, &settings);
    }

    fn open_editor(&self, app: &App) {
        let settings = self.settings();
        let (id, doc) = {
            let state = self.state.borrow();
            let Some(last) = &state.last else { return };
            let Captured::Image { hdr } = &last.kind else { return };
            let doc = EditorDoc {
                image: (*last.image).clone(),
                hdr: hdr.clone(),
                tone_map: ToneMapParams { mode: settings.hdr.mode, exposure_stops: settings.hdr.exposure_stops },
                monitor: Some(last.monitor.clone()),
                saved_path: last.saved.clone(),
            };
            (last.id, doc)
        };
        self.show_editor(app, id, doc, &settings);
    }

    /// Opens an editor window; `id` scopes its "saved" reports to the capture it shows.
    fn show_editor(&self, app: &App, id: u64, doc: EditorDoc, settings: &Settings) {
        let host = EditorHost {
            new_snip: Rc::new(|app: &App, mode: CaptureMode, delay_secs: u32| app.post(AppEvent::NewSnip { mode, delay_secs })),
            saved: Rc::new(move |app: &App, path: PathBuf| app.post(AppEvent::EditorSaved { id, path })),
        };
        match glint_editor::open_editor(app, doc, settings, host) {
            Ok(window) => {
                let mut state = self.state.borrow_mut();
                state.editors.retain(|w| app.hwnd(*w).is_some());
                state.editors.push(window);
            }
            Err(error) => self.error_toast(app, "Couldn't open the editor", &error),
        }
    }

    // ---- recording ------------------------------------------------------------------------------------------

    fn start_recording(&self, app: &App, region: RecordRegion) {
        let settings = self.settings();
        let output = paths::recordings_dir(&settings).and_then(|dir| paths::unique_path(&dir, "Screen Recording", "mp4"));
        let output = match output {
            Ok(path) => path,
            Err(error) => return self.error_toast(app, "Couldn't start recording", &error),
        };
        let config = RecordConfig {
            monitor: region.monitor.clone(),
            region: region.region,
            fps: settings.record.fps,
            system_audio: region.system_audio,
            microphone: region.microphone,
            include_cursor: settings.record.include_cursor,
            output,
            tonemap: ToneMapParams { mode: settings.record.tone_map, exposure_stops: settings.hdr.exposure_stops },
        };
        let recorder = match Recorder::start(config) {
            Ok(recorder) => recorder,
            Err(error) => return self.error_toast(app, "Couldn't start recording", &error),
        };
        let audio = region.system_audio || region.microphone;
        let monitor = region.monitor.clone();
        let hud = HudView::new(audio, region.microphone);
        let hud_size = hud.window_size(&app.gfx());
        let scale = monitor.scale();
        let hud_origin = PointI::new(monitor.rect.x + (monitor.rect.w - (hud_size.w * scale).round() as i32) / 2, monitor.rect.y);
        let hud = app
            .open(WindowSpec::popup(hud_origin, hud_size).exclude_from_capture(), hud)
            .map_err(|e| log::error!("HUD window: {e:#}"))
            .ok();
        let absolute = region.region.offset(monitor.rect.x, monitor.rect.y);
        let frame = border_rect_px(absolute, monitor.rect, scale);
        let frame_size = SizeF::new(frame.w as f32 / scale, frame.h as f32 / scale);
        let border = app
            .open(WindowSpec::popup(PointI::new(frame.x, frame.y), frame_size).click_through().exclude_from_capture(), BorderView)
            .map_err(|e| log::error!("border window: {e:#}"))
            .ok();
        self.state.borrow_mut().recording = Some(ActiveRecording { recorder, hud, border, monitor });
        self.schedule_hud_tick(app);
    }

    fn schedule_hud_tick(&self, app: &App) {
        let glint = self.clone();
        app.set_timer(HUD_REFRESH, move |app| glint.hud_tick(app));
    }

    fn hud_tick(&self, app: &App) {
        let (readout, hud, failed) = {
            let state = self.state.borrow();
            let Some(recording) = &state.recording else { return };
            let status = recording.recorder.status();
            let phase = match status {
                RecorderStatus::Starting => HudPhase::Starting,
                RecorderStatus::Paused => HudPhase::Paused,
                RecorderStatus::Recording | RecorderStatus::Failed(_) => HudPhase::Recording,
            };
            let readout = HudReadout { phase, elapsed: recording.recorder.elapsed(), level: recording.recorder.audio_level() };
            (readout, recording.hud, matches!(status, RecorderStatus::Failed(_)))
        };
        if failed {
            self.stop_recording(app);
            return;
        }
        if let Some(hud) = hud {
            app.with_view(hud, |view: &mut HudView, cx| view.update(cx, readout));
        }
        self.schedule_hud_tick(app);
    }

    fn take_recording(&self, app: &App) -> Option<ActiveRecording> {
        let recording = self.state.borrow_mut().recording.take()?;
        for window in [recording.hud, recording.border].into_iter().flatten() {
            app.close(window);
        }
        Some(recording)
    }

    /// Finalizes the recording on a worker. Stopping before the first frame arrived discards it instead.
    fn stop_recording(&self, app: &App) {
        let Some(recording) = self.take_recording(app) else { return };
        if recording.recorder.status() == RecorderStatus::Starting {
            recording.recorder.discard();
            let icon = ToastIcon::Symbol(glint_ui::Icon::Video, Tint::Neutral);
            self.toast(app, Toast::new(icon, "Nothing recorded", Some("Stopped before the first frame")));
            return;
        }
        let proxy = self.proxy.clone();
        let monitor = recording.monitor;
        let recorder = recording.recorder;
        let started = self.spawn_tracked("glint-record-stop", move || {
            let result = recorder.stop();
            proxy.post(AppEvent::RecordingFinished { monitor, result });
        });
        if !started {
            log::error!("the recording could not be finalized on a worker");
        }
    }

    /// Stops a recording on the calling thread (quit, uninstall, end of session).
    fn finish_recording_now(&self, app: Option<&App>) {
        let recording = match app {
            Some(app) => self.take_recording(app),
            None => self.state.try_borrow_mut().ok().and_then(|mut state| state.recording.take()),
        };
        let Some(recording) = recording else { return };
        if recording.recorder.status() == RecorderStatus::Starting {
            recording.recorder.discard();
        } else if let Err(error) = recording.recorder.stop() {
            log::error!("finishing the recording: {error:#}");
        }
    }

    fn on_hud(&self, app: &App, command: HudCommand) {
        match command {
            HudCommand::Stop => self.stop_recording(app),
            HudCommand::Discard => {
                if let Some(recording) = self.take_recording(app) {
                    recording.recorder.discard();
                    self.toast(app, Toast::new(ToastIcon::Symbol(glint_ui::Icon::Trash, Tint::Neutral), "Recording discarded", None));
                }
            }
            HudCommand::TogglePause => {
                if let Some(recording) = &self.state.borrow().recording {
                    if recording.recorder.is_paused() {
                        recording.recorder.resume();
                    } else {
                        recording.recorder.pause();
                    }
                }
            }
            HudCommand::Microphone(on) => {
                if let Some(recording) = &self.state.borrow().recording {
                    recording.recorder.set_microphone(on);
                }
            }
        }
    }

    fn on_recording_finished(&self, app: &App, monitor: MonitorInfo, result: Result<RecordingInfo>) {
        let info = match result {
            Ok(info) => info,
            Err(error) => return self.error_toast(app, "Recording failed", &error),
        };
        for warning in &info.warnings {
            log::warn!("recording: {warning}");
        }
        log::info!("recorded {} ({:?}, {}×{}, {} bytes)", info.path.display(), info.duration, info.width, info.height, info.bytes);
        let image = info.thumbnail.clone().unwrap_or_else(|| placeholder_frame(info.width, info.height));
        let id = self.next_id();
        let image = Rc::new(image);
        self.state.borrow_mut().last = Some(LastCapture {
            id,
            image: image.clone(),
            kind: Captured::Video { path: info.path.clone() },
            monitor: monitor.clone(),
            saved: Some(info.path.clone()),
            drag_file: None,
        });
        if self.state.borrow().settings.after_capture.show_thumbnail {
            self.open_thumbnail(app, id, image, monitor, ThumbnailContent::Video { duration: info.duration });
        } else {
            self.toast(app, Toast::new(ToastIcon::Symbol(glint_ui::Icon::Video, Tint::Accent), "Recording saved", info.path.file_name().and_then(|n| n.to_str())));
        }
    }

    // ---- toasts -----------------------------------------------------------------------------------------------

    fn error_toast(&self, app: &App, title: &str, error: &anyhow::Error) {
        log::error!("{title}: {error:#}");
        self.toast(app, Toast::new(ToastIcon::Symbol(glint_ui::Icon::Info, Tint::Destructive), title, Some(&error_text(error))));
    }

    fn toast(&self, app: &App, toast: Toast) {
        let placement = self.state.borrow().thumbnail.as_ref().map(|t| (t.work, t.scale, occupied_height_px(&t.layout, t.scale)));
        let (work, scale, lift) = placement.unwrap_or_else(|| {
            let cursor = cursor_pos();
            let work = glint_ui::win::work_area_at_point(cursor).unwrap_or(RectI::new(0, 0, 1920, 1040));
            (work, glint_ui::win::dpi_at_point(cursor) as f32 / 96.0, 0)
        });
        let card = crate::toast::card_size(&app.gfx(), &toast);
        let layout = corner_layout(card, work, scale, lift);
        if let Some(old) = self.state.borrow_mut().toast.take() {
            app.close(old.window);
        }
        let id = self.next_id();
        let view = ToastView::new(id, toast, layout.card);
        match app.open(WindowSpec::popup(layout.origin_px, layout.size).exclude_from_capture(), view) {
            Ok(window) => self.state.borrow_mut().toast = Some(OpenPopup { id, window, layout, scale, work }),
            Err(error) => log::error!("toast window: {error:#}"),
        }
    }

    // ---- settings ---------------------------------------------------------------------------------------------

    fn open_settings(&self, app: &App) {
        if let Some(window) = self.state.borrow().settings_window {
            app.with_view(window, |_: &mut SettingsView, cx| cx.activate());
            return;
        }
        let settings = self.settings();
        let env = settings_env(&settings);
        let view = SettingsView::new(settings, env);
        let work = glint_ui::win::work_area_at_point(cursor_pos()).unwrap_or(RectI::new(0, 0, 1920, 1040));
        let spec = WindowSpec::normal("Glint Settings", WINDOW_SIZE).min_size(MIN_SIZE).centered_in(work);
        match app.open(spec, view) {
            Ok(window) => self.state.borrow_mut().settings_window = Some(window),
            Err(error) => self.error_toast(app, "Couldn't open settings", &error),
        }
    }

    fn on_settings(&self, app: &App, message: SettingsMessage) {
        match message {
            SettingsMessage::Changed(new) => self.apply_settings(app, *new),
            SettingsMessage::Request(request) => self.on_settings_request(app, request),
            SettingsMessage::Closed => self.state.borrow_mut().settings_window = None,
        }
    }

    fn apply_settings(&self, app: &App, new: Settings) {
        let (changes, hook_missing) = {
            let mut state = self.state.borrow_mut();
            let changes = effects(&state.settings, &new);
            state.settings = new.clone();
            if changes.hotkeys
                && let Some(hook) = &state.hook
            {
                hook.set_config(HookConfig::from(&new.hotkeys));
            }
            (changes, state.hook.is_none())
        };
        if changes.hotkeys && hook_missing && !self.start_hook() {
            let icon = ToastIcon::Symbol(glint_ui::Icon::Keyboard, Tint::Destructive);
            self.toast(app, Toast::new(icon, "Shortcuts are unavailable", Some("Use the tray icon to snip")));
        }
        if changes.appearance {
            app.set_appearance(new.appearance.theme, new.appearance.system_accent);
        }
        if let Some(enabled) = changes.launch_at_login
            && let Err(error) = install::set_launch_at_login(enabled, &exe_for_login())
        {
            self.state.borrow_mut().settings.general.launch_at_login = !enabled;
            self.refresh_settings_window(app);
            self.error_toast(app, "Couldn't change launch at login", &error);
        }
        self.schedule_save(app);
    }

    fn on_settings_request(&self, app: &App, request: SettingsRequest) {
        let window = self.state.borrow().settings_window;
        let owner = window.and_then(|w| app.hwnd(w)).unwrap_or_else(|| self.owner());
        match request {
            SettingsRequest::PickFolder(kind) => {
                let mut settings = self.settings();
                let current = match kind {
                    FolderKind::Screenshots => paths::screenshots_dir(&settings),
                    FolderKind::Recordings => paths::recordings_dir(&settings),
                }
                .unwrap_or_default();
                let Some(folder) = dialogs::pick_folder(hwnd(owner), &current) else { return };
                match kind {
                    FolderKind::Screenshots => settings.after_capture.save_dir = Some(folder),
                    FolderKind::Recordings => settings.record.save_dir = Some(folder),
                }
                self.apply_settings(app, settings.clone());
                self.save_now();
                self.refresh_settings_window(app);
            }
            SettingsRequest::OpenDefaultApps => {
                if let Err(error) = install::open_default_apps_settings() {
                    self.error_toast(app, "Couldn't open Windows Settings", &error);
                }
            }
            SettingsRequest::Install => self.install_from_settings(app),
            SettingsRequest::Uninstall => self.uninstall(app),
        }
    }

    fn refresh_settings_window(&self, app: &App) {
        let Some(window) = self.state.borrow().settings_window else { return };
        let settings = self.settings();
        let env = settings_env(&settings);
        app.with_view(window, |view: &mut SettingsView, cx| {
            view.set_settings(cx, settings);
            view.set_env(cx, env);
        });
    }

    fn install_from_settings(&self, app: &App) {
        self.save_now();
        let result = std::env::current_exe()
            .context("locating glint.exe")
            .and_then(|exe| install::install(&exe, &self.settings()).map(|target| (exe, target)));
        match result {
            Ok((exe, target)) if !crate::same_file(&exe, &target) => {
                log::info!("installed to {}; handing over", target.display());
                self.state.borrow_mut().relaunch = Some(target);
                self.quit();
            }
            Ok(_) => {
                self.refresh_settings_window(app);
                self.toast(app, Toast::new(ToastIcon::App, "Glint is installed", Some("It starts with Windows and lives in the tray")));
            }
            Err(error) => self.error_toast(app, "Couldn't install Glint", &error),
        }
    }

    /// Stops the hook and tray, removes Glint's registrations and exits.
    fn uninstall(&self, app: &App) {
        self.finish_recording_now(Some(app));
        {
            let mut state = self.state.borrow_mut();
            state.hook = None;
            state.tray.remove();
        }
        match install::uninstall() {
            Ok(()) => {
                log::info!("uninstalled");
                self.quit();
            }
            Err(error) => {
                self.error_toast(app, "Couldn't uninstall Glint", &error);
                self.start_hook();
                self.show_tray(app);
            }
        }
    }

    fn schedule_save(&self, app: &App) {
        if let Some(timer) = self.state.borrow_mut().save_timer.take() {
            app.cancel_timer(timer);
        }
        let glint = self.clone();
        let timer = app.set_timer(SAVE_DEBOUNCE, move |_| glint.save_now());
        self.state.borrow_mut().save_timer = Some(timer);
    }

    fn save_now(&self) {
        let settings = {
            let mut state = self.state.borrow_mut();
            state.save_timer = None;
            state.settings.clone()
        };
        if let Err(error) = settings_store::save_settings(&settings) {
            log::error!("saving settings: {error:#}");
        }
    }

    // ---- quit -------------------------------------------------------------------------------------------------

    /// Finalizes a running recording, waits (bounded) for files and clipboard writes in flight, stops the hook and
    /// tray, saves settings and leaves the loop.
    fn quit(&self) {
        self.finish_recording_now(None);
        self.save_now();
        let workers = std::mem::take(&mut self.state.borrow_mut().workers);
        join_bounded(workers, QUIT_WORKER_WAIT);
        {
            let mut state = self.state.borrow_mut();
            state.hook = None;
            state.tray.remove();
            state.host = None;
        }
        self.proxy.post(QuitRequest);
    }

    /// WM_ENDSESSION: Windows is logging off or shutting down and will end the process when this returns.
    fn end_session(&self) {
        log::info!("session ending");
        self.finish_recording_now(None);
        if self.state.try_borrow_mut().is_ok() {
            self.save_now();
        }
        let workers = self.state.try_borrow_mut().map(|mut state| std::mem::take(&mut state.workers)).unwrap_or_default();
        join_bounded(workers, END_SESSION_WORKER_WAIT);
    }
}

/// Posted to leave the loop after the current handler returned.
struct QuitRequest;

fn freeze(hdr: &HdrSettings, intent: Intent) -> Result<Frozen> {
    let started = std::time::Instant::now();
    let foreground = (intent == Intent::ActiveWindow).then(glint_capture::foreground_window).flatten();
    let captures = glint_capture::capture_all(hdr)?;
    let windows = match intent {
        Intent::Overlay { .. } => glint_capture::windows_snapshot(Some(std::process::id())),
        Intent::ActiveWindow => Vec::new(),
    };
    log::info!("froze {} monitors in {:?}", captures.len(), started.elapsed());
    Ok(Frozen { captures, windows, foreground })
}

/// A neutral 16:9 frame for a recording without a decoded first frame.
fn placeholder_frame(width: u32, height: u32) -> Image {
    let (w, h) = if width > 0 && height > 0 { (width.min(640), (height * width.min(640) / width).max(1)) } else { (640, 360) };
    Image::from_bgra(w, h, [0x2A, 0x26, 0x24, 255].repeat((w * h) as usize))
}

fn outcome_label(outcome: &OverlayOutcome) -> String {
    match outcome {
        OverlayOutcome::Cancelled => "cancelled".into(),
        OverlayOutcome::Snip(snip) => format!("snip {:?} {}×{}", snip.mode, snip.image.width, snip.image.height),
        OverlayOutcome::Text(snip) => format!("text {}×{}", snip.image.width, snip.image.height),
        OverlayOutcome::Color { bgra } => format!("color {bgra:?}"),
        OverlayOutcome::Record(region) => format!("record {}×{}", region.region.w, region.region.h),
        OverlayOutcome::Delay { secs } => format!("delay {secs} s"),
    }
}
