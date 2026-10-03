//! Glint's icon artwork: the app icon (gradient tile, viewfinder brackets, sparkle) and the monochrome tray glyph.
//! Everything is drawn with glint-ui so the same art serves the .ico, the tray, toasts and the settings header.

use std::rc::Rc;

use anyhow::Result;
use glint_ui::{
    Brush, Color, Gfx, Image, OffscreenSpec, Painter, Path, PathBuilder, PointF, RectF, Shadow, StrokeStyle, Theme,
    render_offscreen,
};

/// Below this many physical pixels the art switches to pixel-snapped brackets and a compact sparkle.
const SMALL_PX: f32 = 30.0;
/// From this size on the app icon gets its drop shadow and margin.
const FULL_DETAIL_PX: f32 = 64.0;

const GRADIENT: [(f32, &str); 3] = [(0.0, "#3D86FF"), (0.5, "#4452EC"), (1.0, "#7B36E9")];

fn gradient(tile: RectF) -> Brush {
    let stops: Vec<(f32, Color)> =
        GRADIENT.iter().map(|(offset, hex)| (*offset, Color::hex(hex).expect("valid gradient color"))).collect();
    Brush::linear(PointF::new(tile.x, tile.y), PointF::new(tile.right(), tile.bottom()), &stops)
}

/// Where the sparkle sits inside a viewfinder frame: the upper-right quadrant, clear of the brackets. Pixel-sized
/// glyphs have no room for that, so their sparkle moves toward the center.
fn sparkle_spot(frame: RectF, pixel_sized: bool) -> (PointF, f32) {
    let (x, y) = if pixel_sized { (0.55, 0.45) } else { (0.64, 0.36) };
    (PointF::new(frame.x + frame.w * x, frame.y + frame.h * y), frame.w * 0.21)
}

/// The four-point sparkle with concave sides; horizontal rays are shorter than vertical ones.
fn sparkle_path(gfx: &Gfx, center: PointF, radius: f32) -> Option<Path> {
    let horizontal = radius * 0.82;
    let pinch = radius * 0.14;
    let mut builder = PathBuilder::new();
    builder
        .move_to(PointF::new(center.x, center.y - radius))
        .quad_to(PointF::new(center.x + pinch, center.y - pinch), PointF::new(center.x + horizontal, center.y))
        .quad_to(PointF::new(center.x + pinch, center.y + pinch), PointF::new(center.x, center.y + radius))
        .quad_to(PointF::new(center.x - pinch, center.y + pinch), PointF::new(center.x - horizontal, center.y))
        .quad_to(PointF::new(center.x - pinch, center.y - pinch), PointF::new(center.x, center.y - radius))
        .close();
    builder.build(gfx).ok()
}

/// Four rounded L-shaped corner brackets whose stroke centerline runs along `frame`.
fn bracket_path(gfx: &Gfx, frame: RectF, arm: f32, bend: f32) -> Option<Path> {
    let (l, t, r, b) = (frame.x, frame.y, frame.right(), frame.bottom());
    let corners = [
        (PointF::new(l, t + arm), PointF::new(l, t + bend), PointF::new(l + bend, t), PointF::new(l + arm, t)),
        (PointF::new(r - arm, t), PointF::new(r - bend, t), PointF::new(r, t + bend), PointF::new(r, t + arm)),
        (PointF::new(r, b - arm), PointF::new(r, b - bend), PointF::new(r - bend, b), PointF::new(r - arm, b)),
        (PointF::new(l + arm, b), PointF::new(l + bend, b), PointF::new(l, b - bend), PointF::new(l, b - arm)),
    ];
    let mut builder = PathBuilder::new();
    for (start, before_bend, after_bend, end) in corners {
        builder.move_to(start).line_to(before_bend).arc_to(bend, bend, 0.0, false, true, after_bend).line_to(end);
    }
    builder.build(gfx).ok()
}

/// Brackets from filled pixel rectangles inside `frame` (outer edge), so 16–24 px icons stay razor sharp.
fn crisp_brackets(p: &mut Painter, frame: RectF, thickness: f32, arm: f32, color: Color) {
    let (l, t, r, b) = (frame.x, frame.y, frame.right(), frame.bottom());
    for rect in [
        RectF::new(l, t, arm, thickness),
        RectF::new(l, t, thickness, arm),
        RectF::new(r - arm, t, arm, thickness),
        RectF::new(r - thickness, t, thickness, arm),
        RectF::new(l, b - thickness, arm, thickness),
        RectF::new(l, b - arm, thickness, arm),
        RectF::new(r - arm, b - thickness, arm, thickness),
        RectF::new(r - thickness, b - arm, thickness, arm),
    ] {
        p.fill_rect(rect, color);
    }
}

/// Pixel-snapped glyph (brackets + sparkle) for icons smaller than `SMALL_PX`; `frame` is the outer bracket box.
fn paint_small_glyph(p: &mut Painter, frame: RectF, thickness_px: f32, color: Color) {
    let px = p.px();
    let frame_px = (frame.w / px).round();
    let thickness = thickness_px * px;
    let arm = (frame_px * 0.34).round().max(thickness_px + 1.0) * px;
    crisp_brackets(p, frame, thickness, arm, color);
    let (spot, radius) = sparkle_spot(frame, true);
    let center = PointF::new((spot.x / px).floor() * px + px * 0.5, (spot.y / px).floor() * px + px * 0.5);
    let radius = ((radius / px).round().max(2.0) + 0.5) * px;
    if let Some(sparkle) = sparkle_path(p.gfx(), center, radius) {
        p.fill_path(&sparkle, color);
    }
}

