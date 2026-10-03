# Glint — design contract

Glint is a resident Windows 11 screenshot and screen-recording app written in Rust that replaces the Snipping Tool.
Its distinguishing feature: captures taken while the desktop is in HDR mode come out looking exactly like what the
user saw on an SDR screen — no washed-out greys, no blown highlights. The UI must feel Apple-made: calm, precise,
fast, animated with springs, crisp at every DPI.

This file is the contract. Workers implement against it; disputes are settled here. Only the main thread edits
this file and `crates/glint-core/src/{geom,image,display,settings}.rs`. If you need a change there, say so in your
report instead of editing.

## 1. Workspace

```
glint/
  Cargo.toml              workspace; all shared deps pinned in [workspace.dependencies]
  DESIGN.md
  crates/
    glint-core/           pure Rust, no Windows deps: geometry, Image/HdrImage, MonitorInfo/WindowInfo,
                          Settings (serde), HDR tone mapping, PNG/JPEG encode
    glint-capture/        monitors + HDR info, DXGI desktop duplication (FP16 when HDR), window list
    glint-ui/             Direct2D/DirectWrite/DirectComposition render kit, event loop, widgets, animation,
                          icons, theme, offscreen rendering
    glint-sys/            keyboard hook, tray, single instance + IPC, settings file, clipboard, file dialogs,
                          OCR, autostart/install, shell helpers
    glint-record/         screen recording engine (Windows.Graphics.Capture + D3D11 tone-map shader +
                          Media Foundation H.264/AAC MP4), headless
    glint-overlay/        the frozen-screen selection overlay (all capture modes), countdown pill
    glint-editor/         the markup editor window (annotations, crop, OCR, HDR panel, export)
    glint-app/            bin `glint`: wiring, tray, thumbnail, settings window, recording HUD, CLI
```

Dependency direction: core ← capture, sys, record, ui ← overlay, editor ← app. Overlay and editor may also use
capture and sys. No cycles. `glint-ui` must not depend on capture/sys/record. The public entry points of overlay
and editor are fixed in their `src/lib.rs` (main thread owns those signatures; additions are fine).

Toolchain: stable MSVC (`x86_64-pc-windows-msvc`), edition 2024. `windows` crate version pinned once in the
workspace with the union of features (§12). No C/C++ build steps; shaders are compiled at runtime with
`D3DCompile` (d3dcompiler_47.dll ships with Windows).

Process: Per-Monitor-V2 DPI aware (manifest embedded by glint-app's build.rs; examples/tests call
`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` first). `#![windows_subsystem = "windows"]`
for the app. Log to `%LOCALAPPDATA%\Glint\glint.log` (keep last 1 MB).

## 2. Rules for workers

- Never launch `glint.exe` (or any example) in a way that shows a window, installs a keyboard hook, writes the
  registry, touches the clipboard, or installs anything. Every crate has a headless verification path (§11); use it.
- Never use computer-use tools. Visual checks go through offscreen PNG renders.
- Never kill processes by name. Stop only PIDs you started.
- Build with your own target dir to avoid lock contention: `$env:CARGO_TARGET_DIR="target/<crate-name>"`
  (PowerShell) or `CARGO_TARGET_DIR=target/<crate-name>` (bash). The repo path contains an apostrophe
  (`claude's-den`); always quote it.
- `cargo clippy -p <crate> -- -D warnings` and `cargo test -p <crate>` must pass for your crate.
- Code style: self-documenting names, few comments, no commented-out code, `anyhow::Result` at boundaries,
  `thiserror` only where callers match on errors. Unsafe Win32 calls wrapped in small safe functions.
- Write `crates/<crate>/API.md`: the public API in ≤150 lines, for the workers who build on it.

## 3. Capture pipeline

1. Hotkey fires (§9) → main thread calls `glint_capture::capture_all(&settings.hdr)`.
2. For every monitor: DXGI `IDXGIOutput5::DuplicateOutput1` with formats `[R16G16B16A16_FLOAT, B8G8R8A8_UNORM]`.
   The OS picks FP16 when the desktop is in HDR/advanced-color mode. First frame → staging texture → CPU.
   Fallback when duplication fails (e.g. exclusive mode, secure desktop): GDI `BitBlt` of the monitor (SDR only).
