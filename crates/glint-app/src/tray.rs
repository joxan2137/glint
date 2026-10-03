//! Notification-area icon: a monochrome glyph rendered for the taskbar theme and DPI, its context menu, and the
//! commands that menu maps to.

use std::rc::Rc;

use anyhow::Result;
use glint_core::CaptureMode;
use glint_sys::tray::{MenuItem, Tray};
use glint_ui::{Color, Gfx};

use crate::art::render_tray_glyph;
use crate::host::TRAY_CALLBACK;
use crate::system::{OwnedIcon, hwnd, small_icon_size, taskbar_is_light};

pub const TOOLTIP: &str = "Glint — Win+Shift+S to snip";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Snip(CaptureMode),
    Record,
    StopRecording,
    Text,
    OpenImage,
    OpenScreenshots,
    Settings,
    Quit,
}

const COMMANDS: [(u32, TrayCommand); 11] = [
    (10, TrayCommand::Snip(CaptureMode::Rectangle)),
    (11, TrayCommand::Snip(CaptureMode::Window)),
    (12, TrayCommand::Snip(CaptureMode::FullScreen)),
    (13, TrayCommand::Snip(CaptureMode::Freeform)),
    (20, TrayCommand::Record),
    (21, TrayCommand::StopRecording),
    (30, TrayCommand::Text),
    (35, TrayCommand::OpenImage),
    (40, TrayCommand::OpenScreenshots),
    (50, TrayCommand::Settings),
    (60, TrayCommand::Quit),
];

fn id_of(command: TrayCommand) -> u32 {
    COMMANDS.iter().find(|(_, c)| *c == command).map(|(id, _)| *id).expect("every command has an id")
}

pub fn command_for(id: u32) -> Option<TrayCommand> {
    COMMANDS.iter().find(|(i, _)| *i == id).map(|(_, c)| *c)
}

/// The context menu; while recording, "Record screen" becomes "Stop recording".
pub fn menu_items(recording: bool) -> Vec<MenuItem> {
    let item = |command: TrayCommand, label: &str| MenuItem::new(id_of(command), label);
    let record = if recording {
        item(TrayCommand::StopRecording, "Stop recording\tWin+Shift+R")
    } else {
        item(TrayCommand::Record, "Record screen\tWin+Shift+R")
    };
    vec![
        MenuItem::submenu(
            "New snip",
            vec![
                item(TrayCommand::Snip(CaptureMode::Rectangle), "Rectangle\tWin+Shift+S"),
                item(TrayCommand::Snip(CaptureMode::Window), "Window"),
                item(TrayCommand::Snip(CaptureMode::FullScreen), "Full screen"),
                item(TrayCommand::Snip(CaptureMode::Freeform), "Freeform"),
            ],
        ),
        record,
        item(TrayCommand::Text, "Extract text\tWin+Shift+T"),
        MenuItem::separator(),
        item(TrayCommand::OpenImage, "Open image…"),
        item(TrayCommand::OpenScreenshots, "Open screenshots folder"),
        item(TrayCommand::Settings, "Settings…"),
        MenuItem::separator(),
        item(TrayCommand::Quit, "Quit Glint"),
    ]
}

/// The tray icon and the HICON it shows (kept alive as long as the tray uses it).
pub struct TrayIcon {
    owner: isize,
    tray: Option<Tray>,
    icon: Option<OwnedIcon>,
}

fn render_icon(gfx: &Rc<Gfx>, dpi: u32) -> Result<OwnedIcon> {
    let ink = if taskbar_is_light() { Color::BLACK } else { Color::WHITE };
    OwnedIcon::from_image(&render_tray_glyph(gfx, small_icon_size(dpi), ink)?)
}

impl TrayIcon {
    pub fn new(owner: isize) -> Self {
        Self { owner, tray: None, icon: None }
    }

    /// Adds the icon, or re-adds it with the current glyph after Explorer restarted.
    pub fn show(&mut self, gfx: &Rc<Gfx>, dpi: u32) -> Result<()> {
        if self.icon.is_none() {
            self.icon = Some(render_icon(gfx, dpi)?);
        }
        let icon = self.icon.as_ref().expect("icon rendered").handle();
        match &mut self.tray {
            Some(tray) => {
                tray.recreate()?;
                tray.set_icon(icon)
            }
            None => {
                self.tray = Some(Tray::add(hwnd(self.owner), TRAY_CALLBACK, icon, TOOLTIP)?);
                Ok(())
            }
        }
    }

    pub fn is_shown(&self) -> bool {
        self.tray.is_some()
    }

    /// Re-renders the glyph for the current taskbar theme and DPI. The new icon becomes the owned, current one
    /// before the shell is told, so the handle the tray remembers stays alive even when the update fails; the old
    /// icon is destroyed only afterwards (the shell keeps its own copy of whatever it displays).
    pub fn refresh(&mut self, gfx: &Rc<Gfx>, dpi: u32) -> Result<()> {
        let icon = render_icon(gfx, dpi)?;
        let handle = icon.handle();
        let previous = self.icon.replace(icon);
        let updated = match &mut self.tray {
            Some(tray) => tray.set_icon(handle),
            None => Ok(()),
        };
        drop(previous);
        updated
    }

    pub fn remove(&mut self) {
        self.tray = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    #[test]
    fn menu_matches_the_spec() {
        let items = menu_items(false);
        assert_eq!(
            labels(&items),
            [
                "New snip",
                "Record screen\tWin+Shift+R",
                "Extract text\tWin+Shift+T",
                "",
                "Open image…",
                "Open screenshots folder",
                "Settings…",
                "",
                "Quit Glint"
            ]
        );
        assert_eq!(labels(&items[0].submenu), ["Rectangle\tWin+Shift+S", "Window", "Full screen", "Freeform"]);
        assert_eq!(menu_items(true)[1].label, "Stop recording\tWin+Shift+R");
    }

    #[test]
    fn every_menu_id_maps_back_to_its_command() {
        let items = menu_items(false);
        let ids: Vec<u32> =
            items.iter().flat_map(|i| std::iter::once(i).chain(&i.submenu)).filter(|i| i.id != 0).map(|i| i.id).collect();
        assert_eq!(ids.len(), 10);
        for id in ids {
            assert!(command_for(id).is_some(), "id {id}");
        }
        assert_eq!(command_for(11), Some(TrayCommand::Snip(CaptureMode::Window)));
        assert_eq!(command_for(0), None);
    }
}
