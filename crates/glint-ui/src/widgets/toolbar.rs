use glint_core::{PointF, RectF, SizeF};

use super::{IconButton, Presence, Response, Segmented};
use crate::event::Event;
use crate::gfx::Gfx;
use crate::painter::{Backdrop, Painter};
use crate::view::Ctx;

pub const TOOLBAR_HEIGHT: f32 = 44.0;
const RADIUS: f32 = 14.0;
const PADDING: f32 = 6.0;
const GAP: f32 = 2.0;
const SEPARATOR_SPACE: f32 = 6.0;
const SEPARATOR_HEIGHT: f32 = 20.0;

#[derive(Clone, Debug)]
pub enum ToolbarItemKind {
    Button(IconButton),
    Segmented(Segmented),
    /// 1×20 DIP divider.
    Separator,
    /// Fixed empty space in DIP.
    Space(f32),
    /// An area the view paints itself (e.g. a badge or timer readout).
    Custom(SizeF),
}

#[derive(Clone, Debug)]
pub struct ToolbarItem {
    pub id: &'static str,
    pub kind: ToolbarItemKind,
    rect: RectF,
}

impl ToolbarItem {
    pub fn button(id: &'static str, button: IconButton) -> Self {
        Self { id, kind: ToolbarItemKind::Button(button), rect: RectF::default() }
    }

    pub fn segmented(id: &'static str, segmented: Segmented) -> Self {
        Self { id, kind: ToolbarItemKind::Segmented(segmented), rect: RectF::default() }
    }

    pub fn separator() -> Self {
        Self { id: "", kind: ToolbarItemKind::Separator, rect: RectF::default() }
    }

    pub fn space(width: f32) -> Self {
        Self { id: "", kind: ToolbarItemKind::Space(width), rect: RectF::default() }
    }