3. FP16 frames → `HdrImage` (with `sdr_white_nits` from `DisplayConfigGetDeviceInfo(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL)`,
   peak from `IDXGIOutput6::GetDesc1`) → `glint_core::tonemap::tonemap` → `Image`. BGRA8 frames → `Image` directly.
4. Window list snapshot (`glint_capture::windows_snapshot()`) taken in the same call, before the overlay exists.
5. Budget: hotkey → overlay visible ≤ 150 ms for 2560×1440 + 1920×1200 on a desktop CPU. Capture threads may run
   per monitor in parallel.
6. Final snip: `tonemap_region(hdr, rect, params)` re-tone-maps the selected region with its own stats, so a region
   that contains only SDR content is bit-exact even if HDR video plays elsewhere. The overlay preview uses the
   whole-monitor result.

## 4. Tone mapping (glint-core::tonemap)

Input scRGB linear (1.0 = 80 nits). Output sRGB 8-bit, alpha 255. Per pixel:

1. `s = 80 / sdr_white_nits * 2^exposure_stops`; `x = rgb * s`. SDR content now spans exactly [0, 1].
2. Gamut: if `min(x) < 0`: `L = 0.2126 r + 0.7152 g + 0.0722 b`; if `L <= 0` → `x = 0`; else move toward grey,
   `x = L + (x - L) * (L / (L - min(x)))`, which puts the smallest channel at 0 and keeps luminance.
3. Highlights, `m = max(x)`:
   - `Clip`: clamp each channel to [0, 1].
   - `Auto`: `src = stats.peak * 2^exposure_stops`. If `src <= 1 + 0.5/255` → clamp (identity for SDR).
     Else compress `m` with the BT.2390 EETF evaluated in PQ space: source white `Lw = PQ(src * sdr_white_nits)`,
     target `Lmax = PQ(sdr_white_nits)`, `E1 = PQ(m * sdr_white_nits) / Lw`, `maxLum = Lmax / Lw`,
     `KS = max(0, 1.5 * maxLum - 0.5)`; below KS identity, above it the Hermite spline
     `T = (E1 - KS)/(1 - KS)`, `E2 = (2T³ - 3T² + 1)KS + (T³ - 2T² + T)(1 - KS) + (-2T³ + 3T²)maxLum`;
     `m' = PQ⁻¹(E2 * Lw) / sdr_white_nits`; `x *= m' / m` (hue preserving); final clamp to [0, 1].
     PQ is SMPTE ST 2084 with 10000-nit reference.
4. Encode each channel with the piecewise sRGB OETF and round to the nearest 8-bit code by comparing against the
   255 midpoint thresholds in linear space (`EOTF((c + 0.5) / 255)`), so SDR content round-trips exactly.
5. Stats (`analyze`): histogram of `log2(max(r,g,b) * 80 / sdr_white_nits)` over 2048 bins in [-16, +8] stops,
   99.9th percentile → `peak`; plus max, `hdr_fraction`, `out_of_gamut_fraction`. `has_hdr_content` =
   `peak > 1 + 0.5/255`.
6. Performance: f16→f32 through a 65 536-entry LUT, the m→m' curve through a 4096-entry LUT over log2(m)
   with linear interpolation, rows in parallel with rayon. 3840×2160 all-HDR worst case ≤ 60 ms release build on 8 cores (best of 3); 2560×1440 desktop ≈ 20 ms.

The record shader (glint-record) implements the same curve in HLSL with a fixed source peak
(`display_peak_nits / sdr_white_nits`) so video brightness never pumps. Because a fixed peak dims SDR white
(245/255 at 280-nit white on a 519-nit display), recordings use `settings.record.tone_map`, default `Clip`.

## 5. Visual language

Everything is drawn by glint-ui with Direct2D; no Win32 common controls.

Type: `Segoe UI Variable Text` ≤ 15 DIP, `Segoe UI Variable Display` ≥ 20 DIP, fallback `Segoe UI`.
Sizes: caption 11, body/control 13, emphasized 13 semibold, title 15 semibold, large title 22 semibold,
countdown 64 semibold with tabular figures. Grayscale antialiasing on transparent surfaces.

Grid 4 DIP. Radii: toolbar 14, popover 12, button 8, swatch full circle, thumbnail 10, window 8 (DWM).
Toolbar height 44, icon buttons 32×32 with 18 DIP icons, separators 1×20 DIP.

