//! Offscreen previews: a synthetic browser screenshot (and an HDR version of it), plus staged documents for every
//! preview kind. Geometry is authored in a 1440×900 design space and scaled to the image.

use std::rc::Rc;

use anyhow::{Context, Result, bail};
use glint_core::{HdrImage, Image, PointF, RectF, SizeF, ToneMapMode, ToneMapParams, f16};
use glint_ui::{
    Brush, Color, Gfx, Icon, OffscreenSpec, PathBuilder, TextStyle, Theme, Weight, render_offscreen,
};

use crate::chrome::ToastKind;
use crate::editor::EditorView;
use crate::model::{Body, Cap, Dash, InkPoint, RedactKind, Redaction, Shape, ShapeKind, Stroke, StrokeKind, TextNote};
use crate::ocr_text::RecognizedText;
use crate::tools::{self, Tool};
use crate::EditorDoc;

pub const KINDS: [&str; 14] = [
    "editor",
    "editor-pen",
    "editor-shapes",
    "editor-text",
    "editor-crop",
    "editor-hdr",
    "editor-redact",
    "editor-select",
    "editor-ocr",
    "editor-picker",
    "editor-menu",
    "editor-narrow",
    "editor-stroke",
    "editor-stroke-pen",
];

pub const WINDOW: SizeF = SizeF::new(1200.0, 760.0);
const NARROW_WINDOW: SizeF = SizeF::new(760.0, 520.0);

fn window_for(kind: &str) -> SizeF {
    if kind == "editor-narrow" { NARROW_WINDOW } else { WINDOW }
}
const DESIGN: SizeF = SizeF::new(1440.0, 900.0);
const PARAGRAPH: [&str; 3] = [
    "Your profile is visible to everyone in the Northwind workspace. Changes to",
    "your email address must be confirmed before they take effect. We will send",
    "a verification link to the new address within a few minutes.",
];
const PARAGRAPH_X: f32 = 300.0;
const PARAGRAPH_Y: f32 = 612.0;
const PARAGRAPH_LEAD: f32 = 26.0;
const PARAGRAPH_SIZE: f32 = 15.0;
const SUN: (f32, f32, f32) = (1296.0, 640.0, 30.0);
const PHOTO: RectF = RectF::new(940.0, 540.0, 460.0, 320.0);

fn hex(s: &str) -> Color {
    Color::hex(s).unwrap_or(Color::BLACK)
}

/// Maps design-space coordinates to image pixels.
#[derive(Clone, Copy)]
struct Space {
    k: f32,
}

impl Space {
    fn p(&self, x: f32, y: f32) -> PointF {
        PointF::new(x * self.k, y * self.k)
    }

    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> RectF {
        RectF::new(x * self.k, y * self.k, w * self.k, h * self.k)
    }
}

