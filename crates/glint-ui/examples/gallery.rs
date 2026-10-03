//! Offscreen kitchen-sink render of every widget and state, dark and light, at scale 1 and 2, plus mock overlay and
//! editor compositions (DESIGN §6, §8). No window is created.
//!
//! cargo run -p glint-ui --release --example gallery -- --out <dir>

use std::path::{Path as FsPath, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result};
use glint_ui::widgets::{
    Badge, BadgeStyle, Button, ButtonStyle, ColorSwatch, Countdown, IconButton, Menu, MenuItem, Popover, Response,
    Segment, Segmented, SegmentedStyle, Slider, Toggle, Toolbar, ToolbarAction, ToolbarItem, Tooltip,
};
use glint_ui::{
    Backdrop, Bitmap, Brush, Color, Ctx, Event, Gfx, Icon, Image, Interpolation, OffscreenSpec, Painter, PointF,
    RectF, SizeF, StrokeStyle, TextAlign, TextStyle, Theme, View, Weight, render_offscreen, render_view_offscreen,
};

fn main() -> Result<()> {
    glint_ui::enable_per_monitor_dpi_awareness();
    let out = output_dir();
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let gfx = Gfx::new()?;
    let (text_family, display_family) = gfx.font_families();
    println!("fonts: text = {text_family}, display = {display_family}; software = {}", gfx.is_software());
    for scale in [1.0f32, 2.0] {
        for theme in [Theme::dark(), Theme::light()] {
            let name = if theme.is_dark() { "dark" } else { "light" };
            save(&out, &format!("widgets-{name}@{scale}x.png"), &widgets_sheet(&gfx, &theme, scale)?)?;
            save(&out, &format!("editor-{name}@{scale}x.png"), &editor_mock(&gfx, &theme, scale)?)?;
        }
        save(&out, &format!("overlay@{scale}x.png"), &overlay_mock(&gfx, scale)?)?;
        for theme in [Theme::dark(), Theme::light()] {
            let name = if theme.is_dark() { "dark" } else { "light" };
            let mut hud = HudView::new();
            let spec = OffscreenSpec::new(SizeF::new(360.0, 96.0), scale, theme).time(12.4);
            save(&out, &format!("hud-view-{name}@{scale}x.png"), &render_view_offscreen(&gfx, &mut hud, &spec)?)?;
        }
    }
    for theme in [Theme::dark(), Theme::light()] {
        let name = if theme.is_dark() { "dark" } else { "light" };
        save(&out, &format!("closeup-toolbar-{name}@4x.png"), &closeup_toolbar(&gfx, &theme)?)?;
    }
    println!("wrote {}", out.display());
    Ok(())
}

fn output_dir() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("glint-ui-gallery"))
}

fn save(dir: &FsPath, name: &str, image: &Image) -> Result<()> {
    let path = dir.join(name);
    let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let rgba: Vec<u8> = image.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    writer.write_image_data(&rgba)?;
    println!("  {}", path.display());
    Ok(())
}

/// Busy, colorful synthetic background so frosted glass is visible.
fn paint_busy_background(p: &mut Painter, size: SizeF) {
    let bounds = RectF::new(0.0, 0.0, size.w, size.h);
    p.fill_rect(
        bounds,
        Brush::linear(
            PointF::new(0.0, 0.0),
            PointF::new(size.w, size.h),
            &[
                (0.0, Color::hex("#1B3A6B").unwrap()),
                (0.35, Color::hex("#6A2C91").unwrap()),
                (0.7, Color::hex("#C2185B").unwrap()),
                (1.0, Color::hex("#F57C00").unwrap()),
            ],
        ),
    );
    let blobs = [
        ("#00BCD4", 0.12, 0.10, 0.16),
        ("#FFEB3B", 0.78, 0.06, 0.12),
        ("#4CAF50", 0.30, 0.42, 0.14),
        ("#FF5722", 0.86, 0.48, 0.18),
        ("#3F51B5", 0.55, 0.30, 0.10),
        ("#E91E63", 0.10, 0.80, 0.15),
        ("#8BC34A", 0.66, 0.86, 0.13),
        ("#03A9F4", 0.40, 0.95, 0.12),
    ];
    for (hex, x, y, r) in blobs {
        let c = Color::hex(hex).unwrap();
        p.fill_circle(PointF::new(size.w * x, size.h * y), size.w.min(1400.0) * r, c.with_alpha(0.85));
    }
    let stripe = Color::rgba(1.0, 1.0, 1.0, 0.10);
    let mut x = -size.h;
    while x < size.w {
        p.line(PointF::new(x, size.h), PointF::new(x + size.h, 0.0), stripe, 10.0, &StrokeStyle::default());
        x += 48.0;
    }
    let words = TextStyle::new(28.0).weight(Weight::Bold);
    let mut y = 60.0;
    while y < size.h {
        p.text("Glint · HDR · Snip · Record · Markup", &words, Color::rgba(1.0, 1.0, 1.0, 0.18), RectF::new(20.0, y, size.w, 40.0));
        y += 140.0;
    }
}

struct Canvas {
    background: Rc<Bitmap>,
    backdrop: Rc<Bitmap>,
    size: SizeF,
}

