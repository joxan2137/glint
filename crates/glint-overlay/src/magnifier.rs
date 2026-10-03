//! Pixel loupe (DESIGN §6): 112 DIP circle, 8× nearest-neighbour cells on the physical pixel grid, faint grid,
//! outlined center pixel and a coordinate/color pill below.

use glint_core::{Image, PointF, PointI, RectF, RectI};
use glint_ui::{Bitmap, Color, Interpolation, Painter, Shadow, TextStyle, Weight};

pub const DIAMETER: f32 = 112.0;
/// Distance from the cursor to the circle's edge, diagonally.
pub const OFFSET: f32 = 20.0;
/// Room the info pill needs under the circle.
pub const INFO_SPACE: f32 = 8.0 + 24.0;
const CELL_DIP: f32 = 8.0;
const INFO_WIDTH: f32 = 200.0;

/// Area the loupe and its pill cover when centered at `center` (for keeping other labels clear of it).
pub fn extent(center: PointF) -> RectF {
    let w = INFO_WIDTH.max(DIAMETER);
    RectF::new(center.x - w / 2.0, center.y - DIAMETER / 2.0, w, DIAMETER + INFO_SPACE)
}

pub struct Loupe<'a> {
    pub source: &'a Bitmap,
    pub image: &'a Image,
    /// Sampled pixel in `image` coordinates (monitor-local physical px).
    pub pixel: PointI,
}

impl Loupe<'_> {
    pub fn color(&self) -> Color {
        let x = self.pixel.x.clamp(0, self.image.width as i32 - 1) as u32;
        let y = self.pixel.y.clamp(0, self.image.height as i32 - 1) as u32;
        Color::from_bgra8(self.image.pixel(x, y))
    }

    pub fn paint(&self, p: &mut Painter, center: PointF) {
        let scale = p.scale();
        let px = p.px();
        let cell_px = (CELL_DIP * scale).round().max(1.0);
        let cell = cell_px / scale;
        let radius = DIAMETER / 2.0;
        let center_cell = RectF::new(
            ((center.x * scale - cell_px / 2.0).round()) / scale,
            ((center.y * scale - cell_px / 2.0).round()) / scale,
            cell,
            cell,
        );
        let middle = center_cell.center();
        let circle = RectF::new(middle.x - radius, middle.y - radius, DIAMETER, DIAMETER);
        let reach = (radius / cell).ceil() as i32 + 1;
        let cells = 2 * reach + 1;
        let grid = PointF::new(center_cell.x - reach as f32 * cell, center_cell.y - reach as f32 * cell);
        let src = RectI::new(self.pixel.x - reach, self.pixel.y - reach, cells, cells);

        p.shadow(circle, radius, &Shadow::new(6.0, 24.0, Color::rgba(0.0, 0.0, 0.0, 0.45)));
        p.clip_round_rect(circle, radius, |p| {
            p.fill_rect(circle, Color::rgb8(0x1C, 0x1C, 0x1E));
            if let Some(visible) = src.intersect(&self.image.bounds()) {
                let dest = RectF::new(
                    grid.x + (visible.x - src.x) as f32 * cell,
                    grid.y + (visible.y - src.y) as f32 * cell,
                    visible.w as f32 * cell,
                    visible.h as f32 * cell,
                );
                p.bitmap(self.source, dest, Some(visible.to_f()), 1.0, Interpolation::Nearest);
            }
            let line = Color::rgba(0.5, 0.5, 0.5, 0.28);
            for i in 0..=cells {
                let offset = i as f32 * cell;
                p.fill_rect(RectF::new(grid.x + offset, circle.y, px, DIAMETER), line);
                p.fill_rect(RectF::new(circle.x, grid.y + offset, DIAMETER, px), line);
            }
            p.stroke_rect(center_cell.inset(-px * 1.5), Color::rgba(0.0, 0.0, 0.0, 0.75), px);
            p.stroke_rect(center_cell.inset(-px * 0.5), Color::WHITE, px);
        });
        p.stroke_ellipse(middle, radius - 0.75, radius - 0.75, Color::rgba(1.0, 1.0, 1.0, 0.92), 1.5);
        p.stroke_ellipse(middle, radius + px / 2.0, radius + px / 2.0, Color::rgba(0.0, 0.0, 0.0, 0.40), px);

        paint_info(p, self.color(), self.pixel, PointF::new(middle.x, circle.bottom() + 8.0 + INFO_HEIGHT / 2.0));
    }
}

const INFO_HEIGHT: f32 = 24.0;
const CHIP_RADIUS: f32 = 5.5;
const EDGE: f32 = 8.0;

/// Glass pill `X 1234  Y 567  ● #RRGGBB`: values in primary text, axis labels in secondary.
fn paint_info(p: &mut Painter, color: Color, pixel: PointI, center: PointF) {
    let theme = p.theme().clone();
    let value = TextStyle::new(12.0).weight(Weight::Semibold).tabular();
    let label = TextStyle::new(12.0).weight(Weight::Medium);
    let (x, y, hex) = (pixel.x.to_string(), pixel.y.to_string(), color.to_hex());
    let chip_slot = 2.0 * CHIP_RADIUS + 5.0;
    let runs: [(&str, &TextStyle, Color, f32); 5] = [
        ("X", &label, theme.text_secondary, 0.0),
        (&x, &value, theme.text, 4.0),
        ("Y", &label, theme.text_secondary, 10.0),
        (&y, &value, theme.text, 4.0),
        (&hex, &value, theme.text, 12.0 + chip_slot),
    ];
    let widths: Vec<f32> = runs.iter().map(|(text, style, ..)| p.measure(text, style).w).collect();
    let content: f32 = runs.iter().zip(&widths).map(|((.., gap), w)| gap + w).sum();
    let width = (content + 20.0).ceil();
    let left = (center.x - width / 2.0).clamp(EDGE, (p.size().w - EDGE - width).max(EDGE));
    let rect = p.snap_rect(RectF::new(left, center.y - INFO_HEIGHT / 2.0, width, INFO_HEIGHT));
    let radius = INFO_HEIGHT / 2.0;
    p.shadow_outside(rect, radius, &Shadow::new(2.0, 8.0, Color::rgba(0.0, 0.0, 0.0, 0.35)));
    p.fill_round_rect(rect, radius, theme.glass_fill_solid);
    p.hairline_round_rect(rect, radius, theme.hairline, true);
    p.hairline_round_rect(rect, radius, theme.outer_border, false);
    let mut cursor = rect.x + 10.0;
    for ((text, style, ink, gap), w) in runs.iter().zip(&widths) {
        cursor += gap;
        p.text(text, style, *ink, RectF::new(cursor, rect.y, w + 1.0, rect.h));
        cursor += w;
    }
    let chip = PointF::new(cursor - widths[4] - chip_slot + CHIP_RADIUS, rect.center().y);
    p.fill_circle(chip, CHIP_RADIUS, color);
    p.stroke_ellipse(chip, CHIP_RADIUS - p.px() / 2.0, CHIP_RADIUS - p.px() / 2.0, Color::rgba(1.0, 1.0, 1.0, 0.35), p.px());
}
