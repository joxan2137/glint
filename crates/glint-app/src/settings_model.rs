//! The settings window's content as data: grouped sections of rows (title, secondary text, control), how an edit
//! changes `Settings`, and which side effects a change needs. Pure, so it is unit tested without a window.

use glint_core::settings::Outline;
use glint_core::{HdrInfo, ImageFormat, Settings, ThemeMode, ToneMapMode};

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayLine {
    pub name: String,
    pub hdr: Option<HdrInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstallState {
    NotInstalled,
    Installed { path: String },
}

/// Facts the settings window shows that are not settings.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsEnv {
    pub displays: Vec<DisplayLine>,
    pub screenshots_dir: String,
    pub recordings_dir: String,
    pub install: InstallState,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    WinShiftS,
    PrintScreen,
    WinShiftR,
    AltPrintScreen,
    CopyToClipboard,
    ShowThumbnail,
    OpenEditor,
    AutoSave,
    SaveFolder,
    Format,
    PlaySound,
    Outline,
    ToneMap,
    Exposure,
    HdrBadge,
    Display(usize),
    RecordFps,
    SystemAudio,
    Microphone,
    Cursor,
    RecordToneMap,
    RecordFolder,
    LaunchAtLogin,
    Appearance,
    Accent,
    DefaultApp,
    Install,
    Version,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Toggle(bool),
    /// A global shortcut: its keys as keycaps, then an on/off switch.
    Shortcut { keys: Vec<String>, on: bool },
    /// Segmented control.
    Segments { options: Vec<String>, selected: usize },
    /// Popup button with a menu.
    Popup { options: Vec<String>, selected: usize },
    Slider { value: f32, min: f32, max: f32, step: f32 },
    Button(String),
    /// A pending destructive action: a cancel button and a red confirm button.
    Confirm { cancel: String, confirm: String },
    /// The whole row is a link with a trailing arrow.
    Link,
    /// A status pill: `HDR` (accent) or `SDR` (subtle).
    Status { text: String, highlighted: bool },
    Value(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: RowId,
    pub title: String,
    pub detail: Option<String>,
    pub control: Control,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
    pub footer: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    Toggle(bool),
    Choose(usize),
    Slide(f32),
    Press,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderKind {
    Screenshots,
    Recordings,
}

/// Edits that need the app (dialogs, shell, install).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsRequest {
    PickFolder(FolderKind),
    OpenDefaultApps,
    Install,
    Uninstall,
}

const OUTLINES: [(&str, Option<Outline>); 5] = [
    ("Off", None),
    ("Black, 1 px", Some(Outline { color: [0, 0, 0, 255], width_px: 1 })),
    ("Gray, 2 px", Some(Outline { color: [0x93, 0x8E, 0x8E, 255], width_px: 2 })),
    ("Red, 2 px", Some(Outline { color: [0x30, 0x3B, 0xFF, 255], width_px: 2 })),
    ("Blue, 2 px", Some(Outline { color: [0xFF, 0x7A, 0x00, 255], width_px: 2 })),
];

const CUSTOM_OUTLINE: &str = "Custom";

fn outline_choice(outline: &Option<Outline>) -> (Vec<String>, usize) {
    let mut options: Vec<String> = OUTLINES.iter().map(|(name, _)| name.to_string()).collect();
    match OUTLINES.iter().position(|(_, o)| o == outline) {
        Some(index) => (options, index),
        None => {
            options.push(CUSTOM_OUTLINE.to_string());
            (options, OUTLINES.len())
        }
    }
}

fn segments(options: &[&str], selected: usize) -> Control {
    Control::Segments { options: options.iter().map(|o| o.to_string()).collect(), selected }
}

fn shortcut(keys: &[&str], on: bool) -> Control {
    Control::Shortcut { keys: keys.iter().map(|k| k.to_string()).collect(), on }
}

/// The key name drawn as the Windows logo.
pub const WIN_KEY: &str = "Win";

fn row(id: RowId, title: &str, detail: Option<&str>, control: Control) -> Row {
    Row { id, title: title.to_string(), detail: detail.map(str::to_string), control }
}

pub fn format_exposure(stops: f32) -> String {
    let rounded = (stops * 10.0).round() / 10.0;
    if rounded.abs() < 0.05 { "0 EV".to_string() } else { format!("{rounded:+.1} EV") }
}

fn display_detail(hdr: &Option<HdrInfo>) -> String {
    match hdr {
        Some(info) => format!("HDR on · SDR content {:.0} nits · peak {:.0} nits", info.sdr_white_nits, info.max_nits),
        None => "Standard dynamic range".to_string(),
    }
}

/// The page's rows. `confirming_uninstall` turns the install row into an "Uninstall Glint?" confirmation.
pub fn sections(settings: &Settings, env: &SettingsEnv, confirming_uninstall: bool) -> Vec<Section> {
    let hotkeys = &settings.hotkeys;
    let after = &settings.after_capture;
    let (outline_options, outline_selected) = outline_choice(&after.outline);
    let mut hdr_rows = vec![
        row(
            RowId::ToneMap,
            "Tone mapping",
            Some("Auto keeps bright highlights; Clip cuts them at SDR white"),
            segments(&["Auto", "Clip"], (settings.hdr.mode == ToneMapMode::Clip) as usize),
        ),
        row(
            RowId::Exposure,
            "Exposure",
            Some(&format_exposure(settings.hdr.exposure_stops)),
            Control::Slider { value: settings.hdr.exposure_stops, min: -2.0, max: 2.0, step: 0.1 },
        ),
        row(RowId::HdrBadge, "Show HDR badge in the editor", None, Control::Toggle(settings.hdr.show_badge)),
    ];
    hdr_rows.extend(env.displays.iter().enumerate().map(|(i, display)| {
        let status = if display.hdr.is_some() { "HDR" } else { "SDR" };
        row(
            RowId::Display(i),
            &display.name,
            Some(&display_detail(&display.hdr)),
            Control::Status { text: status.to_string(), highlighted: display.hdr.is_some() },
        )
    }));
    let install = match &env.install {
        InstallState::NotInstalled => row(
            RowId::Install,
            "Install Glint",
            Some("Copies Glint to your programs folder and adds it to Start"),
            Control::Button("Install".into()),
        ),
        InstallState::Installed { .. } if confirming_uninstall => row(
            RowId::Install,
            "Uninstall Glint?",
            Some("Removes its Start shortcut, startup entry and registrations, then quits"),
            Control::Confirm { cancel: "Cancel".into(), confirm: "Uninstall".into() },
        ),
        InstallState::Installed { path } => row(RowId::Install, "Installed", Some(path), Control::Button("Uninstall…".into())),
    };
    vec![
        Section {
            title: "Shortcuts",
            rows: vec![
                row(
                    RowId::WinShiftS,
                    "Snip",
                    Some("Win + Shift + T extracts text instead"),
                    shortcut(&[WIN_KEY, "Shift", "S"], hotkeys.win_shift_s),
                ),
                row(RowId::PrintScreen, "Snip with Print Screen", None, shortcut(&["PrtSc"], hotkeys.print_screen)),
                row(
                    RowId::WinShiftR,
                    "Record the screen",
                    Some("Press again to stop"),
                    shortcut(&[WIN_KEY, "Shift", "R"], hotkeys.win_shift_r),
                ),
                row(RowId::AltPrintScreen, "Copy the active window", None, shortcut(&["Alt", "PrtSc"], hotkeys.alt_print_screen)),
            ],
            footer: Some(
                "While an app running as administrator is in front, Windows can't pass these keys to Glint, so the \
                 Snipping Tool answers instead.",
            ),
        },
        Section {
            title: "After capture",
            rows: vec![
                row(RowId::CopyToClipboard, "Copy to clipboard", None, Control::Toggle(after.copy_to_clipboard)),
                row(RowId::ShowThumbnail, "Show floating thumbnail", Some("Bottom-right corner; drag it into any app"), Control::Toggle(after.show_thumbnail)),
                row(RowId::OpenEditor, "Open the editor", Some("Instead of the thumbnail"), Control::Toggle(after.open_editor)),
                row(RowId::AutoSave, "Save automatically", None, Control::Toggle(after.auto_save)),
                row(RowId::SaveFolder, "Save to", Some(&env.screenshots_dir), Control::Button("Change…".into())),
                row(RowId::Format, "Format", None, segments(&["PNG", "JPEG"], (after.format == ImageFormat::Jpeg) as usize)),
                row(RowId::PlaySound, "Play shutter sound", None, Control::Toggle(after.play_sound)),
                row(RowId::Outline, "Snip outline", None, Control::Popup { options: outline_options, selected: outline_selected }),
            ],
            footer: None,
        },
        Section {
            title: "HDR",
            rows: hdr_rows,
            footer: Some("Captures from HDR displays are tone mapped to look exactly like your screen."),
        },
        Section {
            title: "Recording",
            rows: vec![
                row(RowId::RecordFps, "Frame rate", None, segments(&["30 fps", "60 fps"], (settings.record.fps >= 45) as usize)),
                row(RowId::SystemAudio, "Record system audio", None, Control::Toggle(settings.record.system_audio)),
                row(RowId::Microphone, "Record microphone", None, Control::Toggle(settings.record.microphone)),
                row(RowId::Cursor, "Show cursor", None, Control::Toggle(settings.record.include_cursor)),
                row(
                    RowId::RecordToneMap,
                    "HDR in recordings",
                    Some("Clip keeps SDR white exact; Auto compresses highlights"),
                    segments(&["Clip", "Auto"], (settings.record.tone_map == ToneMapMode::Auto) as usize),
                ),
                row(RowId::RecordFolder, "Save to", Some(&env.recordings_dir), Control::Button("Change…".into())),
            ],
            footer: None,
        },
        Section {
            title: "General",
            rows: vec![
                row(RowId::LaunchAtLogin, "Launch at login", None, Control::Toggle(settings.general.launch_at_login)),
                row(
                    RowId::Appearance,
                    "Appearance",
                    None,
                    segments(&["System", "Light", "Dark"], match settings.appearance.theme {
                        ThemeMode::System => 0,
                        ThemeMode::Light => 1,
                        ThemeMode::Dark => 2,
                    }),
                ),
                row(RowId::Accent, "Use Windows accent color", None, Control::Toggle(settings.appearance.system_accent)),
                row(
                    RowId::DefaultApp,
                    "Make Glint the default screen clipping app…",
                    Some("Choose Glint in Settings › Apps › Default apps"),
                    Control::Link,
                ),
                install,
                row(RowId::Version, "Version", None, Control::Value(env.version.clone())),
            ],
            footer: None,
        },
    ]
}

/// Applies an edit to `settings`; returns the app-side request it needs, if any.
pub fn apply_edit(settings: &mut Settings, env: &SettingsEnv, id: RowId, edit: Edit) -> Option<SettingsRequest> {
    let on = matches!(edit, Edit::Toggle(true));
    let choice = match edit {
        Edit::Choose(index) => index,
        _ => 0,
    };
    match (id, edit) {
        (RowId::WinShiftS, Edit::Toggle(_)) => settings.hotkeys.win_shift_s = on,
        (RowId::PrintScreen, Edit::Toggle(_)) => settings.hotkeys.print_screen = on,
        (RowId::WinShiftR, Edit::Toggle(_)) => settings.hotkeys.win_shift_r = on,
        (RowId::AltPrintScreen, Edit::Toggle(_)) => settings.hotkeys.alt_print_screen = on,
        (RowId::CopyToClipboard, Edit::Toggle(_)) => settings.after_capture.copy_to_clipboard = on,
        (RowId::ShowThumbnail, Edit::Toggle(_)) => settings.after_capture.show_thumbnail = on,
        (RowId::OpenEditor, Edit::Toggle(_)) => settings.after_capture.open_editor = on,
        (RowId::AutoSave, Edit::Toggle(_)) => settings.after_capture.auto_save = on,
        (RowId::PlaySound, Edit::Toggle(_)) => settings.after_capture.play_sound = on,
        (RowId::HdrBadge, Edit::Toggle(_)) => settings.hdr.show_badge = on,
        (RowId::SystemAudio, Edit::Toggle(_)) => settings.record.system_audio = on,
        (RowId::Microphone, Edit::Toggle(_)) => settings.record.microphone = on,
        (RowId::Cursor, Edit::Toggle(_)) => settings.record.include_cursor = on,
        (RowId::LaunchAtLogin, Edit::Toggle(_)) => settings.general.launch_at_login = on,
        (RowId::Accent, Edit::Toggle(_)) => settings.appearance.system_accent = on,
        (RowId::Format, Edit::Choose(_)) => {
            settings.after_capture.format = if choice == 1 { ImageFormat::Jpeg } else { ImageFormat::Png };
        }
        (RowId::Outline, Edit::Choose(_)) => {
            if let Some((_, outline)) = OUTLINES.get(choice) {
                settings.after_capture.outline = *outline;
            }
        }
        (RowId::ToneMap, Edit::Choose(_)) => {
            settings.hdr.mode = if choice == 1 { ToneMapMode::Clip } else { ToneMapMode::Auto };
        }
        (RowId::RecordFps, Edit::Choose(_)) => settings.record.fps = if choice == 1 { 60 } else { 30 },
        (RowId::RecordToneMap, Edit::Choose(_)) => {
            settings.record.tone_map = if choice == 1 { ToneMapMode::Auto } else { ToneMapMode::Clip };
        }
        (RowId::Appearance, Edit::Choose(_)) => {
            settings.appearance.theme = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark][choice.min(2)];
        }
        (RowId::Exposure, Edit::Slide(stops)) => settings.hdr.exposure_stops = ((stops * 10.0).round() / 10.0).clamp(-2.0, 2.0),
        (RowId::SaveFolder, Edit::Press) => return Some(SettingsRequest::PickFolder(FolderKind::Screenshots)),
        (RowId::RecordFolder, Edit::Press) => return Some(SettingsRequest::PickFolder(FolderKind::Recordings)),
        (RowId::DefaultApp, Edit::Press) => return Some(SettingsRequest::OpenDefaultApps),
        (RowId::Install, Edit::Press) => {
            return Some(match env.install {
                InstallState::NotInstalled => SettingsRequest::Install,
                InstallState::Installed { .. } => SettingsRequest::Uninstall,
            });
        }
        _ => {}
    }
    None
}