impl Canvas {
    fn new(gfx: &Rc<Gfx>, theme: &Theme, size: SizeF, scale: f32, paint: impl FnOnce(&mut Painter, SizeF)) -> Result<Self> {
        let spec = OffscreenSpec::new(size, scale, theme.clone());
        let image = render_offscreen(gfx, &spec, |_, p| paint(p, size))?;
        let background = Bitmap::new(image);
        let backdrop = background.glass_backdrop(theme, scale);
        Ok(Self { background, backdrop, size })
    }

    fn draw(&self, p: &mut Painter) {
        p.bitmap(&self.background, RectF::new(0.0, 0.0, self.size.w, self.size.h), None, 1.0, Interpolation::Linear);
    }

    fn backdrop(&self) -> Backdrop<'_> {
        Backdrop::new(&self.backdrop, RectF::new(0.0, 0.0, self.size.w, self.size.h))
    }
}

fn section(p: &mut Painter, title: &str, y: f32) {
    let style = TextStyle::new(13.0).weight(Weight::Semibold);
    let shadow = Color::rgba(0.0, 0.0, 0.0, 0.35);
    p.text(title, &style, shadow, RectF::new(41.0, y + 1.0, 600.0, 20.0));
    p.text(title, &style, Color::WHITE, RectF::new(40.0, y, 600.0, 20.0));
}

fn caption(p: &mut Painter, text: &str, center_x: f32, y: f32) {
    let style = TextStyle::caption().weight(Weight::Medium).centered();
    let rect = RectF::new(center_x - 150.0, y, 300.0, 16.0);
    p.text(text, &style, Color::rgba(0.0, 0.0, 0.0, 0.45), rect.offset(0.0, 1.0));
    p.text(text, &style, Color::WHITE, rect);
}

fn panel(p: &mut Painter, canvas: &Canvas, rect: RectF) {
    p.glass(rect, 14.0, Some(&canvas.backdrop()));
}

fn capture_toolbar(gfx: &Gfx) -> Toolbar {
    let modes = Segmented::new(
        vec![
            Segment::icon(Icon::SquareDashed).tooltip("Rectangle", Some("R")),
            Segment::icon(Icon::AppWindow).tooltip("Window", Some("W")),
            Segment::icon(Icon::Monitor).tooltip("Full screen", Some("F")),
            Segment::icon(Icon::Lasso).tooltip("Freeform", Some("L")),
        ],
        0,
    );
    let media = Segmented::new(
        vec![Segment::icon(Icon::Camera).tooltip("Photo", None), Segment::icon(Icon::Video).tooltip("Video", Some("V"))],
        0,
    );
    let mut toolbar = Toolbar::new(vec![
        ToolbarItem::segmented("mode", modes),
        ToolbarItem::separator(),
        ToolbarItem::button("text", IconButton::new(Icon::ScanText).tooltip("Text", Some("T"))),
        ToolbarItem::button("color", IconButton::new(Icon::Pipette).tooltip("Color", Some("C"))),
        ToolbarItem::separator(),
        ToolbarItem::button("timer", IconButton::new(Icon::Timer).tooltip("Delay", None)),
        ToolbarItem::separator(),
        ToolbarItem::segmented("media", media),
        ToolbarItem::separator(),
        ToolbarItem::button("close", IconButton::new(Icon::X).tooltip("Close", Some("Esc"))),
    ]);
    toolbar.layout_at(gfx, PointF::new(0.0, 0.0));
    toolbar
}

