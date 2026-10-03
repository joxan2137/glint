use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tonemap::ToneMapMode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureMode {
    #[default]
    Rectangle,
    Window,
    FullScreen,
    Freeform,
    /// Select a region, OCR it, copy the text.
    Text,
    /// Click a pixel, copy its hex color.
    ColorPicker,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkeys: HotkeySettings,
    pub after_capture: AfterCaptureSettings,
    pub capture: CaptureSettings,
    pub hdr: HdrSettings,
    pub record: RecordSettings,
    pub appearance: AppearanceSettings,
    pub general: GeneralSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeySettings {
    /// Win+Shift+S opens the snip overlay.
    pub win_shift_s: bool,
    /// Print Screen opens the snip overlay.
    pub print_screen: bool,
    /// Win+Shift+R opens the overlay in video mode.
    pub win_shift_r: bool,
    /// Alt+Print Screen captures the active window straight to the clipboard.
    pub alt_print_screen: bool,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self { win_shift_s: true, print_screen: true, win_shift_r: true, alt_print_screen: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AfterCaptureSettings {
    pub copy_to_clipboard: bool,
    /// Floating thumbnail in the bottom-right corner.
    pub show_thumbnail: bool,
    /// Open the editor right away instead of the thumbnail.
    pub open_editor: bool,
    pub auto_save: bool,
    /// None = %USERPROFILE%\Pictures\Screenshots
    pub save_dir: Option<PathBuf>,
    pub format: ImageFormat,
    pub play_sound: bool,
    /// Border drawn around every snip ("snip outline").
    pub outline: Option<Outline>,
}

impl Default for AfterCaptureSettings {
    fn default() -> Self {
        Self {
            copy_to_clipboard: true,
            show_thumbnail: true,
            open_editor: false,
            auto_save: true,
            save_dir: None,
            format: ImageFormat::Png,
            play_sound: false,
            outline: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    /// BGRA
    pub color: [u8; 4],
    pub width_px: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSettings {
    pub last_mode: CaptureMode,
    pub show_magnifier: bool,
    /// 0, 3, 5 or 10.
    pub delay_secs: u32,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self { last_mode: CaptureMode::Rectangle, show_magnifier: true, delay_secs: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrSettings {
    pub mode: ToneMapMode,
    /// Applied on top of SDR-white normalisation; 0 = as displayed.
    pub exposure_stops: f32,
    pub show_badge: bool,
}

impl Default for HdrSettings {
    fn default() -> Self {
        Self { mode: ToneMapMode::Auto, exposure_stops: 0.0, show_badge: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordSettings {
    /// 30 or 60.
    pub fps: u32,
    pub system_audio: bool,
    pub microphone: bool,
    pub include_cursor: bool,
    /// Clip keeps SDR white at 255 (a fixed-peak curve would dim it); Auto compresses HDR highlights.
    pub tone_map: ToneMapMode,
    /// None = %USERPROFILE%\Videos\Screen Recordings
    pub save_dir: Option<PathBuf>,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            fps: 30,
            system_audio: true,
            microphone: false,
            include_cursor: true,
            tone_map: ToneMapMode::Clip,
            save_dir: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: ThemeMode,
    /// Use the Windows accent color instead of the default blue.
    pub system_accent: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    pub launch_at_login: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self { launch_at_login: true }
    }
}