/// A browser window showing an account-settings page, at `scale` physical pixels per DIP.
pub fn synthetic_screenshot(gfx: &Rc<Gfx>, scale: f32) -> Result<Image> {
    let spec = OffscreenSpec::new(DESIGN, scale, Theme::light()).background(Color::WHITE);
    render_offscreen(gfx, &spec, |_, p| {
        let ink = hex("#202124");
        let secondary = hex("#5F6368");
        let hairline = hex("#E3E5E8");
        p.fill_rect(RectF::new(0.0, 0.0, DESIGN.w, 40.0), hex("#DFE3E8"));
        let tab = RectF::new(12.0, 6.0, 270.0, 34.0);
        p.fill_round_rect(RectF::new(tab.x, tab.y, tab.w, tab.h + 10.0), 9.0, Color::WHITE);
        p.fill_circle(PointF::new(tab.x + 20.0, tab.center().y), 7.0, hex("#0A66FF"));
        p.text("Account settings – Northwind", &TextStyle::new(12.5), ink, RectF::new(tab.x + 36.0, tab.y, 200.0, tab.h));
        p.icon(Icon::X, PointF::new(tab.right() - 18.0, tab.center().y), 13.0, secondary);
        p.icon(Icon::Plus, PointF::new(tab.right() + 22.0, tab.center().y), 15.0, secondary);
        p.fill_rect(RectF::new(0.0, 40.0, DESIGN.w, 46.0), Color::WHITE);
        for (i, icon) in [Icon::ChevronLeft, Icon::ChevronRight, Icon::Redo].iter().enumerate() {
            p.icon(*icon, PointF::new(26.0 + i as f32 * 34.0, 63.0), 18.0, secondary);
        }
        let address = RectF::new(126.0, 48.0, 1180.0, 30.0);
        p.fill_round_rect(address, 15.0, hex("#F1F3F4"));
        p.icon(Icon::Search, PointF::new(address.x + 20.0, address.center().y), 14.0, secondary);
        p.text("northwind.app/settings/account", &TextStyle::new(13.5), ink, RectF::new(address.x + 38.0, address.y, 400.0, address.h));
        p.icon(Icon::More, PointF::new(1410.0, 63.0), 18.0, secondary);
        p.fill_rect(RectF::new(0.0, 86.0, DESIGN.w, 1.0), hairline);

        p.fill_rect(RectF::new(0.0, 87.0, 252.0, DESIGN.h - 87.0), hex("#F7F8FA"));
        p.fill_rect(RectF::new(252.0, 87.0, 1.0, DESIGN.h - 87.0), hairline);
        p.fill_round_rect(RectF::new(24.0, 108.0, 28.0, 28.0), 7.0, hex("#0A66FF"));
        p.text("Northwind", &TextStyle::new(17.0).weight(Weight::Semibold), ink, RectF::new(62.0, 108.0, 160.0, 28.0));
        let items = ["Overview", "Account", "Billing", "Security", "Notifications", "Integrations", "Team members"];
        for (i, item) in items.iter().enumerate() {
            let row = RectF::new(14.0, 160.0 + i as f32 * 40.0, 224.0, 34.0);
            if i == 1 {
                p.fill_round_rect(row, 8.0, hex("#E7EFFF"));
            }
            let color = if i == 1 { hex("#0A58E0") } else { hex("#3C4043") };
            let style = TextStyle::new(14.0).weight(if i == 1 { Weight::Semibold } else { Weight::Regular });
            p.text(item, &style, color, RectF::new(row.x + 14.0, row.y, row.w - 20.0, row.h));
        }

        p.text("Account settings", &TextStyle::new(28.0).weight(Weight::Semibold), ink, RectF::new(300.0, 112.0, 600.0, 40.0));
        p.text(
            "Manage your profile, contact details and sign-in preferences.",
            &TextStyle::new(15.0),
            secondary,
            RectF::new(300.0, 152.0, 700.0, 24.0),
        );

        let card = RectF::new(300.0, 200.0, 600.0, 300.0);
        p.fill_round_rect(card, 12.0, Color::WHITE);
        p.stroke_round_rect(card.inset(0.5), 12.0, hairline, 1.0);
        let avatar = PointF::new(352.0, 252.0);
        p.fill_circle(avatar, 30.0, Brush::linear(PointF::new(322.0, 222.0), PointF::new(382.0, 282.0), &[(0.0, hex("#FF9F0A")), (1.0, hex("#FF375F"))]));
        p.text("JL", &TextStyle::new(20.0).weight(Weight::Semibold).centered(), Color::WHITE, RectF::new(322.0, 222.0, 60.0, 60.0));
        p.text("Jordan Lee", &TextStyle::new(20.0).weight(Weight::Semibold), ink, RectF::new(398.0, 228.0, 300.0, 26.0));
        p.text("Product designer · Seattle", &TextStyle::new(14.0), secondary, RectF::new(398.0, 256.0, 300.0, 20.0));
        let fields = [("Email", "jordan.lee@northwind.app"), ("Phone", "+1 (206) 555-0148"), ("Member since", "March 2021")];
        for (i, (label, value)) in fields.iter().enumerate() {
            let y = 312.0 + i as f32 * 58.0;
            p.fill_rect(RectF::new(card.x + 24.0, y - 10.0, card.w - 48.0, 1.0), hairline);
            p.text(label, &TextStyle::new(13.0), secondary, RectF::new(card.x + 24.0, y, 160.0, 40.0));
            p.text(value, &TextStyle::new(15.0).weight(Weight::Medium), ink, RectF::new(card.x + 190.0, y, 380.0, 40.0));
        }

        let save = RectF::new(300.0, 528.0, 150.0, 40.0);
        p.fill_round_rect(save, 8.0, hex("#0A66FF"));
        p.text("Save changes", &TextStyle::new(14.5).weight(Weight::Semibold).centered(), Color::WHITE, save);
        let cancel = RectF::new(462.0, 528.0, 96.0, 40.0);
        p.stroke_round_rect(cancel.inset(0.5), 8.0, hex("#D0D4D9"), 1.0);
        p.text("Cancel", &TextStyle::new(14.5).weight(Weight::Medium).centered(), ink, cancel);

        let body = TextStyle::new(PARAGRAPH_SIZE);
        for (i, line) in PARAGRAPH.iter().enumerate() {
            p.text_at(line, &body, hex("#3C4043"), PointF::new(PARAGRAPH_X, PARAGRAPH_Y + i as f32 * PARAGRAPH_LEAD));
        }
        p.text_at("Last changed 2 days ago by Jordan Lee.", &TextStyle::new(13.0), secondary, PointF::new(PARAGRAPH_X, 720.0));

        let chart = RectF::new(940.0, 200.0, 460.0, 300.0);
        p.fill_round_rect(chart, 12.0, Color::WHITE);
        p.stroke_round_rect(chart.inset(0.5), 12.0, hairline, 1.0);
        p.text("Monthly activity", &TextStyle::new(16.0).weight(Weight::Semibold), ink, RectF::new(chart.x + 22.0, chart.y + 16.0, 300.0, 24.0));
        p.text("Sessions per month, 2026", &TextStyle::new(12.5), secondary, RectF::new(chart.x + 22.0, chart.y + 40.0, 300.0, 18.0));
        let heights = [62.0, 78.0, 70.0, 96.0, 118.0, 104.0, 132.0, 168.0, 142.0, 120.0, 98.0, 110.0];
        let months = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"];
        for (i, h) in heights.iter().enumerate() {
            let x = chart.x + 28.0 + i as f32 * 35.0;
            let bar = RectF::new(x, chart.bottom() - 40.0 - h, 20.0, *h);
            let color = if i == 7 { hex("#0A66FF") } else { hex("#B9CFFB") };
            p.fill_round_rect(bar, 4.0, color);
            p.text(months[i], &TextStyle::new(11.0).centered(), secondary, RectF::new(x - 6.0, chart.bottom() - 34.0, 32.0, 18.0));
        }

        p.clip_round_rect(PHOTO, 12.0, |p| {
            p.fill_rect(PHOTO, Brush::vertical(PHOTO, hex("#3C6FB4"), hex("#F2B56B")));
            p.fill_circle(PointF::new(SUN.0, SUN.1), SUN.2 * 2.2, hex("#FFE2A8").with_alpha(0.35));
            p.fill_circle(PointF::new(SUN.0, SUN.1), SUN.2, hex("#FFF4D6"));
            let mut far = PathBuilder::new();
            far.polyline(
                &[
                    PointF::new(PHOTO.x, 760.0),
                    PointF::new(1010.0, 690.0),
                    PointF::new(1080.0, 728.0),
                    PointF::new(1170.0, 668.0),
                    PointF::new(1260.0, 722.0),
                    PointF::new(1340.0, 686.0),
                    PointF::new(PHOTO.right(), 716.0),
                    PointF::new(PHOTO.right(), PHOTO.bottom()),
                    PointF::new(PHOTO.x, PHOTO.bottom()),
                ],
                true,
            );
            if let Ok(path) = far.build(p.gfx()) {
                p.fill_path(&path, hex("#5B5F86"));
            }
            let mut near = PathBuilder::new();
            near.polyline(
                &[
                    PointF::new(PHOTO.x, 800.0),
                    PointF::new(1050.0, 748.0),
                    PointF::new(1150.0, 790.0),
                    PointF::new(1250.0, 742.0),
                    PointF::new(PHOTO.right(), 796.0),
                    PointF::new(PHOTO.right(), PHOTO.bottom()),
                    PointF::new(PHOTO.x, PHOTO.bottom()),
                ],
                true,
            );
            if let Ok(path) = near.build(p.gfx()) {
                p.fill_path(&path, hex("#2E3352"));
            }
        });
        p.text("Cover photo", &TextStyle::new(12.5).weight(Weight::Medium), Color::WHITE, RectF::new(PHOTO.x + 16.0, PHOTO.bottom() - 34.0, 200.0, 20.0));
    })
}