fn widgets_sheet(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let size = SizeF::new(1200.0, 1780.0);
    let canvas = Canvas::new(gfx, theme, size, scale, paint_busy_background)?;
    let spec = OffscreenSpec::new(size, scale, theme.clone());
    render_offscreen(gfx, &spec, |_, p| {
        canvas.draw(p);
        let mut y = 28.0;
        let title = TextStyle::large_title();
        p.text(
            if theme.is_dark() { "glint-ui · dark" } else { "glint-ui · light" },
            &title,
            Color::WHITE,
            RectF::new(40.0, y, 600.0, 32.0),
        );
        p.text(&format!("scale {scale}"), &TextStyle::body().align(TextAlign::Trailing), Color::WHITE, RectF::new(600.0, y, 560.0, 32.0));
        y += 60.0;

        section(p, "Toolbar (overlay capture modes)", y);
        let mut toolbar = capture_toolbar(p.gfx());
        let w = toolbar.preferred_size(p.gfx()).w;
        toolbar.layout_at(p.gfx(), PointF::new(40.0, y + 28.0));
        if let Some(close) = toolbar.button_mut("close") {
            close.force_state(true, false);
        }
        toolbar.paint(p, Some(&canvas.backdrop()));
        let tip_anchor = toolbar.items()[0].rect();
        Tooltip::paint_bubble(p, RectF::new(tip_anchor.x, tip_anchor.y, 32.0, 32.0), "Rectangle", Some("R"), 1.0, size);
        let mut plain = capture_toolbar(p.gfx());
        plain.layout_at(p.gfx(), PointF::new(80.0 + w, y + 28.0));
        plain.paint(p, None);
        caption(p, "with backdrop · hover ✕ · tooltip", 40.0 + w / 2.0, y + 112.0);
        caption(p, "without backdrop (solid glass)", 80.0 + w * 1.5, y + 112.0);
        y += 150.0;

        section(p, "Icon buttons: normal · hover · pressed · selected · disabled · tinted · label", y);
        panel(p, &canvas, RectF::new(40.0, y + 28.0, 560.0, 64.0));
        let mut x = 56.0;
        for name in ["normal", "hover", "pressed", "selected", "disabled", "tinted", "label"] {
            let mut b = match name {
                "tinted" => IconButton::new(Icon::RecordFill).tint(theme.destructive),
                "label" => IconButton::new(Icon::Download).label("Save"),
                "selected" => IconButton::new(Icon::Pen).with_selected(true),
                "disabled" => IconButton::new(Icon::Pen).disabled(),
                _ => IconButton::new(Icon::Pen),
            };
            match name {
                "hover" => b.force_state(true, false),
                "pressed" => b.force_state(true, true),
                _ => {}
            }
            let bs = b.preferred_size(p.gfx());
            b.set_rect(RectF::new(x, y + 44.0, bs.w, bs.h));
            b.paint(p);
            caption(p, name, x + bs.w / 2.0, y + 100.0);
            x += bs.w + 40.0;
        }
        y += 130.0;

        section(p, "Segmented: toolbar style (hover on 2nd) · track style", y);
        panel(p, &canvas, RectF::new(40.0, y + 28.0, 146.0, 44.0));
        let mut seg = Segmented::new(
            vec![Segment::icon(Icon::SquareDashed), Segment::icon(Icon::AppWindow), Segment::icon(Icon::Monitor), Segment::icon(Icon::Lasso)],
            0,
        );
        seg.layout(p.gfx(), RectF::new(46.0, y + 34.0, 134.0, 32.0));
        seg.force_hover(Some(1), false);
        seg.paint(p);
        panel(p, &canvas, RectF::new(248.0, y + 22.0, 520.0, 56.0));
        let mut track = Segmented::new(vec![Segment::label("Auto"), Segment::label("Clip")], 0).style(SegmentedStyle::Track);
        let ts = track.preferred_size(p.gfx());
        track.layout(p.gfx(), RectF::new(264.0, y + 36.0, ts.w, ts.h));
        track.paint(p);
        let mut track2 = Segmented::new(
            vec![Segment::icon_label(Icon::Camera, "Photo"), Segment::icon_label(Icon::Video, "Video")],
            1,
        )
        .style(SegmentedStyle::Track);
        let ts2 = track2.preferred_size(p.gfx());
        track2.layout(p.gfx(), RectF::new(288.0 + ts.w, y + 36.0, ts2.w, ts2.h));
        track2.paint(p);
        let mut dots = Segmented::new(vec![Segment::dot(4.0), Segment::dot(7.0), Segment::dot(11.0)], 1);
        let ds = dots.preferred_size(p.gfx());
        dots.layout(p.gfx(), RectF::new(312.0 + ts.w + ts2.w, y + 34.0, ds.w, ds.h));
        dots.paint(p);
        y += 110.0;

        section(p, "Buttons: primary · secondary · plain · destructive (normal / hover / pressed / disabled)", y);
        panel(p, &canvas, RectF::new(40.0, y + 28.0, 760.0, 168.0));
        let styles = [
            ("Done", ButtonStyle::Primary),
            ("Cancel", ButtonStyle::Secondary),
            ("Reset", ButtonStyle::Plain),
            ("Discard", ButtonStyle::Destructive),
        ];
        for (row, (label, style)) in styles.iter().enumerate() {
            let by = y + 40.0 + row as f32 * 38.0;
            for (col, state) in ["normal", "hover", "pressed", "disabled"].iter().enumerate() {
                let mut b = Button::new(label, *style);
                if row == 1 && col == 0 {
                    b = b.icon(Icon::Copy);
                }
                match *state {
                    "hover" => b.force_state(true, false),
                    "pressed" => b.force_state(true, true),
                    "disabled" => b.enabled = false,
                    _ => {}
                }
                let bs = b.preferred_size(p.gfx());
                b.set_rect(RectF::new(64.0 + col as f32 * 180.0, by, bs.w.max(110.0), bs.h));
                b.paint(p);
            }
        }
        y += 220.0;

        section(p, "Toggles · sliders", y);
        panel(p, &canvas, RectF::new(40.0, y + 28.0, 760.0, 72.0));
        let toggle_states = [(false, false, true), (true, false, true), (true, true, true), (false, false, false), (true, false, false)];
        for (i, (on, pressed, enabled)) in toggle_states.iter().enumerate() {
            let mut t = Toggle::new(*on);
            t.enabled = *enabled;
            t.set_origin(PointF::new(64.0 + i as f32 * 60.0, y + 52.0));
            t.force_state(*pressed, *pressed);
            t.paint(p);
        }
        let mut s1 = Slider::new(-0.6, -2.0, 2.0).origin(0.0);
        s1.set_rect(RectF::new(380.0, y + 52.0, 180.0, Slider::HEIGHT));
        s1.paint(p);
        let mut s2 = Slider::new(0.35, 0.0, 1.0);
        s2.set_rect(RectF::new(588.0, y + 52.0, 180.0, Slider::HEIGHT));
        s2.paint(p);
        y += 130.0;

        section(p, "Menu (checkmarks, highlight, separator, shortcuts) · tooltip · popover", y);
        let mut menu = Menu::new(vec![
            MenuItem::new("No delay").checked(true),
            MenuItem::new("3 seconds"),
            MenuItem::new("5 seconds"),
            MenuItem::new("10 seconds"),
            MenuItem::separator(),
            MenuItem::new("Show magnifier").icon(Icon::ZoomIn).shortcut("M"),
            MenuItem::new("Settings…").icon(Icon::Settings).shortcut("Ctrl+,"),
            MenuItem::new("Disabled item").disabled(),
        ]);
        let anchor = RectF::new(40.0, y + 24.0, 32.0, 4.0);
        menu.force_open(p.gfx(), anchor, size, Some(2));
        menu.paint(p, Some(&canvas.backdrop()));
        let menu_right = menu.frame().right();
        Tooltip::paint_bubble(p, RectF::new(menu_right + 40.0, y + 30.0, 32.0, 32.0), "Full screen", Some("F"), 1.0, size);
        Tooltip::paint_bubble(p, RectF::new(menu_right + 40.0, y + 80.0, 32.0, 32.0), "Copy to clipboard", Some("Ctrl+C"), 1.0, size);
        let mut popover = Popover::new(SizeF::new(240.0, 116.0));
        popover.force_open(RectF::new(menu_right + 400.0, y + 28.0, 52.0, 24.0), size);
        Badge::new("HDR").style(BadgeStyle::Accent).paint(p, RectF::new(menu_right + 400.0, y + 28.0, 52.0, 24.0));
        popover.paint(p, Some(&canvas.backdrop()), |p, r| {
            let theme = p.theme().clone();
            p.text("Tone mapping", &TextStyle::emphasized(), theme.text, RectF::new(r.x, r.y, r.w, 20.0));
            let mut mode = Segmented::new(vec![Segment::label("Auto"), Segment::label("Clip")], 0).style(SegmentedStyle::Track);
            mode.layout(p.gfx(), RectF::new(r.x, r.y + 28.0, r.w, 28.0));
            mode.paint(p);
            p.text("Exposure", &TextStyle::body(), theme.text_secondary, RectF::new(r.x, r.y + 68.0, r.w, 20.0));
            p.text("+0.5 EV", &TextStyle::body().tabular().align(TextAlign::Trailing), theme.text_secondary, RectF::new(r.x, r.y + 68.0, r.w, 20.0));
            let mut slider = Slider::new(0.5, -2.0, 2.0).origin(0.0);
            slider.set_rect(RectF::new(r.x - 2.0, r.y + 92.0, r.w + 4.0, Slider::HEIGHT));
            slider.paint(p);
        });
        y += 320.0;

        section(p, "Swatches (selected, hover) · custom well · badges · countdown", y);
        panel(p, &canvas, RectF::new(40.0, y + 28.0, 420.0, 52.0));
        let colors = ["#FF3B30", "#FF9500", "#FFCC00", "#34C759", "#007AFF", "#AF52DE", "#000000", "#FFFFFF"];
        for (i, hex) in colors.iter().enumerate() {
            let mut s = ColorSwatch::new(Color::hex(hex).unwrap()).with_selected(i == 4);
            if i == 1 {
                s.force_state(true, false);
            }
            s.set_center(PointF::new(68.0 + i as f32 * 36.0, y + 54.0));
            s.paint(p);
        }
        let mut well = ColorSwatch::well(None);
        well.set_center(PointF::new(68.0 + 8.0 * 36.0 + 8.0, y + 54.0));
        well.paint(p);
        let mut well2 = ColorSwatch::well(Some(Color::hex("#5AC8FA").unwrap())).with_selected(true);
        well2.set_center(PointF::new(68.0 + 9.0 * 36.0 + 8.0, y + 54.0));
        well2.paint(p);
        let mut bx = 490.0;
        for badge in [
            Badge::new("1280 × 720"),
            Badge::new("HDR").style(BadgeStyle::Accent),
            Badge::new("100 %").style(BadgeStyle::Subtle),
            Badge::new("REC").style(BadgeStyle::Destructive).icon(Icon::RecordFill),
        ] {
            let bs = badge.size(p.gfx());
            badge.paint(p, RectF::new(bx, y + 42.0, bs.w, bs.h));
            bx += bs.w + 16.0;
        }
        let pill = RectF::new(880.0, y + 20.0, 120.0, 120.0);
        p.glass(pill, 28.0, Some(&canvas.backdrop()));
        let mut countdown = Countdown::new();
        countdown.force(None, 3, 1.0);
        countdown.paint(p, pill.center(), p.theme().text);
        let pill2 = RectF::new(1030.0, y + 20.0, 120.0, 120.0);
        p.glass(pill2, 28.0, Some(&canvas.backdrop()));
        let mut transition = Countdown::new();
        transition.force(Some(3), 2, 0.45);
        transition.paint(p, pill2.center(), p.theme().text);
        y += 160.0;

        section(p, &format!("Icons ({}) — 18 DIP, stroke 1.75 on the 24 grid", Icon::ALL.len()), y);
        let per_row = 14;
        let rows = Icon::ALL.len().div_ceil(per_row);
        let grid = RectF::new(40.0, y + 28.0, 1120.0, rows as f32 * 64.0 + 16.0);
        panel(p, &canvas, grid);
        let theme = p.theme().clone();
        for (i, icon) in Icon::ALL.iter().enumerate() {
            let col = (i % per_row) as f32;
            let row = (i / per_row) as f32;
            let cx = grid.x + 40.0 + col * 80.0;
            let cy = grid.y + 28.0 + row * 64.0;
            p.icon(*icon, PointF::new(cx, cy), 18.0, theme.text);
            let style = TextStyle::new(10.0).centered();
            p.text(icon.name(), &style, theme.text_secondary, RectF::new(cx - 40.0, cy + 16.0, 80.0, 14.0));
        }
    })
}