Icons: line icons on a 24 grid, round caps and joins, stroke 1.75 at 18–20 DIP (Lucide geometry; see glint-ui).

Dark palette (overlay always dark, other windows follow system theme):
- glass: blurred backdrop (Gaussian σ 24 DIP, saturation 1.5) under fill `rgba(30,30,32,0.70)`
- hairline inner border `rgba(255,255,255,0.14)` 1 physical px; outer border `rgba(0,0,0,0.45)` 1 px
- shadow: two layers, `0 12 40 rgba(0,0,0,0.40)` and `0 2 6 rgba(0,0,0,0.25)`
- text primary `rgba(255,255,255,0.92)`, secondary `0.60`, tertiary `0.35`
- control hover `rgba(255,255,255,0.08)`, pressed `0.14`, selected `0.20`
- accent `#0A84FF`, destructive/record `#FF453A`, success `#30D158`
- overlay dim `rgba(0,0,0,0.40)`

Light palette: glass fill `rgba(246,246,248,0.78)`, hairline `rgba(0,0,0,0.08)`, outer `rgba(0,0,0,0.12)`,
text `rgba(0,0,0,0.88)` / `0.55` / `0.30`, hover `rgba(0,0,0,0.05)`, pressed `0.09`, selected `0.12`,
accent `#007AFF`, destructive `#FF3B30`, success `#34C759`. Shadows half as strong.

Motion: springs for anything that moves or resizes (default stiffness 420, damping ratio 0.86; "snappy" for
hover highlights stiffness 700, ratio 0.9). Fades 140 ms `cubic-bezier(0.2, 0, 0, 1)`. Appear: opacity 0→1,
scale 0.96→1, y −6→0 DIP. Disappear: 120 ms fade + scale to 0.98. Nothing over 350 ms. Honour
`SPI_GETCLIENTAREAANIMATION` = off by snapping. Only render while something animates or changes; idle cost 0 %.

No first-frame flash: windows are shown only after their first frame is committed.

## 6. Overlay (glint-overlay)

One borderless topmost window per monitor covering it exactly, showing that monitor's frozen `sdr` image, dimmed.
All overlay windows close together. Escape or right-click cancels.

Toolbar (only on the monitor under the cursor when the overlay opens; follows the cursor to another monitor with
a fade): glass pill, top-center, 24 DIP from the top edge. Contents, left to right:
`[Rectangle | Window | Full screen | Freeform]` segmented · `[Text | Color]` · timer menu `[No delay, 3 s, 5 s, 10 s]`
· `[Photo | Video]` segmented · close ✕. Tooltips after 500 ms with name and key: R, W, F, L, T, C, V.
The toolbar fades out while dragging a selection and returns after.

Modes:
- Rectangle: custom crisp crosshair HCURSOR (hardware cursor, no lag). Drag selects; the selected area shows
  undimmed with a 1 px white border plus 1 px black 25 % outer line. A size pill `1280 × 720` (tabular figures)
  follows the selection. Shift = square, Space while dragging = move the selection, arrows nudge the cursor 1 px
  (Shift 10 px). Release = capture. Magnifier (if enabled): 112 DIP circle near the cursor, 8× nearest-neighbour,
  faint pixel grid, center pixel outlined, below it `X 1234  Y 567` and the hex color.
- Window: hovering highlights the topmost window under the cursor (from the snapshot) with an accent-tinted rounded
  rect that morphs between windows with the snappy spring. Click captures its rect (cropped from the frozen image).
- Full screen: hovering highlights the monitor; click captures it. Ctrl+click captures all monitors stitched.
- Freeform: drag a lasso; result is the lasso's bounding box with transparent pixels outside the path.
- Text: rectangle selection, then OCR; result goes to the clipboard as text.
- Color: magnifier always on; click copies `#RRGGBB`.
- Video: rectangle (or window/full-screen) selection, then a small glass bar under the region:
  `[● Record] [mic toggle] [system audio toggle] [Cancel]`. Record → 3-2-1 countdown → recording.
- Timer: choosing 3/5/10 s closes the overlay, shows a countdown pill (excluded from capture), then captures again
  and reopens the overlay in the same mode. The delay is remembered in settings.

Result type: `OverlayOutcome` (see glint-overlay API.md) carries the cropped `Image`, the cropped raw `HdrImage`
if any, the virtual-desktop rect, the monitor, and the mode.

