use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use glint_core::{HdrInfo, Image, RectI, ToneMapParams};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, IMFMediaBuffer, MFCreateDXGIDeviceManager};

use crate::audio::{CHANNELS, MIX_DELAY_HNS};
use crate::capture::{CaptureOptions, CapturedFrame, ScreenCapture};
use crate::clock::{
    PreciseSleeper, audio_frame_at, audio_frame_time, frame_time, frames_due, hns_to_duration, now_hns,
};
use crate::com::{ComApartment, MediaFoundation};
use crate::encoder::{Encoder, EncoderSettings, SampleTracker, VideoInput, memory_buffer, surface_buffer};
use crate::gpu::{Converter, Gpu, Readback, RenderTarget};
use crate::state::{Outcome, Shared, lock};
use crate::tonecurve::ToneCurve;

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(3);
/// Converted frames the GPU encoder may hold at once before new screen content is skipped.
const MAX_GPU_FRAMES: usize = 8;
const MIN_WAIT_HNS: i64 = 10_000;
const IDLE_WAIT_HNS: i64 = 100_000;
/// Lets the last WASAPI packets before stop reach the mixer.
const AUDIO_TAIL_WAIT: Duration = Duration::from_millis(80);
/// Set to any value to skip the GPU encoder input path (for testing the fallback).
pub const CPU_ENCODE_ENV: &str = "GLINT_RECORD_CPU_ENCODE";

/// Everything the video thread needs, validated by `Recorder::start`.
pub(crate) struct Job {
    pub monitor: isize,
    pub hdr: Option<HdrInfo>,
    pub region: RectI,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub include_cursor: bool,
    pub tonemap: ToneMapParams,
    pub output: PathBuf,
    pub audio: bool,
}

/// Body of the video thread: capture, convert, encode, finalize.
pub(crate) fn run(job: Job, shared: Arc<Shared>) {
    let result = record(&job, &shared);
    shared.audio_stop.store(true, Ordering::Release);
    let discarded = shared.discard.load(Ordering::Acquire);
    let started = lock(&shared.clock).is_started();
    match result {
        Ok(outcome) if !discarded => *lock(&shared.outcome) = Some(outcome),
        Ok(_) => {}
        Err(error) => shared.fail(error),
    }
    if discarded || !started {
        match std::fs::remove_file(&job.output) {
            Ok(()) => log::info!("removed {}", job.output.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("cannot remove {}: {error}", job.output.display()),
        }
    }
}

fn record(job: &Job, shared: &Shared) -> anyhow::Result<Outcome> {
    let setup_started = Instant::now();
    let _apartment = ComApartment::join_multithreaded()?;
    let _media_foundation = MediaFoundation::start()?;
    let gpu = Gpu::for_monitor(HMONITOR(job.monitor as *mut _))?;
    let device_ready = setup_started.elapsed();
    let mut capture = ScreenCapture::start(
        &gpu,
        &CaptureOptions {
            monitor: HMONITOR(job.monitor as *mut _),
            fp16: job.hdr.is_some(),
            include_cursor: job.include_cursor,
            frame_interval_hns: frame_time(1, job.fps),
        },
    )?;
    let curve = ToneCurve::new(job.hdr.as_ref(), &job.tonemap);
    log::info!("recording {:?} -> {}x{} @ {} fps, {:?}", job.region, job.width, job.height, job.fps, curve);
    let capture_ready = setup_started.elapsed();
    let converter = Converter::new(&gpu, job.region, curve)?;
    let shaders_ready = setup_started.elapsed();
    let video = VideoOutput::open(&gpu, job)?;
    let encoder_ready = setup_started.elapsed();
    let mut recording =
        Recording { job, shared, gpu, converter, video, frames_written: 0, frames_captured: 0, frames_skipped: 0 };

    let first = capture.wait_for_frame(FIRST_FRAME_TIMEOUT, || shared.stop_requested())?;
    recording.store(&first)?;
    drop(first);
    *lock(&shared.first_frame) = Some(recording.video.latest_image(&recording.gpu)?);
    lock(&shared.clock).start(now_hns());
    log::info!("recording started with {:?} encoder input", recording.video.encoder()?.input());
    log::info!(
        "setup times: device {device_ready:?}, capture {capture_ready:?}, shaders {shaders_ready:?}, \
         encoder {encoder_ready:?}, first frame {:?}",
        setup_started.elapsed()
    );

    let looped = recording.run(&mut capture);
    if shared.discard.load(Ordering::Acquire) {
        return Ok(Outcome { duration: Duration::ZERO, bytes: 0 });
    }
    match looped {
        Ok(()) => recording.finish(),
        Err(error) => {
            if let Err(salvage) = recording.finish() {
                log::warn!("could not finalize the partial recording: {salvage:#}");
            }
            Err(error)
        }
    }
}

struct Recording<'a> {
    job: &'a Job,
    shared: &'a Shared,
    gpu: Gpu,
    converter: Converter,
    video: VideoOutput,
    frames_written: u64,
    frames_captured: u64,
    frames_skipped: u64,
}

