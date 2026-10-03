# glint-sys

Windows platform services. Image pixels are straight-alpha BGRA, as in `glint-core`.
Functions return `anyhow::Result` unless noted below. No service starts at import time.

## Keyboard shortcuts

- `KeyboardHook::start(HookConfig, on_action)` starts a hook thread and a callback dispatcher.
- `set_config` updates enabled shortcuts without restarting the hook.
- `on_session_unlock` requests immediate reinstallation; call from the app's unlock handler.
- The hook also reinstalls every ten minutes. Drop stops and joins both threads.
- `HotkeyAction`: `Snip`, `Record`, `Text`, `WindowToClipboard`.
- `HookConfig::from(&settings.hotkeys)` makes Win+Shift+T follow Win+Shift+S.
- `HookState::new(config).on_key(vk, is_up)` is the pure decision engine.
- Callbacks execute off the UI thread. Post actions to your event loop.
- Drop waits for any running callback; callbacks must return promptly.

## Single instance

- `acquire_single_instance() -> InstanceRole`: retain `Primary(InstanceGuard)` for process lifetime.
- `Secondary` also covers mutex acquisition failure, which is logged.
- Create a message-only window with class `IPC_WINDOW_CLASS` (`Glint.Ipc`).
- `forward_to_primary(&[String]) -> Result<bool>` sends UTF-8 JSON with a two-second timeout.
- In `WM_COPYDATA`, pass the actual message LPARAM to `parse_copydata`.
- Return a nonzero LRESULT when accepting the parsed arguments. Maximum message size: 1 MiB.
- The LPARAM must remain valid for the call; never pass arbitrary pointer values.

## Tray and menus

- `Tray::add(hwnd, callback_msg, HICON, tooltip)` installs a version-4 tray icon, ID 1.
- `set_tooltip`, `set_icon`, `recreate` (call on the TaskbarCreated message; keep the same Tray value); Drop removes it. The caller owns the icon handle.
- Register `taskbar_created_message()` and recreate the tray after Explorer restarts.
- `MenuItem::new(id, label)`, `separator()`, `submenu(label, items)` build native menus.
- `.checked(bool)` and `.enabled(bool)` set item state. IDs must be nonzero.
- `show_context_menu(hwnd, &items, PointI) -> Option<u32>` returns the chosen ID.
- `set_dark_menus(bool)` dynamically loads Windows menu theme functions.

## Settings and output paths

- `settings_path() -> PathBuf`: `%APPDATA%/Glint/settings.json`.
- `GLINT_DATA_DIR` overrides the containing directory.
- `load_settings() -> Settings` defaults on missing/unreadable files; invalid JSON is backed up to `.json.bak`.
- `save_settings(&Settings)` writes a unique temporary file and atomically replaces the target.
- `screenshots_dir(&Settings)`, `recordings_dir(&Settings) -> Result<PathBuf>` create directories.
- Defaults use the Windows Pictures and Videos known folders.
- `local_data_dir() -> Result<PathBuf>` creates `%LOCALAPPDATA%/Glint`.
- `timestamped_name(prefix, ext, SystemTime) -> String` uses local time.
- `unique_path(dir, prefix, ext) -> Result<PathBuf>` adds ` (2)`, ` (3)`, etc.
- `unique_path` does not reserve the returned filename against concurrent writers.

## Clipboard and dialogs

- `copy_image(HWND, &Image)` writes CF_DIBV5 and registered PNG data.
- `copy_text(HWND, &str)` writes CF_UNICODETEXT.
- `copy_files(HWND, &[PathBuf])` writes CF_HDROP and copy drop effect.
- File paths are converted to absolute paths before copying.
- Clipboard opening retries ten times, ten milliseconds apart. Pass a valid owner window.
- `dibv5_bytes(&Image) -> Vec<u8>` is pure and keeps straight alpha, with bottom-up BGRA rows.
- `save_image_dialog(owner, dir, name, ImageFormat) -> Option<(PathBuf, ImageFormat)>`.
- `pick_folder(owner, initial) -> Option<PathBuf>`.
- Dialog callers must initialize a COM STA apartment; cancellation and failure return `None`.

## Shell integration

- `open_path(&Path)`, `open_uri(&str)`, `reveal_in_explorer(&Path)`.
- `drag_files(HWND, &[PathBuf])` runs a blocking OLE drag. Initialize OLE on the caller's STA thread.
- `share_files(HWND, &[PathBuf])` opens the Windows share sheet; use a WinRT STA with a message pump.
- Share file loading runs on a worker with a request deferral.
- Share registrations remain alive per window. Call `release_share_window(HWND)` on window destruction.

## OCR

- `ocr(&Image) -> Result<OcrResult>` blocks; run it on a worker thread.
- Windows 0.62.2 exposes the blocking async wait as `.join()` (the earlier `.get()` name is unavailable).
- `ocr_available() -> bool` checks whether a supported engine can be created.
- OCR uses profile languages, with en-US fallback. A Windows OCR language must be installed.
- Small images are enlarged 2×; oversized images fit the engine's maximum dimension.
- `OcrResult { text, lines, language }`; text uses newline-separated lines.
- `OcrLine { text, words }`; `OcrWord { text, rect: RectF }`.
- Word rectangles are in original image pixels.
- `cargo run -p glint-sys --example ocr -- <png>` prints recognized text.
- `--self-test` renders GDI text in an offscreen DIB and checks recognition of `12345`.

## Installation, logging, and sound

- `installed_exe_path() -> Result<PathBuf>`, `is_installed() -> bool`.
- `install(current_exe, &Settings) -> Result<PathBuf>` copies the executable and registers per-user integration.
- Initialize COM before install so the Start-menu shortcut can be created.
- `set_launch_at_login(enabled, exe)` updates the per-user Run entry.
- `uninstall()` restores the saved Print Screen setting and removes Glint registrations and shortcut.
- When uninstalling the running installed copy, exit promptly: a detached helper deletes it after about two seconds.
- The install directory is removed only when empty; unrelated files are preserved.
- `open_default_apps_settings()` opens Glint's default-app settings. Glint never writes UserChoice.
- `init_logging(&Path)` registers a file logger and panic/backtrace hook once per process.
- Logs rotate at 1 MiB to a `.1` sibling.
- `play_shutter()` asynchronously plays a cached 60 ms mono WAV at 48 kHz and −12 dBFS peak.
- Tests do not install hooks, access the clipboard, play audio, write registry values, or open UI.
