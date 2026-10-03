//! Glint's app: process lifecycle and CLI, the resident instance (tray, hotkeys, capture flows, after-capture
//! pipeline, floating thumbnail, toasts, settings window, recording HUD), icons, previews and the self-test.

pub mod art;
pub mod cli;
mod host;
pub mod hud;
pub mod ico;
pub mod pipeline;
pub mod popup;
pub mod preview;
mod resident;
mod selftest;
pub mod settings_model;
pub mod settings_view;
mod system;
pub mod thumbnail;
pub mod toast;
mod tray;

use std::path::Path;

use anyhow::{Context, Result};
use glint_core::ImageFormat;
use glint_sys::instance::{InstanceRole, acquire_single_instance, forward_to_primary};
use glint_sys::{install, logging, paths, settings_store};

use crate::cli::{Command, Greeting, INSTALLED_FLAG, PreviewArgs};
use crate::resident::Exit;
use crate::system::{OleGuard, attach_parent_console};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runs `glint` with the process arguments; returns the exit code.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    init_logging();
    let parsed = cli::parse(&args);
    for ignored in &parsed.ignored {
        log::info!("ignoring argument {ignored:?}");
    }
    match parsed.command {
        Command::Version => {
            attach_parent_console();
            println!("Glint {VERSION}");
            0
        }
        Command::Selftest { json } => {
            attach_parent_console();
            selftest::run(json)
        }
        Command::Preview(preview) => {
            attach_parent_console();
            match render_preview(&preview) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("preview {}: {error:#}", preview.kind);
                    log::error!("preview {}: {error:#}", preview.kind);
                    1
                }
            }
        }
        Command::Install => install_and_start(),
        command => resident(command, &args),
    }
}

fn init_logging() {
    let initialized = paths::local_data_dir().and_then(|dir| logging::init_logging(&dir.join("glint.log")));
    if let Err(error) = initialized {
        eprintln!("logging unavailable: {error:#}");
    }
    log::info!("Glint {VERSION} started: {:?}", std::env::args().collect::<Vec<_>>());
}

fn render_preview(args: &PreviewArgs) -> Result<()> {
    anyhow::ensure!(!args.kind.is_empty(), "missing preview kind");
    let gfx = glint_ui::Gfx::new()?;
    let image = preview::render(&gfx, args)?;
    if let Some(dir) = args.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    glint_core::encode::save(&image, &args.out, ImageFormat::Png)?;
    println!("wrote {} ({}×{})", args.out.display(), image.width, image.height);
    Ok(())
}

pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn spawn_background(exe: &Path, greeting_flag: &str) -> Result<()> {
    std::process::Command::new(exe)
        .args(["--background", greeting_flag])
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    Ok(())
}

/// `--install`: copy and register per user, then start the installed copy (or become it).
fn install_and_start() -> i32 {
    attach_parent_console();
    let installed = {
        let _ole = OleGuard::new();
        std::env::current_exe()
            .context("locating glint.exe")
            .and_then(|exe| install::install(&exe, &settings_store::load_settings()).map(|target| (exe, target)))
    };
    match installed {
        Ok((exe, target)) if same_file(&exe, &target) => {
            println!("Glint is installed at {}", target.display());
            resident(Command::Start(Greeting::Installed), &[INSTALLED_FLAG.to_string()])
        }
        Ok((_, target)) => {
            println!("Glint is installed at {}", target.display());
            match spawn_background(&target, INSTALLED_FLAG) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("{error:#}");
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("Install failed: {error:#}");
            log::error!("install failed: {error:#}");
            1
        }
    }
}

/// Resident commands: forwarded to a running instance, or this process becomes it.
fn resident(command: Command, args: &[String]) -> i32 {
    let guard = match acquire_single_instance() {
        InstanceRole::Primary(guard) => guard,
        InstanceRole::Secondary => {
            let args = cli::forwarded_args(&command, args);
            return match forward_to_primary(&args) {
                Ok(true) => 0,
                Ok(false) => {
                    log::error!("another Glint is running but did not accept {args:?}");
                    1
                }
                Err(error) => {
                    log::error!("forwarding {args:?}: {error:#}");
                    1
                }
            };
        }
    };
    if command == Command::Quit {
        log::info!("--quit: no Glint is running");
        return 0;
    }
    if command == Command::Uninstall {
        let _ole = OleGuard::new();
        return match install::uninstall() {
            Ok(()) => 0,
            Err(error) => {
                log::error!("uninstall failed: {error:#}");
                1
            }
        };
    }
    match resident::run(command) {
        Ok(Exit::Done) => 0,
        Ok(Exit::Relaunch(target)) => {
            drop(guard);
            match spawn_background(&target, INSTALLED_FLAG) {
                Ok(()) => 0,
                Err(error) => {
                    log::error!("{error:#}");
                    1
                }
            }
        }
        Err(error) => {
            log::error!("Glint stopped: {error:#}");
            1
        }
    }
}
