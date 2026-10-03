use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use glint_core::Image;

use crate::audio::Mixer;
use crate::clock::Timeline;

/// What the recording threads share with the `Recorder` handle.
pub(crate) struct Shared {
    pub clock: Mutex<Timeline>,
    pub mixer: Mutex<Mixer>,
    /// Set by `Recorder::stop`/`discard`/drop.
    pub stop: AtomicBool,
    pub discard: AtomicBool,
    /// Set by the video thread once it no longer needs audio.
    pub audio_stop: AtomicBool,
    pub first_frame: Mutex<Option<Image>>,
    pub outcome: Mutex<Option<Outcome>>,
    failure: Mutex<Option<anyhow::Error>>,
    warnings: Mutex<Vec<String>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Outcome {
    pub duration: Duration,
    pub bytes: u64,
}

impl Shared {
    pub fn new(microphone_enabled: bool) -> Self {
        Self {
            clock: Mutex::new(Timeline::default()),
            mixer: Mutex::new(Mixer::new(microphone_enabled)),
            stop: AtomicBool::new(false),
            discard: AtomicBool::new(false),
            audio_stop: AtomicBool::new(false),
            first_frame: Mutex::new(None),
            outcome: Mutex::new(None),
            failure: Mutex::new(None),
            warnings: Mutex::new(Vec::new()),
        }
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub fn audio_stopped(&self) -> bool {
        self.audio_stop.load(Ordering::Acquire)
    }

    /// Records the first fatal error and winds every thread down.
    pub fn fail(&self, error: anyhow::Error) {
        log::error!("recording failed: {error:#}");
        lock(&self.failure).get_or_insert(error);
        self.stop.store(true, Ordering::Release);
        self.audio_stop.store(true, Ordering::Release);
    }

    pub fn failure_message(&self) -> Option<String> {
        lock(&self.failure).as_ref().map(|error| format!("{error:#}"))
    }

    pub fn take_failure(&self) -> Option<anyhow::Error> {
        lock(&self.failure).take()
    }

    /// A non-fatal problem (e.g. an audio device that could not be opened); the recording continues.
    pub fn warn(&self, message: String) {
        log::warn!("{message}");
        lock(&self.warnings).push(message);
    }

    pub fn warnings(&self) -> Vec<String> {
        lock(&self.warnings).clone()
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
