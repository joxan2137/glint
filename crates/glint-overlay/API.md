# glint-overlay API

The selection overlay (DESIGN §6) and the delay countdown pill. UI thread only (`App` from glint-ui). All rects are
physical pixels in virtual-desktop coordinates unless noted.

```rust
pub struct OverlayPool;                 // hidden overlay windows, one per monitor, ready to show
impl OverlayPool {
    pub fn create(app: &App, monitors: &[MonitorInfo]) -> Result<OverlayPool>;   // startup + display changes
    pub fn covers(&self, monitors: &[MonitorInfo]) -> bool;  pub fn monitors(&self) -> Vec<MonitorInfo>;
    pub fn is_ready(&self, app: &App) -> bool;              pub fn destroy(&self, app: &App);
    pub fn open(&self, app: &App, request: OverlayRequest, done: impl FnOnce(&App, OverlayOutcome, OverlayPrefs))
        -> Result<OverlaySession>;      // new windows instead when the pool does not fit
}
pub fn open_overlay(app, request, done) -> Result<OverlaySession>;   // one-off windows, closed afterwards
#[derive(Clone)] pub struct OverlaySession;
impl OverlaySession {
    pub fn set_captures(&self, app: &App, captures: Vec<MonitorCapture>);  // the freeze arrives
    pub fn cancel(&self, app: &App);  pub fn is_open(&self) -> bool;  pub fn has_captures(&self) -> bool;
}
pub fn show_countdown(app, monitor: &MonitorInfo, secs: u32, done: impl FnOnce(&App, bool)) -> Result<()>;
pub fn snip_rect(captures: &[MonitorCapture], rect_px: RectI, hdr: &HdrSettings, mode: CaptureMode) -> Option<Snip>;
pub fn warm_up(gfx: &Rc<Gfx>, scales: &[f32]);     // OverlayPool::create calls it
pub fn render_preview(gfx: &Gfx, kind: &str, theme: ThemeMode, scale: f32, captures: Option<&[MonitorCapture]>)
    -> Result<Image>;
pub const PREVIEW_KINDS: [&str; 11];
```

`OverlayRequest { monitors, captures, windows, mode, video, show_magnifier, delay_secs, hdr, system_audio,
microphone, requested_at }`: `monitors` = the windows to open (empty: the captures' monitors); `captures` may be
empty; `requested_at` = hotkey time for latency logs.

## Instant overlay, late freeze
- Overlay windows use `exclude_from_capture`, so they open before capturing. Pending state: transparent over the live
  desktop with the dim, solid-glass toolbar, hover highlights from `windows`, drags, Text/Video work; magnifier and
  color sample wait for pixels. `set_captures` swaps the frozen image in under the dim (same pixels) and crossfades
  the controls to frosted glass in 140 ms. Captures are matched to monitors by rect, then device name.
- An ending that needs pixels (selection, lasso, color) waits with a "Capturing…" pill and progress cursor, then
  completes when the captures arrive; Esc / right-click still cancel. `cancel` (capture failed) ends with
  `Cancelled`. Record, Delay and Cancel never wait.
- Pool: windows are created hidden with their swapchain rendered (`Ctx::render_hidden`) and the controls' text,
  icon and shadow caches warmed. `open` resets the views, raises them above newer topmost popups and shows; the first
  frame is a dim fill only (no upload, no blur). At session end the last frame is empty, then the window hides
  (reused next time). Rebuild the pool when monitors or DPI change; `open` falls back to new windows otherwise.
- `done` runs exactly once after every window left the session (120 ms fade; windows still in it 400 ms after the
  end are hidden or closed directly). The outcome (crops, region tone mapping, lasso mask) is built then.
- Info logs: `monitor N on screen X ms after the request`, `first frame painted in X ms`, `captures delivered X ms
  after the request`.

## Outcomes
| User action | Outcome |
|---|---|
| Esc, right-click, ✕, Cancel in the record bar, Alt+F4, `cancel` | `Cancelled` |
| Rectangle drag ≥ 4×4 px, Window click, Full-screen click, Ctrl+click (all monitors stitched), Enter | `Snip(snip)` |
| Freeform lasso (bounding box, alpha 0 outside; `hdr`/`hdr_stats` None) | `Snip(snip)`, `mode = Freeform` |
| Text mode drag (Enter = whole monitor) | `Text(snip)` |
| Color mode click / Enter | `Color { bgra }` |
| Video: region (drag / window / monitor), then Record or Enter | `Record(RecordRegion)` (monitor-relative) |
| Delay menu 3/5/10 s | `Delay { secs }` |

`OverlayPrefs` always carries the final mode, video flag, delay, magnifier and audio toggles. Keys: R W F L T C
modes, V photo/video, M magnifier, Space Rectangle/Window (while dragging: move), Shift square, arrows nudge 1 px
(Shift 10), Enter captures (no key repeat), Esc cancels, Ctrl in Full screen = all monitors.

## snip_rect, countdown, previews
- `snip_rect`: clipped to the covered area; one monitor = crop (HDR: region re-tone-mapped, raw crop + stats);
  several = side by side, gaps alpha 0, `hdr` None; `monitor` = largest overlap.
- `show_countdown`: glass capsule at the top center, excluded from capture; only the pill takes clicks
  (`interactive_region`); `done(app, true)` at zero, `false` on ✕ or early close.
- Previews: synthetic desktop unless `captures`; `overlay-pending` / `overlay-busy` show the live-desktop states.
  `cargo run -p glint-overlay --release --example preview -- --out <dir> [--live]`.
- Latency probe: `cargo test -p glint-overlay --release latency -- --ignored --nocapture`.
