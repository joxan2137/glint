//! Settings window: Apple System Settings style grouped inset sections on Mica. Rows put a label and secondary text
//! on the left and a glint-ui control on the right; edits apply live (the app reacts to `SettingsMessage`).
//! Scrolls with the wheel, touchpad and keyboard.

use std::collections::HashMap;
use std::time::Duration;

use glint_core::Settings;
use glint_ui::widgets::{
    Badge, BadgeStyle, Button, ButtonStyle, Interaction, Menu, MenuItem, Response, Segment, Segmented, SegmentedStyle,
    Slider, Toggle,
};
use glint_ui::{
    Animated, Color, Ctx, Event, Gfx, Icon, Key, Modifiers, Motion, Painter, PointF, RectF, SizeF, Spring, TextAlign,
    TextStyle, View, Weight,
};

use crate::art::paint_app_icon;
use crate::settings_model::{
    Control, Edit, InstallState, Row, RowId, Section, SettingsEnv, SettingsRequest, WIN_KEY, apply_edit, sections,
};

pub const WINDOW_SIZE: SizeF = SizeF::new(700.0, 780.0);
pub const MIN_SIZE: SizeF = SizeF::new(560.0, 420.0);

const MAX_CONTENT: f32 = 620.0;
const SIDE: f32 = 28.0;
const TOP: f32 = 28.0;
const HEADER: f32 = 64.0;
const HEADER_GAP: f32 = 28.0;
const SECTION_TITLE: f32 = 20.0;
const SECTION_TITLE_GAP: f32 = 8.0;
const SECTION_GAP: f32 = 26.0;
const FOOTER_GAP: f32 = 8.0;
const ROW: f32 = 44.0;
const ROW_WITH_DETAIL: f32 = 56.0;
const ROW_PAD: f32 = 16.0;
const CONTROL_GAP: f32 = 16.0;
const GROUP_RADIUS: f32 = 10.0;
const BOTTOM: f32 = 36.0;
const SLIDER_WIDTH: f32 = 200.0;
const WHEEL_STEP: f32 = 64.0;
const SCROLLBAR_TOKEN: u64 = 1;

/// What the settings view tells the app.
#[derive(Debug)]
pub enum SettingsMessage {
    Changed(Box<Settings>),
    Request(SettingsRequest),
    Closed,
}

struct PopupButton {
    label: String,
    rect: RectF,
    interaction: Interaction,
}

impl PopupButton {
    fn preferred_size(&self, gfx: &Gfx) -> SizeF {
        SizeF::new((gfx.measure_text(&self.label, &label_style()).w + 46.0).max(132.0).ceil(), 28.0)
    }

    fn paint(&self, p: &mut Painter) {
        let theme = p.theme().clone();
        let rect = p.snap_rect(self.rect);
        let hover = self.interaction.hover_amount();
        let base = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.10) } else { Color::rgba(1.0, 1.0, 1.0, 0.9) };
        let fill = base.lerp(&base.with_alpha(base.a + 0.06), hover).lerp(&theme.pressed.over(&base), self.interaction.press_amount());
        p.fill_round_rect(rect, 7.0, fill);
        let line = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.09) } else { Color::rgba(0.0, 0.0, 0.0, 0.11) };
        p.hairline_round_rect(rect, 7.0, line, true);
        p.text(&self.label, &label_style(), theme.text, RectF::new(rect.x + 12.0, rect.y, rect.w - 40.0, rect.h));
        let chevrons = PointF::new(rect.right() - 16.0, rect.center().y);
        p.icon_with_stroke(Icon::ChevronUp, PointF::new(chevrons.x, chevrons.y - 3.5), 10.0, theme.text_secondary, 2.0);
        p.icon_with_stroke(Icon::ChevronDown, PointF::new(chevrons.x, chevrons.y + 3.5), 10.0, theme.text_secondary, 2.0);
    }
}

enum Widget {
    Toggle(Toggle),
    Segments(Segmented),
    Popup(PopupButton),
    Slider(Slider),
    Button(Button),
    /// Cancel and confirm buttons of a pending destructive action.
    Confirm(Button, Button),
    Link(Interaction),
    Passive,
}

