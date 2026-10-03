# glint-capture API

Monitors with HDR state, one-shot desktop capture, window list. Windows only, all calls are blocking and headless
(no window, hook, clipboard or registry). The process must be per-monitor-v2 DPI aware (manifest, or call
`enable_per_monitor_dpi_awareness()` first in examples and tests), otherwise rects are virtualised.

```rust
use glint_capture::*;          // types come from glint_core
pub fn monitors() -> anyhow::Result<Vec<MonitorInfo>>;
pub fn capture_all(hdr: &HdrSettings) -> anyhow::Result<Vec<MonitorCapture>>;
pub fn capture_monitor(monitor: &MonitorInfo, hdr: &HdrSettings) -> anyhow::Result<MonitorCapture>;
pub fn windows_snapshot(exclude_pid: Option<u32>) -> Vec<WindowInfo>;
pub fn foreground_window() -> Option<WindowInfo>;
pub fn window_at(windows: &[WindowInfo], p: PointI) -> Option<&WindowInfo>;
pub fn enable_per_monitor_dpi_awareness();
// extras
pub fn warm_up() -> anyhow::Result<usize>;     // call once, off the UI thread; see "Warm-up"
pub fn release_warm_devices();
pub type SdrReady<'a> = &'a (dyn Fn(usize, &MonitorInfo, &Image) + Sync);
pub fn capture_all_streaming(hdr, on_sdr: SdrReady) -> Result<Vec<(MonitorCapture, CaptureReport)>>;
pub fn capture_all_reported(hdr) -> Result<Vec<(MonitorCapture, CaptureReport)>>;
pub fn capture_monitor_reported(monitor, hdr) -> Result<(MonitorCapture, CaptureReport)>;
pub fn capture_monitor_via(monitor, hdr, order: &[CaptureMethod]) / capture_all_via(hdr, order) -> ...; // forced order
pub fn capture_monitor_wgc(monitor, hdr) / capture_monitor_gdi(monitor) -> Result<MonitorCapture>; // force one method
pub fn set_gpu_tonemap(enabled: bool);          // default on; off = CPU tone map of glint-core
pub enum CaptureMethod { Dda, Wgc, Gdi }        // default_order(&MonitorInfo); FromStr/Display "dda"/"wgc"/"gdi"
pub struct CaptureReport { path: CapturePath, fallback_reason: Option<String>, grab, tonemap, total: Duration,
                           stages: StageTimings }
pub struct StageTimings { setup, acquire, copy_map, convert, tonemap: Duration }
pub enum CapturePath { DdaFp16, DdaBgra, WgcFp16, WgcBgra, Gdi }  // Display: "DDA FP16", "WGC BGRA", "GDI", ...
```

## Behaviour

- `monitors()`: `rect`/`work_rect` physical px in virtual-desktop coordinates, `dpi` effective DPI, `friendly_name`
  from the display config (falls back to the GDI name). `hdr` is `Some` only while the output reports the
  `G2084_NONE_P2020` colour space; `sdr_white_nits` = Windows "SDR content brightness" (SDRWhiteLevel / 1000 * 80,
  80 when unavailable). Advanced-colour info (`_INFO_2`, then `_INFO`) is logged at info level.
- `capture_all`: one thread per monitor (priority above normal), results in `monitors()` order. A monitor that fails on
  every method is logged and skipped; `Err` only when no monitor could be captured. `capture_monitor` keeps
  `monitor` as passed. `capture_all_streaming` additionally calls `on_sdr(index, monitor, &sdr)` on the capture thread
  as soon as that monitor's `sdr` image exists, before its raw HDR copy has finished (for the HDR monitor that is
  roughly 10-20 ms earlier); the returned captures still carry `sdr` as usual.
- Method order per monitor (`CaptureMethod::default_order`), until one yields a frame: SDR monitors DDA, GDI; HDR
  monitors DDA, WGC, GDI. DDA = DXGI duplication (FP16 then BGRA8 formats, output rotation undone), waits at most
  30 ms for a frame with `LastPresentTime != 0`, so a static desktop falls through. WGC = Windows.Graphics.Capture,
  one frame (FP16 for HDR, cursor off, border off when the OS accepts `SetIsBorderRequired(false)`, first frame
  within 300 ms, session and pool closed at once). GDI = BitBlt with `CAPTUREBLT`, SDR only, aborted after 2 s.
  GPU methods share one device per capture (the warm device after `warm_up`). Nothing stays resident except the
  warm devices; a fresh device is torn down on a helper thread. `fallback_reason` lists why earlier methods failed.
- FP16 frames are tone mapped on the GPU in one command batch (statistics, 99.9th percentile curve, DESIGN section 4
  tone map, all compute shaders compiled once with D3DCompile) and the raw FP16 copy is queued behind it, so there is
  a single wait for the GPU. SDR content is bit-exact against glint-core, HDR content within 1 code (unit-tested on
  a GPU/WARP device). Outputs with rotation, or devices without compute, use the CPU tone map. Results fill
  `MonitorCapture::hdr` (`sdr_white_nits` from the monitor or 80, `display_peak_nits` from `max_nits` or 1000),
  `hdr_stats` (same numbers as `analyze`), `sdr`. BGRA8 frames give `sdr` with alpha 255, `hdr`/`hdr_stats` = `None`.
- Every capture logs `<device>: <path> <ms> (setup, acquire, copy+map, convert, tone map)` at info level;
  `CaptureReport::stages` carries the same numbers. setup = duplication or WGC session creation, acquire = wait for
  the first frame, copy+map = GPU waits, convert = CPU copies, tone map = GPU batch recording.
- The device asks for the highest `SetGPUThreadPriority` that succeeds (7 on the dev machine, logged once).
- `windows_snapshot`: topmost first, `z_order` = index in the result. Keeps visible, non-minimised, non-cloaked
  windows with non-empty DWM extended frame bounds (`rect`, physical px). Skips `exclude_pid`, `Progman`, `WorkerW`,
  zero-alpha layered windows, and tool windows without a title. `foreground_window()` applies the same filters,
  `z_order` is 0. `window_at` returns the first window whose rect contains the point (list must be z-ordered).

## Warm-up

`warm_up()` (call at startup, off the UI thread, repeat after display changes) keeps one idle D3D11 device per
adapter (cold NVIDIA device creation is ~120 ms, later ones ~25 ms), compiles and creates the tone map shaders, and
runs one complete WGC session with the border switched off (skipped when the border cannot be disabled, so nothing
flashes). The first capture then costs about 1.5x a steady-state one instead of 3 to 6x. `release_warm_devices()` drops
them. Without warm-up everything works, just slower on the first capture.

## Verification

`cargo run -p glint-capture --release --example probe -- --out %TEMP%\glint-probe [--runs N] [--warm]
[--order dda,wgc,gdi] [--compare-gdi] [--wgc] [--cpu-tonemap] [--burn-cpu N] [--verbose]` prints monitors, top 15
windows, per-run wall time with path and "sdr at" time per monitor, stage timings, and writes `monitor-N.png` /
`monitor-N.json`. `--order wgc,gdi` simulates a static desktop, `--compare-gdi`/`--wgc` diff against a GDI / forced WGC
grab, `--cpu-tonemap` switches the GPU tone map off, `--burn-cpu N` runs N spinning threads to imitate a busy game.
