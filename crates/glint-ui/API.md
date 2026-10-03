# glint-ui API

Direct2D/DirectWrite/DirectComposition render kit, single-threaded event loop, springs, Lucide icons, DESIGN §5
themes and widgets. Everything is in DIPs (`f32`) unless a name ends in `_px` (physical pixels, virtual desktop).
Everything except `AppProxy` is `!Send`: one UI thread owns `App`, `Gfx`, windows and views.

## Startup and the loop
```rust
glint_ui::enable_per_monitor_dpi_awareness();            // examples/tests; the app uses its manifest
glint_ui::run(|app: &App| {                               // GetMessage-style loop until app.quit()
    app.on_event(|app, e: MyEvent| { /* typed app events */ });
    let proxy = app.proxy();                              // Send + Sync + Clone: proxy.post(MyEvent::X) -> bool
    app.open(WindowSpec::overlay(monitor.rect), MyView::new())?;
    Ok(())
})?;
```
`App` (cheap `Rc` clone, UI thread only): `gfx()`, `proxy()`, `on_event::<E>(FnMut(&App, E))` (one handler per type),
`post(e)` (queued, never re-entrant), `open(spec, view) -> WindowId`, `close(id)` (deferred), `request_paint(id)`,
`with_view::<V, R>(id, |v: &mut V, cx| ..) -> Option<R>` (downcast + run with a `Ctx`), `hwnd(id) -> Option<isize>`,
`window_ids()`, `set_timer(Duration, FnOnce(&App)) -> TimerId`, `cancel_timer`, `quit()`,
`set_quit_when_no_windows(bool)` (default false: resident), `set_appearance(ThemeMode, use_system_accent)`,
`theme_for(ThemeMode) -> Theme`, `system_prefers_dark()`.
Frames: a window renders only when it asked (`request_paint`, `animate`, a moving `Animated`). While anything
animates the loop waits on `DCompositionWaitForCompositorClock` (fallback `DwmFlush`) and renders every due window
each tick, so monitors at different refresh rates never block each other; idle = blocked in
`MsgWaitForMultipleObjectsEx`, 0 % CPU. Modal loops entered from any callout (window procs, timer callbacks, event
handlers: TrackPopupMenu, IFileSaveDialog, DoDragDrop, window drags) keep frames, timers and posted events running
via a wake timer armed only while work is pending.

## Windows — `WindowSpec`
- `WindowSpec::overlay(rect_px)`: borderless topmost popup exactly covering a monitor, no taskbar button, always dark,
  forced to the foreground when shown (see `win::force_foreground`), DWM transitions off. Alt+F4 → `CloseRequested`.
- `WindowSpec::normal(title, size_dip)`: resizable, Mica, dark title bar following the theme, rounded corners.
  Builders: `.centered_in(work_rect_px)` (size clamped to the work area minus 8 DIP, never below `min_size`; the
  title bar always stays on screen), `.min_size(size)`, `.unified_title_bar()` (content under the title bar;
  return `HitArea::Caption` from `View::hit_test` for drag areas; keep `cx.caption_buttons_rect()` free — Windows
  draws min/max/close there and snap layouts work), `.mica(bool)`, `.fixed_size()`.
- `WindowSpec::popup(origin_px, size_dip)`: borderless, per-pixel transparent, topmost, non-activating by default.
  `.topmost(bool)`, `.activating()`, `.click_through()` (all input passes to windows below). Popups with shadow or
  tooltip margins implement `View::interactive_region` so only the visible pill takes clicks (see Views).
- Common: `.theme(ThemeMode)` (`System` = follow app appearance), `.hidden()` (show later with `cx.show_window`),
  `.exclude_from_capture()` (WDA_EXCLUDEFROMCAPTURE).
Windows appear only after their first frame is presented and committed (no flash). Each window is a DComp target with
a flip-model premultiplied swapchain resized on WM_SIZE/WM_DPICHANGED (per-monitor v2). Device loss is transparent.