fn srgb_to_linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// The screenshot as an HDR capture would deliver it (scRGB at 240-nit SDR white) with a sun several times
/// brighter than SDR white in the cover photo.
pub fn synthetic_hdr(sdr: &Image) -> HdrImage {
    let sdr_white = 240.0;
    let white = sdr_white / 80.0;
    let k = sdr.width as f32 / DESIGN.w;
    let (sx, sy, sr) = (SUN.0 * k, SUN.1 * k, SUN.2 * k);
    let photo = RectF::new(PHOTO.x * k, PHOTO.y * k, PHOTO.w * k, PHOTO.h * k);
    let mut data = Vec::with_capacity(sdr.data.len());
    for y in 0..sdr.height {
        for x in 0..sdr.width {
            let [b, g, r, _] = sdr.pixel(x, y);
            let mut rgb = [srgb_to_linear(r) * white, srgb_to_linear(g) * white, srgb_to_linear(b) * white];
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if photo.contains(PointF::new(fx, fy)) {
                let d = ((fx - sx).powi(2) + (fy - sy).powi(2)).sqrt() / sr;
                let boost = if d < 1.0 { 5.0 - 2.0 * d } else { 1.0 + 1.6 * (-(d - 1.0) * 1.2).exp() };
                let horizon = 1.0 + 0.35 * ((fy - photo.y) / photo.h).clamp(0.0, 1.0);
                for c in &mut rgb {
                    *c *= boost * horizon;
                }
            }
            data.extend(rgb.iter().map(|v| f16::from_f32(*v)));
            data.push(f16::from_f32(1.0));
        }
    }
    HdrImage { width: sdr.width, height: sdr.height, data, sdr_white_nits: sdr_white, display_peak_nits: 1000.0 }
}

