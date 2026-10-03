use glint_core::{PointF, RectF, SizeF};

use super::{Presence, Response};
use crate::event::{Event, Key, MouseButton};
use crate::gfx::Gfx;
use crate::icons::Icon;
use crate::painter::{Backdrop, Painter};
use crate::text::{TextAlign, TextStyle};
use crate::view::Ctx;

const ITEM_HEIGHT: f32 = 28.0;
const SEPARATOR_HEIGHT: f32 = 8.0;
const PADDING: f32 = 6.0;
const RADIUS: f32 = 12.0;
const ROW_RADIUS: f32 = 6.0;
const LEAD: f32 = 26.0;
const ICON_COLUMN: f32 = 24.0;
const TRAIL: f32 = 12.0;
const MIN_WIDTH: f32 = 180.0;
const GAP: f32 = 4.0;
const MARGIN: f32 = 8.0;

#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub icon: Option<Icon>,
    pub checked: bool,
    pub shortcut: Option<String>,
    pub enabled: bool,
    pub separator: bool,
}

impl MenuItem {
    pub fn new(label: &str) -> Self {
        Self { label: label.to_string(), icon: None, checked: false, shortcut: None, enabled: true, separator: false }
    }

    pub fn separator() -> Self {
        Self { separator: true, enabled: false, ..Self::new("") }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn shortcut(mut self, shortcut: &str) -> Self {
        self.shortcut = Some(shortcut.to_string());
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    fn selectable(&self) -> bool {
        self.enabled && !self.separator
    }
}

/// In-window glass dropdown with checkmarks, icons, shortcut hints, separators and keyboard navigation.
/// While open it consumes all pointer and key events; a click outside closes it.
#[derive(Clone, Debug)]
pub struct Menu {
    items: Vec<MenuItem>,
    frame: RectF,
    rows: Vec<RectF>,
    highlighted: Option<usize>,
    presence: Presence,
    origin: PointF,
}

fn label_style() -> TextStyle {
    TextStyle::body()
}

impl Menu {
    pub fn new(items: Vec<MenuItem>) -> Self {
        Self {
            items,
            frame: RectF::default(),
            rows: Vec::new(),
            highlighted: None,
            presence: Presence::new(false),
            origin: PointF::default(),
        }
    }

    pub fn items(&self) -> &[MenuItem] {
        &self.items
    }

    pub fn items_mut(&mut self) -> &mut Vec<MenuItem> {
        &mut self.items
    }

    /// Marks exactly one item checked.
    pub fn check_only(&mut self, index: usize) {
        for (i, item) in self.items.iter_mut().enumerate() {
            item.checked = i == index;
        }
    }

    pub fn is_open(&self) -> bool {
        self.presence.is_shown()
    }

    /// True while open or fading out (keep painting).
    pub fn is_visible(&self) -> bool {
        self.presence.is_visible()
    }

    pub fn frame(&self) -> RectF {
        self.frame
    }

    pub fn preferred_size(&self, gfx: &Gfx) -> SizeF {
        let style = label_style();
        let mut width: f32 = MIN_WIDTH;
        let mut height = 2.0 * PADDING;
        for item in &self.items {
            if item.separator {
                height += SEPARATOR_HEIGHT;
                continue;
            }
            height += ITEM_HEIGHT;
            let icon = if item.icon.is_some() { ICON_COLUMN } else { 0.0 };
            let shortcut = item.shortcut.as_ref().map_or(0.0, |s| 24.0 + gfx.measure_text(s, &style).w);
            width = width.max(2.0 * PADDING + LEAD + icon + gfx.measure_text(&item.label, &style).w + shortcut + TRAIL);
        }
        SizeF::new(width.ceil(), height)
    }

    /// Opens below `anchor` (above if there is no room), left-aligned, clamped to `bounds`.
    pub fn open(&mut self, gfx: &Gfx, anchor: RectF, bounds: SizeF) {
        let size = self.preferred_size(gfx);
        let mut y = anchor.bottom() + GAP;
        let mut origin_y = anchor.bottom();
        if y + size.h > bounds.h - MARGIN && anchor.y - GAP - size.h >= MARGIN {
            y = anchor.y - GAP - size.h;
            origin_y = anchor.y;
        }
        let x = anchor.x.min(bounds.w - MARGIN - size.w).max(MARGIN);
        self.frame = RectF::new(x.round(), y.round(), size.w, size.h);
        self.origin = PointF::new(anchor.center().x.clamp(self.frame.x, self.frame.right()), origin_y);
        self.layout_rows();
        self.highlighted = None;
        self.presence.set_shown(true);
    }

    pub fn close(&mut self) {
        self.presence.set_shown(false);
        self.highlighted = None;
    }

    /// Opens instantly with `highlighted` (previews and tests).
    pub fn force_open(&mut self, gfx: &Gfx, anchor: RectF, bounds: SizeF, highlighted: Option<usize>) {
        self.open(gfx, anchor, bounds);
        self.presence.snap(true);
        self.highlighted = highlighted;
    }

    fn layout_rows(&mut self) {
        self.rows.clear();
        let mut y = self.frame.y + PADDING;
        for item in &self.items {
            let h = if item.separator { SEPARATOR_HEIGHT } else { ITEM_HEIGHT };
            self.rows.push(RectF::new(self.frame.x + PADDING, y, self.frame.w - 2.0 * PADDING, h));
            y += h;
        }
    }

    fn row_at(&self, pos: PointF) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(pos)).filter(|&i| self.items[i].selectable())
    }