impl Recording<'_> {
    fn run(&mut self, capture: &mut ScreenCapture) -> anyhow::Result<()> {
        let sleeper = PreciseSleeper::new();
        loop {
            let stopping = self.shared.stop_requested();
            if let Some(frame) = capture.latest_frame()? {
                self.store(&frame)?;
            }
            let (media_now, paused) = {
                let clock = lock(&self.shared.clock);
                (clock.media_time(now_hns()), clock.is_paused())
            };
            self.write_due_frames(media_now)?;
            self.write_mixed_audio(audio_frame_at(media_now - MIX_DELAY_HNS))?;
            if stopping {
                return Ok(());
            }
            let until_next_frame = frame_time(self.frames_written, self.job.fps) - media_now;
            let wait = if paused { IDLE_WAIT_HNS } else { until_next_frame.clamp(MIN_WAIT_HNS, IDLE_WAIT_HNS) };
            sleeper.sleep_hns(wait);
        }
    }

    fn store(&mut self, frame: &CapturedFrame) -> anyhow::Result<()> {
        self.frames_captured += 1;
        let Recording { gpu, converter, video, .. } = self;
        let stored =
            video.store(gpu, |target| converter.convert(gpu, &frame.texture, frame.width, frame.height, target))?;
        if !stored {
            self.frames_skipped += 1;
        }
        Ok(())
    }

    fn write_due_frames(&mut self, media_now: i64) -> anyhow::Result<()> {
        let due = frames_due(media_now, self.job.fps);
        while self.frames_written < due {
            self.video.write_frame(&self.gpu, self.frames_written, self.job.fps)?;
            self.frames_written += 1;
        }
        Ok(())
    }

    fn write_mixed_audio(&mut self, until_frame: u64) -> anyhow::Result<()> {
        if !self.job.audio {
            return Ok(());
        }
        let (from, pcm) = {
            let mut mixer = lock(&self.shared.mixer);
            (mixer.mixed_until(), mixer.mix_until(until_frame))
        };
        if pcm.is_empty() {
            return Ok(());
        }
        let to = from + (pcm.len() / CHANNELS) as u64;
        let time = audio_frame_time(from);
        self.video.encoder()?.write_audio(&pcm, time, audio_frame_time(to) - time)
    }

    fn finish(mut self) -> anyhow::Result<Outcome> {
        let video_end = frame_time(self.frames_written, self.job.fps);
        if self.job.audio {
            thread::sleep(AUDIO_TAIL_WAIT);
            self.write_mixed_audio(audio_frame_at(video_end))?;
        }
        log::info!(
            "finalizing: {} frames written, {} screen updates, {} skipped",
            self.frames_written,
            self.frames_captured,
            self.frames_skipped
        );
        self.video.encoder.take().ok_or_else(|| anyhow!("no encoder to finalize"))?.finish()?;
        let bytes = std::fs::metadata(&self.job.output).map(|meta| meta.len()).unwrap_or(0);
        Ok(Outcome { duration: hns_to_duration(video_end), bytes })
    }
}

/// The encoder plus the converted frame it repeats until the screen changes.
struct VideoOutput {
    /// None only after a failed switch to the system-memory encoder.
    encoder: Option<Encoder>,
    frames: FrameStore,
    settings: EncoderSettings,
    output: PathBuf,
    _device_manager: Option<IMFDXGIDeviceManager>,
}

enum FrameStore {
    Gpu { slots: Vec<GpuSlot>, latest: Option<usize> },
    Cpu { target: RenderTarget, readback: Readback, latest: Option<IMFMediaBuffer> },
}

struct GpuSlot {
    target: RenderTarget,
    tracker: SampleTracker,
}

impl FrameStore {
    fn cpu(gpu: &Gpu, width: u32, height: u32) -> anyhow::Result<Self> {
        Ok(FrameStore::Cpu {
            target: gpu.render_target(width, height)?,
            readback: Readback::new(gpu, width, height)?,
            latest: None,
        })
    }
}

impl VideoOutput {
    fn open(gpu: &Gpu, job: &Job) -> anyhow::Result<Self> {
        let settings = EncoderSettings {
            width: job.width,
            height: job.height,
            fps: job.fps,
            bitrate: job.bitrate,
            audio: job.audio,
        };
        if std::env::var_os(CPU_ENCODE_ENV).is_none() {
            let gpu_encoder = device_manager(gpu)
                .and_then(|manager| Ok((Encoder::create(&job.output, &settings, Some(&manager))?, manager)));
            match gpu_encoder {
                Ok((encoder, manager)) => {
                    return Ok(Self {
                        encoder: Some(encoder),
                        frames: FrameStore::Gpu { slots: Vec::new(), latest: None },
                        settings,
                        output: job.output.clone(),
                        _device_manager: Some(manager),
                    });
                }
                Err(error) => log::warn!("GPU encoder input unavailable, using system memory: {error:#}"),
            }
        }
        Ok(Self {
            encoder: Some(Encoder::create(&job.output, &settings, None)?),
            frames: FrameStore::cpu(gpu, job.width, job.height)?,
            settings,
            output: job.output.clone(),
            _device_manager: None,
        })
    }

