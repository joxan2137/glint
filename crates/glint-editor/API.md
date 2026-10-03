# glint-editor API

The markup editor window (DESIGN §8): annotations, crop, OCR text mode, HDR panel, export, copy/save/share.
UI-thread only, like glint-ui.

## Opening an editor
```rust
let id = glint_editor::open_editor(app, EditorDoc { image, hdr, tone_map, monitor, saved_path }, &settings, EditorHost {
    new_snip: Rc::new(|app, mode, delay_secs| { /* start a snip */ }),
    saved: Rc::new(|app, path| { /* remember the last saved file */ }),
})?;
```
- `EditorDoc.image` is the tone-mapped base; `hdr` (raw scRGB) enables the HDR badge once
  `tonemap::analyze` (worker thread) finds HDR content; `tone_map` are the params `image` was made with.
- `monitor` places the window (its work area and DPI) and scales tool sizes to the capture DPI; None = monitor
  under the cursor. `saved_path`: Ctrl+S overwrites it; otherwise Save opens Save As.
- `settings` is a snapshot: `after_capture.{format, save_dir, copy_to_clipboard}` (auto-copy of settled edits),
  `capture.{last_mode, delay_secs}` (New button defaults).
- Window: `WindowSpec::normal(..).unified_title_bar().centered_in(work_rect).min_size(MIN_WINDOW)`, Mica, follows
  the app theme, sized by `window_size(image_px, scale, work_dip)` (image at 100 % plus chrome, ≤ 80 % of the work
  area, ≥ 760×520 DIP). Closes on Ctrl+W / caption ✕ without prompting.
- Each call re-registers one `App::on_event` handler for the editor's private worker messages (same handler every
  time); results are routed to the right window by id.
- Host callbacks: `new_snip(app, mode, delay)` from New / the mode menu / Ctrl+N; `saved(app, path)` after every
  successful save (Ctrl+S, Save As), also when the window closed before the save finished.

## Side effects (only inside a running app, never in previews)
Clipboard (`copy_image` on Copy/Ctrl+C and 500 ms after edits settle, flushed on close; `copy_text` in text mode),
Save As dialog (COM STA, run from an app timer outside the view borrow), `encode::save` on a worker, share sheet via
a PNG in a unique `%TEMP%\Glint\Share\<id>` folder (folders older than 30 min are pruned on open/close), OCR on a
worker, `release_share_window` on close. Images beyond `gfx.max_bitmap_size()` show a downscaled canvas and export
their crop at full resolution; a crop beyond the limit makes copy/save fail with an error toast. Tool options are
remembered per tool on the UI thread for the next editor window.

## Previews and verification
- `render_preview(gfx, kind, theme, scale, image) -> Image`: `PREVIEW_KINDS` = `editor`, `editor-pen`,
  `editor-shapes`, `editor-text`, `editor-crop`, `editor-hdr`, `editor-redact`, `editor-select`, plus
  `editor-ocr`, `editor-picker`, `editor-menu`, `editor-narrow` (760×520). 1200×760 DIP; `image` None = a
  synthetic browser screenshot (HDR kinds synthesize an scRGB version with a 5× SDR-white sun).
- `render_preview_export(gfx, image, cropped) -> Image`: full-resolution export of the `editor` document.
- `cargo run -p glint-editor --release --example preview -- --out <dir> [--image <png>]` writes every kind in dark
  and light at scale 1 and 2, plus `export.png` and `export-cropped.png`. No window.
- `gfx` must be owned by an `Rc` (`Gfx::new()`); previews use `Gfx::shared()`.

## Stroke customization
The options pill keeps three size presets (none is selected for a custom width) plus a stroke button with a live
sample; it opens the Stroke popover: preview, Width 1–64 px (image px, log slider), Opacity 10–100 %, Style
Solid/Dashed/Dotted (pattern in multiples of the width, round caps), Pen pressure + Smoothing 0–100, Line/Arrow start
and end caps (None/Arrow/Filled arrow/Dot) + Arrowhead 50–200 %, Rectangle corner radius, Fill opacity when filled;
the highlighter gets width and opacity only. `[` / `]` (Shift: two steps) and the wheel over the button step the
width. Edits apply to the selected object (one undo step per slider drag) or else to the tool's defaults, which persist
in `editor.json` beside the settings file (`settings_store::settings_path()`; missing, corrupt or partial files fall
back per field). Every object draws inside one group layer at its opacity, so canvas, export and the preview match.

## Behaviour summary
Tools V P H E S T B C; Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z, Ctrl+C, Ctrl+S, Ctrl+Shift+S, Ctrl+N, Ctrl+W, Ctrl+0 fit,
Ctrl+1 100 %, Ctrl+± zoom, Ctrl+wheel/pinch zoom about the cursor, wheel/trackpad/Space-drag/middle-drag pan,
Delete, Esc, arrows nudge (Shift ×10). Annotations live in image pixels; export renders base + annotations at
scale 1 offscreen over the crop rectangle, so copies match the canvas exactly.