/// Pen samples along `path(t)` for t in 0..1 with a natural pressure swell.
fn pen_stroke(space: Space, samples: usize, path: impl Fn(f32) -> (f32, f32), pressure: impl Fn(f32) -> f32) -> Vec<InkPoint> {
    (0..=samples)
        .map(|i| {
            let t = i as f32 / samples as f32;
            let (x, y) = path(t);
            let p = space.p(x, y);
            InkPoint { pos: p, pressure: pressure(t) }
        })
        .collect()
}

fn swell(t: f32) -> f32 {
    (0.25 + 0.75 * (std::f32::consts::PI * t).sin().max(0.0).powf(0.6)) * 0.82
}

fn ink(points: Vec<InkPoint>, color: Color, width: f32, kind: StrokeKind, pressure: bool) -> Stroke {
    let min = width * 0.08;
    Stroke { pressure, ..Stroke::new(kind, crate::ink::simplify(&points, min), color, width) }
}

fn stroke(points: Vec<InkPoint>, color: Color, width: f32, kind: StrokeKind, pressure: bool) -> Body {
    Body::Stroke(ink(points, color, width, kind, pressure))
}

fn shape(kind: ShapeKind, a: PointF, b: PointF, color: Color, width: f32, filled: bool) -> Body {
    Body::Shape(Shape { filled, ..Shape::new(kind, a, b, color, width) })
}

fn note(origin: PointF, text: &str, size: f32, color: Color, background: bool) -> Body {
    Body::Text(TextNote { text: text.into(), origin, size, color, background })
}

/// Highlighter pass over the first `words` words of paragraph line `line`.
fn highlight(gfx: &Gfx, space: Space, line: usize, words: usize, color: Color, width: f32) -> Body {
    let text: Vec<&str> = PARAGRAPH[line].split(' ').take(words).collect();
    let x0 = PARAGRAPH_X - 3.0;
    let x1 = PARAGRAPH_X + gfx.measure_text(&text.join(" "), &TextStyle::new(PARAGRAPH_SIZE)).w + 3.0;
    let y = PARAGRAPH_Y + line as f32 * PARAGRAPH_LEAD - 5.0;
    let points = pen_stroke(space, 24, |t| (x0 + (x1 - x0) * t, y + (t * 9.0).sin() * 0.6), |_| 0.5);
    stroke(points, color, width, StrokeKind::Highlighter, false)
}