/// The app icon filling the square `rect` (DIP or px); detail follows the physical size.
pub fn paint_app_icon(p: &mut Painter, rect: RectF) {
    let size_px = (rect.w * p.scale()).round();
    if size_px < SMALL_PX {
        paint_small_app_icon(p, rect, size_px);
    } else {
        paint_detailed_app_icon(p, rect, size_px);
    }
}

fn paint_detailed_app_icon(p: &mut Painter, rect: RectF, size_px: f32) {
    let size = rect.w;
    let full = size_px >= FULL_DETAIL_PX;
    let tile = if full { rect.inset(size * 0.06) } else { p.snap_rect(rect.inset(size * 0.03)) };
    let radius = tile.w * 0.225;
    if full {
        p.shadow(tile, radius, &Shadow::new(size * 0.012, size * 0.05, Color::rgba(0.06, 0.02, 0.28, 0.38)));
    }
    p.fill_round_rect(tile, radius, gradient(tile));
    let sheen = Brush::linear(
        PointF::new(tile.x, tile.y),
        PointF::new(tile.x, tile.y + tile.h * 0.6),
        &[(0.0, Color::rgba(1.0, 1.0, 1.0, 0.24)), (1.0, Color::rgba(1.0, 1.0, 1.0, 0.0))],
    );
    p.fill_round_rect(tile, radius, sheen);
    let rim = (size / 256.0).max(p.px());
    p.stroke_round_rect(tile.inset(rim / 2.0), radius - rim / 2.0, Color::rgba(1.0, 1.0, 1.0, 0.18), rim);

    let frame_size = tile.w * 0.54;
    let frame = RectF::new(tile.center().x - frame_size / 2.0, tile.center().y - frame_size / 2.0, frame_size, frame_size);
    let stroke = tile.w * if full { 0.062 } else { 0.075 };
    let arm = frame_size * 0.3;
    if let Some(brackets) = bracket_path(p.gfx(), frame, arm, stroke * 0.9) {
        if full {
            let depth = Color::rgba(0.05, 0.02, 0.25, 0.22);
            p.translate(0.0, size * 0.008, |p| p.stroke_path(&brackets, depth, stroke, &StrokeStyle::round()));
        }
        p.stroke_path(&brackets, Color::WHITE, stroke, &StrokeStyle::round());
    }
    let (center, radius) = sparkle_spot(frame, false);
    if full {
        let glow = radius * 0.55;
        let glow_rect = RectF::new(center.x - glow, center.y - glow, glow * 2.0, glow * 2.0);
        p.shadow(glow_rect, glow, &Shadow::new(0.0, radius * 1.3, Color::rgba(1.0, 1.0, 1.0, 0.55)));
    }
    if let Some(sparkle) = sparkle_path(p.gfx(), center, radius) {
        p.fill_path(&sparkle, Color::WHITE);
    }
}

fn paint_small_app_icon(p: &mut Painter, rect: RectF, size_px: f32) {
    let px = p.px();
    let tile = p.snap_rect(rect);
    p.fill_round_rect(tile, (size_px * 0.22).round() * px, gradient(tile));
    let inset = (size_px * 0.19).round() * px;
    let frame = p.snap_rect(tile.inset(inset));
    paint_small_glyph(p, frame, (size_px / 10.0).round().clamp(1.0, 2.0), Color::WHITE);
}

/// Monochrome brackets + sparkle for the notification area, filling `rect` with a one-pixel margin.
pub fn paint_tray_glyph(p: &mut Painter, rect: RectF, color: Color) {
    let px = p.px();
    let size_px = (rect.w / px).round();
    let thickness_px = (size_px / 14.0).round().max(1.0);
    let frame = p.snap_rect(rect.inset((size_px / 16.0).round().max(1.0) * px));
    if size_px < SMALL_PX {
        paint_small_glyph(p, frame, thickness_px, color);
        return;
    }
    let stroke = thickness_px * px;
    if let Some(brackets) = bracket_path(p.gfx(), frame.inset(stroke / 2.0), frame.w * 0.32, stroke) {
        p.stroke_path(&brackets, color, stroke, &StrokeStyle::round());
    }
    let (center, radius) = sparkle_spot(frame, false);
    if let Some(sparkle) = sparkle_path(p.gfx(), p.snap_point(center), radius) {
        p.fill_path(&sparkle, color);
    }
}

/// The app icon at exactly `size_px` × `size_px`.
pub fn render_app_icon(gfx: &Rc<Gfx>, size_px: u32) -> Result<Image> {
    let spec = OffscreenSpec::pixels(size_px, size_px, 1.0, Theme::dark());
    let side = size_px as f32;
    render_offscreen(gfx, &spec, |_, p| paint_app_icon(p, RectF::new(0.0, 0.0, side, side)))
}

/// The tray glyph at exactly `size_px` × `size_px` in `color`.
pub fn render_tray_glyph(gfx: &Rc<Gfx>, size_px: u32, color: Color) -> Result<Image> {
    let spec = OffscreenSpec::pixels(size_px, size_px, 1.0, Theme::dark());
    let side = size_px as f32;
    render_offscreen(gfx, &spec, |_, p| paint_tray_glyph(p, RectF::new(0.0, 0.0, side, side), color))
}
