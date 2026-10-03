//! Headless screen recorder: Windows.Graphics.Capture → D3D11 crop/tone-map pass → Media Foundation H.264 + AAC
//! in MP4. See API.md for the contract and DESIGN.md §10 for the product rules.

mod audio;
mod capture;
mod clock;
mod com;
mod encoder;
mod gpu;
mod pipeline;
mod plan;
mod state;
mod tonecurve;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, anyhow, ensure};
use glint_core::{Image, MonitorInfo, RectI, ToneMapParams};

use crate::audio::Source;
use crate::clock::{hns_to_duration, now_hns};
use crate::state::{Shared, lock};

pub use crate::pipeline::CPU_ENCODE_ENV;

pub struct RecordConfig {
    pub monitor: MonitorInfo,
    /// Physical pixels relative to `monitor.rect`'s origin; clamped to the monitor and snapped to an even size.
    pub region: RectI,
    /// 30 or 60.
    pub fps: u32,
    pub system_audio: bool,
    pub microphone: bool,
    pub include_cursor: bool,
    /// Destination `.mp4`; its directory is created if missing.
    pub output: PathBuf,
    /// Applied when the monitor is in HDR mode.
    pub tonemap: ToneMapParams,
}

#[derive(Clone, Debug)]
pub struct RecordingInfo {
    pub path: PathBuf,
    pub duration: Duration,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub thumbnail: Option<Image>,
    /// Non-fatal problems, e.g. an audio device that could not be opened (its track part is silent).
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecorderStatus {
    /// Threads are setting up capture and the encoder; `elapsed()` is still zero.
    Starting,
    Recording,
    Paused,
    /// The recording stopped on its own; `stop()` returns this error.
    Failed(String),
}

/// A running recording. All methods return within a few milliseconds except `stop`.
/// Dropping it without `stop`/`discard` finalizes the file in the background.
pub struct Recorder {
    shared: Arc<Shared>,
    video_thread: Option<JoinHandle<()>>,
    audio_threads: Mutex<Vec<JoinHandle<()>>>,
    microphone_thread_started: Mutex<bool>,
    audio: bool,
    output: PathBuf,
    width: u32,
    height: u32,
}

impl Recorder {
    /// Validates the configuration and starts the recording threads; capture begins within a few hundred ms
    /// (watch `status()`), and setup failures surface as `RecorderStatus::Failed` and from `stop()`.
    pub fn start(cfg: RecordConfig) -> anyhow::Result<Recorder> {
        ensure!((1..=120).contains(&cfg.fps), "unsupported frame rate {}", cfg.fps);
        let region = plan::snap_region(cfg.region, cfg.monitor.rect.w, cfg.monitor.rect.h)?;
        let (width, height) = plan::encoded_size(region.w as u32, region.h as u32);
        if let Some(directory) = cfg.output.parent().filter(|directory| !directory.as_os_str().is_empty()) {
            std::fs::create_dir_all(directory).with_context(|| format!("cannot create {}", directory.display()))?;
        }
        let audio = cfg.system_audio || cfg.microphone;
        let shared = Arc::new(Shared::new(cfg.microphone));
        let job = pipeline::Job {
            monitor: cfg.monitor.handle,
            hdr: cfg.monitor.hdr,
            region,
            width,
            height,
            fps: cfg.fps,
            bitrate: plan::video_bitrate(width, height, cfg.fps),
            include_cursor: cfg.include_cursor,
            tonemap: cfg.tonemap,
            output: cfg.output.clone(),
            audio,
        };
        let video_shared = shared.clone();
        let video_thread = thread::Builder::new()
            .name("glint-record-video".into())
            .spawn(move || pipeline::run(job, video_shared))
            .context("spawning the video thread")?;
        let recorder = Recorder {
            shared,
            video_thread: Some(video_thread),
            audio_threads: Mutex::new(Vec::new()),
            microphone_thread_started: Mutex::new(false),
            audio,
            output: cfg.output,
            width,
            height,
        };
        if cfg.system_audio {
            recorder.spawn_audio(Source::System)?;
        }
        if cfg.microphone {
            recorder.spawn_microphone()?;
        }
        Ok(recorder)
    }

