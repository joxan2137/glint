//! Input and lifecycle events delivered to views. Positions are DIPs relative to the window's client area.

use glint_core::{PointF, PointI};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers { shift: false, ctrl: false, alt: false, win: false };
    pub const CTRL: Modifiers = Modifiers { shift: false, ctrl: true, alt: false, win: false };
    pub const SHIFT: Modifiers = Modifiers { shift: true, ctrl: false, alt: false, win: false };
    pub const CTRL_SHIFT: Modifiers = Modifiers { shift: true, ctrl: true, alt: false, win: false };

    pub fn is_empty(&self) -> bool {
        *self == Self::NONE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerKind {
    Mouse,
    Pen,
    Touch,
    Touchpad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// Buttons currently held. For pens, `left` is tip contact, `right` the barrel button, `eraser` the eraser end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Buttons {
    pub left: bool,
    pub right: bool,
    pub middle: bool,
    pub x1: bool,
    pub x2: bool,
    pub eraser: bool,
}

impl Buttons {
    pub fn any(&self) -> bool {
        self.left || self.right || self.middle || self.x1 || self.x2
    }
}

/// One coalesced intermediate pointer sample (high-frequency pen/mouse input between two events).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerSample {
    pub pos: PointF,
    pub pressure: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointerEvent {
    /// DIP position in the client area.
    pub pos: PointF,
    /// Physical-pixel position on the virtual desktop.
    pub screen_px: PointI,
    pub kind: PointerKind,
    pub id: u32,
    /// The button that went down or up (None for moves).
    pub button: Option<MouseButton>,
    pub buttons: Buttons,
    /// 0..=1; 1.0 for mouse, 0.5 for pens without pressure data.
    pub pressure: f32,
    pub mods: Modifiers,
    /// 1 for a single click, 2 for a double click, … (down events only).
    pub click_count: u32,
    /// Coalesced samples since the previous event, oldest first, not including `pos`.
    pub history: Vec<PointerSample>,
}

impl PointerEvent {
    pub fn is_primary_button(&self) -> bool {
        self.button == Some(MouseButton::Left)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WheelEvent {
    pub pos: PointF,
    /// Scroll amount in notches (1.0 = one wheel detent = 120 units). Positive y = away from the user (scroll up),
    /// positive x = right. Precision touchpads deliver fractional values.
    pub delta: PointF,
    /// True when the delta is not a whole number of notches (precision touchpad or free-spinning wheel).
    pub precise: bool,
    pub mods: Modifiers,
}

/// Virtual keys. Letters and digits arrive as `Char('A'..='Z' | '0'..='9')` regardless of layout shift state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Escape,
    Enter,
    Tab,
    Space,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Shift,
    Control,
    Alt,
    Win,
    PrintScreen,
    F(u8),
    Char(char),
    /// `+`/`=` key and numpad plus.
    Plus,
    /// `-` key and numpad minus.
    Minus,
    Other(u16),
}

impl Key {
    pub fn from_vk(vk: u16) -> Key {
        match vk {
            0x1B => Key::Escape,
            0x0D => Key::Enter,
            0x09 => Key::Tab,
            0x20 => Key::Space,
            0x08 => Key::Backspace,
            0x2E => Key::Delete,
            0x2D => Key::Insert,
            0x25 => Key::Left,
            0x27 => Key::Right,
            0x26 => Key::Up,
            0x28 => Key::Down,
            0x24 => Key::Home,
            0x23 => Key::End,
            0x21 => Key::PageUp,
            0x22 => Key::PageDown,
            0x10 | 0xA0 | 0xA1 => Key::Shift,
            0x11 | 0xA2 | 0xA3 => Key::Control,
            0x12 | 0xA4 | 0xA5 => Key::Alt,
            0x5B | 0x5C => Key::Win,
            0x2C => Key::PrintScreen,
            0x70..=0x87 => Key::F((vk - 0x70 + 1) as u8),
            0x30..=0x39 | 0x41..=0x5A => Key::Char(vk as u8 as char),
            0x60..=0x69 => Key::Char((b'0' + (vk - 0x60) as u8) as char),
            0xBB | 0x6B => Key::Plus,
            0xBD | 0x6D => Key::Minus,
            other => Key::Other(other),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    pub key: Key,
    /// Raw virtual-key code.
    pub vk: u16,
    pub mods: Modifiers,
    pub repeat: bool,
}

impl KeyEvent {
    /// True for `key` with exactly `mods` held.
    pub fn is(&self, key: Key, mods: Modifiers) -> bool {
        self.key == key && self.mods == mods
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    PointerDown(PointerEvent),
    PointerMove(PointerEvent),
    PointerUp(PointerEvent),
    /// The pointer left the window (hover ends).
    PointerLeave,
    /// Capture was lost mid-gesture (e.g. another window took it); abandon any drag.
    PointerCancel,
    Wheel(WheelEvent),
    KeyDown(KeyEvent),
    KeyUp(KeyEvent),
    /// Typed text, including committed IME compositions.
    Text(String),
    Focus(bool),
    /// Client size changed; read `cx.size()`.
    Resized,
    /// DPI changed; read `cx.scale()`. Followed by `Resized`.
    ScaleChanged,
    /// System or app theme changed; read `cx.theme()`.
    ThemeChanged,
    /// A timer set with `Ctx::set_timer` fired with its token.
    Timer(u64),
    /// The user asked to close (Alt+F4, caption ✕, taskbar). Call `cx.close()` to accept.
    CloseRequested,
    /// The window is about to be destroyed; the view is dropped right after.
    Closed,
    /// The first frame is on screen and the window is visible.
    Shown,
}

impl Event {
    pub fn pointer(&self) -> Option<&PointerEvent> {
        match self {
            Event::PointerDown(e) | Event::PointerMove(e) | Event::PointerUp(e) => Some(e),
            _ => None,
        }
    }

    pub fn pointer_pos(&self) -> Option<PointF> {
        match self {
            Event::Wheel(w) => Some(w.pos),
            other => other.pointer().map(|e| e.pos),
        }
    }
}

/// What a point in a custom-title-bar window means to the system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HitArea {
    #[default]
    Client,
    /// Drags the window, double-click maximizes, right-click opens the system menu.
    Caption,
}
