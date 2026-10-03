//! Command line (DESIGN §9): `glint`, `--background`, `--snip [kind]`, `--record`, `--settings`, `--install`,
//! `--uninstall`, `--edit <image>`, `--quit`, `--selftest [--json]`,
//! `--preview <kind> --out <png> [--theme] [--scale] [--live]`, `ms-screenclip:...`, `--version`. Unknown arguments are collected so the caller can log and ignore them.

use std::path::PathBuf;

use glint_core::{CaptureMode, ThemeMode};

/// What a resident start says to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Greeting {
    /// `--background`: start silently.
    Silent,
    /// Plain `glint`: "Glint is running".
    Running,
    /// Started by `--install`: "Glint is installed".
    Installed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewArgs {
    pub kind: String,
    pub out: PathBuf,
    pub theme: ThemeMode,
    pub scale: f32,
    pub live: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Start(Greeting),
    /// None = the last used mode.
    Snip(Option<CaptureMode>),
    Record,
    Settings,
    /// Open an image file in the editor.
    Edit(PathBuf),
    /// Ask the running instance to quit.
    Quit,
    Install,
    Uninstall,
    Selftest { json: bool },
    Preview(PreviewArgs),
    Version,
}

impl Command {
    /// Commands that run in the resident instance (forwarded to it when one is running).
    pub fn is_resident(&self) -> bool {
        matches!(
            self,
            Command::Start(_)
                | Command::Snip(_)
                | Command::Record
                | Command::Settings
                | Command::Edit(_)
                | Command::Quit
                | Command::Uninstall
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    pub command: Command,
    /// Arguments that were not understood; logged and ignored.
    pub ignored: Vec<String>,
}

/// Internal flag added by `--install` when it starts the installed copy.
pub const INSTALLED_FLAG: &str = "--installed";

pub fn snip_kind(word: &str) -> Option<CaptureMode> {
    match word.to_ascii_lowercase().as_str() {
        "rect" | "rectangle" => Some(CaptureMode::Rectangle),
        "window" => Some(CaptureMode::Window),
        "full" | "fullscreen" | "full-screen" => Some(CaptureMode::FullScreen),
        "free" | "freeform" => Some(CaptureMode::Freeform),
        "text" => Some(CaptureMode::Text),
        "color" | "colour" => Some(CaptureMode::ColorPicker),
        _ => None,
    }
}

/// `ms-screenclip:?clippingMode=Window` and friends; anything unrecognised means "last mode".
pub fn screenclip_mode(uri: &str) -> Option<CaptureMode> {
    let lower = uri.to_ascii_lowercase();
    let value = lower.split(['?', '&']).find_map(|pair| pair.strip_prefix("clippingmode="));
    match value {
        Some("rectangle") => Some(CaptureMode::Rectangle),
        Some("window") => Some(CaptureMode::Window),
        Some("fullscreen") => Some(CaptureMode::FullScreen),
        Some("freeform") => Some(CaptureMode::Freeform),
        _ => None,
    }
}

fn parse_theme(word: &str) -> Option<ThemeMode> {
    match word.to_ascii_lowercase().as_str() {
        "dark" => Some(ThemeMode::Dark),
        "light" => Some(ThemeMode::Light),
        _ => None,
    }
}

fn parse_scale(word: &str) -> Option<f32> {
    word.parse::<f32>().ok().filter(|s| s.is_finite() && (0.5..=4.0).contains(s))
}

pub fn parse(args: &[String]) -> Cli {
    let mut command: Option<Command> = None;
    let mut ignored = Vec::new();
    let mut background = false;
    let mut installed = false;
    let mut json = false;
    let mut preview_kind: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut theme = ThemeMode::Dark;
    let mut scale = 1.0;
    let mut live = false;
    let mut iter = args.iter().peekable();
    let set = |command: &mut Option<Command>, ignored: &mut Vec<String>, arg: &str, value: Command| {
        if command.is_none() {
            *command = Some(value);
        } else {
            ignored.push(arg.to_string());
        }
    };
    while let Some(arg) = iter.next() {
        let lower = arg.to_ascii_lowercase();
        match lower.as_str() {
            "--background" => background = true,
            INSTALLED_FLAG => installed = true,
            "--snip" => {
                let mode = iter.peek().and_then(|next| snip_kind(next));
                if mode.is_some() {
                    iter.next();
                }
                set(&mut command, &mut ignored, arg, Command::Snip(mode));
            }
            "--record" => set(&mut command, &mut ignored, arg, Command::Record),
            "--settings" => set(&mut command, &mut ignored, arg, Command::Settings),
            "--edit" => match iter.next_if(|next| !next.starts_with("--")) {
                Some(path) => set(&mut command, &mut ignored, arg, Command::Edit(PathBuf::from(path))),
                None => ignored.push(arg.clone()),
            },
            "--quit" | "--exit" => set(&mut command, &mut ignored, arg, Command::Quit),
            "--install" => set(&mut command, &mut ignored, arg, Command::Install),
            "--uninstall" => set(&mut command, &mut ignored, arg, Command::Uninstall),
            "--version" | "-v" | "-V" => set(&mut command, &mut ignored, arg, Command::Version),
            "--selftest" => set(&mut command, &mut ignored, arg, Command::Selftest { json: false }),
            "--json" => json = true,
            "--live" => live = true,
            "--preview" => {
                match iter.peek().filter(|next| !next.starts_with("--")) {
                    Some(kind) => {
                        preview_kind = Some(kind.to_string());
                        iter.next();
                    }
                    None => preview_kind = Some(String::new()),
                }
                set(&mut command, &mut ignored, arg, Command::Preview(placeholder_preview()));
            }
            "--out" => match iter.next() {
                Some(path) => out = Some(PathBuf::from(path)),
                None => ignored.push(arg.clone()),
            },
            "--theme" => match iter.next().and_then(|w| parse_theme(w)) {
                Some(t) => theme = t,
                None => ignored.push(arg.clone()),
            },
            "--scale" => match iter.next().and_then(|w| parse_scale(w)) {
                Some(s) => scale = s,
                None => ignored.push(arg.clone()),
            },
            _ if lower.starts_with("ms-screenclip:") => {
                set(&mut command, &mut ignored, arg, Command::Snip(screenclip_mode(arg)));
            }
            _ => ignored.push(arg.clone()),
        }
    }
    let command = match command {
        Some(Command::Selftest { .. }) => Command::Selftest { json },
        Some(Command::Preview(_)) => {
            let kind = preview_kind.unwrap_or_default();
            let out = out.unwrap_or_else(|| PathBuf::from(format!("{kind}.png")));
            Command::Preview(PreviewArgs { kind, out, theme, scale, live })
        }
        Some(other) => other,
        None if installed => Command::Start(Greeting::Installed),
        None if background => Command::Start(Greeting::Silent),
        None => Command::Start(Greeting::Running),
    };
    Cli { command, ignored }
}

/// The arguments to send to a running instance: `args` as given, except that an `--edit` path is made absolute.
pub fn forwarded_args(command: &Command, args: &[String]) -> Vec<String> {
    match command {
        Command::Edit(path) => {
            let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            vec!["--edit".to_string(), absolute.display().to_string()]
        }
        _ => args.to_vec(),
    }
}

fn placeholder_preview() -> PreviewArgs {
    PreviewArgs { kind: String::new(), out: PathBuf::new(), theme: ThemeMode::Dark, scale: 1.0, live: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn start_variants() {
        assert_eq!(cli(&[]).command, Command::Start(Greeting::Running));
        assert_eq!(cli(&["--background"]).command, Command::Start(Greeting::Silent));
        assert_eq!(cli(&["--background", "--installed"]).command, Command::Start(Greeting::Installed));
    }

    #[test]
    fn snip_kinds_and_screenclip() {
        assert_eq!(cli(&["--snip"]).command, Command::Snip(None));
        assert_eq!(cli(&["--snip", "window"]).command, Command::Snip(Some(CaptureMode::Window)));
        assert_eq!(cli(&["--snip", "free"]).command, Command::Snip(Some(CaptureMode::Freeform)));
        assert_eq!(cli(&["--snip", "text"]).command, Command::Snip(Some(CaptureMode::Text)));
        assert_eq!(cli(&["--snip", "color"]).command, Command::Snip(Some(CaptureMode::ColorPicker)));
        assert_eq!(cli(&["ms-screenclip:"]).command, Command::Snip(None));
        assert_eq!(
            cli(&["ms-screenclip:?source=Snip&clippingMode=Window"]).command,
            Command::Snip(Some(CaptureMode::Window))
        );
        let unknown_kind = cli(&["--snip", "hexagon"]);
        assert_eq!(unknown_kind.command, Command::Snip(None));
        assert_eq!(unknown_kind.ignored, vec!["hexagon".to_string()]);
    }

    #[test]
    fn preview_with_options() {
        let parsed = cli(&["--preview", "thumbnail", "--out", "x.png", "--theme", "light", "--scale", "1.5", "--live"]);
        assert_eq!(
            parsed.command,
            Command::Preview(PreviewArgs {
                kind: "thumbnail".into(),
                out: PathBuf::from("x.png"),
                theme: ThemeMode::Light,
                scale: 1.5,
                live: true,
            })
        );
        assert!(parsed.ignored.is_empty());
        let defaults = cli(&["--out", "y.png", "--preview", "hud"]);
        assert_eq!(
            defaults.command,
            Command::Preview(PreviewArgs { kind: "hud".into(), out: "y.png".into(), theme: ThemeMode::Dark, scale: 1.0, live: false })
        );
        assert_eq!(cli(&["--preview", "toast", "--scale", "9"]).ignored, vec!["--scale".to_string()]);
    }

    #[test]
    fn edit_and_quit() {
        assert_eq!(cli(&["--edit", "C:/shots/a b.png"]).command, Command::Edit(PathBuf::from("C:/shots/a b.png")));
        let missing = cli(&["--edit", "--background"]);
        assert_eq!(missing.command, Command::Start(Greeting::Silent));
        assert_eq!(missing.ignored, vec!["--edit".to_string()]);
        assert_eq!(cli(&["--quit"]).command, Command::Quit);
        assert!(Command::Quit.is_resident() && Command::Edit(PathBuf::new()).is_resident());
    }

    /// The path a secondary instance forwards must not depend on its working directory.
    #[test]
    fn forwarded_edit_paths_are_absolute() {
        let args = forwarded_args(&Command::Edit(PathBuf::from("shot.png")), &["--edit".into(), "shot.png".into()]);
        assert_eq!(args[0], "--edit");
        assert!(std::path::Path::new(&args[1]).is_absolute());
        let other = vec!["--snip".to_string(), "window".to_string()];
        assert_eq!(forwarded_args(&Command::Snip(Some(CaptureMode::Window)), &other), other);
    }

    #[test]
    fn selftest_json_and_one_command_wins() {
        assert_eq!(cli(&["--selftest", "--json"]).command, Command::Selftest { json: true });
        assert_eq!(cli(&["--json", "--selftest"]).command, Command::Selftest { json: true });
        let parsed = cli(&["--record", "--settings", "--frobnicate"]);
        assert_eq!(parsed.command, Command::Record);
        assert_eq!(parsed.ignored, vec!["--settings".to_string(), "--frobnicate".to_string()]);
        assert_eq!(cli(&["--version"]).command, Command::Version);
        assert!(Command::Uninstall.is_resident() && !Command::Install.is_resident());
    }
}