impl Widget {
    fn for_control(control: &Control) -> Widget {
        match control {
            Control::Toggle(on) | Control::Shortcut { on, .. } => Widget::Toggle(Toggle::new(*on)),
            Control::Segments { options, selected } => Widget::Segments(
                Segmented::new(options.iter().map(|o| Segment::label(o)).collect(), *selected).style(SegmentedStyle::Track),
            ),
            Control::Popup { options, selected } => Widget::Popup(PopupButton {
                label: options.get(*selected).cloned().unwrap_or_default(),
                rect: RectF::default(),
                interaction: Interaction::new(),
            }),
            Control::Slider { value, min, max, step } => Widget::Slider(Slider::new(*value, *min, *max).origin(0.0).step(*step)),
            Control::Button(label) => Widget::Button(Button::new(label, ButtonStyle::Secondary)),
            Control::Confirm { cancel, confirm } => {
                Widget::Confirm(Button::new(cancel, ButtonStyle::Secondary), Button::new(confirm, ButtonStyle::Destructive))
            }
            Control::Link => Widget::Link(Interaction::new()),
            Control::Status { .. } | Control::Value(_) => Widget::Passive,
        }
    }

    /// True when this widget can show `control` (otherwise it is replaced).
    fn fits(&self, control: &Control) -> bool {
        matches!(
            (self, control),
            (Widget::Toggle(_), Control::Toggle(_) | Control::Shortcut { .. })
                | (Widget::Segments(_), Control::Segments { .. })
                | (Widget::Popup(_), Control::Popup { .. })
                | (Widget::Slider(_), Control::Slider { .. })
                | (Widget::Button(_), Control::Button(_))
                | (Widget::Confirm(..), Control::Confirm { .. })
                | (Widget::Link(_), Control::Link)
                | (Widget::Passive, Control::Status { .. } | Control::Value(_))
        )
    }

