# glint-app (`glint.exe`)

The resident Windows 11 screenshot and screen-recording app that replaces the Snipping Tool. It wires the other
crates together: keyboard hook and tray (glint-sys), frozen-screen capture (glint-capture), the selection overlay
(glint-overlay), the markup editor (glint-editor) and the recorder (glint-record), all drawn with glint-ui.

## What it does

- **Shortcuts**: Win+Shift+S / Print Screen snip, Win+Shift+T extracts text, Win+Shift+R records (press again to
  stop), Alt+Print Screen copies the active window. Captures are frozen on a worker thread, then the overlay opens.
- **After a snip** (DESIGN §7): optional outline → clipboard → auto-save as `Screenshot 2026-10-03 141530.png` →
  shutter sound → floating thumbnail (Edit · Copy · Save · ✕, drag to any app, swipe right to dismiss) or the editor.
- **Text and color**: OCR result or `#RRGGBB` goes to the clipboard with a toast.
- **Recording** (DESIGN §10): 3-2-1 countdown, glass HUD at the top of the recorded monitor (timer, level meter,
  pause, stop, discard, mic) and a red border around the region, both hidden from the capture; then a thumbnail of
  the first frame that opens the MP4.
- **Tray**: click to snip, right-click menu (New snip, Record screen, Extract text, Open image…, Open screenshots
  folder, Settings…, Quit Glint). The glyph follows the taskbar theme and DPI.
- **Settings window**: grouped sections (Shortcuts, After capture, HDR, Recording, General); changes apply live.

## Command line

```
glint                         start resident and show "Glint is running"
glint --background            start resident silently (used by the Run key)
glint --snip [rect|window|full|free|text|color]
glint --record | --settings
glint --edit <image>          open a PNG/JPEG in the editor (Ctrl+S saves back to it)
glint --quit                  ask the running instance to quit
glint --install | --uninstall per-user install to %LOCALAPPDATA%\Programs\Glint, or remove it
glint ms-screenclip:...       same as --snip (Glint can be the ms-screenclip handler)
glint --selftest [--json]     headless checks; exit code 0 only when all pass
glint --preview <kind> --out <png> [--theme dark|light] [--scale 1|1.5|2] [--live]
glint --version
```

A second `glint …` forwards its arguments to the running instance (`WM_COPYDATA` to `Glint.Ipc`) and exits.
Unknown arguments are logged and ignored. `--preview` kinds: `thumbnail`, `thumbnail-hover`, `thumbnail-video`,
`toast`, `toast-color`, `settings`, `settings-light`, `hud`, `hud-paused`, `border`, `tray-icon`, `app-icon`, plus
every overlay and editor kind (`glint_overlay::PREVIEW_KINDS`, `glint_editor::PREVIEW_KINDS`). `--live` uses a real
capture for the overlay, editor and thumbnail kinds.

## Files it writes

- `%LOCALAPPDATA%\Glint\glint.log` (rotated at 1 MB to `glint.log.1`)
- `%APPDATA%\Glint\settings.json` (`GLINT_DATA_DIR` overrides the folder)
- Screenshots in `Pictures\Screenshots`, recordings in `Videos\Screen Recordings` (configurable)
- `%TEMP%\Glint\*.png`: drag sources when auto-save is off (cleared at start)
- `%TEMP%\glint-selftest\`: self-test images; `--install` also writes the Run key, Start-menu shortcut, uninstall
  entry and `ms-screenclip` registration (all removed by `--uninstall`)

## Building

`build.rs` embeds `res/glint.manifest` (per-monitor-v2 DPI, Common Controls v6, UTF-8, long paths), `res/glint.ico`
and version info via `res/glint.rc`. Regenerate the icon with `cargo run -p glint-app --example make_icon`.