fn closeup_toolbar(gfx: &Rc<Gfx>, theme: &Theme) -> Result<Image> {
    let size = SizeF::new(280.0, 92.0);
    let scale = 4.0;
    let canvas = Canvas::new(gfx, theme, size, scale, paint_busy_background)?;
    let spec = OffscreenSpec::new(size, scale, theme.clone());
    render_offscreen(gfx, &spec, |_, p| {
        canvas.draw(p);
        let mut toolbar = capture_toolbar(p.gfx());
        toolbar.layout_at(p.gfx(), PointF::new(24.0, 24.0));
        if let Some(text) = toolbar.button_mut("text") {
            text.force_state(true, false);
        }
        toolbar.paint(p, Some(&canvas.backdrop()));
    })
}

/// A fake desktop: wallpaper, app windows and a taskbar.
fn paint_fake_desktop(p: &mut Painter, size: SizeF) {
    p.fill_rect(
        RectF::new(0.0, 0.0, size.w, size.h),
        Brush::linear(
            PointF::new(0.0, 0.0),
            PointF::new(size.w * 0.6, size.h),
            &[(0.0, Color::hex("#0F2C59").unwrap()), (0.5, Color::hex("#3A6EA5").unwrap()), (1.0, Color::hex("#F2A65A").unwrap())],
        ),
    );
    p.fill_circle(PointF::new(size.w * 0.82, size.h * 0.22), 220.0, Color::hex("#FFD166").unwrap().with_alpha(0.55));
    let window = |p: &mut Painter, r: RectF, title: &str, accent: Color| {
        p.shadow(r, 8.0, &glint_ui::Shadow::new(12.0, 40.0, Color::rgba(0.0, 0.0, 0.0, 0.35)));
        p.fill_round_rect(r, 8.0, Color::hex("#FAFAFA").unwrap());
        p.clip_round_rect(r, 8.0, |p| {
            p.fill_rect(RectF::new(r.x, r.y, r.w, 36.0), Color::hex("#EDEDED").unwrap());
            p.text(title, &TextStyle::body(), Color::rgba(0.0, 0.0, 0.0, 0.8), RectF::new(r.x + 14.0, r.y, r.w - 28.0, 36.0));
            p.fill_rect(RectF::new(r.x + 16.0, r.y + 52.0, r.w * 0.45, 120.0), accent);
            for i in 0..8 {
                let w = r.w * (0.35 + 0.05 * ((i * 7) % 5) as f32);
                p.fill_round_rect(
                    RectF::new(r.x + r.w * 0.5 + 8.0, r.y + 56.0 + i as f32 * 18.0, w * 0.8, 8.0),
                    4.0,
                    Color::rgba(0.0, 0.0, 0.0, 0.12),
                );
            }
            p.text(
                "The quick brown fox jumps over the lazy dog. 0123456789",
                &TextStyle::body(),
                Color::rgba(0.0, 0.0, 0.0, 0.75),
                RectF::new(r.x + 16.0, r.y + 190.0, r.w - 32.0, 20.0),
            );
        });
        p.stroke_round_rect(r, 8.0, Color::rgba(0.0, 0.0, 0.0, 0.2), 1.0);
    };
    window(p, RectF::new(120.0, 140.0, 620.0, 420.0), "Photos — Lake Tahoe.heic", Color::hex("#2E86AB").unwrap());
    window(p, RectF::new(640.0, 260.0, 560.0, 380.0), "Quarterly report.xlsx", Color::hex("#43AA8B").unwrap());
    p.fill_rect(RectF::new(0.0, size.h - 48.0, size.w, 48.0), Color::rgba(0.12, 0.12, 0.14, 0.92));
    for i in 0..7 {
        let c = PointF::new(size.w / 2.0 - 120.0 + i as f32 * 40.0, size.h - 24.0);
        p.fill_round_rect(RectF::new(c.x - 14.0, c.y - 14.0, 28.0, 28.0), 6.0, Color::hex(["#0078D4", "#FFB900", "#E74856", "#0099BC", "#7A7574", "#10893E", "#8764B8"][i]).unwrap());
    }
}

