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
pub fn warm_up() -> anyhow::Result<usize>;          // idle D3D11 device per GPU + WGC class load
pub fn release_warm_devices();
pub fn capture_all_reported(hdr) -> Result<Vec<(MonitorCapture, CaptureReport)>>;
pub fn capture_monitor_reported(monitor, hdr) -> Result<(MonitorCapture, CaptureReport)>;
pub fn capture_monitor_via(monitor, hdr, order: &[CaptureMethod]) -> Result<(MonitorCapture, CaptureReport)>;
pub fn capture_all_via(hdr, order: &[CaptureMethod]) -> Result<Vec<(MonitorCapture, CaptureReport)>>;
pub fn capture_monitor_wgc(monitor, hdr) / capture_monitor_gdi(monitor) -> Result<MonitorCapture>; // force one method
pub enum CaptureMethod { Dda, Wgc, Gdi }             // DEFAULT_ORDER = [Dda, Wgc, Gdi]; FromStr/Display "dda"/"wgc"/"gdi"
pub struct CaptureReport { path: CapturePath, fallback_reason: Option<String>, grab, tonemap, total: Duration }
pub enum CapturePath { DdaFp16, DdaBgra, WgcFp16, WgcBgra, Gdi }  // Display: "DDA FP16", "WGC BGRA", "GDI", ...
```

## Behaviour

- `monitors()`: `rect`/`work_rect` physical px in virtual-desktop coordinates, `dpi` effective DPI, `friendly_name`
  from the display config (falls back to the GDI name). `hdr` is `Some` only while the output reports the
  `G2084_NONE_P2020` colour space; `sdr_white_nits` = Windows "SDR content brightness" (SDRWhiteLevel / 1000 * 80,
  80 when unavailable). Advanced-colour info (`_INFO_2`, then `_INFO`) is logged at info level.
- `capture_all`: one thread per monitor, results in `monitors()` order. A monitor that fails on every method is
  logged and skipped; `Err` only when no monitor could be captured. `capture_monitor` keeps `monitor` as passed.
- Per monitor, in this order until one yields a frame: (1) DXGI duplication (formats FP16 then BGRA8, output rotation
  undone so the image matches `monitor.rect`), waits at most 30 ms for a frame with `LastPresentTime != 0`;
  (2) Windows.Graphics.Capture, one frame (FP16 when `monitor.hdr` is `Some`, else BGRA8; cursor off; border switched
  off when the OS accepts `SetIsBorderRequired(false)`, otherwise captured with it; first frame within 300 ms; session
  and pool closed immediately); (3) GDI BitBlt (`CAPTUREBLT`, SDR only, `hdr` = `None`). Both GPU methods use the
  adapter that owns the output and share one device per capture, or the warm device when `warm_up` ran.
  Everything is released after each capture; no duplication or session stays resident. A fresh D3D device is torn
  down on a helper thread (`glint-gpu-release`) because teardown costs 20-300 ms.
- FP16 frames fill `MonitorCapture::hdr` (`sdr_white_nits` from the monitor or 80, `display_peak_nits` from
  `max_nits` or 1000), `hdr_stats` = `analyze`, `sdr` = `tonemap_with_stats` with
  `ToneMapParams { mode: hdr.mode, exposure_stops: hdr.exposure_stops }`. This also happens on a non-HDR monitor if
  the OS hands out FP16. BGRA8 frames are `sdr` with alpha forced to 255, `hdr`/`hdr_stats` = `None`.
- Duplication only delivers a desktop image when something was presented, so a static desktop falls through to WGC,
  which delivers its first frame immediately and keeps HDR (FP16) intact. `CaptureReport::fallback_reason` lists why
  earlier methods were abandoned, e.g. "DDA: no frame with a desktop image within 30 ms (static desktop)".
- `windows_snapshot`: topmost first, `z_order` = index in the result. Keeps visible, non-minimised, non-cloaked
  windows with non-empty DWM extended frame bounds (`rect`, physical px). Skips `exclude_pid`, `Progman`, `WorkerW`,
  zero-alpha layered windows, and tool windows without a title. `foreground_window()` applies the same filters,
  `z_order` is 0. `window_at` returns the first window whose rect contains the point (list must be z-ordered).

## Cost and warm-up

`D3D11CreateDevice` on a cold NVIDIA driver costs ~120 ms; with any device alive in the process it costs ~25 ms.
The warm device is shared by the monitor threads (multithread protection is switched on). The hotkey path therefore
wants `warm_up()` once at startup (call it off the UI thread, it pays the cold cost; re-run
after display changes). It keeps one idle device per adapter, never a duplication, and also loads the
Windows.Graphics.Capture classes (capture item + 1x1 frame pool created and dropped, no session, so no border): the
first WGC capture in a process otherwise costs 0.3 to 2 s extra, after `warm_up` only ~30-50 ms extra. `release_warm_devices()`
drops them. Without warm-up everything still works, just slower on the first capture after idle.

## Verification

`cargo run -p glint-capture --release --example probe -- --out %TEMP%\glint-probe [--runs N] [--warm]
[--order dda,wgc,gdi] [--compare-gdi] [--wgc] [--verbose]` prints monitors, top 15 windows, timings and path per
monitor and writes `monitor-N.png` / `monitor-N.json`. `--order wgc,gdi` simulates a static desktop, `--compare-gdi`
and `--wgc` diff the result against a GDI grab / a forced WGC grab (meaningful on static content).