    /// Updates the value without recreating the widget, so animations stay continuous.
    fn sync(&mut self, control: &Control) {
        match (self, control) {
            (Widget::Toggle(t), Control::Toggle(on) | Control::Shortcut { on, .. }) if t.is_on() != *on => t.set_on(*on),
            (Widget::Segments(s), Control::Segments { selected, .. }) if s.selected() != *selected => s.set_selected(*selected),
            (Widget::Popup(b), Control::Popup { options, selected }) => b.label = options.get(*selected).cloned().unwrap_or_default(),
            (Widget::Slider(s), Control::Slider { value, .. }) if (s.value() - value).abs() > 1e-4 => s.set_value(*value),
            (Widget::Button(b), Control::Button(label)) => b.label = label.clone(),
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct PlacedRow {
    id: RowId,
    rect: RectF,
    first: bool,
}

struct PlacedSection {
    title: RectF,
    group: RectF,
    footer: Option<RectF>,
}

pub struct SettingsView {
    settings: Settings,
    env: SettingsEnv,
    sections: Vec<Section>,
    widgets: HashMap<RowId, Widget>,
    rows: Vec<PlacedRow>,
    placed_sections: Vec<PlacedSection>,
    header: RectF,
    content_height: f32,
    laid_out_width: f32,
    scroll: Animated<f32>,
    scroll_target: f32,
    viewport: SizeF,
    scrollbar: Animated<f32>,
    menu: Option<(RowId, Menu)>,
    confirming_uninstall: bool,
}

fn label_style() -> TextStyle {
    TextStyle::body()
}

fn detail_style() -> TextStyle {
    TextStyle::new(12.0)
}

fn footer_style() -> TextStyle {
    TextStyle::new(12.0).wrap().line_height(16.0)
}

impl SettingsView {
    pub fn new(settings: Settings, env: SettingsEnv) -> Self {
        let mut view = Self {
            settings,
            env,
            sections: Vec::new(),
            widgets: HashMap::new(),
            rows: Vec::new(),
            placed_sections: Vec::new(),
            header: RectF::default(),
            content_height: 0.0,
            laid_out_width: 0.0,
            scroll: Animated::with_motion(0.0, Motion::Spring(Spring { stiffness: 520.0, damping_ratio: 1.0 })),
            scroll_target: 0.0,
            viewport: SizeF::default(),
            scrollbar: Animated::fade(0.0),
            menu: None,
            confirming_uninstall: false,
        };
        view.rebuild();
        view
    }

    pub fn set_settings(&mut self, cx: &mut Ctx, settings: Settings) {
        self.settings = settings;
        self.rebuild();
        cx.request_paint();
    }

    pub fn set_env(&mut self, cx: &mut Ctx, env: SettingsEnv) {
        self.env = env;
        self.rebuild();
        cx.request_paint();
    }

    /// Height of the whole page at `width` (previews render it in one piece).
    pub fn content_height(&mut self, gfx: &Gfx, width: f32) -> f32 {
        self.layout(gfx, width);
        self.content_height
    }

    fn rebuild(&mut self) {
        self.sections = sections(&self.settings, &self.env, self.confirming_uninstall);
        for row in self.sections.iter().flat_map(|s| &s.rows) {
            match self.widgets.get_mut(&row.id) {
                Some(widget) if widget.fits(&row.control) => widget.sync(&row.control),
                _ => {
                    self.widgets.insert(row.id, Widget::for_control(&row.control));
                }
            }
        }
        self.laid_out_width = 0.0;
    }

    fn row(&self, id: RowId) -> Option<&Row> {
        self.sections.iter().flat_map(|s| &s.rows).find(|r| r.id == id)
    }

    fn layout(&mut self, gfx: &Gfx, width: f32) {
        if (self.laid_out_width - width).abs() < 0.01 {
            return;
        }
        self.laid_out_width = width;
        let column = (width - 2.0 * SIDE).min(MAX_CONTENT);
        let x = ((width - column) / 2.0).round();
        let mut y = TOP;
        self.header = RectF::new(x, y, column, HEADER);
        y += HEADER + HEADER_GAP;
        self.rows.clear();
        self.placed_sections.clear();
        for section in &self.sections {
            let title = RectF::new(x + 4.0, y, column, SECTION_TITLE);
            y += SECTION_TITLE + SECTION_TITLE_GAP;
            let top = y;
            for (i, row) in section.rows.iter().enumerate() {
                let height = if row.detail.is_some() { ROW_WITH_DETAIL } else { ROW };
                let rect = RectF::new(x, y, column, height);
                self.rows.push(PlacedRow { id: row.id, rect, first: i == 0 });
                y += height;
            }
            let group = RectF::new(x, top, column, y - top);
            let footer = section.footer.and_then(|text| {
                let layout = gfx.text_layout(text, &footer_style(), Some(column - 8.0)).ok()?;
                Some(RectF::new(x + 4.0, y + FOOTER_GAP, column - 8.0, layout.height))
            });
            if let Some(f) = footer {
                y = f.bottom();
            }
            self.placed_sections.push(PlacedSection { title, group, footer });
            y += SECTION_GAP;
        }
        self.content_height = y - SECTION_GAP + BOTTOM;
        let rows = self.rows.clone();
        for placed in rows {
            let right = placed.rect.right() - ROW_PAD;
            let center_y = placed.rect.center().y;
            let Some(widget) = self.widgets.get_mut(&placed.id) else { continue };
            match widget {
                Widget::Toggle(t) => t.set_origin(PointF::new(right - Toggle::SIZE.w, center_y - Toggle::SIZE.h / 2.0)),
                Widget::Segments(s) => {
                    let size = s.preferred_size(gfx);
                    let w = size.w.max(150.0);
                    s.layout(gfx, RectF::new(right - w, center_y - size.h / 2.0, w, size.h));
                }
                Widget::Popup(b) => {
                    let size = b.preferred_size(gfx);
                    b.rect = RectF::new(right - size.w, center_y - size.h / 2.0, size.w, size.h);
                }
                Widget::Slider(s) => s.set_rect(RectF::new(right - SLIDER_WIDTH, center_y - Slider::HEIGHT / 2.0, SLIDER_WIDTH, Slider::HEIGHT)),
                Widget::Button(b) => {
                    let size = b.preferred_size(gfx);
                    let w = size.w.max(92.0);
                    b.set_rect(RectF::new(right - w, center_y - size.h / 2.0, w, size.h));
                }
                Widget::Confirm(cancel, confirm) => {
                    let confirm_w = confirm.preferred_size(gfx).w.max(92.0);
                    let cancel_w = cancel.preferred_size(gfx).w.max(80.0);
                    let top = center_y - 14.0;
                    confirm.set_rect(RectF::new(right - confirm_w, top, confirm_w, 28.0));
                    cancel.set_rect(RectF::new(right - confirm_w - 8.0 - cancel_w, top, cancel_w, 28.0));
                }
                Widget::Link(_) | Widget::Passive => {}
            }
        }
    }

    fn max_scroll(&self) -> f32 {
        (self.content_height - self.viewport.h).max(0.0)
    }

    fn scroll_to(&mut self, cx: &mut Ctx, target: f32, animate: bool) {
        let target = target.clamp(0.0, self.max_scroll());
        self.scroll_target = target;
        if animate {
            self.scroll.set(target);
        } else {
            self.scroll.snap(target);
        }
        self.scrollbar.set(1.0);
        cx.set_timer(Duration::from_millis(900), SCROLLBAR_TOKEN);
        cx.request_paint();
    }

    fn edit(&mut self, cx: &mut Ctx, id: RowId, edit: Edit) {
        if id == RowId::Install && matches!(self.env.install, InstallState::Installed { .. }) {
            let confirming = match edit {
                Edit::Press if !self.confirming_uninstall => Some(true),
                Edit::Cancel => Some(false),
                _ => None,
            };
            if let Some(confirming) = confirming {
                self.confirming_uninstall = confirming;
                self.rebuild();
                cx.request_paint();
                return;
            }
            self.confirming_uninstall = false;
        }
        let request = apply_edit(&mut self.settings, &self.env, id, edit);
        if !matches!(edit, Edit::Press | Edit::Cancel) {
            self.rebuild();
            cx.post(SettingsMessage::Changed(Box::new(self.settings.clone())));
        }
        if let Some(request) = request {
            cx.post(SettingsMessage::Request(request));
        }
        cx.request_paint();
    }

    fn open_menu(&mut self, cx: &mut Ctx, id: RowId) {
        let Some(Control::Popup { options, selected }) = self.row(id).map(|r| r.control.clone()) else { return };
        let Some(Widget::Popup(button)) = self.widgets.get(&id) else { return };
        let anchor = button.rect.offset(0.0, -self.scroll.get());
        let items = options.iter().enumerate().map(|(i, o)| MenuItem::new(o).checked(i == selected)).collect();
        let mut menu = Menu::new(items);
        menu.open(cx.gfx(), anchor, cx.size());
        self.menu = Some((id, menu));
        cx.request_paint();
    }

    fn route_to_widgets(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        let visible: Vec<PlacedRow> = self.rows.clone();
        let mut consumed = false;
        for placed in visible {
            let Some(widget) = self.widgets.get_mut(&placed.id) else { continue };
            let edit = match widget {
                Widget::Toggle(t) => respond(&mut consumed, t.event(cx, event)).map(Edit::Toggle),
                Widget::Segments(s) => respond(&mut consumed, s.event(cx, event)).map(Edit::Choose),
                Widget::Slider(s) => respond(&mut consumed, s.event(cx, event)).map(Edit::Slide),
                Widget::Button(b) => respond(&mut consumed, b.event(cx, event)).map(|()| Edit::Press),
                Widget::Confirm(cancel, confirm) => respond(&mut consumed, cancel.event(cx, event))
                    .map(|()| Edit::Cancel)
                    .or_else(|| respond(&mut consumed, confirm.event(cx, event)).map(|()| Edit::Press)),
                Widget::Link(interaction) => {
                    let was = interaction.hovered;
                    let response = interaction.update(event, placed.rect, true);
                    if was != interaction.hovered {
                        cx.request_paint();
                    }
                    respond(&mut consumed, response).map(|()| Edit::Press)
                }
                Widget::Popup(button) => {
                    let was = button.interaction.hovered;
                    let response = button.interaction.update(event, button.rect, true);
                    if was != button.interaction.hovered {
                        cx.request_paint();
                    }
                    if respond(&mut consumed, response).is_some() {
                        self.open_menu(cx, placed.id);
                        return true;
                    }
                    None
                }
                Widget::Passive => None,
            };
            if let Some(edit) = edit {
                self.edit(cx, placed.id, edit);
                return true;
            }
        }
        consumed
    }

    fn paint_header(&self, p: &mut Painter) {
        let theme = p.theme().clone();
        let icon = RectF::new(self.header.x, self.header.y, HEADER, HEADER);
        paint_app_icon(p, icon);
        let text_x = icon.right() + 16.0;
        let width = self.header.right() - text_x;
        p.text("Glint", &TextStyle::large_title(), theme.text, RectF::new(text_x, self.header.y + 6.0, width, 30.0));
        p.text(
            "Screenshots and recordings that look exactly like your screen — even in HDR.",
            &TextStyle::body(),
            theme.text_secondary,
            RectF::new(text_x, self.header.y + 36.0, width, 20.0),
        );
    }

    fn paint_sections(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let dark = theme.is_dark();
        let group_fill = if dark { Color::rgba(1.0, 1.0, 1.0, 0.055) } else { Color::rgba(1.0, 1.0, 1.0, 0.78) };
        let group_line = if dark { Color::rgba(1.0, 1.0, 1.0, 0.07) } else { Color::rgba(0.0, 0.0, 0.0, 0.07) };
        let separator = if dark { Color::rgba(1.0, 1.0, 1.0, 0.08) } else { Color::rgba(0.0, 0.0, 0.0, 0.08) };
        for (section, placed) in self.sections.iter().zip(&self.placed_sections) {
            p.text(section.title, &TextStyle::emphasized(), theme.text, placed.title);
            if !dark {
                p.shadow(placed.group, GROUP_RADIUS, &glint_ui::Shadow::new(1.0, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.05)));
            }
            p.fill_round_rect(placed.group, GROUP_RADIUS, group_fill);
            p.hairline_round_rect(placed.group, GROUP_RADIUS, group_line, true);
            if let (Some(text), Some(rect)) = (section.footer, placed.footer) {
                p.text(text, &footer_style(), theme.text_secondary, rect);
            }
        }
        let rows = self.rows.clone();
        for placed in rows {
            let Some(row) = self.row(placed.id).cloned() else { continue };
            if !placed.first {
                let y = p.snap(placed.rect.y);
                p.fill_rect(RectF::new(placed.rect.x + ROW_PAD, y, placed.rect.w - ROW_PAD, p.px()), separator);
            }
            self.paint_row(p, &placed, &row);
        }
    }

    fn control_left(&self, p: &Painter, placed: &PlacedRow, control: &Control) -> f32 {
        let right = placed.rect.right() - ROW_PAD;
        let left = match (self.widgets.get(&placed.id), control) {
            (Some(Widget::Toggle(t)), Control::Shortcut { keys, .. }) => t.rect().x - KEYCAP_GAP - keycaps_width(p.gfx(), keys),
            (Some(Widget::Toggle(t)), _) => t.rect().x,
            (Some(Widget::Segments(s)), _) => s.rect().x,
            (Some(Widget::Popup(b)), _) => b.rect.x,
            (Some(Widget::Slider(s)), _) => s.rect().x,
            (Some(Widget::Button(b)), _) => b.rect().x,
            (Some(Widget::Confirm(cancel, _)), _) => cancel.rect().x,
            (_, Control::Link) => right - 16.0,
            (_, Control::Status { text, .. }) => right - Badge::new(text).size(p.gfx()).w,
            (_, Control::Value(text)) => right - p.measure(text, &label_style()).w,
            _ => right,
        };
        left - CONTROL_GAP
    }

    fn paint_row(&mut self, p: &mut Painter, placed: &PlacedRow, row: &Row) {
        let theme = p.theme().clone();
        let rect = placed.rect;
        if let Some(Widget::Link(interaction)) = self.widgets.get(&placed.id) {
            let amount = interaction.hover_amount();
            if amount > 0.0 {
                let highlight = Color::TRANSPARENT.lerp(&theme.hover, amount).lerp(&theme.pressed, interaction.press_amount());
                p.clip_round_rect(self.group_of(placed), GROUP_RADIUS, |p| p.fill_rect(rect, highlight));
            }
        }
        let text_x = rect.x + ROW_PAD;
        let text_w = (self.control_left(p, placed, &row.control) - text_x).max(40.0);
        match &row.detail {
            Some(detail) => {
                p.text(&row.title, &label_style(), theme.text, RectF::new(text_x, rect.y + 9.0, text_w, 20.0));
                let detail = shorten_middle(p.gfx(), detail, &detail_style(), text_w);
                p.text(&detail, &detail_style(), theme.text_secondary, RectF::new(text_x, rect.y + 28.0, text_w, 18.0));
            }
            None => {
                p.text(&row.title, &label_style(), theme.text, RectF::new(text_x, rect.y, text_w, rect.h));
            }
        }
        let right = rect.right() - ROW_PAD;
        match (self.widgets.get_mut(&placed.id), &row.control) {
            (Some(Widget::Toggle(t)), Control::Shortcut { keys, .. }) => {
                let right = t.rect().x - KEYCAP_GAP;
                paint_keycaps(p, keys, right, rect.center().y);
                t.paint(p);
            }
            (Some(Widget::Toggle(t)), _) => t.paint(p),
            (Some(Widget::Segments(s)), _) => s.paint(p),
            (Some(Widget::Popup(b)), _) => b.paint(p),
            (Some(Widget::Slider(s)), _) => s.paint(p),
            (Some(Widget::Button(b)), _) => b.paint(p),
            (Some(Widget::Confirm(cancel, confirm)), _) => {
                cancel.paint(p);
                confirm.paint(p);
            }
            (_, Control::Link) => p.icon(Icon::ExternalLink, PointF::new(right - 8.0, rect.center().y), 16.0, theme.text_secondary),
            (_, Control::Status { text, highlighted }) => {
                let style = if *highlighted { BadgeStyle::Accent } else { BadgeStyle::Subtle };
                let badge = Badge::new(text).style(style);
                let size = badge.size(p.gfx());
                badge.paint(p, p.snap_rect(RectF::new(right - size.w, rect.center().y - size.h / 2.0, size.w, size.h)));
            }
            (_, Control::Value(text)) => {
                let style = label_style().align(TextAlign::Trailing);
                p.text(text, &style, theme.text_secondary, RectF::new(rect.x, rect.y, rect.w - ROW_PAD, rect.h));
            }
            _ => {}
        }
    }

    fn group_of(&self, placed: &PlacedRow) -> RectF {
        self.placed_sections
            .iter()
            .map(|s| s.group)
            .find(|g| g.contains(placed.rect.center()))
            .unwrap_or(placed.rect)
    }

    fn paint_scrollbar(&self, p: &mut Painter) {
        let opacity = self.scrollbar.get();
        let max = self.max_scroll();
        if opacity <= 0.0 || max <= 0.0 {
            return;
        }
        let theme = p.theme().clone();
        let track = self.viewport.h - 8.0;
        let thumb = (track * self.viewport.h / self.content_height).max(36.0);
        let y = 4.0 + (track - thumb) * (self.scroll.get() / max).clamp(0.0, 1.0);
        let rect = RectF::new(self.viewport.w - 9.0, y, 5.0, thumb);
        let ink = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.35) } else { Color::rgba(0.0, 0.0, 0.0, 0.32) };
        p.fill_round_rect(p.snap_rect(rect), 2.5, ink.multiply_alpha(opacity));
    }
}