fn overlay_mock(gfx: &Rc<Gfx>, scale: f32) -> Result<Image> {
    let size = SizeF::new(1440.0, 900.0);
    let theme = Theme::dark();
    let desktop_spec = OffscreenSpec::new(size, scale, theme.clone());
    let desktop = Bitmap::new(render_offscreen(gfx, &desktop_spec, |_, p| paint_fake_desktop(p, size))?);
    let backdrop = desktop.glass_backdrop(&theme, scale);
    let spec = OffscreenSpec::new(size, scale, theme.clone());
    render_offscreen(gfx, &spec, |_, p| {
        let full = RectF::new(0.0, 0.0, size.w, size.h);
        p.bitmap(&desktop, full, None, 1.0, Interpolation::Nearest);
        let sel = RectF::new(360.0, 220.0, 640.0, 360.0);
        let dim = theme.overlay_dim;
        p.fill_rect(RectF::from_ltrb(0.0, 0.0, size.w, sel.y), dim);
        p.fill_rect(RectF::from_ltrb(0.0, sel.bottom(), size.w, size.h), dim);
        p.fill_rect(RectF::from_ltrb(0.0, sel.y, sel.x, sel.bottom()), dim);
        p.fill_rect(RectF::from_ltrb(sel.right(), sel.y, size.w, sel.bottom()), dim);
        let px = p.px();
        p.stroke_rect(sel.inset(-px * 1.5), Color::rgba(0.0, 0.0, 0.0, 0.25), px);
        p.stroke_rect(sel.inset(-px * 0.5), Color::WHITE, px);
        let label = format!("{} × {}", (sel.w * scale) as i32, (sel.h * scale) as i32);
        Badge::new(&label).paint_centered(p, PointF::new(sel.center().x, sel.bottom() + 24.0));

        let mut toolbar = capture_toolbar(p.gfx());
        toolbar.layout_centered(p.gfx(), size.w / 2.0, 24.0);
        if let Some(timer) = toolbar.button_mut("timer") {
            timer.force_state(true, false);
        }
        let back = Backdrop::new(&backdrop, full).dimmed(theme.overlay_dim);
        toolbar.paint(p, Some(&back));
        let timer_rect = toolbar.item("timer").map(|i| i.rect()).unwrap_or_default();
        Tooltip::paint_bubble(p, timer_rect, "Delay", None, 1.0, size);

        let cursor = PointF::new(sel.right(), sel.bottom());
        paint_magnifier(p, &desktop, cursor, scale);
    })
}