fn paragraph_words(gfx: &Gfx, space: Space) -> RecognizedText {
    let style = TextStyle::new(PARAGRAPH_SIZE);
    let space_w = gfx.measure_text("a b", &style).w - gfx.measure_text("ab", &style).w;
    let mut lines = Vec::new();
    for (i, line) in PARAGRAPH.iter().enumerate() {
        let baseline = PARAGRAPH_Y + i as f32 * PARAGRAPH_LEAD;
        let mut x = PARAGRAPH_X;
        let mut words = Vec::new();
        for word in line.split(' ') {
            let w = gfx.measure_text(word, &style).w;
            words.push((word.to_string(), space.r(x - 1.0, baseline - 13.0, w + 2.0, 17.5)));
            x += w + space_w;
        }
        lines.push(words);
    }
    RecognizedText::new(lines, PointF::default())
}

struct Staged {
    view: EditorView,
}

fn base_doc(image: Image, hdr: Option<HdrImage>, tone_map: ToneMapParams) -> EditorDoc {
    EditorDoc { image, hdr, tone_map, monitor: None, saved_path: None }
}

fn stage(gfx: &Rc<Gfx>, kind: &str, image: Option<&Image>, scale: f32) -> Result<Staged> {
    let synthetic = image.is_none();
    let screenshot = match image {
        Some(image) => image.clone(),
        None => synthetic_screenshot(gfx, scale)?,
    };
    let space = Space { k: screenshot.width as f32 / DESIGN.w };
    let mut tone_map = ToneMapParams::default();
    let mut hdr_stats = None;
    let (screenshot, hdr) = if kind == "editor-hdr" {
        tone_map = ToneMapParams { mode: ToneMapMode::Auto, exposure_stops: 0.3 };
        let hdr = synthetic_hdr(&screenshot);
        let stats = glint_core::tonemap::analyze(&hdr);
        hdr_stats = Some(stats);
        (glint_core::tonemap::tonemap_with_stats(&hdr, &tone_map, &stats), Some(hdr))
    } else {
        (screenshot, None)
    };
    let settings = glint_core::Settings::default();
    let mut view = EditorView::new(base_doc(screenshot, hdr, tone_map), &settings, None);
    view.stage_image_scale(space.k);
    let px = space.k;
    let red = tools::palette(0);
    let orange = tools::palette(1);
    let yellow = tools::palette(2);
    let green = tools::palette(3);
    let blue = tools::palette(4);
    let black = tools::palette(6);
    let shape_w = |i: usize| tools::SHAPE_SIZES[i] * px;
    let pen_w = |i: usize| tools::PEN_SIZES[i] * px;
    let hl_w = |i: usize| tools::HIGHLIGHTER_SIZES[i] * px;
    let text_size = |i: usize| tools::TEXT_SIZES[i] * px;
    let before = view.doc_mut().clone();

    match kind {
        "editor" | "editor-select" => {
            view.stage_tool(if kind == "editor" { Tool::Pen } else { Tool::Select }, |o| o.color = red);
            let doc = view.doc_mut();
            doc.add(highlight(gfx, space, 1, 6, yellow, hl_w(1)));
            let rect = doc.add(shape(ShapeKind::Rectangle, space.p(318.0, 300.0), space.p(882.0, 350.0), red, shape_w(1), false));
            doc.add(shape(ShapeKind::Arrow, space.p(700.0, 252.0), space.p(652.0, 294.0), red, shape_w(1), false));
            let circle = pen_stroke(
                space,
                90,
                |t| {
                    let a = -2.2 + t * 6.9;
                    (1223.0 + 42.0 * a.cos() + t * 5.0, 298.0 + 30.0 * a.sin() - t * 4.0)
                },
                swell,
            );
            doc.add(stroke(circle, blue, pen_w(1), StrokeKind::Pen, true));
            doc.add(note(space.p(600.0, 214.0), "Use the work address", text_size(1), red, false));
            if kind == "editor-select" {
                view.stage_selection(Some(rect));
            }
        }
        "editor-pen" => {
            view.stage_tool(Tool::Pen, |o| {
                o.color = red;
                o.width = tools::PEN_SIZES[1];
            });
            let doc = view.doc_mut();
            doc.add(highlight(gfx, space, 0, usize::MAX, yellow, hl_w(1)));
            doc.add(highlight(gfx, space, 2, 7, green, hl_w(1)));
            let ring = pen_stroke(
                space,
                110,
                |t| {
                    let a = -1.6 + t * 6.75;
                    (352.0 + 46.0 * a.cos() * (1.0 + 0.05 * t), 252.0 + 42.0 * a.sin())
                },
                swell,
            );
            doc.add(stroke(ring, red, pen_w(1), StrokeKind::Pen, true));
            let underline = pen_stroke(space, 60, |t| (300.0 + 236.0 * t, 150.0 + 2.0 * (t * 5.0).sin() + 1.5 * t), |t| swell(t) * 1.05);
            doc.add(stroke(underline, blue, pen_w(1), StrokeKind::Pen, true));
            let check = pen_stroke(
                space,
                40,
                |t| if t < 0.35 { (580.0 + 30.0 * t, 540.0 + 50.0 * t) } else { (590.5 + 70.0 * (t - 0.35), 557.5 - 64.0 * (t - 0.35)) },
                |t| 0.35 + 0.6 * t,
            );
            doc.add(stroke(check, green, pen_w(2), StrokeKind::Pen, true));
            let signature = pen_stroke(
                space,
                160,
                |t| {
                    let a = t * std::f32::consts::TAU * 4.5;
                    let envelope = 1.0 - 0.45 * t;
                    (318.0 + 250.0 * t + 17.0 * a.sin(), 800.0 - 24.0 * (a + 1.1).cos() * envelope - 10.0 * t)
                },
                |t| (0.35 + 0.5 * (t * 23.0).sin().abs()) * (1.0 - 0.3 * t),
            );
            doc.add(stroke(signature, black, pen_w(1), StrokeKind::Pen, true));
            let mouse = pen_stroke(space, 50, |t| (1180.0 - 160.0 * t, 520.0 + 60.0 * (t * 3.0).sin() * (1.0 - t)), |_| 1.0);
            doc.add(stroke(mouse, orange, pen_w(1), StrokeKind::Pen, false));
        }
        "editor-shapes" => {
            view.stage_tool(Tool::Shapes, |o| {
                o.shape = ShapeKind::Arrow;
                o.color = red;
            });
            let doc = view.doc_mut();
            doc.add(shape(ShapeKind::Rectangle, space.p(926.0, 186.0), space.p(1414.0, 514.0), blue, shape_w(1), false));
            doc.add(shape(ShapeKind::Ellipse, space.p(306.0, 206.0), space.p(398.0, 298.0), orange, shape_w(1), false));
            doc.add(shape(ShapeKind::Line, space.p(300.0, 151.0), space.p(540.0, 151.0), green, shape_w(2), false));
            doc.add(shape(ShapeKind::Rectangle, space.p(484.0, 432.0), space.p(592.0, 464.0), black, shape_w(0), true));
            doc.add(shape(ShapeKind::Arrow, space.p(1200.0, 120.0), space.p(1050.0, 196.0), red, shape_w(0), false));
            doc.add(shape(ShapeKind::Arrow, space.p(760.0, 700.0), space.p(560.0, 560.0), blue, shape_w(1), false));
            let last = doc.add(shape(ShapeKind::Arrow, space.p(720.0, 470.0), space.p(560.0, 388.0), red, shape_w(2), false));
            view.stage_selection(Some(last));
        }
        "editor-text" => {
            view.stage_tool(Tool::Text, |o| {
                o.color = red;
                o.text_size = 1;
            });
            let doc = view.doc_mut();
            doc.add(note(space.p(560.0, 118.0), "Rename to “Profile”", text_size(1), red, false));
            doc.add(note(space.p(1010.0, 410.0), "Peak in August", text_size(2), Color::WHITE, true));
            doc.add(note(space.p(578.0, 537.0), "Primary action – keep it blue", text_size(0), blue, false));
            let editing = doc.add(note(space.p(410.0, 778.0), "Looks great!\nShip it", text_size(1), green, false));
            view.stage_selection(Some(editing));
            view.stage_editing(editing, "Looks great!\nShip it".len());
        }
        "editor-crop" => {
            view.stage_tool(Tool::Pen, |_| {});
            let doc = view.doc_mut();
            doc.add(shape(ShapeKind::Arrow, space.p(700.0, 252.0), space.p(652.0, 294.0), red, shape_w(1), false));
            doc.add(shape(ShapeKind::Rectangle, space.p(318.0, 300.0), space.p(882.0, 350.0), red, shape_w(1), false));
            view.stage_crop(space.r(280.0, 100.0, 650.0, 520.0), true);
        }
        "editor-hdr" => {
            view.stage_tool(Tool::Select, |_| {});
            if let Some(stats) = hdr_stats {
                view.stage_hdr(stats);
            }
        }
        "editor-redact" => {
            view.stage_tool(Tool::Redact, |o| o.redact = RedactKind::Blur);
            let doc = view.doc_mut();
            doc.add(Body::Redact(Redaction { rect: space.r(484.0, 306.0, 220.0, 40.0), kind: RedactKind::Blur }));
            doc.add(Body::Redact(Redaction { rect: space.r(484.0, 364.0, 170.0, 40.0), kind: RedactKind::Pixelate }));
            doc.add(Body::Redact(Redaction { rect: space.r(318.0, 218.0, 68.0, 68.0), kind: RedactKind::Pixelate }));
            let name = doc.add(Body::Redact(Redaction { rect: space.r(394.0, 226.0, 128.0, 30.0), kind: RedactKind::Blur }));
            view.stage_selection(Some(name));
        }
        "editor-ocr" => {
            view.stage_tool(Tool::Select, |_| {});
            if synthetic {
                let text = paragraph_words(gfx, space);
                view.stage_ocr(text, Some((5, 17)));
                view.stage_toast("Copied 2 lines", ToastKind::Done);
            }
        }
        "editor-picker" => {
            let custom = hex("#5AC8FA");
            view.stage_tool(Tool::Pen, |o| {
                o.custom = Some(custom);
                o.color = custom;
            });
            let ring = pen_stroke(space, 90, |t| (1223.0 + 44.0 * (t * 6.6).cos(), 298.0 + 32.0 * (t * 6.6).sin()), swell);
            view.doc_mut().add(stroke(ring, custom, pen_w(1), StrokeKind::Pen, true));
        }
        "editor-stroke" => {
            view.stage_tool(Tool::Shapes, |o| {
                o.shape = ShapeKind::Arrow;
                o.color = red;
            });
            let purple = tools::palette(5);
            let doc = view.doc_mut();
            doc.add(highlight(gfx, space, 1, 6, yellow, hl_w(1)));
            doc.add(Body::Shape(Shape {
                dash: Dash::Dashed,
                corner_radius: 18.0 * px,
                filled: true,
                fill_opacity: 0.10,
                ..Shape::new(ShapeKind::Rectangle, space.p(926.0, 186.0), space.p(1414.0, 514.0), blue, 3.0 * px)
            }));
            doc.add(Body::Shape(Shape {
                dash: Dash::Dotted,
                ..Shape::new(ShapeKind::Line, space.p(302.0, 152.0), space.p(534.0, 152.0), orange, 4.0 * px)
            }));
            let lasso = pen_stroke(
                space,
                140,
                |t| {
                    let a = -2.0 + t * 7.4;
                    (352.0 + 52.0 * a.cos() + 10.0 * t, 252.0 + 44.0 * a.sin() - 6.0 * t)
                },
                swell,
            );
            doc.add(Body::Stroke(Stroke { opacity: 0.55, ..ink(lasso, purple, 9.0 * px, StrokeKind::Pen, true) }));
            let arrow = doc.add(Body::Shape(Shape {
                caps: [Cap::Dot, Cap::FilledArrow],
                head_scale: 1.2,
                ..Shape::new(ShapeKind::Arrow, space.p(800.0, 548.0), space.p(566.0, 548.0), red, 5.0 * px)
            }));
            view.stage_selection(Some(arrow));
        }
        "editor-stroke-pen" => {
            view.stage_tool(Tool::Pen, |o| {
                o.color = blue;
                o.width = 6.0;
                o.opacity = 0.85;
                o.smoothing = 45.0;
            });
            let doc = view.doc_mut();
            let ring = pen_stroke(
                space,
                110,
                |t| {
                    let a = -1.6 + t * 6.75;
                    (1223.0 + 46.0 * a.cos(), 296.0 + 34.0 * a.sin())
                },
                swell,
            );
            doc.add(Body::Stroke(Stroke { opacity: 0.85, smoothing: 45.0, ..ink(ring, blue, 6.0 * px, StrokeKind::Pen, true) }));
            let underline = pen_stroke(space, 60, |t| (300.0 + 236.0 * t, 150.0 + 2.0 * (t * 5.0).sin() + 1.5 * t), |_| 0.5);
            doc.add(Body::Stroke(Stroke { dash: Dash::Dashed, ..ink(underline, red, 4.0 * px, StrokeKind::Pen, false) }));
            let check = pen_stroke(
                space,
                40,
                |t| if t < 0.35 { (580.0 + 30.0 * t, 540.0 + 50.0 * t) } else { (590.5 + 70.0 * (t - 0.35), 557.5 - 64.0 * (t - 0.35)) },
                |t| 0.35 + 0.6 * t,
            );
            doc.add(stroke(check, green, pen_w(2), StrokeKind::Pen, true));
        }
        "editor-narrow" => {
            view.stage_tool(Tool::Shapes, |o| o.shape = ShapeKind::Arrow);
            let doc = view.doc_mut();
            doc.add(shape(ShapeKind::Arrow, space.p(700.0, 252.0), space.p(652.0, 294.0), red, shape_w(1), false));
            doc.add(shape(ShapeKind::Rectangle, space.p(318.0, 300.0), space.p(882.0, 350.0), red, shape_w(1), false));
        }
        "editor-menu" => {
            view.stage_tool(Tool::Highlighter, |_| {});
            view.doc_mut().add(highlight(gfx, space, 0, usize::MAX, yellow, hl_w(1)));
        }
        other => bail!("unknown editor preview kind {other:?}; expected one of {KINDS:?}"),
    }
    let after = view.doc_mut().clone();
    if after != before {
        view.stage_history(&before);
    }
    Ok(Staged { view })
}

