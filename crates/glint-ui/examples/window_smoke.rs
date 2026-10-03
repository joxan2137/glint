//! Opens real windows: a Mica editor-style window with a unified title bar, a menu and a popover, plus a
//! topmost, non-activating, capture-excluded HUD popup. Workers must not run this (it shows windows); it exists to
//! keep the windowed code paths compiling and as a usage reference.

use std::time::Duration;

use anyhow::Result;
use glint_ui::widgets::{
    Button, ButtonStyle, IconButton, Menu, MenuItem, Popover, Response, Segment, Segmented, SegmentedStyle, Slider,
    Toggle, Toolbar, ToolbarAction, ToolbarItem,
};
use glint_ui::{
    App, Ctx, Cursor, Event, HitArea, Icon, Key, Modifiers, Painter, PointF, PointI, RectF, SizeF, TextStyle,
    View, WindowSpec, run,
};

#[derive(Debug)]
enum SmokeEvent {
    Tick(u32),
    Quit,
}

struct EditorView {
    toolbar: Toolbar,
    menu: Menu,
    popover: Popover,
    exposure: Slider,
    mode: Segmented,
    toggle: Toggle,
    done: Button,
    status: String,
}

impl EditorView {
    fn new() -> Self {
        let tools = Segmented::new(
            vec![
                Segment::icon(Icon::MousePointer).tooltip("Select", Some("V")),
                Segment::icon(Icon::Pen).tooltip("Pen", Some("P")),
                Segment::icon(Icon::Highlighter).tooltip("Highlighter", Some("H")),
                Segment::icon(Icon::Crop).tooltip("Crop", Some("C")),
            ],
            1,
        );
        Self {
            toolbar: Toolbar::new(vec![
                ToolbarItem::button("new", IconButton::new(Icon::Scissors).label("New")),
                ToolbarItem::separator(),
                ToolbarItem::segmented("tools", tools),
                ToolbarItem::separator(),
                ToolbarItem::button("hdr", IconButton::new(Icon::Sun).tooltip("HDR", None)),
            ]),
            menu: Menu::new(vec![
                MenuItem::new("Rectangle").checked(true).shortcut("R"),
                MenuItem::new("Window").shortcut("W"),
                MenuItem::separator(),
                MenuItem::new("Full screen").shortcut("F"),
            ]),
            popover: Popover::new(SizeF::new(220.0, 72.0)),
            exposure: Slider::new(0.0, -2.0, 2.0).origin(0.0),
            mode: Segmented::new(vec![Segment::label("Auto"), Segment::label("Clip")], 0).style(SegmentedStyle::Track),
            toggle: Toggle::new(true),
            done: Button::new("Done", ButtonStyle::Primary).default_action(),
            status: "Ready".into(),
        }
    }

    fn title_bar(&self) -> RectF {
        RectF::new(0.0, 0.0, 10_000.0, 52.0)
    }
}

