# glint-overlay API

The frozen-screen selection overlay (DESIGN §6) and the delay countdown pill. UI thread only (`App` from glint-ui).
All rects are physical pixels in virtual-desktop coordinates unless noted.

```rust
pub fn open_overlay(app: &App, request: OverlayRequest,
                    done: impl FnOnce(&App, OverlayOutcome, OverlayPrefs) + 'static) -> anyhow::Result<()>;
pub fn show_countdown(app: &App, monitor: &MonitorInfo, secs: u32, done: impl FnOnce(&App, bool) + 'static)
    -> anyhow::Result<()>;
pub fn snip_rect(captures: &[MonitorCapture], rect_px: RectI, hdr: &HdrSettings, mode: CaptureMode) -> Option<Snip>;
pub fn render_preview(gfx: &Gfx, kind: &str, theme: ThemeMode, scale: f32, captures: Option<&[MonitorCapture]>)
    -> anyhow::Result<Image>;
pub const PREVIEW_KINDS: [&str; 9];   // overlay, overlay-edge, overlay-window, overlay-full, overlay-freeform,
                                      // overlay-video, overlay-color, overlay-menu, countdown
```

## open_overlay
- One `WindowSpec::overlay(monitor.rect)` per capture, each showing its frozen `sdr` (moved, not copied) dimmed by
  `theme.overlay_dim`; the toolbar monitor's window opens last so it takes focus. `Err` (and `done` is never called)
  only when no capture was given or no window could be opened.
- `done` runs exactly once, on a zero-delay timer after the last overlay window closed (120 ms fade first; windows
  still open 400 ms after the session ended are closed directly, and `done` fires even if one never closes). The
  outcome is built then: crops, region tone mapping and the freeform mask never delay the fade.
- `OverlayPrefs` always reflects the final mode, video flag, delay, magnifier and audio toggles: persist it.
- `request.mode`/`video` are normalised: video only with Rectangle, Window or Full screen (else Rectangle).
- `request.delay_secs` only checks the menu item. Picking 3/5/10 s ends with `Delay { secs }`; picking
  "No delay" just sets `prefs.delay_secs = 0`.

Outcomes:
| User action | Outcome |
|---|---|
| Esc, right-click, ✕, Cancel in the record bar, Alt+F4 | `Cancelled` |
| Rectangle drag ≥ 4×4 px, Window click, Full-screen click, Ctrl+click (all monitors stitched), Enter | `Snip(snip)` |
| Freeform lasso (bounding box, alpha 0 outside the antialiased path) | `Snip(snip)` with `mode = Freeform` |
| Text mode drag (or Enter = whole monitor) | `Text(snip)` (app runs OCR) |
| Color mode click / Enter | `Color { bgra }` from the frozen image |
| Video: region (drag / window / monitor) then Record or Enter | `Record(RecordRegion)` (region monitor-relative) |
| Delay menu 3/5/10 s | `Delay { secs }` |

`Snip.mode` is Rectangle, Window, FullScreen (also Enter in Rectangle mode), Freeform or Text.

Keys: R W F L T C mode, V photo/video, M magnifier, Space toggles Rectangle/Window (while dragging: move the
selection), Shift square, arrows nudge the cursor 1 px (Shift 10), Enter captures the hovered window/monitor (or
records the armed region), Esc cancels. Ctrl held in Full screen highlights every monitor.

## snip_rect
`rect_px` is clipped to the area the monitors cover; `None` if it touches no monitor. One monitor: crop; for HDR
monitors the region is re-tone-mapped with its own stats (bit-identical to `tonemap_region`) and `hdr`/`hdr_stats`
hold the raw crop and its stats. Several monitors: parts placed side by side (HDR parts re-tone-mapped per part),
uncovered gaps alpha 0, `hdr = None`. `monitor` = largest overlap.

## show_countdown
Glass capsule (digit + ✕) at the top center of `monitor`, `exclude_from_capture`, topmost, non-activating.
Ticks on absolute one-second deadlines; `done(app, true)` at zero (window closes at once, so capture right away),
`done(app, false)` on ✕ or if the window is closed early. `secs == 0` calls `done(app, true)` on the next turn.

## render_preview
Deterministic offscreen render (no window) of one state at `scale`, dark overlay; `theme` only affects `countdown`.
Synthetic desktop with fake windows when `captures` is `None`; otherwise the primary capture plus a live
`windows_snapshot`. `gfx` must come from `Gfx::new` (uses `Gfx::shared`).

`cargo run -p glint-overlay --release --example preview -- --out <dir> [--live]` writes every kind at scale 1 and 2.

## Cost
Per monitor on the UI thread, warm process, 2560×1440: ~4 ms first frame (upload + paint), ~5 ms for the frame
that first shows the toolbar (glass backdrop blur, cached afterwards). Cold process: ~15 ms each.