/// Side effects of going from `old` to `new` settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsEffects {
    pub hotkeys: bool,
    pub appearance: bool,
    pub launch_at_login: Option<bool>,
}

pub fn effects(old: &Settings, new: &Settings) -> SettingsEffects {
    SettingsEffects {
        hotkeys: old.hotkeys != new.hotkeys,
        appearance: old.appearance != new.appearance,
        launch_at_login: (old.general.launch_at_login != new.general.launch_at_login).then_some(new.general.launch_at_login),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(install: InstallState) -> SettingsEnv {
        SettingsEnv {
            displays: vec![
                DisplayLine {
                    name: "DELL U2723QE".into(),
                    hdr: Some(HdrInfo { sdr_white_nits: 280.0, max_nits: 600.0, max_full_frame_nits: 400.0, min_nits: 0.1 }),
                },
                DisplayLine { name: "LG 27GL850".into(), hdr: None },
            ],
            screenshots_dir: r"C:\Users\You\Pictures\Screenshots".into(),
            recordings_dir: r"C:\Users\You\Videos\Screen Recordings".into(),
            install,
            version: "0.1.0".into(),
        }
    }

    fn find(sections: &[Section], id: RowId) -> &Row {
        sections.iter().flat_map(|s| &s.rows).find(|r| r.id == id).expect("row exists")
    }

    #[test]
    fn sections_follow_the_spec_order_and_reflect_settings() {
        let settings = Settings::default();
        let sections = sections(&settings, &env(InstallState::NotInstalled), false);
        let titles: Vec<_> = sections.iter().map(|s| s.title).collect();
        assert_eq!(titles, ["Shortcuts", "After capture", "HDR", "Recording", "General"]);
        assert_eq!(
            find(&sections, RowId::WinShiftS).control,
            Control::Shortcut { keys: vec![WIN_KEY.into(), "Shift".into(), "S".into()], on: true }
        );
        assert_eq!(find(&sections, RowId::OpenEditor).control, Control::Toggle(false));
        assert_eq!(find(&sections, RowId::Exposure).detail.as_deref(), Some("0 EV"));
        assert_eq!(
            find(&sections, RowId::RecordToneMap).control,
            Control::Segments { options: vec!["Clip".into(), "Auto".into()], selected: 0 }
        );
        let hdr = find(&sections, RowId::Display(0));
        assert_eq!(hdr.control, Control::Status { text: "HDR".into(), highlighted: true });
        assert!(hdr.detail.as_deref().unwrap().contains("280 nits"));
        assert_eq!(find(&sections, RowId::Display(1)).control, Control::Status { text: "SDR".into(), highlighted: false });
        assert_eq!(find(&sections, RowId::Install).control, Control::Button("Install".into()));
        assert_eq!(find(&sections, RowId::Version).control, Control::Value("0.1.0".into()));
    }

    #[test]
    fn custom_outlines_show_up_as_an_extra_choice() {
        let mut settings = Settings::default();
        settings.after_capture.outline = Some(Outline { color: [1, 2, 3, 255], width_px: 3 });
        let sections = sections(&settings, &env(InstallState::NotInstalled), false);
        match &find(&sections, RowId::Outline).control {
            Control::Popup { options, selected } => {
                assert_eq!(options.last().map(String::as_str), Some(CUSTOM_OUTLINE));
                assert_eq!(*selected, OUTLINES.len());
            }
            other => panic!("unexpected control {other:?}"),
        }
    }

    #[test]
    fn edits_change_settings_and_requests_go_to_the_app() {
        let installed = env(InstallState::Installed { path: r"C:\x\glint.exe".into() });
        let mut settings = Settings::default();
        assert_eq!(apply_edit(&mut settings, &installed, RowId::PrintScreen, Edit::Toggle(false)), None);
        assert!(!settings.hotkeys.print_screen);
        apply_edit(&mut settings, &installed, RowId::Format, Edit::Choose(1));
        assert_eq!(settings.after_capture.format, ImageFormat::Jpeg);
        apply_edit(&mut settings, &installed, RowId::Outline, Edit::Choose(3));
        assert_eq!(settings.after_capture.outline, OUTLINES[3].1);
        apply_edit(&mut settings, &installed, RowId::Exposure, Edit::Slide(0.53));
        assert!((settings.hdr.exposure_stops - 0.5).abs() < 1e-6);
        apply_edit(&mut settings, &installed, RowId::RecordFps, Edit::Choose(1));
        assert_eq!(settings.record.fps, 60);
        apply_edit(&mut settings, &installed, RowId::Appearance, Edit::Choose(2));
        assert_eq!(settings.appearance.theme, ThemeMode::Dark);
        assert_eq!(
            apply_edit(&mut settings, &installed, RowId::SaveFolder, Edit::Press),
            Some(SettingsRequest::PickFolder(FolderKind::Screenshots))
        );
        assert_eq!(apply_edit(&mut settings, &installed, RowId::Install, Edit::Press), Some(SettingsRequest::Uninstall));
        assert_eq!(apply_edit(&mut settings, &installed, RowId::Install, Edit::Cancel), None);
        let confirming = sections(&settings, &installed, true);
        assert_eq!(
            find(&confirming, RowId::Install).control,
            Control::Confirm { cancel: "Cancel".into(), confirm: "Uninstall".into() }
        );
        assert_eq!(find(&sections(&settings, &installed, false), RowId::Install).control, Control::Button("Uninstall…".into()));
        assert_eq!(
            apply_edit(&mut settings, &env(InstallState::NotInstalled), RowId::Install, Edit::Press),
            Some(SettingsRequest::Install)
        );
    }

    #[test]
    fn effects_name_only_what_changed() {
        let old = Settings::default();
        let mut new = old.clone();
        assert_eq!(effects(&old, &new), SettingsEffects::default());
        new.hotkeys.win_shift_r = false;
        new.general.launch_at_login = false;
        assert_eq!(effects(&old, &new), SettingsEffects { hotkeys: true, appearance: false, launch_at_login: Some(false) });
        let mut themed = old.clone();
        themed.appearance.system_accent = true;
        assert!(effects(&old, &themed).appearance);
        assert_eq!(format_exposure(-1.25), "-1.3 EV");
        assert_eq!(format_exposure(0.04), "0 EV");
    }
}