const KEYCAP_GAP: f32 = 14.0;
const KEYCAP_HEIGHT: f32 = 22.0;
const KEYCAP_SPACING: f32 = 4.0;
const WIN_LOGO: f32 = 10.0;

fn keycap_style() -> TextStyle {
    TextStyle::new(11.5).weight(Weight::Medium)
}

fn keycap_width(gfx: &Gfx, key: &str) -> f32 {
    let content = if key == WIN_KEY { WIN_LOGO } else { gfx.measure_text(key, &keycap_style()).w };
    (content + 14.0).max(KEYCAP_HEIGHT).ceil()
}

fn keycaps_width(gfx: &Gfx, keys: &[String]) -> f32 {
    keys.iter().map(|k| keycap_width(gfx, k)).sum::<f32>() + KEYCAP_SPACING * keys.len().saturating_sub(1) as f32
}

/// Keyboard keys as small raised caps, right-aligned to `right`; the Windows key shows the Windows logo.
fn paint_keycaps(p: &mut Painter, keys: &[String], right: f32, center_y: f32) {
    let theme = p.theme().clone();
    let dark = theme.is_dark();
    let (fill, edge, base) = if dark {
        (Color::rgba(1.0, 1.0, 1.0, 0.09), Color::rgba(1.0, 1.0, 1.0, 0.10), Color::rgba(0.0, 0.0, 0.0, 0.45))
    } else {
        (Color::WHITE, Color::rgba(0.0, 0.0, 0.0, 0.13), Color::rgba(0.0, 0.0, 0.0, 0.10))
    };
    let mut x = right - keycaps_width(p.gfx(), keys);
    for key in keys {
        let width = keycap_width(p.gfx(), key);
        let cap = p.snap_rect(RectF::new(x, center_y - KEYCAP_HEIGHT / 2.0, width, KEYCAP_HEIGHT));
        p.fill_round_rect(cap.offset(0.0, 1.0), 5.0, base);
        p.fill_round_rect(cap, 5.0, fill);
        p.hairline_round_rect(cap, 5.0, edge, true);
        if key == WIN_KEY {
            let tile = (WIN_LOGO - 1.0) / 2.0;
            let origin = p.snap_point(PointF::new(cap.center().x - WIN_LOGO / 2.0, cap.center().y - WIN_LOGO / 2.0));
            for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let square = RectF::new(origin.x + dx * (tile + 1.0), origin.y + dy * (tile + 1.0), tile, tile);
                p.fill_rect(square, theme.text_secondary);
            }
        } else {
            p.text(key, &keycap_style().centered(), theme.text_secondary, cap);
        }
        x += width + KEYCAP_SPACING;
    }
}

