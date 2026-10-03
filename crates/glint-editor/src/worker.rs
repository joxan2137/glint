//! Background work (HDR re-tone-mapping, OCR, encoding) and the messages that bring results back to the window.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use glint_core::{HdrImage, HdrStats, Image, ToneMapParams};
use glint_sys::ocr::OcrResult;
use glint_ui::{App, AppProxy, WindowId};

use crate::SavedFn;
use crate::editor::EditorView;

pub enum Message {
    HdrStats(HdrStats),
    Retoned { generation: u64, image: Image },
    /// `job` matches the window's current OCR request, or the result is stale.
    Ocr { job: u64, result: Result<OcrResult, String> },
    Saved { job: u64, result: Result<PathBuf, String> },
    ShareReady(Result<PathBuf, String>),
}

thread_local! {
    static SAVE_LISTENERS: RefCell<HashMap<u64, SavedFn>> = RefCell::new(HashMap::new());
    static NEXT_SAVE: Cell<u64> = const { Cell::new(1) };
}

/// Remembers whom to tell when save `job` completes, even if its window has closed by then.
pub fn track_save(saved: Option<SavedFn>) -> u64 {
    let job = NEXT_SAVE.with(|n| n.replace(n.get() + 1));
    if let Some(saved) = saved {
        SAVE_LISTENERS.with(|l| l.borrow_mut().insert(job, saved));
    }
    job
}

/// Calls the host's `saved` for a successful save, exactly once per job.
fn settle_save(app: &App, job: u64, result: &Result<PathBuf, String>) {
    let listener = SAVE_LISTENERS.with(|l| l.borrow_mut().remove(&job));
    if let (Some(saved), Ok(path)) = (listener, result) {
        saved(app, path.clone());
    }
}

/// A message for one editor window, posted from any thread.
pub struct Envelope {
    pub window: WindowId,
    pub message: Message,
}

/// Routes envelopes to their window. Registered on every `open_editor`; the handler is the same each time.
pub fn install_handler(app: &App) {
    app.on_event(|app: &App, envelope: Envelope| {
        let Envelope { window, message } = envelope;
        if let Message::Saved { job, result } = &message {
            settle_save(app, *job, result);
        }
        let mut pending = Some(message);
        let delivered = app.with_view::<EditorView, _>(window, |view, cx| {
            if let Some(message) = pending.take() {
                view.on_message(cx, message);
            }
        });
        if delivered.is_none()
            && let Some(message) = pending
            && app.window_ids().contains(&window)
        {
            app.set_timer(std::time::Duration::from_millis(30), move |app| app.post(Envelope { window, message }));
        }
    });
}

#[derive(Clone)]
pub struct Poster {
    proxy: AppProxy,
    window: WindowId,
}

impl Poster {
    pub fn new(proxy: AppProxy, window: WindowId) -> Self {
        Self { proxy, window }
    }

    pub fn post(&self, message: Message) {
        self.proxy.post(Envelope { window: self.window, message });
    }

    /// Runs `job` on a new thread and posts what it returns.
    pub fn spawn(&self, name: &str, job: impl FnOnce() -> Message + Send + 'static) {
        let poster = self.clone();
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || poster.post(job()));
        if let Err(e) = spawned {
            log::error!("spawning {name}: {e}");
        }
    }
}

#[derive(Default)]
struct Slot {
    request: Option<(u64, ToneMapParams)>,
    closed: bool,
}

/// One worker thread that re-tone-maps the raw HDR capture. Requests overwrite each other, so a fast slider drag
/// only ever renders the newest parameters; the old base image stays on screen until the new one arrives.
pub struct Retoner {
    shared: Arc<(Mutex<Slot>, Condvar)>,
}

impl Retoner {
    pub fn start(hdr: Arc<HdrImage>, poster: Poster) -> Self {
        let shared = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
        let worker = shared.clone();
        let spawned = std::thread::Builder::new().name("glint-retone".into()).spawn(move || {
            let stats = glint_core::tonemap::analyze(&hdr);
            poster.post(Message::HdrStats(stats));
            loop {
                let (lock, signal) = &*worker;
                let request = {
                    let Ok(mut slot) = lock.lock() else { return };
                    while slot.request.is_none() && !slot.closed {
                        slot = match signal.wait(slot) {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                    }
                    if slot.closed {
                        return;
                    }
                    slot.request.take()
                };
                if let Some((generation, params)) = request {
                    let image = glint_core::tonemap::tonemap_with_stats(&hdr, &params, &stats);
                    poster.post(Message::Retoned { generation, image });
                }
            }
        });
        if let Err(e) = spawned {
            log::error!("spawning the tone-map worker: {e}");
        }
        Self { shared }
    }

    pub fn request(&self, generation: u64, params: ToneMapParams) {
        let (lock, signal) = &*self.shared;
        if let Ok(mut slot) = lock.lock() {
            slot.request = Some((generation, params));
            signal.notify_one();
        }
    }
}

impl Drop for Retoner {
    fn drop(&mut self) {
        let (lock, signal) = &*self.shared;
        if let Ok(mut slot) = lock.lock() {
            slot.closed = true;
            signal.notify_one();
        }
    }
}