impl View for EditorView {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        if self.menu.is_open() {
            if let Response::Action(index) = self.menu.event(cx, event) {
                self.menu.check_only(index);
                self.status = format!("mode {index}");
            }
            return true;
        }
        if self.popover.is_open() {
            if let Some(value) = self.exposure.event(cx, event).action() {
                self.status = format!("exposure {value:+.1}");
            }
            self.mode.event(cx, event);
            return self.popover.event(cx, event).consumed();
        }
        match self.toolbar.event(cx, event) {
            Response::Action(ToolbarAction::Clicked("new")) => {
                let anchor = self.toolbar.item("new").map(|i| i.rect()).unwrap_or_default();
                self.menu.open(cx.gfx(), anchor, cx.size());
                return true;
            }
            Response::Action(ToolbarAction::Clicked("hdr")) => {
                let anchor = self.toolbar.item("hdr").map(|i| i.rect()).unwrap_or_default();
                self.popover.open(anchor, cx.size());
                return true;
            }
            Response::Action(ToolbarAction::Selected("tools", index)) => {
                self.status = format!("tool {index}");
                return true;
            }
            response if response.consumed() => return true,
            _ => {}
        }
        if let Some(on) = self.toggle.event(cx, event).action() {
            self.status = format!("toggle {on}");
        }
        if self.done.event(cx, event).action().is_some() {
            cx.close();
        }
        match event {
            Event::PointerMove(e) => cx.set_cursor(if e.pos.y > 52.0 { Cursor::Crosshair } else { Cursor::Arrow }),
            Event::KeyDown(k) if k.is(Key::Char('W'), Modifiers::CTRL) => cx.close(),
            Event::CloseRequested => cx.close(),
            Event::Timer(token) => self.status = format!("timer {token}"),
            Event::Shown => {
                cx.set_timer(Duration::from_secs(2), 7);
            }
            _ => return false,
        }
        true
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        let size = cx.size();
        let gfx = cx.gfx().clone();
        let buttons = cx.caption_buttons_rect().unwrap_or(RectF::new(size.w - 138.0, 0.0, 138.0, 32.0));
        self.toolbar.layout_centered(&gfx, (size.w - buttons.w) / 2.0, 4.0);
        self.toolbar.paint(p, None);
        let theme = p.theme().clone();
        p.text(&self.status, &TextStyle::body(), theme.text_secondary, RectF::new(16.0, 64.0, 400.0, 20.0));
        self.toggle.set_origin(PointF::new(16.0, 96.0));
        self.toggle.paint(p);
        let done = self.done.preferred_size(&gfx);
        self.done.set_rect(RectF::new(size.w - done.w - 16.0, size.h - done.h - 16.0, done.w, done.h));
        self.done.paint(p);
        self.menu.paint(p, None);
        let (exposure, mode) = (&mut self.exposure, &mut self.mode);
        self.popover.paint(p, None, |p, r| {
            mode.layout(p.gfx(), RectF::new(r.x, r.y, r.w, 28.0));
            mode.paint(p);
            exposure.set_rect(RectF::new(r.x, r.y + 44.0, r.w, 24.0));
            exposure.paint(p);
        });
    }

    fn hit_test(&self, pos: PointF) -> HitArea {
        if self.title_bar().contains(pos) && !self.toolbar.rect().contains(pos) { HitArea::Caption } else { HitArea::Client }
    }
}

struct HudView {
    ticks: u32,
}

impl View for HudView {
    fn event(&mut self, _cx: &mut Ctx, event: &Event) -> bool {
        matches!(event, Event::PointerDown(_))
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        let pill = RectF::new(8.0, 8.0, cx.size().w - 16.0, 44.0);
        p.glass(pill, 14.0, None);
        let theme = p.theme().clone();
        let pulse = 0.5 + 0.5 * (cx.time() * 4.0).cos() as f32;
        p.fill_circle(PointF::new(pill.x + 22.0, pill.center().y), 5.0, theme.destructive.with_alpha(0.5 + 0.5 * pulse));
        let label = format!("{:02}:{:02}", self.ticks / 60, self.ticks % 60);
        p.text(&label, &TextStyle::body().tabular(), theme.text, RectF::new(pill.x + 36.0, pill.y, 80.0, pill.h));
        cx.animate();
    }
}

fn main() -> Result<()> {
    glint_ui::enable_per_monitor_dpi_awareness();
    run(|app: &App| {
        app.set_quit_when_no_windows(true);
        let editor = WindowSpec::normal("Glint smoke test", SizeF::new(960.0, 600.0)).unified_title_bar().min_size(SizeF::new(760.0, 520.0));
        app.open(editor, EditorView::new())?;
        let hud = WindowSpec::popup(PointI::new(200, 40), SizeF::new(180.0, 60.0)).exclude_from_capture();
        let hud_id = app.open(hud, HudView { ticks: 0 })?;

        app.on_event(move |app: &App, event: SmokeEvent| match event {
            SmokeEvent::Tick(n) => {
                app.with_view(hud_id, |hud: &mut HudView, cx| {
                    hud.ticks = n;
                    cx.request_paint();
                });
            }
            SmokeEvent::Quit => app.quit(),
        });
        let proxy = app.proxy();
        std::thread::spawn(move || {
            for n in 1..=600 {
                std::thread::sleep(Duration::from_secs(1));
                if !proxy.post(SmokeEvent::Tick(n)) {
                    return;
                }
            }
            proxy.post(SmokeEvent::Quit);
        });
        app.set_timer(Duration::from_secs(3), |_| log::info!("three seconds"));
        Ok(())
    })
}