    pub fn custom(id: &'static str, size: SizeF) -> Self {
        Self { id, kind: ToolbarItemKind::Custom(size), rect: RectF::default() }
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    fn width(&self, gfx: &Gfx) -> f32 {
        match &self.kind {
            ToolbarItemKind::Button(b) => b.preferred_size(gfx).w,
            ToolbarItemKind::Segmented(s) => s.preferred_size(gfx).w,
            ToolbarItemKind::Separator => 1.0 + 2.0 * SEPARATOR_SPACE,
            ToolbarItemKind::Space(w) => *w,
            ToolbarItemKind::Custom(size) => size.w,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolbarAction {
    Clicked(&'static str),
    Selected(&'static str, usize),
}

/// Glass pill that lays out buttons, segmented controls and separators, with the DESIGN §5 appear animation.
#[derive(Clone, Debug)]
pub struct Toolbar {
    items: Vec<ToolbarItem>,
    rect: RectF,
    presence: Presence,
}

impl Toolbar {
    pub fn new(items: Vec<ToolbarItem>) -> Self {
        Self { items, rect: RectF::default(), presence: Presence::new(true) }
    }

    pub fn items(&self) -> &[ToolbarItem] {
        &self.items
    }

    pub fn item(&self, id: &str) -> Option<&ToolbarItem> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn button_mut(&mut self, id: &str) -> Option<&mut IconButton> {
        self.items.iter_mut().find(|i| i.id == id).and_then(|i| match &mut i.kind {
            ToolbarItemKind::Button(b) => Some(b),
            _ => None,
        })
    }

    pub fn segmented_mut(&mut self, id: &str) -> Option<&mut Segmented> {
        self.items.iter_mut().find(|i| i.id == id).and_then(|i| match &mut i.kind {
            ToolbarItemKind::Segmented(s) => Some(s),
            _ => None,
        })
    }

    pub fn preferred_size(&self, gfx: &Gfx) -> SizeF {
        let widths: f32 = self.items.iter().map(|i| i.width(gfx)).sum();
        let gaps = GAP * self.items.len().saturating_sub(1) as f32;
        SizeF::new(2.0 * PADDING + widths + gaps, TOOLBAR_HEIGHT)
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    /// Places the toolbar with its top-left at `origin` (snap to whole DIPs for crisp edges).
    pub fn layout_at(&mut self, gfx: &Gfx, origin: PointF) {
        let size = self.preferred_size(gfx);
        self.rect = RectF::new(origin.x.round(), origin.y.round(), size.w, size.h);
        let mut x = self.rect.x + PADDING;
        let inner_y = self.rect.y + PADDING;
        for item in &mut self.items {
            let w = item.width(gfx);
            item.rect = match &item.kind {
                ToolbarItemKind::Custom(size) => RectF::new(x, self.rect.y + (TOOLBAR_HEIGHT - size.h) / 2.0, w, size.h),
                _ => RectF::new(x, inner_y, w, TOOLBAR_HEIGHT - 2.0 * PADDING),
            };
            match &mut item.kind {
                ToolbarItemKind::Button(b) => b.set_rect(item.rect),
                ToolbarItemKind::Segmented(s) => s.layout(gfx, item.rect),
                _ => {}
            }
            x += w + GAP;
        }
    }

    /// Places the toolbar horizontally centered on `center_x` with its top at `top`.
    pub fn layout_centered(&mut self, gfx: &Gfx, center_x: f32, top: f32) {
        let size = self.preferred_size(gfx);
        self.layout_at(gfx, PointF::new(center_x - size.w / 2.0, top));
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.presence.set_shown(visible);
    }

    pub fn snap_visible(&mut self, visible: bool) {
        self.presence.snap(visible);
    }

    pub fn is_visible(&self) -> bool {
        self.presence.is_shown()
    }

    pub fn contains(&self, pos: PointF) -> bool {
        self.presence.is_shown() && self.rect.contains(pos)
    }

    /// Routes pointer events to items. Presses anywhere on the pill are consumed so they never reach content
    /// below it.
    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<ToolbarAction> {
        if !self.presence.is_shown() {
            return Response::Ignored;
        }
        let mut result = Response::Ignored;
        for item in &mut self.items {
            let response = match &mut item.kind {
                ToolbarItemKind::Button(b) => b.event(cx, event).map(|()| ToolbarAction::Clicked(item.id)),
                ToolbarItemKind::Segmented(s) => s.event(cx, event).map(|i| ToolbarAction::Selected(item.id, i)),
                _ => Response::Ignored,
            };
            match response {
                Response::Action(a) => result = Response::Action(a),
                Response::Consumed if !matches!(result, Response::Action(_)) => result = Response::Consumed,
                _ => {}
            }
        }
        if let Some(pos) = event.pointer_pos()
            && self.rect.contains(pos)
            && !result.consumed()
            && matches!(event, Event::PointerDown(_) | Event::PointerUp(_) | Event::Wheel(_))
        {
            result = Response::Consumed;
        }
        result
    }

    /// Paints the pill and its items; custom items are left for the caller (use `item(id).rect()`).
    pub fn paint(&mut self, p: &mut Painter, backdrop: Option<&Backdrop>) {
        let rect = self.rect;
        let origin = PointF::new(rect.center().x, rect.y);
        let items = &mut self.items;
        self.presence.paint(p, origin, |p| {
            p.glass(rect, RADIUS, backdrop);
            let separator = p.theme().separator;
            for item in items.iter_mut() {
                match &mut item.kind {
                    ToolbarItemKind::Button(b) => b.paint(p),
                    ToolbarItemKind::Segmented(s) => s.paint(p),
                    ToolbarItemKind::Separator => {
                        let x = p.snap(item.rect.center().x - 0.5);
                        let y = p.snap(rect.center().y - SEPARATOR_HEIGHT / 2.0);
                        p.fill_rect(RectF::new(x, y, 1.0, SEPARATOR_HEIGHT), separator);
                    }
                    _ => {}
                }
            }
        });
    }
}