    pub fn status(&self) -> RecorderStatus {
        if let Some(message) = self.shared.failure_message() {
            return RecorderStatus::Failed(message);
        }
        let clock = lock(&self.shared.clock);
        if !clock.is_started() {
            RecorderStatus::Starting
        } else if clock.is_paused() {
            RecorderStatus::Paused
        } else {
            RecorderStatus::Recording
        }
    }

    pub fn pause(&self) {
        lock(&self.shared.clock).pause(now_hns());
    }

    pub fn resume(&self) {
        lock(&self.shared.clock).resume(now_hns());
    }

    pub fn is_paused(&self) -> bool {
        lock(&self.shared.clock).is_paused()
    }

    /// Mutes or unmutes the microphone in the mix. Opens the microphone on first use; does nothing when the
    /// recording was started without any audio track.
    pub fn set_microphone(&self, enabled: bool) {
        if !self.audio {
            log::warn!("set_microphone({enabled}) ignored: the recording has no audio track");
            return;
        }
        lock(&self.shared.mixer).set_microphone_enabled(enabled);
        if enabled && let Err(error) = self.spawn_microphone() {
            self.shared.warn(format!("microphone unavailable: {error:#}"));
        }
    }

    /// Recorded time so far, excluding pauses.
    pub fn elapsed(&self) -> Duration {
        hns_to_duration(lock(&self.shared.clock).media_time(now_hns()))
    }

    /// Recent peak of the mixed audio, 0..1 linear amplitude.
    pub fn audio_level(&self) -> f32 {
        lock(&self.shared.mixer).level(now_hns())
    }

    /// The first recorded frame, tone mapped, at the encoded size.
    pub fn first_frame(&self) -> Option<Image> {
        lock(&self.shared.first_frame).clone()
    }

    /// Stops and finalizes the MP4. Blocks until the file is complete (well under 2 s).
    pub fn stop(mut self) -> anyhow::Result<RecordingInfo> {
        self.request_stop(false);
        self.join_threads();
        if let Some(error) = self.shared.take_failure() {
            return Err(error);
        }
        let outcome = (*lock(&self.shared.outcome)).ok_or_else(|| anyhow!("the recording ended without a result"))?;
        Ok(RecordingInfo {
            path: self.output.clone(),
            duration: outcome.duration,
            width: self.width,
            height: self.height,
            bytes: outcome.bytes,
            thumbnail: self.first_frame(),
            warnings: self.shared.warnings(),
        })
    }

    /// Stops without finalizing and deletes the file. Returns immediately; cleanup finishes in the background.
    pub fn discard(mut self) {
        self.request_stop(true);
        self.video_thread.take();
        lock(&self.audio_threads).clear();
    }

    fn request_stop(&self, discard: bool) {
        lock(&self.shared.clock).stop(now_hns());
        self.shared.discard.store(discard, Ordering::Release);
        self.shared.stop.store(true, Ordering::Release);
    }

    fn join_threads(&mut self) {
        if let Some(video_thread) = self.video_thread.take() {
            let _ = video_thread.join();
        }
        self.shared.audio_stop.store(true, Ordering::Release);
        for audio_thread in lock(&self.audio_threads).drain(..) {
            let _ = audio_thread.join();
        }
    }

    fn spawn_microphone(&self) -> anyhow::Result<()> {
        let mut started = lock(&self.microphone_thread_started);
        if !*started {
            self.spawn_audio(Source::Microphone)?;
            *started = true;
        }
        Ok(())
    }

    fn spawn_audio(&self, source: Source) -> anyhow::Result<()> {
        let shared = self.shared.clone();
        let handle = thread::Builder::new()
            .name(format!("glint-record-{source:?}").to_lowercase())
            .spawn(move || audio::run_source(source, shared))
            .context("spawning an audio thread")?;
        lock(&self.audio_threads).push(handle);
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.video_thread.is_some() {
            self.request_stop(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_can_move_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Recorder>();
        assert_send_sync::<RecordingInfo>();
    }
}
