use glint_core::{PointF, RectF, SizeF};

use super::{Presence, Response};
use crate::event::{Event, Key};
use crate::painter::{Backdrop, Painter};
use crate::view::Ctx;

const RADIUS: f32 = 12.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 8.0;
const MARGIN: f32 = 8.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverPlacement {
    /// Below the anchor unless there is no room.
    #[default]
    Auto,
    Below,
    Above,
}

/// Glass panel anchored to a rect. The view paints the content into `content_rect()` via `paint`.
/// Clicks outside or Escape dismiss it (reported as `Action(())`).
#[derive(Clone, Debug)]
pub struct Popover {
    content: SizeF,
    pub placement: PopoverPlacement,
    frame: RectF,
    origin: PointF,
    presence: Presence,
}

impl Popover {
    /// `content` is the inner size in DIP; 12 DIP padding is added around it.
    pub fn new(content: SizeF) -> Self {
        Self {
            content,
            placement: PopoverPlacement::Auto,
            frame: RectF::default(),
            origin: PointF::default(),
            presence: Presence::new(false),
        }
    }

    pub fn set_content_size(&mut self, content: SizeF) {
        self.content = content;
    }

    pub fn open(&mut self, anchor: RectF, bounds: SizeF) {
        let size = SizeF::new(self.content.w + 2.0 * PADDING, self.content.h + 2.0 * PADDING);
        let below = anchor.bottom() + GAP;
        let above = anchor.y - GAP - size.h;
        let fits_below = below + size.h <= bounds.h - MARGIN;
        let use_below = match self.placement {
            PopoverPlacement::Below => true,
            PopoverPlacement::Above => false,
            PopoverPlacement::Auto => fits_below || above < MARGIN,
        };
        let y = if use_below { below } else { above };
        let x = (anchor.center().x - size.w / 2.0).clamp(MARGIN, (bounds.w - MARGIN - size.w).max(MARGIN));
        self.frame = RectF::new(x.round(), y.round(), size.w, size.h);
        self.origin = PointF::new(anchor.center().x, if use_below { anchor.bottom() } else { anchor.y });
        self.presence.set_shown(true);
    }

    /// Opens without animation (previews and tests).
    pub fn force_open(&mut self, anchor: RectF, bounds: SizeF) {
        self.open(anchor, bounds);
        self.presence.snap(true);
    }

    pub fn close(&mut self) {
        self.presence.set_shown(false);
    }

    pub fn is_open(&self) -> bool {
        self.presence.is_shown()
    }

    pub fn is_visible(&self) -> bool {
        self.presence.is_visible()
    }

    pub fn frame(&self) -> RectF {
        self.frame
    }

    pub fn content_rect(&self) -> RectF {
        self.frame.inset(PADDING)
    }

    /// Route events here first while open. Pointer events inside are `Consumed` after the caller has handled its
    /// content widgets; outside presses and Escape close the popover and return `Action(())`.
    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        let _ = cx;
        if !self.presence.is_shown() {
            return Response::Ignored;
        }
        match event {
            Event::PointerDown(e) if !self.frame.contains(e.pos) => {
                self.close();
                Response::Action(())
            }
            Event::KeyDown(k) if k.key == Key::Escape => {
                self.close();
                Response::Action(())
            }
            e if e.pointer_pos().is_some_and(|p| self.frame.contains(p)) => Response::Consumed,
            _ => Response::Ignored,
        }
    }

    pub fn paint(&mut self, p: &mut Painter, backdrop: Option<&Backdrop>, content: impl FnOnce(&mut Painter, RectF)) {
        if !self.presence.is_visible() {
            return;
        }
        let frame = self.frame;
        let inner = self.content_rect();
        self.presence.paint(p, self.origin, |p| {
            p.glass(frame, RADIUS, backdrop);
            content(p, inner);
        });
    }
}