/// Records whether a widget used the event and passes its result on.
fn respond<T>(consumed: &mut bool, response: Response<T>) -> Option<T> {
    *consumed |= response.consumed();
    response.action()
}

/// Shifts pointer positions from window to content coordinates.
fn shifted(event: &Event, dy: f32) -> Event {
    let mut event = event.clone();
    match &mut event {
        Event::PointerDown(e) | Event::PointerMove(e) | Event::PointerUp(e) => {
            e.pos.y += dy;
            for sample in &mut e.history {
                sample.pos.y += dy;
            }
        }
        Event::Wheel(w) => w.pos.y += dy,
        _ => {}
    }
    event
}

/// Ellipsizes the middle of `text` (paths keep their drive and folder name) to fit `max_width`.
pub fn shorten_middle(gfx: &Gfx, text: &str, style: &TextStyle, max_width: f32) -> String {
    if gfx.measure_text(text, style).w <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let candidate = |keep: usize| {
        let head = keep * 2 / 5;
        let tail = keep - head;
        let mut s: String = chars[..head].iter().collect();
        s.push('…');
        s.extend(&chars[chars.len() - tail..]);
        s
    };
    let (mut low, mut high) = (0usize, chars.len().saturating_sub(1));
    while low < high {
        let mid = (low + high).div_ceil(2);
        if gfx.measure_text(&candidate(mid), style).w <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    candidate(low)
}

impl View for SettingsView {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        if let Some((id, menu)) = &mut self.menu {
            let id = *id;
            if menu.is_open() {
                let response = menu.event(cx, event);
                if let Response::Action(index) = response {
                    self.edit(cx, id, Edit::Choose(index));
                }
                cx.request_paint();
                return true;
            }
            if !menu.is_visible() {
                self.menu = None;
            }
        }
        match event {
            Event::Wheel(wheel) => {
                let target = self.scroll_target - wheel.delta.y * WHEEL_STEP;
                self.scroll_to(cx, target, !wheel.precise);
                return true;
            }
            Event::KeyDown(key) => {
                let page = (self.viewport.h - 80.0).max(80.0);
                let target = match key.key {
                    Key::Down => self.scroll_target + 48.0,
                    Key::Up => self.scroll_target - 48.0,
                    Key::PageDown | Key::Space => self.scroll_target + page,
                    Key::PageUp => self.scroll_target - page,
                    Key::Home => 0.0,
                    Key::End => self.max_scroll(),
                    Key::Char('W') if key.mods == Modifiers::CTRL => {
                        cx.close();
                        return true;
                    }
                    _ => return false,
                };
                self.scroll_to(cx, target, true);
                return true;
            }
            Event::Timer(SCROLLBAR_TOKEN) => {
                self.scrollbar.set(0.0);
                cx.request_paint();
                return true;
            }
            Event::Resized => {
                self.viewport = cx.size();
                self.layout(cx.gfx(), cx.size().w);
                let target = self.scroll_target;
                self.scroll_to(cx, target, false);
                return true;
            }
            Event::CloseRequested => {
                cx.close();
                return true;
            }
            Event::Closed => {
                cx.post(SettingsMessage::Closed);
                return true;
            }
            _ => {}
        }
        if event.pointer_pos().is_some() || matches!(event, Event::PointerLeave | Event::PointerCancel) {
            let content_event = shifted(event, self.scroll.get());
            return self.route_to_widgets(cx, &content_event);
        }
        false
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.viewport = cx.size();
        self.layout(cx.gfx(), cx.size().w);
        let theme = p.theme().clone();
        if cx.is_offscreen() {
            p.fill_rect(p.bounds(), theme.window_background);
        }
        let scroll = self.scroll.get();
        p.translate(0.0, -scroll, |p| {
            self.paint_header(p);
            self.paint_sections(p);
        });
        self.paint_scrollbar(p);
        if let Some((_, menu)) = &mut self.menu {
            menu.paint(p, None);
        }
    }
}
