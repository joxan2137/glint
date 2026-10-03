//! The overlay's controls: the capture toolbar (DESIGN §6), the delay/options menu and the video record bar.

use glint_core::{CaptureMode, PointF, RectF, SizeF};
use glint_ui::widgets::{
    Button, ButtonStyle, IconButton, Menu, MenuItem, Presence, Response, Segment, Segmented, Toolbar, ToolbarItem,
};
use glint_ui::{Backdrop, Ctx, Event, Gfx, Icon, Painter};

use crate::geometry::place_near;
use crate::state::DELAYS;

pub const TOOLBAR_TOP: f32 = 24.0;
pub const MODE_SEGMENTS: [CaptureMode; 4] =
    [CaptureMode::Rectangle, CaptureMode::Window, CaptureMode::FullScreen, CaptureMode::Freeform];
pub const MAGNIFIER_ITEM: usize = DELAYS.len() + 1;

pub fn capture_toolbar(mode: CaptureMode, video: bool) -> Toolbar {
    let mut modes = Segmented::new(
        vec![
            Segment::icon(Icon::SquareDashed).tooltip("Rectangle", Some("R")),
            Segment::icon(Icon::AppWindow).tooltip("Window", Some("W")),
            Segment::icon(Icon::Monitor).tooltip("Full screen", Some("F")),
            Segment::icon(Icon::Lasso).tooltip("Freeform", Some("L")),
        ],
        MODE_SEGMENTS.iter().position(|m| *m == mode).unwrap_or(0),
    );
    if !MODE_SEGMENTS.contains(&mode) {
        modes.clear_selection();
    }
    let media = Segmented::new(
        vec![Segment::icon(Icon::Camera).tooltip("Photo", Some("V")), Segment::icon(Icon::Video).tooltip("Video", Some("V"))],
        usize::from(video),
    );
    Toolbar::new(vec![
        ToolbarItem::segmented("mode", modes),
        ToolbarItem::separator(),
        ToolbarItem::button(
            "text",
            IconButton::new(Icon::ScanText).tooltip("Text", Some("T")).with_selected(mode == CaptureMode::Text),
        ),
        ToolbarItem::button(
            "color",
            IconButton::new(Icon::Pipette).tooltip("Color", Some("C")).with_selected(mode == CaptureMode::ColorPicker),
        ),
        ToolbarItem::separator(),
        ToolbarItem::button("delay", IconButton::new(Icon::Timer).tooltip("Delay and options", None)),
        ToolbarItem::separator(),
        ToolbarItem::segmented("media", media),
        ToolbarItem::separator(),
        ToolbarItem::button("close", IconButton::new(Icon::X).tooltip("Close", Some("Esc"))),
    ])
}

/// Keeps the toolbar's selection states in step with the session.
pub fn sync_toolbar(toolbar: &mut Toolbar, mode: CaptureMode, video: bool, menu_open: bool) {
    if let Some(modes) = toolbar.segmented_mut("mode") {
        match MODE_SEGMENTS.iter().position(|m| *m == mode) {
            Some(i) if !modes.has_selection() || modes.selected() != i => modes.set_selected(i),
            None if modes.has_selection() => modes.clear_selection(),
            _ => {}
        }
    }
    if let Some(media) = toolbar.segmented_mut("media")
        && media.selected() != usize::from(video)
    {
        media.set_selected(usize::from(video));
    }
    for (id, selected) in [("text", mode == CaptureMode::Text), ("color", mode == CaptureMode::ColorPicker), ("delay", menu_open)] {
        if let Some(button) = toolbar.button_mut(id) {
            button.set_selected(selected);
        }
    }
}

pub fn options_menu() -> Menu {
    let mut items: Vec<MenuItem> = DELAYS
        .iter()
        .map(|&secs| MenuItem::new(&if secs == 0 { "No delay".to_string() } else { format!("{secs} seconds") }))
        .collect();
    items.push(MenuItem::separator());
    items.push(MenuItem::new("Show magnifier").icon(Icon::ZoomIn).shortcut("M"));
    Menu::new(items)
}