## 7. After capture (glint-app)

In order: optional outline is drawn → copy to clipboard (`CF_DIBV5` with alpha + registered `PNG`) → auto-save
to `save_dir` as `Screenshot 2026-10-03 141530.png` → floating thumbnail or editor.

Floating thumbnail: bottom-right of the capture's monitor work area, 16 DIP margin, image ≤ 200×150 DIP,
radius 10, hairline, shadow. Springs in from the right edge. Hover pauses the 6 s auto-dismiss and shows
`Edit · Copy · Save · ✕` icon buttons. Click opens the editor. Drag starts an OLE file drag of the saved PNG
(saves to `%TEMP%` first if auto-save is off). Swipe right dismisses. New capture replaces the old thumbnail.

## 8. Editor (glint-editor)

Window sized to the image (fit inside 80 % of the work area, min 760×520 DIP), centered on the capture's monitor,
Mica backdrop, dark/light per system, rounded DWM corners. Unified title bar 52 DIP: left `New` button with a
mode chevron menu and delay menu; center the tool pill; right action buttons `Text` (OCR), `Copy`, `Save`,
`Share`, then the system caption buttons.

Tools (keys): Select V, Pen P, Highlighter H, Eraser E, Shapes S (rectangle, ellipse, line, arrow), Text T,
Redact B (blur or pixelate), Crop C. Undo Ctrl+Z, redo Ctrl+Y / Ctrl+Shift+Z.
When a tool is active a second glass pill under the toolbar shows its options: 8 color swatches
(`#FF3B30 #FF9500 #FFCC00 #34C759 #007AFF #AF52DE #000000 #FFFFFF`) plus a custom color well, three stroke
sizes, shape kind, fill toggle, text size, redact kind.

Canvas: image centered with 24 DIP padding, radius 6, soft shadow; Ctrl+wheel / pinch zoom around the cursor,
wheel/trackpad pan, Space+drag pan, `Ctrl+0` fit, `Ctrl+1` 100 %. Bottom-center glass pill: `− 100 % +`, Fit,
image size, and an `HDR` badge when the capture had HDR content → popover with tone-map mode
`[Auto | Clip]` and an exposure slider (−2…+2 stops) that re-renders the base image live from the raw `HdrImage`.

Annotations are vector objects in image pixel coordinates: Stroke (points with pressure; pen or highlighter),
Shape (kind, rect/line endpoints, color, width, filled), Text (string, origin, size, color), Redact (rect, kind).
Pen strokes use `WM_POINTER` pressure when a pen is used, smoothed with Catmull-Rom → cubic Béziers; highlighter
is a single flat-nib geometry at 40 % alpha (no self-overlap darkening). Eraser removes whole objects it touches.
Select tool: click to select, drag to move, handles to resize shapes/redacts, Delete removes. Text tool: click to
place, type with a caret, Enter = new line, Esc = finish. Crop: Apple Photos style corner brackets, rule-of-thirds
grid while dragging, outside dimmed, `Cancel / Reset / Done`, Enter/Esc. Crop is non-destructive.

Undo stack stores whole document snapshots (annotations + crop + HDR params).
Export (copy/save/share/drag) renders base + annotations at full image resolution offscreen, then crops.
Shortcuts: Ctrl+C copy, Ctrl+S save (to the auto-save path if the doc was auto-saved, else Save As),
Ctrl+Shift+S save as, Ctrl+N new snip, Ctrl+W close, Delete delete selection, Esc deselect.

## 9. Snipping Tool replacement (glint-sys + glint-app)

- `RegisterHotKey` cannot take Win+Shift+S (Explorer owns it). Dedicated hook thread with its own message loop
  runs `WH_KEYBOARD_LL`. The hook proc only matches, swallows (returns 1) and posts to a channel; it never blocks
  (Windows silently removes hooks that exceed `LowLevelHooksTimeout`, default 300 ms). Track Win/Shift/Ctrl/Alt
  state from the hook's own events (GetAsyncKeyState is stale inside the callback). Always `CallNextHookEx` for
  keys we do not handle. Re-install the hook every 10 minutes and on session unlock as a watchdog.