    /// Converts new screen content into a free frame; false when every frame is still queued in the encoder.
    fn store(&mut self, gpu: &Gpu, render: impl FnOnce(&RenderTarget) -> anyhow::Result<()>) -> anyhow::Result<bool> {
        match &mut self.frames {
            FrameStore::Gpu { slots, latest } => {
                let free = (0..slots.len()).find(|&index| Some(index) != *latest && slots[index].tracker.is_idle());
                let index = match free {
                    Some(index) => index,
                    None if slots.len() < MAX_GPU_FRAMES => {
                        let target = gpu.render_target(self.settings.width, self.settings.height)?;
                        slots.push(GpuSlot { target, tracker: SampleTracker::new() });
                        slots.len() - 1
                    }
                    None => return Ok(false),
                };
                render(&slots[index].target)?;
                *latest = Some(index);
            }
            FrameStore::Cpu { target, readback, latest } => {
                render(target)?;
                *latest = Some(memory_buffer(&readback.read(gpu, &target.texture)?)?);
            }
        }
        Ok(true)
    }

    fn write_frame(&mut self, gpu: &Gpu, index: u64, fps: u32) -> anyhow::Result<()> {
        let time = frame_time(index, fps);
        let duration = frame_time(index + 1, fps) - time;
        let written = match &self.frames {
            FrameStore::Gpu { slots, latest: Some(latest) } => {
                let slot = &slots[*latest];
                surface_buffer(&slot.target.texture)
                    .and_then(|buffer| self.encoder()?.write_video(&buffer, time, duration, Some(&slot.tracker)))
            }
            FrameStore::Cpu { latest: Some(buffer), .. } => self.encoder()?.write_video(buffer, time, duration, None),
            _ => return Err(anyhow!("no converted frame to encode")),
        };
        match written {
            Err(error) if index == 0 && self.encoder()?.input() == VideoInput::Gpu => {
                log::warn!("GPU frame rejected by the encoder, switching to system memory: {error:#}");
                self.switch_to_cpu(gpu)?;
                self.write_frame(gpu, index, fps)
            }
            result => result,
        }
    }

    fn switch_to_cpu(&mut self, gpu: &Gpu) -> anyhow::Result<()> {
        let latest_texture = match &self.frames {
            FrameStore::Gpu { slots, latest: Some(latest) } => slots[*latest].target.texture.clone(),
            _ => return Err(anyhow!("no frame to carry over to the CPU encoder")),
        };
        let mut frames = FrameStore::cpu(gpu, self.settings.width, self.settings.height)?;
        if let FrameStore::Cpu { readback, latest, .. } = &mut frames {
            *latest = Some(memory_buffer(&readback.read(gpu, &latest_texture)?)?);
        }
        self.frames = frames;
        self.encoder = None;
        self._device_manager = None;
        self.encoder = Some(Encoder::create(&self.output, &self.settings, None).context("system-memory encoder")?);
        Ok(())
    }

    fn encoder(&self) -> anyhow::Result<&Encoder> {
        self.encoder.as_ref().ok_or_else(|| anyhow!("the encoder was lost while switching to system memory"))
    }

    fn latest_image(&self, gpu: &Gpu) -> anyhow::Result<Image> {
        let pixels = match &self.frames {
            FrameStore::Gpu { slots, latest: Some(latest) } => {
                Readback::new(gpu, self.settings.width, self.settings.height)?
                    .read(gpu, &slots[*latest].target.texture)?
            }
            FrameStore::Cpu { target, readback, .. } => readback.read(gpu, &target.texture)?,
            FrameStore::Gpu { latest: None, .. } => return Err(anyhow!("no converted frame yet")),
        };
        Ok(Image::from_bgra(self.settings.width, self.settings.height, pixels))
    }
}

fn device_manager(gpu: &Gpu) -> anyhow::Result<IMFDXGIDeviceManager> {
    let mut reset_token = 0;
    let mut manager = None;
    unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager) }.context("MFCreateDXGIDeviceManager")?;
    let manager = manager.ok_or_else(|| anyhow!("MFCreateDXGIDeviceManager returned nothing"))?;
    unsafe { manager.ResetDevice(&gpu.device, reset_token) }.context("IMFDXGIDeviceManager::ResetDevice")?;
    Ok(manager)
}