pub fn sync_menu(menu: &mut Menu, delay_secs: u32, show_magnifier: bool) {
    for (i, item) in menu.items_mut().iter_mut().enumerate() {
        item.checked = match DELAYS.get(i) {
            Some(&secs) => secs == delay_secs,
            None => i == MAGNIFIER_ITEM && show_magnifier,
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordAction {
    Record,
    Microphone,
    SystemAudio,
    Cancel,
}

const BAR_HEIGHT: f32 = 44.0;
const BAR_RADIUS: f32 = 14.0;
const BAR_PADDING: f32 = 8.0;
const BAR_GAP: f32 = 4.0;
const RECORD_GAP: f32 = 10.0;
const SEPARATOR_SPACE: f32 = 6.0;

/// Glass bar under a chosen video region: `[● Record] [mic] [system audio] | [Cancel]`.
pub struct RecordBar {
    presence: Presence,
    rect: RectF,
    record: Button,
    microphone: IconButton,
    system_audio: IconButton,
    cancel: Button,
    separator_x: f32,
}

impl RecordBar {
    pub fn new() -> Self {
        Self {
            presence: Presence::new(false),
            rect: RectF::default(),
            record: Button::new("Record", ButtonStyle::Destructive).icon(Icon::RecordFill),
            microphone: IconButton::new(Icon::Mic).tooltip("Microphone", None),
            system_audio: IconButton::new(Icon::Volume).tooltip("System audio", None),
            cancel: Button::new("Cancel", ButtonStyle::Secondary),
            separator_x: 0.0,
        }
    }

    pub fn set_audio(&mut self, microphone: bool, system_audio: bool) {
        self.microphone.icon = if microphone { Icon::Mic } else { Icon::MicOff };
        self.microphone.set_selected(microphone);
        self.system_audio.icon = if system_audio { Icon::Volume } else { Icon::VolumeX };
        self.system_audio.set_selected(system_audio);
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.presence.set_shown(visible);
    }

    pub fn is_visible(&self) -> bool {
        self.presence.is_shown()
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    pub fn contains(&self, pos: PointF) -> bool {
        self.presence.is_shown() && self.rect.contains(pos)
    }

    pub fn force_hover_record(&mut self) {
        self.record.force_state(true, false);
    }

    fn size(&self, gfx: &Gfx) -> SizeF {
        let w = BAR_PADDING
            + self.record.preferred_size(gfx).w
            + RECORD_GAP
            + BAR_GAP
            + 64.0
            + 2.0 * SEPARATOR_SPACE
            + 1.0
            + self.cancel.preferred_size(gfx).w
            + BAR_PADDING;
        SizeF::new(w.ceil(), BAR_HEIGHT)
    }

    /// Centers the bar under `region` (above it, or inside it, when there is no room), within `bounds`.
    pub fn layout(&mut self, gfx: &Gfx, region: RectF, bounds: RectF) {
        let size = self.size(gfx);
        let rect = place_near(region, size, 12.0, bounds, None);
        self.rect = RectF::new(rect.x.round(), rect.y.round(), size.w, size.h);
        let center_y = self.rect.center().y;
        let mut x = self.rect.x + BAR_PADDING;
        let record = self.record.preferred_size(gfx);
        self.record.set_rect(RectF::new(x, center_y - record.h / 2.0, record.w, record.h));
        x += record.w + RECORD_GAP;
        self.microphone.set_rect(RectF::new(x, center_y - 16.0, 32.0, 32.0));
        x += 32.0 + BAR_GAP;
        self.system_audio.set_rect(RectF::new(x, center_y - 16.0, 32.0, 32.0));
        x += 32.0 + SEPARATOR_SPACE;
        self.separator_x = x;
        x += 1.0 + SEPARATOR_SPACE;
        let cancel = self.cancel.preferred_size(gfx);
        self.cancel.set_rect(RectF::new(x, center_y - cancel.h / 2.0, cancel.w, cancel.h));
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<RecordAction> {
        if !self.presence.is_shown() {
            return Response::Ignored;
        }
        let responses = [
            self.record.event(cx, event).map(|()| RecordAction::Record),
            self.microphone.event(cx, event).map(|()| RecordAction::Microphone),
            self.system_audio.event(cx, event).map(|()| RecordAction::SystemAudio),
            self.cancel.event(cx, event).map(|()| RecordAction::Cancel),
        ];
        if let Some(action) = responses.iter().find_map(|r| match r {
            Response::Action(a) => Some(*a),
            _ => None,
        }) {
            return Response::Action(action);
        }
        let on_bar = event.pointer_pos().is_some_and(|p| self.rect.contains(p))
            && matches!(event, Event::PointerDown(_) | Event::PointerUp(_) | Event::Wheel(_));
        if on_bar || responses.iter().any(Response::consumed) { Response::Consumed } else { Response::Ignored }
    }

    pub fn paint(&mut self, p: &mut Painter, backdrop: Option<&Backdrop>) {
        let rect = self.rect;
        let origin = PointF::new(rect.center().x, rect.y);
        let separator_x = self.separator_x;
        let (record, microphone, system_audio, cancel) = (&mut self.record, &mut self.microphone, &mut self.system_audio, &mut self.cancel);
        self.presence.paint(p, origin, |p| {
            p.glass(rect, BAR_RADIUS, backdrop);
            record.paint(p);
            microphone.paint(p);
            system_audio.paint(p);
            let separator = p.theme().separator;
            let x = p.snap(separator_x);
            let y = p.snap(rect.center().y - 10.0);
            p.fill_rect(RectF::new(x, y, 1.0, 20.0), separator);
            cancel.paint(p);
        });
    }
}