## Views
```rust
pub trait View: Any {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool { false } // true = consumed
    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter);
    fn hit_test(&self, pos: PointF) -> HitArea { HitArea::Client }      // unified title bars only
    fn interactive_region(&self) -> Option<Vec<RectF>> { None }       // popups: clickable client rects (DIP)
}
```
`interactive_region` (popups only; `None` = whole window): outside the returned rects clicks reach the windows below,
including other processes. Popups are layered; the window toggles `WS_EX_TRANSPARENT` from the cursor position,
polled with `GetCursorPos` at 30 Hz over/near the popup, less often the farther the cursor is (≤ 2 Hz far away),
only while visible; leaving the region switches immediately, and nothing toggles while a button is held. Return the
current pill rect (e.g. `vec![toolbar.rect()]`, plus an open menu's frame); touch/pen follow the mouse cursor state.
`Ctx`: `size()`, `bounds()`, `scale()` (px per DIP), `px()`, `snap(v)`, `theme()`, `time()` (frame clock, s),
`gfx()`, `app() -> Option<&App>` (None offscreen), `window()`, `hwnd()`, `is_offscreen()`, `request_paint()`,
`animate()` (one more frame; call every paint for continuous motion), `set_cursor(Cursor)`, `capture_pointer()` /
`release_pointer()` (primary presses auto-capture until release), `close()`, `set_window_rect_px(RectI)`,
`move_window_px(PointI)`, `resize_window(SizeF)`, `show_window(activate)`, `hide_window()`, `render_hidden()` (one
frame while hidden: swapchain ready for an instant later show),
`set_window_opacity(f32)` (composition visual, no repaint), `set_topmost`, `set_title`, `minimize`,
`toggle_maximize`, `activate()` (robust foreground + focus, as overlays get on show), `post(e)`,
`set_timer(Duration, token) -> TimerId` (→ `Event::Timer(token)`), `cancel_timer`,
`show_tooltip(anchor, text, shortcut)` / `hide_tooltip()` / `hide_tooltip_for(anchor)` (500 ms delay, fade),
`set_ime_caret(rect)`, `measure_text`, `window_rect_px()`, `client_origin_px()`, `caption_buttons_rect()`.
Window operations are applied right after your call returns, so they never re-enter the view.

`Event`: `PointerDown/Move/Up(PointerEvent)`, `PointerLeave`, `PointerCancel`, `Wheel(WheelEvent)`,
`KeyDown/KeyUp(KeyEvent)`, `Text(String)` (incl. IME commits), `Focus(bool)`, `Resized`, `ScaleChanged`,
`ThemeChanged`, `Timer(u64)`, `CloseRequested` (call `cx.close()` to accept), `Closed`, `Shown`.
`PointerEvent { pos, screen_px, kind: Mouse|Pen|Touch|Touchpad, id, button: Option<MouseButton>, buttons, pressure
0..1, mods, click_count, history: Vec<PointerSample> }` (WM_POINTER; `history` = coalesced pen/mouse samples).
`WheelEvent { pos, delta: PointF /* notches, +y = up */, precise, mods }`. `KeyEvent { key: Key, vk, mods, repeat }`
with `Key::{Escape, Enter, Char('A'..'Z'|'0'..'9'), Left, F(n), Plus, Minus, ..}`; `ev.is(Key::Char('Z'), Modifiers::CTRL)`.

## Painter (DIPs; `p.context()` exposes the raw `ID2D1DeviceContext`)
- Info: `scale()`, `px()`, `snap(v)`, `snap_point`, `snap_rect`, `theme()`, `size()`, `bounds()`, `gfx()`.
- Shapes: `fill_rect`, `stroke_rect`, `fill_round_rect`, `stroke_round_rect`, `hairline_round_rect(r, radius, color,
  inset)` (exactly 1 physical px), `fill_ellipse`, `fill_circle`, `stroke_ellipse`, `line(a, b, brush, w, &style)`,
  `polyline(points, brush, w, &style, closed)`, `fill_path(&Path, brush)`, `stroke_path(&Path, brush, w, &style)`.
  Brushes: anything `Into<Brush>`: a `Color`, `Brush::linear(start, end, &[(offset, color)])`, `Brush::vertical`.
  `StrokeStyle { cap: LineCap, join: LineJoin, dashes, dash_offset }`, `StrokeStyle::round()`, `::dashed(&[..])`.
- Paths: `PathBuilder::new().move_to().line_to().quad_to().cubic_to().arc_to(rx, ry, rot, large, sweep, p)
  .round_rect().ellipse().polyline().close()`, `.even_odd()`, `.build(&gfx) -> Path` (device-independent; cache it).
  `Path::from_svg(&gfx, "M..")`, `path.bounds()`, `path.contains(pt)`.
- Bitmaps: `bitmap(&Bitmap, dest, src_px: Option<RectF>, opacity, Interpolation::{Nearest, Linear, Cubic})`.
- State (closures keep push/pop balanced): `clip_rect(r, |p| ..)`, `clip_round_rect(r, radius, ..)`,
  `clip_path(&path, ..)`, `layer(opacity, ..)` (group opacity, e.g. highlighter strokes), `with_transform(Matrix3x2,
  ..)`, `translate(dx, dy, ..)`, `scale_around(s, center, ..)`.
- Effects: `shadow(rect, radius, &Shadow)` (CSS box-shadow; cached, nine-sliced), `shadow_outside(..)` (not under the
  shape), `glass(rect, radius, Option<&Backdrop>)` — DESIGN §5 frosted panel. `Backdrop::new(&blurred, dest)
  .dimmed(theme.overlay_dim)` where `blurred = bitmap.glass_backdrop(theme, px_per_dip)` and `dest` is where the
  unblurred bitmap is drawn. Without a backdrop glass uses `theme.glass_fill_solid`.
- Text: `text(str, &TextStyle, color, rect) -> width` (single line: optically centered on cap height, aligned per
  style, ellipsis-trimmed; `wrap()` styles flow from the top), `text_at(str, style, color, baseline_origin)`,
  `measure(str, style) -> SizeF`, `layout(str, style, max_width) -> Option<Rc<TextLayout>>` (cached; `caret_rect(i)`,
  `hit_test(pt)`, `raw()`), `draw_layout(&layout, top_left, color)`. `TextStyle::{caption 11, body 13, emphasized,
  title 15sb, large_title 22sb, countdown 64sb tnum}` + `.size() .weight(Weight::Semibold) .tabular() .centered()
  .align(TextAlign::Trailing) .wrap() .line_height()`. Families per DESIGN with Segoe UI fallback; grayscale AA.
- Icons: `icon(Icon::Pen, center, size_dip, color)` (stroke 1.75 on the 24 grid, round caps, pixel-snapped),
  `icon_with_stroke(.., stroke)`. `Icon::ALL`, `icon.name()`. Includes SquareDashed, AppWindow, Monitor, Lasso,
  ScanText, Pipette, Timer, Camera, Video, X, Pen, Pencil, Highlighter, Eraser, Square, Circle, Line, Arrow,
  ArrowUpRight, Minus, Plus, Type, TextSize, Crop, Undo, Redo, Copy, Download, Save, Share, Settings, MousePointer,
  Grid, Redact, Blur, PaintBucket, ZoomIn/Out, Hand, Move, Pause, Play, Stop, PauseFill, PlayFill, StopFill,
  RecordFill, Record, Mic, MicOff, Volume, VolumeX, Check, Chevron{Down,Up,Left,Right}, Folder, Image, Sun, Moon,
  Trash, Maximize, Expand, More, ExternalLink, Scissors, Keyboard, Info, Clipboard, Search.

## Bitmaps, offscreen, Gfx
`Bitmap::new(Image) -> Rc<Bitmap>` (straight BGRA; premultiplied on upload; re-uploaded after device loss),
`.blurred(sigma_px, saturation)` / `.glass_backdrop(theme, px_per_dip)` (σ 24 DIP, ×1.5 sat; GPU, cached, edges
clamped), `.read_back(&gfx) -> Image`, `.image()`, `.width()/.height()`.
`render_offscreen(&gfx, &OffscreenSpec, |cx, p| ..) -> Result<Image>` and `render_view_offscreen(&gfx, &mut view,
&spec)` produce straight-alpha images identical to on-screen output (same painter, DPI, AA). `OffscreenSpec::new(
size_dip, scale, theme)` or `::pixels(w_px, h_px, scale, theme)` (full-resolution export: scale 1, draw in image px),
`.time(t)` (deterministic animation clock), `.background(color)`. Limit: `gfx.max_bitmap_size()` per side.
`Gfx::new()` (WARP fallback; `new_software()`), `shared() -> Option<Rc<Gfx>>` (the owning `Rc`, for `&Gfx` callers of
`render_offscreen`), `factory()`, `dwrite()`, `wic()`, `context()`, `d3d_device()`,
`generation()`, `text_layout()`, `measure_text()`, `font_families()`. Use `app.gfx()` inside a running app. Device
creation raises `IDXGIDevice::SetGPUThreadPriority` to the highest permitted value (7…1, logged; needs privilege).

## Animation and theme
`Animated<T>` for `f32`, `PointF`, `SizeF`, `RectF`, `Color`: `Animated::new(v)` (spring 420/0.86), `::snappy(v)`
(700/0.9), `::fade(v)` (140 ms `cubic-bezier(.2,0,0,1)`), `::with_motion(v, Motion::Tween(..))`; `.set(target)`
(velocity carried over), `.set_with(target, motion)`, `.snap(v)`, `.get()`, `.target()`, `.is_animating()`.
`get()` on a moving value or `set()` automatically schedules the next frame for the window being handled/painted.
`anim::reduced_motion()` (SPI_GETCLIENTAREAANIMATION off → everything snaps), `anim::with_clock(t, ..)`.
`Theme::dark()/light()/resolve(mode, system_dark)/with_accent(c)`, fields per DESIGN §5: `glass_fill`, `hairline`,
`outer_border`, `shadows`, `text`, `text_secondary`, `text_tertiary`, `hover`, `pressed`, `selected`, `accent`,
`destructive`, `success`, `overlay_dim`, `on_accent`, `separator`, `control_track`, `knob`, `window_background`.
`theme::system_prefers_dark()`, `system_accent()`. `Color::hex("#0A84FF")`, `rgba`, `rgba8`, `with_alpha`, `lerp`
(premultiplied), `over`, `to_hex`, `from_bgra8`.

## Widgets (`glint_ui::widgets`) — retained structs owned by your view
Pattern: lay out (`set_rect` / `layout` / `layout_at`), route events (`event(cx, ev) -> Response<T>`:
`Ignored | Consumed | Action(T)`, `.consumed()`, `.action()`), then `paint(p)`. Open menus/popovers first, and
stop routing once something consumed the event. `force_state(hovered, pressed)` snaps visuals for previews.
- `IconButton::new(Icon).label("Save").tooltip("Copy", Some("Ctrl+C")).tint(c).disabled().with_selected(b)`; 32×32.
- `Segmented::new(vec![Segment::icon(..).tooltip(..), Segment::label("Auto"), Segment::dot(7.0)], sel)`
  `.style(SegmentedStyle::Track)`; `Action(index)`; the pill springs to the new segment. `clear_selection()` fades the
  pill out (no segment selected; any click then selects), `has_selection()`.
- `Toolbar::new(vec![ToolbarItem::segmented("mode", seg), ToolbarItem::separator(), ToolbarItem::button("close", b),
  ToolbarItem::custom("time", size)])`; `layout_centered(gfx, center_x, top)`, `paint(p, backdrop)`,
  `Action(ToolbarAction::Clicked(id) | Selected(id, i))`, `set_visible(bool)` (appear/disappear per §5),
  `item(id).rect()`, `button_mut(id)`, `segmented_mut(id)`. Presses on the pill never fall through.
- `Toggle::new(on)` 40×24, `Action(bool)`; `Slider::new(v, min, max).origin(0.0).step(s)` (tick + fill from
  origin), `Action(f32)`; `Button::new("Done", ButtonStyle::Primary|Secondary|Plain|Destructive).icon(..)
  .default_action()` (Enter), 28 high.
- `Menu::new(vec![MenuItem::new("3 s").checked(true).shortcut("3"), MenuItem::separator()])`, `open(gfx, anchor,
  bounds)`, `Action(index)`, keyboard ↑↓ Enter Esc, `check_only(i)`, `paint(p, backdrop)`.
- `Popover::new(content_size)`, `open(anchor, bounds)`, `content_rect()`, `paint(p, backdrop, |p, r| ..)`,
  `Action(())` = dismissed (outside click / Esc).
- `ColorSwatch::new(color)` / `::well(Option<Color>)`, `set_center`, `set_selected`, `Action(())`.
- `Badge::new("1280 × 720").style(BadgeStyle::Glass|Accent|Subtle|Destructive).icon(..).chip(color)`,
  `paint(p, rect)`, `paint_centered(p, center) -> rect`, `size(gfx)`.
- `Countdown::new()`, `set(n)` (pop-in/fade per digit), `paint(p, center, color)`.
- `Tooltip::paint_bubble(p, anchor, text, shortcut, opacity, bounds)` (windows show tooltips automatically).
- `Presence` (appear/disappear helper) and `Interaction` (hover/press amounts) for custom widgets.

## Cursors and window helpers
`Cursor::{Arrow, Hand, IBeam, Crosshair (crisp 1-px, white halo, open center, per DPI), SystemCrosshair, Move,
ResizeNS/EW/NWSE/NESW, NotAllowed, Wait, Progress, Hidden}`, `cursor.handle(dpi)`.
`glint_ui::win`: `set_mica`, `set_dark_mode`, `set_corner_preference`, `disable_transitions`,
`extend_frame_into_client`, `set_exclude_from_capture`, `set_topmost`, `show_no_activate`, `set_window_rect_px`,
`window_rect_px`, `client_origin_px`, `caption_buttons_rect_px`, `dpi_for_window`, `dpi_at_point`, `dpi_for_rect`,
`work_area_at_point`, `force_foreground` (SetForegroundWindow; if refused: zero-length injected mouse move,
AttachThreadInput, then Alt down/up tagged `INJECTED_INPUT_MARKER` = 0x474C4E54 "GLNT"; returns success) (all take
raw HWNDs as `isize`).

## Example: a view with a toolbar
```rust
struct Hud { toolbar: Toolbar }
impl View for Hud {
    fn event(&mut self, cx: &mut Ctx, ev: &Event) -> bool {
        match self.toolbar.event(cx, ev) {
            Response::Action(ToolbarAction::Clicked("stop")) => { cx.post(HudCommand::Stop); true }
            r => r.consumed(),
        }
    }
    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.toolbar.layout_centered(cx.gfx(), cx.size().w / 2.0, 12.0);
        self.toolbar.paint(p, None);
        let slot = self.toolbar.item("time").unwrap().rect();
        let pulse = 0.5 + 0.5 * (cx.time() * 4.0).cos() as f32;
        p.fill_circle(PointF::new(slot.x + 14.0, slot.center().y), 5.0, p.theme().destructive.with_alpha(0.5 + 0.5 * pulse));
        cx.animate();
    }
}
// app.open(WindowSpec::popup(origin_px, SizeF::new(360.0, 72.0)).exclude_from_capture(), Hud { .. })?;
```
Full versions: `examples/gallery.rs` (`HudView`, overlay and editor mocks), `examples/window_smoke.rs` (windows).
Verify visuals with `cargo run -p glint-ui --release --example gallery -- --out <dir>` (PNGs, no window).