fn paint_magnifier(p: &mut Painter, source: &Bitmap, cursor: PointF, scale: f32) {
    let diameter = 112.0;
    let zoom = 8.0;
    let center = p.snap_point(PointF::new(cursor.x + 24.0 + diameter / 2.0, cursor.y + 24.0 + diameter / 2.0));
    let circle = RectF::new(center.x - diameter / 2.0, center.y - diameter / 2.0, diameter, diameter);
    let half = (diameter / zoom / 2.0).ceil();
    let cells = 2.0 * half + 1.0;
    let src_px = PointF::new((cursor.x * scale).floor(), (cursor.y * scale).floor());
    let src = RectF::new(src_px.x - half, src_px.y - half, cells, cells);
    let dest = RectF::new(center.x - (half + 0.5) * zoom, center.y - (half + 0.5) * zoom, cells * zoom, cells * zoom);
    p.shadow(circle, diameter / 2.0, &glint_ui::Shadow::new(6.0, 20.0, Color::rgba(0.0, 0.0, 0.0, 0.45)));
    p.clip_round_rect(circle, diameter / 2.0, |p| {
        p.bitmap(source, dest, Some(src), 1.0, Interpolation::Nearest);
        let grid = Color::rgba(0.0, 0.0, 0.0, 0.10);
        for i in 0..=cells as i32 {
            let x = p.snap(dest.x + i as f32 * zoom);
            let y = p.snap(dest.y + i as f32 * zoom);
            p.fill_rect(RectF::new(x, dest.y, p.px(), dest.h), grid);
            p.fill_rect(RectF::new(dest.x, y, dest.w, p.px()), grid);
        }
        let cell = RectF::new(center.x - zoom / 2.0, center.y - zoom / 2.0, zoom, zoom);
        p.stroke_rect(cell.inset(-p.px()), Color::BLACK, p.px() * 2.0);
        p.stroke_rect(cell.inset(-p.px() * 0.5), Color::WHITE, p.px());
    });
    p.stroke_ellipse(center, diameter / 2.0 - 1.0, diameter / 2.0 - 1.0, Color::WHITE, 2.0);
    p.stroke_ellipse(center, diameter / 2.0 + p.px() / 2.0, diameter / 2.0 + p.px() / 2.0, Color::rgba(0.0, 0.0, 0.0, 0.35), p.px());
    let pixel = source
        .image()
        .map(|img| img.pixel((src_px.x as u32).min(img.width - 1), (src_px.y as u32).min(img.height - 1)))
        .unwrap_or([0, 0, 0, 255]);
    let color = Color::from_bgra8(pixel);
    let info = format!("X {}  Y {}   {}", src_px.x as i32, src_px.y as i32, color.to_hex());
    Badge::new(&info).chip(color).paint_centered(p, PointF::new(center.x, circle.bottom() + 22.0));
}