- Win+Shift+S → snip overlay (last mode). Print Screen → snip overlay. Win+Shift+R → overlay in Video mode.
  Win+Shift+T → overlay in Text mode. Alt+Print Screen → capture the foreground window straight to clipboard +
  thumbnail. Swallow keydown, auto-repeat and keyup of the trigger key.
- When a swallowed combo leaves Win logically down, inject the unassigned mask key (VK 0xE8 down/up, tagged with
  our `dwExtraInfo` marker) so the Start menu does not open on Win release. Ignore injected events carrying our
  marker.
- Install also sets `HKCU\Control Panel\Keyboard\PrintScreenKeyForSnippingEnabled` = 0 (prior value saved in
  settings and restored on uninstall) so Print Screen falls back to a plain clipboard copy if Glint is not running,
  and registers Glint per-user as an `ms-screenclip` URL handler (Capabilities + RegisteredApplications +
  ProgID with `"%1"`, then `SHChangeNotify(SHCNE_ASSOCCHANGED)`). Choosing the default is the user's decision in
  Settings › Apps › Default apps (UserChoice is hash-protected); the settings window links there.
- Known limit: while an elevated window has focus, a non-elevated hook sees no keys (UIPI), so Windows' own
  Snipping Tool answers there.
- Single instance: named mutex `Local\Glint.SingleInstance` + message-only window `Glint.Ipc`; a second
  `glint.exe <args>` forwards its command line with `WM_COPYDATA` and exits.
- CLI: `glint` (start resident, tray, show a short "Glint is running" thumbnail-style toast), `glint --background`
  (start resident silently; used by the Run key), `glint --snip [rect|window|full|free|text|color]`, `glint --record`,
  `glint --settings`, `glint --install`, `glint --uninstall`, `glint --selftest [--json]`,
  `glint --preview <view> --out <png> [--theme dark|light] [--scale 1|1.5|2]`, `glint ms-screenclip:...`
  (treated as `--snip`).
- Install (`--install`, also offered in settings): copy the exe to `%LOCALAPPDATA%\Programs\Glint\glint.exe`,
  HKCU `Run` value `Glint`, Start-menu shortcut, HKCU uninstall entry, then start the installed copy.
  `--uninstall` reverses all of it. Snipping Tool itself is never removed; when Glint is not running the keys go
  back to Windows.

## 10. Recording (glint-record + app HUD)

Windows.Graphics.Capture on the monitor (cursor per settings, border off where the OS allows), FP16 frame pool
when HDR. Per frame on the GPU: crop to the region, tone map (§4, fixed peak), convert to BGRA8. Encoder:
Media Foundation sink writer, H.264 (hardware MFT when available), MP4, fps 30/60, bitrate ≈ 0.15 bits/pixel/frame,
even dimensions. Audio: WASAPI loopback (system) and/or microphone, mixed to 48 kHz stereo, AAC 192 kbps.
Pause/resume keeps timestamps continuous. Output `Screen Recording 2026-10-03 141530.mp4` in the record save dir.

HUD (app): glass pill top-center of the recorded monitor: pulsing red dot, elapsed `00:12`, pause/resume,
stop, discard, mic toggle. A 2 px red rounded border sits just outside the region. Both windows use
`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`. Stop → thumbnail of the first frame; click opens the file
with the default player.

## 11. Verification

- glint-core: `cargo test -p glint-core` (round-trip exactness, curve monotonicity, gamut, stats, timing).
- glint-capture: `cargo run -p glint-capture --example probe -- --out <dir>` prints monitors/HDR info/windows and
  writes each monitor as PNG + stats JSON. No window, no hook.
- glint-ui: `cargo run -p glint-ui --example gallery -- --out <dir>` renders every widget/state, dark and light,
  scale 1 and 2, to PNGs offscreen.
- glint-record: `cargo run -p glint-record --example rec -- --seconds 3 --out <file.mp4>`; check with `ffprobe`.
- glint-sys: `cargo run -p glint-sys --example ocr -- <png>` prints text; other pieces are unit tested without
  side effects.
- overlay/editor/app: `glint --preview overlay|overlay-window|overlay-video|editor|editor-crop|thumbnail|settings|hud
  --out x.png` renders offscreen from a synthetic or live capture; `glint --selftest --json` runs headless checks.

## 12. Shared dependencies

Pinned in the workspace root; crates use `{ workspace = true }`. Add a dependency only through the main thread
(say so in your report).
