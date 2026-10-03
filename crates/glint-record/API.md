# glint-record API

Headless screen recorder (DESIGN §10): Windows.Graphics.Capture → D3D11 crop + tone-map pass → Media Foundation
H.264 (High, hardware MFT when available) + AAC 192 kbps → MP4. Depends on glint-core only.

```rust
use glint_record::{RecordConfig, Recorder, RecorderStatus, RecordingInfo};

pub struct RecordConfig {
    pub monitor: MonitorInfo,    // from glint-capture; `handle`, `rect` and `hdr` are used
    pub region: RectI,           // physical px relative to monitor.rect origin; clamped + snapped to even size
    pub fps: u32,                // 30 or 60 (1..=120 accepted)
    pub system_audio: bool,      // WASAPI loopback of the default render device
    pub microphone: bool,        // default capture device
    pub include_cursor: bool,
    pub output: PathBuf,         // .mp4; parent directory is created
    pub tonemap: ToneMapParams,  // used only when monitor.hdr is Some
}

impl Recorder {
    pub fn start(cfg: RecordConfig) -> anyhow::Result<Recorder>; // < 1 ms; validates, spawns threads
    pub fn status(&self) -> RecorderStatus;     // Starting | Recording | Paused | Failed(msg)
    pub fn pause(&self);
    pub fn resume(&self);
    pub fn is_paused(&self) -> bool;
    pub fn set_microphone(&self, enabled: bool); // mute/unmute in the mix; opens the mic on first enable
    pub fn elapsed(&self) -> Duration;          // media time, pauses excluded; 0 while Starting
    pub fn audio_level(&self) -> f32;           // recent peak, 0..1 linear amplitude, ~300 ms decay
    pub fn first_frame(&self) -> Option<Image>; // tone-mapped BGRA at the encoded size; Some once Recording
    pub fn stop(self) -> anyhow::Result<RecordingInfo>; // finalizes; typically 100–300 ms, < 2 s
    pub fn discard(self);                       // returns at once; file is deleted in the background
}

pub struct RecordingInfo {
    pub path: PathBuf, pub duration: Duration, pub width: u32, pub height: u32, pub bytes: u64,
    pub thumbnail: Option<Image>,  // same as first_frame()
    pub warnings: Vec<String>,     // non-fatal, e.g. "microphone capture failed: no default device"
}

pub const CPU_ENCODE_ENV: &str = "GLINT_RECORD_CPU_ENCODE"; // set to force the system-memory encoder path
```

`Recorder` is `Send + Sync`; every method except `stop` returns within a few ms, so call them from the UI thread.

## Lifecycle

1. `start` returns immediately with status `Starting`. Device, capture session and encoder setup take
   ~0.6 s warm (1–2 s cold or under heavy GPU load). The clock starts with the first converted frame, so
   `elapsed()` stays 0 and the HUD should show `00:00` until status becomes `Recording`.
2. Setup or runtime failures turn the status into `Failed(msg)` and stop the threads; `stop()` returns the error.
   If nothing was recorded yet the file is deleted; otherwise the partial file is finalized when possible.
3. Dropping a `Recorder` without `stop`/`discard` finalizes the file in the background (call `stop` before exit).

## Behaviour

- Output size: region snapped to even width/height; regions larger than 4096 px per side or 4096×2304 px total
  are scaled down uniformly (encoder limits). `RecordingInfo.width/height` are the encoded size.
- Constant frame rate: a clock thread emits `fps` frames per second, repeating the latest converted frame when
  the screen did not change. Duration is a whole number of frames (≤ 1 frame longer than the elapsed time).
- Target bitrate ≈ width × height × fps × 0.15 bit/s, keyframe every 2 s, tagged BT.709 / sRGB / limited range.
- HDR monitors: FP16 capture, DESIGN §4 curve with a fixed source peak `hdr.max_nits / hdr.sdr_white_nits`
  (Auto) or clamp (Clip), exposure applied. In Auto the top of the SDR range is compressed too (SDR white
  lands around 245/255 on a 520-nit display at 280-nit SDR white); Clip keeps SDR content exact.
- SDR monitors: BGRA capture, region copied as is.
- Audio: each source is converted to 48 kHz stereo (windowed-sinc resampler, surround downmix), placed on the
  QPC timeline by packet time (loopback: when heard), mixed ~150 ms behind real time; gaps (silent loopback, no mic)
  becomes silence, so the AAC track always spans the video. No audio track when both sources are off at start
  (then `set_microphone` is ignored). Device errors are retried every 0.5 s, reported once in `warnings`.
- Pause/resume: frames and samples captured while paused are dropped; timestamps continue seamlessly.
- GPU path: frames reach the encoder as D3D11 textures (DXGI device manager); if the sink writer rejects them it
  falls back to CPU readback + memory buffers. A yellow capture border may appear where the OS requires it.

## Verification

```
cargo run -p glint-record --release --example rec -- --seconds 3 --out %TEMP%\glint-rec\a.mp4
    [--monitor N] [--region x,y,w,h] [--fps 60] [--no-audio] [--mic] [--pause-at 1] [--thumb a.png]
    [--clip] [--exposure <stops>] [--no-cursor] [--discard]
```
Lists monitors, records without a window, prints duration/size/warnings; `GLINT_LOG_DEBUG=1` adds debug logs.