fn editor_mock(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let size = SizeF::new(1200.0, 760.0);
    let spec = OffscreenSpec::new(size, scale, theme.clone());
    let shot_size = SizeF::new(900.0, 520.0);
    let shot_spec = OffscreenSpec::new(shot_size, scale, Theme::dark());
    let screenshot = Bitmap::new(render_offscreen(gfx, &shot_spec, |_, p| paint_fake_desktop(p, SizeF::new(1440.0, 900.0)))?);
    render_offscreen(gfx, &spec, |_, p| {
        let theme = p.theme().clone();
        let mica = if theme.is_dark() {
            Brush::linear(PointF::new(0.0, 0.0), PointF::new(size.w, size.h), &[(0.0, Color::hex("#202027").unwrap()), (1.0, Color::hex("#1C1F26").unwrap())])
        } else {
            Brush::linear(PointF::new(0.0, 0.0), PointF::new(size.w, size.h), &[(0.0, Color::hex("#EEF0F5").unwrap()), (1.0, Color::hex("#F3EFF2").unwrap())])
        };
        p.fill_rect(RectF::new(0.0, 0.0, size.w, size.h), mica);

        let caption_w = 46.0 * 3.0;
        let caption_color = theme.text_secondary;
        for (i, icon) in [Icon::Minus, Icon::Square, Icon::X].iter().enumerate() {
            let c = PointF::new(size.w - caption_w + 23.0 + i as f32 * 46.0, 16.0);
            p.icon_with_stroke(*icon, c, 12.0, caption_color, 1.6);
        }

        let mut new = IconButton::new(Icon::Scissors).label("New");
        let ns = new.preferred_size(p.gfx());
        new.set_rect(RectF::new(12.0, 10.0, ns.w, 32.0));
        new.paint(p);
        p.icon_with_stroke(Icon::ChevronDown, PointF::new(12.0 + ns.w + 6.0, 26.0), 12.0, theme.text_secondary, 2.0);
        let mut delay = IconButton::new(Icon::Timer).tooltip("Delay", None);
        delay.set_rect(RectF::new(12.0 + ns.w + 20.0, 10.0, 32.0, 32.0));
        delay.paint(p);

        let tools = Segmented::new(
            vec![
                Segment::icon(Icon::MousePointer).tooltip("Select", Some("V")),
                Segment::icon(Icon::Pen).tooltip("Pen", Some("P")),
                Segment::icon(Icon::Highlighter).tooltip("Highlighter", Some("H")),
                Segment::icon(Icon::Eraser).tooltip("Eraser", Some("E")),
                Segment::icon(Icon::Square).tooltip("Shapes", Some("S")),
                Segment::icon(Icon::Type).tooltip("Text", Some("T")),
                Segment::icon(Icon::Redact).tooltip("Redact", Some("B")),
                Segment::icon(Icon::Crop).tooltip("Crop", Some("C")),
            ],
            4,
        );
        let mut pill = Toolbar::new(vec![
            ToolbarItem::segmented("tools", tools),
            ToolbarItem::separator(),
            ToolbarItem::button("undo", IconButton::new(Icon::Undo).tooltip("Undo", Some("Ctrl+Z"))),
            ToolbarItem::button("redo", IconButton::new(Icon::Redo).tooltip("Redo", Some("Ctrl+Y")).disabled()),
        ]);
        pill.layout_centered(p.gfx(), size.w / 2.0, 4.0);
        pill.paint(p, None);

        let mut x = size.w - caption_w - 8.0;
        for (icon, label) in [(Icon::Share, None), (Icon::Download, Some("Save")), (Icon::Copy, Some("Copy")), (Icon::ScanText, Some("Text"))] {
            let mut b = IconButton::new(icon);
            if let Some(l) = label {
                b = b.label(l);
            }
            let bs = b.preferred_size(p.gfx());
            x -= bs.w;
            b.set_rect(RectF::new(x, 10.0, bs.w, 32.0));
            if label == Some("Copy") {
                b.force_state(true, false);
            }
            b.paint(p);
            x -= 4.0;
        }

        let canvas_rect = RectF::new(150.0, 128.0, 900.0, 520.0);
        p.shadow(canvas_rect, 6.0, &glint_ui::Shadow::new(8.0, 28.0, Color::rgba(0.0, 0.0, 0.0, if theme.is_dark() { 0.5 } else { 0.18 })));
        p.clip_round_rect(canvas_rect, 6.0, |p| {
            p.bitmap(&screenshot, canvas_rect, None, 1.0, Interpolation::Cubic);
            let arrow_color = Color::hex("#FF3B30").unwrap();
            p.line(PointF::new(420.0, 470.0), PointF::new(560.0, 360.0), arrow_color, 5.0, &StrokeStyle::round());
            p.stroke_round_rect(RectF::new(620.0, 300.0, 240.0, 140.0), 4.0, Color::hex("#007AFF").unwrap(), 4.0);
        });
        p.hairline_round_rect(canvas_rect, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.2), false);

        let colors = ["#FF3B30", "#FF9500", "#FFCC00", "#34C759", "#007AFF", "#AF52DE", "#000000", "#FFFFFF"];
        const SWATCH_IDS: [&str; 9] = ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "custom"];
        let mut items: Vec<ToolbarItem> = SWATCH_IDS.iter().map(|id| ToolbarItem::custom(id, SizeF::new(32.0, 32.0))).collect();
        items.push(ToolbarItem::separator());
        items.push(ToolbarItem::segmented("size", Segmented::new(vec![Segment::dot(4.0), Segment::dot(7.0), Segment::dot(11.0)], 1)));
        items.push(ToolbarItem::separator());
        items.push(ToolbarItem::segmented(
            "shape",
            Segmented::new(
                vec![Segment::icon(Icon::Square), Segment::icon(Icon::Circle), Segment::icon(Icon::Line), Segment::icon(Icon::Arrow)],
                0,
            ),
        ));
        items.push(ToolbarItem::separator());
        items.push(ToolbarItem::button("fill", IconButton::new(Icon::PaintBucket).tooltip("Fill", None)));
        let mut options = Toolbar::new(items);
        options.layout_centered(p.gfx(), size.w / 2.0, 60.0);
        options.paint(p, None);
        for (i, id) in SWATCH_IDS.iter().enumerate() {
            let center = options.item(id).map(|item| item.rect().center()).unwrap_or_default();
            let mut swatch = match colors.get(i) {
                Some(hex) => ColorSwatch::new(Color::hex(hex).unwrap()).with_selected(i == 0),
                None => ColorSwatch::well(None),
            };
            swatch.set_center(center);
            swatch.paint(p);
        }

        let zoom_bar = RectF::new(size.w / 2.0 - 170.0, size.h - 64.0, 340.0, 44.0);
        p.glass(zoom_bar, 14.0, None);
        let mut minus = IconButton::new(Icon::Minus);
        minus.set_rect(RectF::new(zoom_bar.x + 6.0, zoom_bar.y + 6.0, 32.0, 32.0));
        minus.paint(p);
        p.text("100 %", &TextStyle::body().weight(Weight::Medium).tabular().centered(), theme.text, RectF::new(zoom_bar.x + 38.0, zoom_bar.y, 56.0, 44.0));
        let mut plus = IconButton::new(Icon::Plus);
        plus.set_rect(RectF::new(zoom_bar.x + 94.0, zoom_bar.y + 6.0, 32.0, 32.0));
        plus.paint(p);
        let mut fit = Button::new("Fit", ButtonStyle::Plain);
        fit.set_rect(RectF::new(zoom_bar.x + 130.0, zoom_bar.y + 8.0, 48.0, 28.0));
        fit.paint(p);
        p.text("2880 × 1664", &TextStyle::body().tabular(), theme.text_secondary, RectF::new(zoom_bar.x + 188.0, zoom_bar.y, 90.0, 44.0));
        Badge::new("HDR").style(BadgeStyle::Accent).paint(p, RectF::new(zoom_bar.right() - 58.0, zoom_bar.y + 10.0, 48.0, 24.0));
    })
}