/// Lays the staged view out for `scale` and opens whatever popover the kind shows.
fn finish(gfx: &Rc<Gfx>, kind: &str, staged: &mut Staged, scale: f32) {
    let view = &mut staged.view;
    view.stage_layout(gfx, window_for(kind), scale);
    view.stage_refit();
    match kind {
        "editor-hdr" => view.stage_hdr_popover(),
        "editor-picker" => view.stage_picker(),
        "editor-menu" => view.stage_menu(gfx, Some(1)),
        "editor-pen" => view.stage_hover(PointF::new(820.0, 420.0)),
        "editor-stroke" | "editor-stroke-pen" => view.stage_stroke_popover(),
        _ => {}
    }
}

pub fn render(gfx: &Gfx, kind: &str, theme: glint_core::ThemeMode, scale: f32, image: Option<&Image>) -> Result<Image> {
    let gfx = gfx.shared().context("the Gfx is not owned by an Rc")?;
    let theme = Theme::resolve(theme, glint_ui::theme::system_prefers_dark());
    let mut staged = stage(&gfx, kind, image, scale)?;
    finish(&gfx, kind, &mut staged, scale);
    let spec = OffscreenSpec::new(window_for(kind), scale, theme);
    glint_ui::render_view_offscreen(&gfx, &mut staged.view, &spec)
}

/// Full-resolution export of the `editor` document, optionally cropped to the profile card.
pub fn export(gfx: &Gfx, image: Option<&Image>, cropped: bool) -> Result<Image> {
    let gfx = gfx.shared().context("the Gfx is not owned by an Rc")?;
    let mut staged = stage(&gfx, "editor", image, 1.0)?;
    if cropped {
        let k = staged.view.image_size().w / DESIGN.w;
        let crop = glint_core::RectI::new((280.0 * k) as i32, (190.0 * k) as i32, (660.0 * k) as i32, (430.0 * k) as i32);
        staged.view.doc_mut().crop = Some(crop);
    }
    staged.view.export(&gfx)
}