    fn step(&self, from: Option<usize>, forward: bool) -> Option<usize> {
        let n = self.items.len();
        if n == 0 {
            return None;
        }
        let mut i = from.unwrap_or(if forward { n - 1 } else { 0 });
        for _ in 0..n {
            i = if forward { (i + 1) % n } else { (i + n - 1) % n };
            if self.items[i].selectable() {
                return Some(i);
            }
        }
        None
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<usize> {
        if !self.presence.is_shown() {
            return Response::Ignored;
        }
        let before = self.highlighted;
        let response = match event {
            Event::PointerMove(e) => {
                self.highlighted = self.row_at(e.pos);
                Response::Consumed
            }
            Event::PointerDown(e) => {
                if !self.frame.contains(e.pos) {
                    self.close();
                }
                Response::Consumed
            }
            Event::PointerUp(e) if e.button == Some(MouseButton::Left) => match self.row_at(e.pos) {
                Some(i) => {
                    self.close();
                    Response::Action(i)
                }
                None => Response::Consumed,
            },
            Event::PointerLeave => {
                self.highlighted = None;
                Response::Consumed
            }
            Event::KeyDown(k) => match k.key {
                Key::Down => {
                    self.highlighted = self.step(self.highlighted, true);
                    Response::Consumed
                }
                Key::Up => {
                    self.highlighted = self.step(self.highlighted, false);
                    Response::Consumed
                }
                Key::Enter | Key::Space => match self.highlighted {
                    Some(i) => {
                        self.close();
                        Response::Action(i)
                    }
                    None => Response::Consumed,
                },
                Key::Escape => {
                    self.close();
                    Response::Consumed
                }
                _ => Response::Consumed,
            },
            Event::Wheel(_) | Event::PointerUp(_) | Event::Text(_) | Event::KeyUp(_) => Response::Consumed,
            Event::Focus(false) => {
                self.close();
                Response::Ignored
            }
            _ => Response::Ignored,
        };
        if before != self.highlighted {
            cx.request_paint();
        }
        response
    }

    pub fn paint(&mut self, p: &mut Painter, backdrop: Option<&Backdrop>) {
        if !self.presence.is_visible() {
            return;
        }
        let frame = self.frame;
        let rows = self.rows.clone();
        let items = &self.items;
        let highlighted = self.highlighted;
        self.presence.paint(p, self.origin, |p| {
            let theme = p.theme().clone();
            p.glass(frame, RADIUS, backdrop);
            let style = label_style();
            let any_icon = items.iter().any(|i| i.icon.is_some());
            for (i, item) in items.iter().enumerate() {
                let row = rows[i];
                if item.separator {
                    let y = p.snap(row.center().y);
                    p.fill_rect(RectF::new(row.x + 8.0, y, row.w - 16.0, p.px()), theme.separator);
                    continue;
                }
                let active = highlighted == Some(i);
                if active {
                    p.fill_round_rect(p.snap_rect(row), ROW_RADIUS, theme.accent);
                }
                let color = if !item.enabled {
                    theme.text_tertiary
                } else if active {
                    theme.on_accent
                } else {
                    theme.text
                };
                if item.checked {
                    p.icon_with_stroke(Icon::Check, PointF::new(row.x + 13.0, row.center().y), 14.0, color, 2.25);
                }
                let mut x = row.x + LEAD;
                if let Some(icon) = item.icon {
                    p.icon(icon, PointF::new(x + 8.0, row.center().y), 16.0, color);
                }
                if any_icon {
                    x += ICON_COLUMN;
                }
                p.text(&item.label, &style, color, RectF::new(x, row.y, row.right() - x - TRAIL, row.h));
                if let Some(shortcut) = &item.shortcut {
                    let secondary = if active { theme.on_accent.with_alpha(0.8) } else { theme.text_secondary };
                    let right = TextStyle { align: TextAlign::Trailing, ..style };
                    p.text(shortcut, &right, secondary, RectF::new(row.x, row.y, row.w - TRAIL, row.h));
                }
            }
        });
    }
}