/// The recording HUD (DESIGN §10) as a real `View`, rendered through `render_view_offscreen` exactly as its popup
/// window would draw it: transparent background, glass pill, pulsing dot driven by `cx.time()`.
struct HudView {
    toolbar: Toolbar,
    started: f64,
    paused: bool,
}

#[derive(Debug)]
enum HudCommand {
    PauseResume,
    Stop,
    Discard,
    MicOn,
    MicOff,
}

impl HudView {
    fn new() -> Self {
        let toolbar = Toolbar::new(vec![
            ToolbarItem::custom("time", SizeF::new(68.0, 32.0)),
            ToolbarItem::separator(),
            ToolbarItem::button("pause", IconButton::new(Icon::PauseFill).tooltip("Pause", None)),
            ToolbarItem::button("stop", IconButton::new(Icon::StopFill).tooltip("Stop", None)),
            ToolbarItem::button("discard", IconButton::new(Icon::Trash).tooltip("Discard", None)),
            ToolbarItem::separator(),
            ToolbarItem::button("mic", IconButton::new(Icon::Mic).tooltip("Microphone", None).with_selected(true)),
        ]);
        Self { toolbar, started: 0.0, paused: false }
    }
}

impl View for HudView {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        match self.toolbar.event(cx, event) {
            Response::Action(ToolbarAction::Clicked(id)) => {
                match id {
                    "pause" => {
                        self.paused = !self.paused;
                        if let Some(b) = self.toolbar.button_mut("pause") {
                            b.icon = if self.paused { Icon::PlayFill } else { Icon::PauseFill };
                        }
                        cx.post(HudCommand::PauseResume);
                    }
                    "stop" => cx.post(HudCommand::Stop),
                    "discard" => cx.post(HudCommand::Discard),
                    "mic" => {
                        if let Some(b) = self.toolbar.button_mut("mic") {
                            let on = !b.is_selected();
                            b.set_selected(on);
                            b.icon = if on { Icon::Mic } else { Icon::MicOff };
                            cx.post(if on { HudCommand::MicOn } else { HudCommand::MicOff });
                        }
                    }
                    _ => {}
                }
                true
            }
            response => response.consumed(),
        }
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.toolbar.layout_centered(cx.gfx(), cx.size().w / 2.0, 12.0);
        self.toolbar.paint(p, None);
        let Some(slot) = self.toolbar.item("time").map(|i| i.rect()) else { return };
        let theme = p.theme().clone();
        let elapsed = (cx.time() - self.started).max(0.0) as u64;
        let pulse = 0.5 + 0.5 * (cx.time() * std::f64::consts::TAU / 1.6).cos() as f32;
        let dot = PointF::new(slot.x + 14.0, slot.center().y);
        p.fill_circle(dot, 5.0, theme.destructive.with_alpha(0.55 + 0.45 * pulse));
        let label = format!("{:02}:{:02}", elapsed / 60, elapsed % 60);
        let style = TextStyle::body().weight(Weight::Semibold).tabular();
        p.text(&label, &style, theme.text, RectF::new(slot.x + 28.0, slot.y, slot.w - 28.0, slot.h));
        if !self.paused {
            cx.animate();
        }
    }
}
